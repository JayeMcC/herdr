//! Per-phase timing for the server loop and render path.
//!
//! A [`PhaseGuard`] times one phase for one pane (or none). Durations land in
//! process-wide atomic histograms over the same rolling five-minute window as
//! input latency, with the pane that set each slot's maximum. On the server
//! loop thread the guard also publishes the phase to the stall watchdog; a
//! phase slower than [`SLOW_PHASE`] logs one `latency.slow_phase` line naming
//! the pane, which is the attribution for "pane X cost N ms in phase Y".
//!
//! Nothing here allocates or locks. A minute rollover can race with a
//! concurrent record and lose that one sample; the stats are a measurement,
//! not an account.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::{bucket_index, bucket_upper, Summary, BUCKETS, SLOT_SECS, UNKNOWN_PANE, WINDOW_SLOTS};

/// A loop-thread phase slower than this logs one line naming its pane.
pub(crate) const SLOW_PHASE: Duration = Duration::from_millis(50);

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    /// Draining internal app events.
    Events,
    /// Handling API socket requests.
    Api,
    /// Handling client transport events (input, resize, connect).
    ClientEvents,
    /// Scheduled tasks (saves, git refresh, timers).
    Scheduled,
    /// One whole render attempt, all clients.
    Render,
    /// Building the client shell session snapshot.
    Snapshot,
    /// Reading one pane's working directories.
    Cwd,
    /// Building one pane's agent info for the snapshot.
    AgentInfo,
    /// One background agent-detection pass for one pane (worker thread).
    Detect,
    /// Tab surface geometry.
    Layout,
    /// Drawing one pane into the virtual frame.
    Draw,
    /// Serializing and queuing one client's frame.
    Send,
}

impl Phase {
    pub(crate) const ALL: [Phase; 12] = [
        Phase::Events,
        Phase::Api,
        Phase::ClientEvents,
        Phase::Scheduled,
        Phase::Render,
        Phase::Snapshot,
        Phase::Cwd,
        Phase::AgentInfo,
        Phase::Detect,
        Phase::Layout,
        Phase::Draw,
        Phase::Send,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Phase::Events => "events",
            Phase::Api => "api",
            Phase::ClientEvents => "client_events",
            Phase::Scheduled => "scheduled",
            Phase::Render => "render",
            Phase::Snapshot => "snapshot",
            Phase::Cwd => "cwd",
            Phase::AgentInfo => "agent_info",
            Phase::Detect => "detect",
            Phase::Layout => "layout",
            Phase::Draw => "draw",
            Phase::Send => "send",
        }
    }

    pub(crate) fn from_index(index: u32) -> Option<Phase> {
        Phase::ALL.get(index as usize).copied()
    }
}

static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

pub(crate) fn epoch() -> Instant {
    *EPOCH.get_or_init(Instant::now)
}

pub(crate) fn us_since_epoch(at: Instant) -> u64 {
    at.saturating_duration_since(epoch()).as_micros() as u64
}

struct AtomicSlot {
    minute: AtomicU64,
    count: AtomicU32,
    max_us: AtomicU64,
    max_pane: AtomicU32,
    buckets: [AtomicU32; BUCKETS],
}

impl AtomicSlot {
    #[allow(clippy::declare_interior_mutable_const)] // array-repeat initializer only
    const EMPTY: AtomicSlot = AtomicSlot {
        minute: AtomicU64::new(u64::MAX),
        count: AtomicU32::new(0),
        max_us: AtomicU64::new(0),
        max_pane: AtomicU32::new(UNKNOWN_PANE),
        buckets: [const { AtomicU32::new(0) }; BUCKETS],
    };

    fn reset(&self, minute: u64) {
        let seen = self.minute.load(Ordering::Acquire);
        if seen == minute {
            return;
        }
        if self
            .minute
            .compare_exchange(seen, minute, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.count.store(0, Ordering::Relaxed);
            self.max_us.store(0, Ordering::Relaxed);
            self.max_pane.store(UNKNOWN_PANE, Ordering::Relaxed);
            for bucket in &self.buckets {
                bucket.store(0, Ordering::Relaxed);
            }
        }
    }
}

pub(crate) struct PhaseStats {
    slots: [[AtomicSlot; WINDOW_SLOTS]; Phase::ALL.len()],
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub(crate) struct PhaseRow {
    pub(crate) phase: Phase,
    #[serde(flatten)]
    pub(crate) summary: Summary,
    /// The pane that took longest in this phase within the window, if the
    /// phase is per pane.
    pub(crate) max_pane: Option<u32>,
}

impl PhaseStats {
    #[allow(clippy::declare_interior_mutable_const)] // array-repeat initializer only
    const ROW: [AtomicSlot; WINDOW_SLOTS] = [const { AtomicSlot::EMPTY }; WINDOW_SLOTS];

    pub(crate) const fn new() -> Self {
        Self {
            slots: [Self::ROW; Phase::ALL.len()],
        }
    }

    pub(crate) fn record(&self, phase: Phase, pane: u32, at_us: u64, us: u64) {
        let minute = at_us / (SLOT_SECS * 1_000_000);
        let slot = &self.slots[phase as usize][(minute % WINDOW_SLOTS as u64) as usize];
        slot.reset(minute);
        slot.count.fetch_add(1, Ordering::Relaxed);
        slot.buckets[bucket_index(us)].fetch_add(1, Ordering::Relaxed);
        if slot.max_us.fetch_max(us, Ordering::Relaxed) < us {
            slot.max_pane.store(pane, Ordering::Relaxed);
        }
    }

    pub(crate) fn report(&self, now_us: u64) -> Vec<PhaseRow> {
        let minute = now_us / (SLOT_SECS * 1_000_000);
        let oldest = minute.saturating_sub(WINDOW_SLOTS as u64 - 1);
        let mut rows = Vec::new();
        for phase in Phase::ALL {
            let mut buckets = [0u64; BUCKETS];
            let mut count = 0u64;
            let mut max_us = 0u64;
            let mut max_pane = UNKNOWN_PANE;
            for slot in &self.slots[phase as usize] {
                let slot_minute = slot.minute.load(Ordering::Acquire);
                if slot_minute == u64::MAX || slot_minute < oldest || slot_minute > minute {
                    continue;
                }
                count += u64::from(slot.count.load(Ordering::Relaxed));
                let slot_max = slot.max_us.load(Ordering::Relaxed);
                if slot_max >= max_us {
                    max_us = slot_max;
                    max_pane = slot.max_pane.load(Ordering::Relaxed);
                }
                for (total, bucket) in buckets.iter_mut().zip(slot.buckets.iter()) {
                    *total += u64::from(bucket.load(Ordering::Relaxed));
                }
            }
            if count == 0 {
                continue;
            }
            let quantile = |q: f64| {
                let rank = ((q * count as f64).ceil() as u64).max(1);
                let mut seen = 0u64;
                for (index, value) in buckets.iter().enumerate() {
                    seen += value;
                    if seen >= rank {
                        return bucket_upper(index).min(max_us);
                    }
                }
                max_us
            };
            rows.push(PhaseRow {
                phase,
                summary: Summary {
                    count,
                    p50_us: quantile(0.50),
                    p95_us: quantile(0.95),
                    max_us,
                },
                max_pane: (max_pane != UNKNOWN_PANE).then_some(max_pane),
            });
        }
        rows
    }
}

pub(crate) static PHASES: PhaseStats = PhaseStats::new();

/// Times one phase until dropped.
pub(crate) struct PhaseGuard {
    phase: Phase,
    pane: u32,
    started: Instant,
    /// Set on the loop thread only: the watchdog slot this guard replaced,
    /// restored on drop so nested phases hand the slot back to their parent.
    previous: Option<Option<super::watchdog::Published>>,
}

/// Starts timing `phase` for `pane` (use [`UNKNOWN_PANE`] when the phase is
/// not per pane).
pub(crate) fn enter(phase: Phase, pane: u32) -> PhaseGuard {
    let started = Instant::now();
    let previous = super::watchdog::publish(phase, pane, started);
    PhaseGuard {
        phase,
        pane,
        started,
        previous,
    }
}

impl Drop for PhaseGuard {
    fn drop(&mut self) {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.started);
        if let Some(previous) = self.previous.take() {
            super::watchdog::restore(previous);
        }
        PHASES.record(
            self.phase,
            self.pane,
            us_since_epoch(now),
            elapsed.as_micros() as u64,
        );
        if elapsed >= SLOW_PHASE && super::watchdog::on_loop_thread() {
            let pane = (self.pane != UNKNOWN_PANE).then_some(self.pane);
            tracing::warn!(
                event = "latency.slow_phase",
                phase = self.phase.name(),
                pane = ?pane,
                elapsed_ms = elapsed.as_millis() as u64,
                "server loop phase was slow"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_quantiles_and_the_slowest_pane() {
        let stats = PhaseStats::new();
        for (pane, us) in [(1, 100), (2, 200), (3, 50_000), (4, 300)] {
            stats.record(Phase::Cwd, pane, 1_000, us);
        }
        let rows = stats.report(2_000);
        assert_eq!(rows.len(), 1);
        let row = rows[0];
        assert_eq!(row.phase, Phase::Cwd);
        assert_eq!(row.summary.count, 4);
        assert_eq!(row.summary.max_us, 50_000);
        assert_eq!(row.max_pane, Some(3));
    }

    #[test]
    fn window_drops_old_minutes() {
        let stats = PhaseStats::new();
        stats.record(Phase::Draw, 1, 0, 10);
        let six_minutes = 6 * 60 * 1_000_000;
        assert!(stats.report(six_minutes).is_empty());
        stats.record(Phase::Draw, 1, six_minutes, 10);
        assert_eq!(stats.report(six_minutes)[0].summary.count, 1);
    }

    #[test]
    fn phase_stats_fit_the_memory_budget() {
        let size = std::mem::size_of::<PhaseStats>();
        assert!(size < 64 * 1024, "phase stats are {size} bytes");
    }
}
