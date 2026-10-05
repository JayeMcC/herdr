//! Server loop stall watchdog.
//!
//! The server loop thread publishes the phase it is in (phase, pane, start)
//! into one slot. A watchdog thread polls the slot and, when one phase has
//! held the loop longer than [`STALL`], logs one `latency.stall` warning that
//! names the phase and pane while the loop is still stuck, so a frozen server
//! says why before it recovers (or never does).
//!
//! The slot is a seqlock over three atomics: only the loop thread writes, so
//! publishing is a handful of relaxed stores and two release increments.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::phase::{us_since_epoch, Phase};
use super::UNKNOWN_PANE;

/// A phase holding the loop longer than this is a stall.
pub(crate) const STALL: Duration = Duration::from_millis(250);
const POLL: Duration = Duration::from_millis(50);
const IDLE: u64 = u64::MAX;

/// What the loop thread is doing, as published to the watchdog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Published {
    pub(crate) phase: Phase,
    pub(crate) pane: u32,
    pub(crate) started_us: u64,
}

/// One phase that has held the loop past [`STALL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stall {
    pub(crate) phase: Phase,
    pub(crate) pane: Option<u32>,
    pub(crate) stuck_ms: u64,
}

pub(crate) struct LoopSlot {
    seq: AtomicU64,
    /// `phase << 32 | pane`, or [`IDLE`].
    what: AtomicU64,
    started_us: AtomicU64,
}

impl LoopSlot {
    pub(crate) const fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            what: AtomicU64::new(IDLE),
            started_us: AtomicU64::new(0),
        }
    }

    fn write(&self, what: u64, started_us: u64) {
        self.seq.fetch_add(1, Ordering::AcqRel);
        self.what.store(what, Ordering::Release);
        self.started_us.store(started_us, Ordering::Release);
        self.seq.fetch_add(1, Ordering::AcqRel);
    }

    /// Publishes `published`; returns what was there before.
    pub(crate) fn publish(&self, published: Published) -> Option<Published> {
        let previous = self.read_own();
        self.write(
            (u64::from(published.phase as u32) << 32) | u64::from(published.pane),
            published.started_us,
        );
        previous
    }

    pub(crate) fn restore(&self, previous: Option<Published>) {
        match previous {
            Some(previous) => {
                self.publish(previous);
            }
            None => self.write(IDLE, 0),
        }
    }

    /// Reader side for the writer thread itself; no torn reads possible.
    fn read_own(&self) -> Option<Published> {
        Self::decode(
            self.what.load(Ordering::Relaxed),
            self.started_us.load(Ordering::Relaxed),
        )
    }

    fn decode(what: u64, started_us: u64) -> Option<Published> {
        if what == IDLE {
            return None;
        }
        Some(Published {
            phase: Phase::from_index((what >> 32) as u32)?,
            pane: what as u32,
            started_us,
        })
    }

    /// Consistent snapshot from another thread, or `None` when idle.
    pub(crate) fn read(&self) -> Option<Published> {
        loop {
            let before = self.seq.load(Ordering::Acquire);
            if before % 2 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let what = self.what.load(Ordering::Acquire);
            let started_us = self.started_us.load(Ordering::Acquire);
            if self.seq.load(Ordering::Acquire) == before {
                return Self::decode(what, started_us);
            }
        }
    }
}

/// Decides whether the published phase is a new stall. `last_warned` holds
/// the start of the last phase already reported, so one stall logs once.
pub(crate) fn check(
    published: Option<Published>,
    now_us: u64,
    last_warned: &mut Option<(Phase, u32, u64)>,
) -> Option<Stall> {
    let published = published?;
    let stuck_us = now_us.saturating_sub(published.started_us);
    if stuck_us < STALL.as_micros() as u64 {
        return None;
    }
    let key = (published.phase, published.pane, published.started_us);
    if *last_warned == Some(key) {
        return None;
    }
    *last_warned = Some(key);
    Some(Stall {
        phase: published.phase,
        pane: (published.pane != UNKNOWN_PANE).then_some(published.pane),
        stuck_ms: stuck_us / 1_000,
    })
}

static SLOT: LoopSlot = LoopSlot::new();
static STARTED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static LOOP_THREAD: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn on_loop_thread() -> bool {
    LOOP_THREAD.with(Cell::get)
}

/// Marks the calling thread as the server loop and starts the watchdog
/// thread once per process.
pub(crate) fn start_for_loop_thread() {
    LOOP_THREAD.with(|flag| flag.set(true));
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("twodr-loop-watchdog".into())
        .spawn(|| run(&SLOT, |stall| log_stall(&stall)));
    if let Err(err) = spawned {
        tracing::warn!(err = %err, "failed to start server loop watchdog");
    }
}

fn log_stall(stall: &Stall) {
    tracing::warn!(
        event = "latency.stall",
        phase = stall.phase.name(),
        pane = ?stall.pane,
        stuck_ms = stall.stuck_ms,
        "server loop is stuck"
    );
}

fn run(slot: &LoopSlot, mut report: impl FnMut(Stall)) -> ! {
    let mut last_warned = None;
    loop {
        std::thread::sleep(POLL);
        if let Some(stall) = check(
            slot.read(),
            us_since_epoch(Instant::now()),
            &mut last_warned,
        ) {
            report(stall);
        }
    }
}

pub(super) fn publish(phase: Phase, pane: u32, started: Instant) -> Option<Option<Published>> {
    on_loop_thread().then(|| {
        SLOT.publish(Published {
            phase,
            pane,
            started_us: us_since_epoch(started),
        })
    })
}

pub(super) fn restore(previous: Option<Published>) {
    SLOT.restore(previous);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Arc};

    fn published(phase: Phase, pane: u32, started_us: u64) -> Published {
        Published {
            phase,
            pane,
            started_us,
        }
    }

    #[test]
    fn nested_phases_restore_their_parent() {
        let slot = LoopSlot::new();
        assert_eq!(slot.read(), None);
        let outer = slot.publish(published(Phase::Render, UNKNOWN_PANE, 10));
        assert_eq!(outer, None);
        let inner = slot.publish(published(Phase::Cwd, 7, 20));
        assert_eq!(slot.read(), Some(published(Phase::Cwd, 7, 20)));
        slot.restore(inner);
        assert_eq!(
            slot.read(),
            Some(published(Phase::Render, UNKNOWN_PANE, 10))
        );
        slot.restore(outer);
        assert_eq!(slot.read(), None);
    }

    #[test]
    fn stall_reports_once_per_stuck_phase() {
        let mut last = None;
        let cwd = Some(published(Phase::Cwd, 7, 0));
        assert_eq!(check(cwd, 100_000, &mut last), None);
        assert_eq!(
            check(cwd, 300_000, &mut last),
            Some(Stall {
                phase: Phase::Cwd,
                pane: Some(7),
                stuck_ms: 300,
            })
        );
        assert_eq!(check(cwd, 900_000, &mut last), None);
        // The same phase entered again later is a new stall.
        let again = Some(published(Phase::Cwd, 7, 1_000_000));
        assert!(check(again, 1_300_000, &mut last).is_some());
        assert_eq!(check(None, 2_000_000, &mut last), None);
        let unpaned = Some(published(Phase::Api, UNKNOWN_PANE, 0));
        assert_eq!(check(unpaned, 3_000_000, &mut last).unwrap().pane, None);
    }

    #[test]
    fn watchdog_names_phase_and_pane_while_the_loop_is_stuck() {
        let slot: &'static LoopSlot = Box::leak(Box::new(LoopSlot::new()));
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            run(slot, move |stall| {
                let _ = tx.send(stall);
            })
        });

        let started = Instant::now();
        slot.publish(published(Phase::Cwd, 42, us_since_epoch(started)));
        // Hold the "loop" stuck; the report must arrive before we let go.
        let stall = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("watchdog reports the stall while stuck");
        let held = started.elapsed();
        assert_eq!(stall.phase, Phase::Cwd);
        assert_eq!(stall.pane, Some(42));
        assert!(stall.stuck_ms >= 250, "{stall:?}");
        assert!(held < Duration::from_millis(600), "reported late: {held:?}");
        slot.restore(None);
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn reader_never_sees_a_torn_slot() {
        let slot = Arc::new(LoopSlot::new());
        let writer = {
            let slot = slot.clone();
            std::thread::spawn(move || {
                for i in 0..20_000u64 {
                    // pane always equals started_us so a torn read is visible.
                    slot.publish(published(Phase::Draw, i as u32, i));
                }
            })
        };
        while !writer.is_finished() {
            if let Some(seen) = slot.read() {
                assert_eq!(u64::from(seen.pane), seen.started_us);
            }
        }
        writer.join().expect("writer");
    }

    #[test]
    fn phase_guard_publishes_only_on_the_loop_thread() {
        let handle = std::thread::spawn(|| {
            assert!(!on_loop_thread());
            assert!(publish(Phase::Detect, 1, Instant::now()).is_none());
        });
        handle.join().expect("worker thread");
    }
}
