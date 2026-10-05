//! Always-on input latency meter.
//!
//! Every client input is stamped four times: when the client transport thread
//! receives it (`arrive`), when the server loop picks it up (`dequeue`), when
//! its bytes are handed to the pane's PTY writer (`pty`), and when the first
//! frame carrying that pane's render after the PTY write is queued for the
//! same client (`frame`). Completed inputs land in fixed-bucket histograms over
//! a rolling five-minute window, per input kind and per pane.
//!
//! The meter is owned by the server loop thread, so it needs no locks. Nothing
//! on the per-input path allocates: in-flight inputs, histograms and the pane
//! table are fixed-size arrays sized once at startup.

use std::time::{Duration, Instant};

pub(crate) mod phase;
pub(crate) mod watchdog;

pub(crate) use phase::{enter as enter_phase, Phase};

use crate::protocol::{ClientKeyCode, ClientKeyKind, ClientMouseKind, ClientPaneInputEvent};

/// Keys arriving closer together than this on the same pane are a dictation
/// (or fast typing) burst rather than individual keystrokes.
pub(crate) const BURST_GAP: Duration = Duration::from_millis(30);
/// Inputs slower than this end to end are logged as `latency.slow`.
pub(crate) const SLOW_INPUT: Duration = Duration::from_millis(100);
/// Pane id used for inputs whose pane is not known (direct terminal attach).
pub(crate) const UNKNOWN_PANE: u32 = u32::MAX;

const WINDOW_SLOTS: usize = 5;
const SLOT_SECS: u64 = 60;
const SUB_BUCKETS: u64 = 4;
const BUCKETS: usize = 96;
const PENDING_CAP: usize = 64;
const PANE_SLOTS: usize = 32;
const FRAME_SOURCE_CAP: usize = 64;
const PENDING_TTL_US: u64 = 10_000_000;

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InputKind {
    Key,
    Burst,
    Paste,
    Scroll,
    Enter,
}

impl InputKind {
    pub(crate) const ALL: [InputKind; 5] = [
        InputKind::Key,
        InputKind::Burst,
        InputKind::Paste,
        InputKind::Scroll,
        InputKind::Enter,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            InputKind::Key => "key",
            InputKind::Burst => "burst",
            InputKind::Paste => "paste",
            InputKind::Scroll => "scroll",
            InputKind::Enter => "enter",
        }
    }

    fn rank(self) -> u8 {
        match self {
            InputKind::Key => 0,
            InputKind::Burst => 1,
            InputKind::Scroll => 2,
            InputKind::Enter => 3,
            InputKind::Paste => 4,
        }
    }

    fn strongest(current: Option<Self>, next: Self) -> Option<Self> {
        Some(match current {
            Some(current) if current.rank() >= next.rank() => current,
            _ => next,
        })
    }
}

/// One stage of an input's trip through the server.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Segment {
    /// Transport thread receipt to server loop pickup.
    Queue,
    /// Server loop pickup to PTY writer handoff.
    Handle,
    /// PTY writer handoff to the first frame queued for the client.
    Present,
    /// Transport thread receipt to the first frame queued for the client.
    Total,
}

impl Segment {
    pub(crate) const ALL: [Segment; 4] = [
        Segment::Queue,
        Segment::Handle,
        Segment::Present,
        Segment::Total,
    ];
}

/// Classifies one targeted pane input batch. Releases, mouse motion and clicks
/// are not latency-relevant and yield `None`.
pub(crate) fn classify_pane_events(events: &[ClientPaneInputEvent]) -> Option<InputKind> {
    let mut kind = None;
    for event in events {
        let next = match event {
            ClientPaneInputEvent::Paste(_) => InputKind::Paste,
            ClientPaneInputEvent::TextCommit(text) => {
                if text.chars().nth(1).is_some() {
                    InputKind::Burst
                } else {
                    InputKind::Key
                }
            }
            ClientPaneInputEvent::Key {
                kind: ClientKeyKind::Release,
                ..
            } => continue,
            ClientPaneInputEvent::Key { code, .. } => match code {
                ClientKeyCode::Enter => InputKind::Enter,
                ClientKeyCode::PageUp | ClientKeyCode::PageDown => InputKind::Scroll,
                _ => InputKind::Key,
            },
            ClientPaneInputEvent::Mouse { kind, .. } => match kind {
                ClientMouseKind::ScrollUp
                | ClientMouseKind::ScrollDown
                | ClientMouseKind::ScrollLeft
                | ClientMouseKind::ScrollRight => InputKind::Scroll,
                _ => continue,
            },
        };
        kind = InputKind::strongest(kind, next);
    }
    kind
}

/// Classifies raw input bytes from a direct terminal attach client.
pub(crate) fn classify_raw_input(data: &[u8]) -> Option<InputKind> {
    if data.is_empty() {
        return None;
    }
    if data.starts_with(b"\x1b[200~") {
        return Some(InputKind::Paste);
    }
    if data.contains(&b'\r') {
        return Some(InputKind::Enter);
    }
    if data.len() > 1 && !data.contains(&0x1b) {
        return Some(InputKind::Burst);
    }
    Some(InputKind::Key)
}

fn bucket_index(us: u64) -> usize {
    if us < SUB_BUCKETS {
        return us as usize;
    }
    let octave = 63 - u64::from(us.leading_zeros());
    let sub = (us >> (octave - 2)) & (SUB_BUCKETS - 1);
    (((octave - 1) * SUB_BUCKETS + sub) as usize).min(BUCKETS - 1)
}

/// Largest value that lands in `index`; quantiles report this bound.
fn bucket_upper(index: usize) -> u64 {
    let index = index as u64;
    if index < SUB_BUCKETS {
        return index;
    }
    let octave = index / SUB_BUCKETS + 1;
    let sub = index % SUB_BUCKETS;
    let width = 1u64 << (octave - 2);
    ((SUB_BUCKETS + sub) << (octave - 2)) + width - 1
}

#[derive(Clone, Copy)]
struct Slot {
    minute: u64,
    count: u32,
    max_us: u64,
    buckets: [u32; BUCKETS],
}

impl Slot {
    const EMPTY: Slot = Slot {
        minute: u64::MAX,
        count: 0,
        max_us: 0,
        buckets: [0; BUCKETS],
    };
}

/// Fixed-bucket log-linear histogram (four sub-buckets per power of two,
/// about 19% relative error) over five one-minute slots.
#[derive(Clone, Copy)]
struct Histogram {
    slots: [Slot; WINDOW_SLOTS],
}

impl Histogram {
    const EMPTY: Histogram = Histogram {
        slots: [Slot::EMPTY; WINDOW_SLOTS],
    };

    fn record(&mut self, minute: u64, us: u64) {
        let slot = &mut self.slots[(minute % WINDOW_SLOTS as u64) as usize];
        if slot.minute != minute {
            *slot = Slot::EMPTY;
            slot.minute = minute;
        }
        slot.count = slot.count.saturating_add(1);
        slot.max_us = slot.max_us.max(us);
        let bucket = &mut slot.buckets[bucket_index(us)];
        *bucket = bucket.saturating_add(1);
    }

    fn summary(&self, minute: u64) -> Option<Summary> {
        let oldest = minute.saturating_sub(WINDOW_SLOTS as u64 - 1);
        let mut buckets = [0u64; BUCKETS];
        let mut count = 0u64;
        let mut max_us = 0u64;
        for slot in &self.slots {
            if slot.minute == u64::MAX || slot.minute < oldest || slot.minute > minute {
                continue;
            }
            count += u64::from(slot.count);
            max_us = max_us.max(slot.max_us);
            for (total, value) in buckets.iter_mut().zip(slot.buckets.iter()) {
                *total += u64::from(*value);
            }
        }
        if count == 0 {
            return None;
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
        Some(Summary {
            count,
            p50_us: quantile(0.50),
            p95_us: quantile(0.95),
            max_us,
        })
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub(crate) struct Summary {
    pub(crate) count: u64,
    pub(crate) p50_us: u64,
    pub(crate) p95_us: u64,
    pub(crate) max_us: u64,
}

#[derive(Clone, Copy)]
struct Pending {
    client_id: u64,
    pane: u32,
    kind: InputKind,
    arrive_us: u64,
    dequeue_us: u64,
    pty_us: u64,
    /// Completes on any frame to the client, not only one carrying the pane's
    /// PTY output. Scrolling moves the viewport without new PTY output.
    any_frame: bool,
}

#[derive(Clone, Copy)]
struct PaneSlot {
    pane: u32,
    last_minute: u64,
    total: Histogram,
}

/// An input that finished its trip, as reported to the caller for logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Completed {
    pub(crate) kind: InputKind,
    pub(crate) pane: u32,
    pub(crate) client_id: u64,
    pub(crate) queue_us: u64,
    pub(crate) handle_us: u64,
    pub(crate) present_us: u64,
    pub(crate) total_us: u64,
}

pub(crate) struct LatencyMeter {
    epoch: Instant,
    by_kind: [[Histogram; Segment::ALL.len()]; InputKind::ALL.len()],
    panes: [Option<PaneSlot>; PANE_SLOTS],
    pending: [Option<Pending>; PENDING_CAP],
    pending_len: usize,
    frame_sources: [u32; FRAME_SOURCE_CAP],
    frame_sources_len: usize,
    frame_sources_all: bool,
    last_key: Option<(u64, u32, u64)>,
    dropped: u64,
    unframed: u64,
}

impl LatencyMeter {
    pub(crate) fn new() -> Box<Self> {
        Self::with_epoch(Instant::now())
    }

    pub(crate) fn with_epoch(epoch: Instant) -> Box<Self> {
        Box::new(Self {
            epoch,
            by_kind: [[Histogram::EMPTY; Segment::ALL.len()]; InputKind::ALL.len()],
            panes: [None; PANE_SLOTS],
            pending: [None; PENDING_CAP],
            pending_len: 0,
            frame_sources: [0; FRAME_SOURCE_CAP],
            frame_sources_len: 0,
            frame_sources_all: false,
            last_key: None,
            dropped: 0,
            unframed: 0,
        })
    }

    fn us(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.epoch).as_micros() as u64
    }

    fn minute(us: u64) -> u64 {
        us / (SLOT_SECS * 1_000_000)
    }

    /// Records an input whose bytes were just handed to the pane's PTY writer.
    /// A key within [`BURST_GAP`] of the previous key on the same pane is
    /// counted as a burst.
    pub(crate) fn input_applied(
        &mut self,
        client_id: u64,
        pane: u32,
        kind: InputKind,
        arrived: Instant,
        dequeued: Instant,
        pty: Instant,
    ) {
        let arrive_us = self.us(arrived);
        let mut kind = kind;
        if matches!(kind, InputKind::Key | InputKind::Burst) {
            if kind == InputKind::Key
                && self.last_key.is_some_and(|(client, last_pane, last_us)| {
                    client == client_id
                        && last_pane == pane
                        && arrive_us.saturating_sub(last_us) <= BURST_GAP.as_micros() as u64
                })
            {
                kind = InputKind::Burst;
            }
            self.last_key = Some((client_id, pane, arrive_us));
        }
        let entry = Pending {
            client_id,
            pane,
            kind,
            arrive_us,
            dequeue_us: self.us(dequeued),
            pty_us: self.us(pty),
            any_frame: kind == InputKind::Scroll || pane == UNKNOWN_PANE,
        };
        let slot = match self.pending.iter().position(Option::is_none) {
            Some(free) => free,
            None => {
                // Full: replace the oldest in-flight input.
                self.dropped += 1;
                self.pending_len -= 1;
                self.pending
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, entry)| entry.map_or(0, |entry| entry.arrive_us))
                    .map_or(0, |(index, _)| index)
            }
        };
        self.pending[slot] = Some(entry);
        self.pending_len += 1;
    }

    /// Declares which panes' PTY output the render about to be streamed
    /// carries. Cleared by [`Self::clear_frame_sources`] after the render.
    pub(crate) fn set_frame_sources(&mut self, panes: impl IntoIterator<Item = u32>) {
        self.frame_sources_len = 0;
        self.frame_sources_all = false;
        if self.pending_len == 0 {
            return;
        }
        for pane in panes {
            if self.frame_sources_len == FRAME_SOURCE_CAP {
                self.frame_sources_all = true;
                return;
            }
            self.frame_sources[self.frame_sources_len] = pane;
            self.frame_sources_len += 1;
        }
    }

    pub(crate) fn clear_frame_sources(&mut self) {
        self.frame_sources_len = 0;
        self.frame_sources_all = false;
    }

    fn frame_carries(&self, pane: u32) -> bool {
        self.frame_sources_all || self.frame_sources[..self.frame_sources_len].contains(&pane)
    }

    /// A frame was queued for `client_id`. Completes that client's in-flight
    /// inputs whose pane output the frame carries, and expires inputs that
    /// never produced a frame. Calls `on_complete` once per completed input.
    pub(crate) fn frame_sent(
        &mut self,
        client_id: u64,
        at: Instant,
        mut on_complete: impl FnMut(&Completed),
    ) {
        if self.pending_len == 0 {
            return;
        }
        let frame_us = self.us(at);
        for index in 0..PENDING_CAP {
            let Some(entry) = self.pending[index] else {
                continue;
            };
            if frame_us.saturating_sub(entry.pty_us) > PENDING_TTL_US {
                self.pending[index] = None;
                self.pending_len -= 1;
                self.unframed += 1;
                continue;
            }
            if entry.client_id != client_id
                || frame_us < entry.pty_us
                || !(entry.any_frame || self.frame_carries(entry.pane))
            {
                continue;
            }
            self.pending[index] = None;
            self.pending_len -= 1;
            let completed = Completed {
                kind: entry.kind,
                pane: entry.pane,
                client_id,
                queue_us: entry.dequeue_us.saturating_sub(entry.arrive_us),
                handle_us: entry.pty_us.saturating_sub(entry.dequeue_us),
                present_us: frame_us.saturating_sub(entry.pty_us),
                total_us: frame_us.saturating_sub(entry.arrive_us),
            };
            self.record(&completed, Self::minute(frame_us));
            on_complete(&completed);
        }
    }

    fn record(&mut self, completed: &Completed, minute: u64) {
        let kind = &mut self.by_kind[completed.kind as usize];
        kind[Segment::Queue as usize].record(minute, completed.queue_us);
        kind[Segment::Handle as usize].record(minute, completed.handle_us);
        kind[Segment::Present as usize].record(minute, completed.present_us);
        kind[Segment::Total as usize].record(minute, completed.total_us);
        if let Some(slot) = self.pane_slot(completed.pane, minute) {
            slot.last_minute = minute;
            slot.total.record(minute, completed.total_us);
        }
    }

    fn pane_slot(&mut self, pane: u32, minute: u64) -> Option<&mut PaneSlot> {
        let index = self
            .panes
            .iter()
            .position(|slot| slot.is_some_and(|slot| slot.pane == pane))
            .or_else(|| self.panes.iter().position(Option::is_none))
            .or_else(|| {
                // Evict the pane idle longest.
                self.panes
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, slot)| slot.map_or(0, |slot| slot.last_minute))
                    .map(|(index, _)| index)
            })?;
        let slot = &mut self.panes[index];
        if slot.is_none_or(|slot| slot.pane != pane) {
            *slot = Some(PaneSlot {
                pane,
                last_minute: minute,
                total: Histogram::EMPTY,
            });
        }
        slot.as_mut()
    }

    /// Snapshot of the rolling window. Allocates; call only on request.
    pub(crate) fn report(&self, now: Instant) -> LatencyReport {
        let minute = Self::minute(self.us(now));
        let mut kinds = Vec::new();
        for kind in InputKind::ALL {
            for segment in Segment::ALL {
                if let Some(summary) = self.by_kind[kind as usize][segment as usize].summary(minute)
                {
                    kinds.push(KindRow {
                        kind,
                        segment,
                        summary,
                    });
                }
            }
        }
        let mut panes = self
            .panes
            .iter()
            .flatten()
            .filter_map(|slot| {
                slot.total.summary(minute).map(|summary| PaneRow {
                    pane: slot.pane,
                    summary,
                })
            })
            .collect::<Vec<_>>();
        panes.sort_by_key(|row| std::cmp::Reverse(row.summary.p95_us));
        LatencyReport {
            window_secs: SLOT_SECS * WINDOW_SLOTS as u64,
            kinds,
            panes,
            in_flight: self.pending_len as u64,
            dropped: self.dropped,
            unframed: self.unframed,
        }
    }
}

#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub(crate) struct KindRow {
    pub(crate) kind: InputKind,
    pub(crate) segment: Segment,
    #[serde(flatten)]
    pub(crate) summary: Summary,
}

/// End-to-end (`total`) latency for one pane.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub(crate) struct PaneRow {
    pub(crate) pane: u32,
    #[serde(flatten)]
    pub(crate) summary: Summary,
}

#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub(crate) struct LatencyReport {
    pub(crate) window_secs: u64,
    pub(crate) kinds: Vec<KindRow>,
    pub(crate) panes: Vec<PaneRow>,
    /// Inputs written to a PTY and still waiting for their frame.
    pub(crate) in_flight: u64,
    /// In-flight inputs evicted because more than the tracked capacity were
    /// waiting at once.
    pub(crate) dropped: u64,
    /// Inputs that produced no frame within ten seconds (no echo, hidden pane).
    pub(crate) unframed: u64,
}

/// Everything `server.latency` returns: input latency and phase timing.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub(crate) struct ServerLatency {
    #[serde(flatten)]
    pub(crate) inputs: LatencyReport,
    pub(crate) phases: Vec<phase::PhaseRow>,
}

impl LatencyMeter {
    pub(crate) fn server_latency(&self, now: Instant) -> ServerLatency {
        ServerLatency {
            inputs: self.report(now),
            phases: phase::PHASES.report(phase::us_since_epoch(now)),
        }
    }
}

/// Logs one `latency.slow` line for an input slower than [`SLOW_INPUT`].
pub(crate) fn log_if_slow(completed: &Completed) {
    if completed.total_us < SLOW_INPUT.as_micros() as u64 {
        return;
    }
    let pane = (completed.pane != UNKNOWN_PANE).then_some(completed.pane);
    tracing::warn!(
        event = "latency.slow",
        kind = completed.kind.name(),
        pane = ?pane,
        client_id = completed.client_id,
        total_ms = completed.total_us / 1_000,
        queue_ms = completed.queue_us / 1_000,
        handle_ms = completed.handle_us / 1_000,
        present_ms = completed.present_us / 1_000,
        "input was slow to reach the screen"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(epoch: Instant, us: u64) -> Instant {
        epoch + Duration::from_micros(us)
    }

    fn key(code: ClientKeyCode, kind: ClientKeyKind) -> ClientPaneInputEvent {
        ClientPaneInputEvent::Key {
            code,
            modifiers: 0,
            kind,
            repeat_count: 1,
            shifted_codepoint: None,
            generated_text: None,
            tracks_release: false,
            physical_key_id: None,
            windows_record: None,
        }
    }

    fn total(meter: &LatencyMeter, now: Instant, kind: InputKind) -> Option<Summary> {
        meter
            .report(now)
            .kinds
            .into_iter()
            .find(|row| row.kind == kind && row.segment == Segment::Total)
            .map(|row| row.summary)
    }

    #[test]
    fn bucket_bounds_cover_every_value_with_bounded_error() {
        let mut previous = 0;
        for us in 0..200_000u64 {
            let index = bucket_index(us);
            assert!(index >= previous, "buckets must be monotonic at {us}");
            previous = index;
            let upper = bucket_upper(index);
            assert!(upper >= us, "{us} above its bucket bound {upper}");
            assert!(
                upper as f64 <= (us as f64) * 1.25 + 1.0,
                "{us} reported as {upper}"
            );
        }
        assert_eq!(bucket_index(u64::MAX), BUCKETS - 1);
    }

    #[test]
    fn classifies_input_batches_by_strongest_kind() {
        assert_eq!(
            classify_pane_events(&[key(ClientKeyCode::Char('a'), ClientKeyKind::Press)]),
            Some(InputKind::Key)
        );
        assert_eq!(
            classify_pane_events(&[key(ClientKeyCode::Char('a'), ClientKeyKind::Release)]),
            None
        );
        assert_eq!(
            classify_pane_events(&[
                key(ClientKeyCode::Char('a'), ClientKeyKind::Press),
                key(ClientKeyCode::Enter, ClientKeyKind::Press),
            ]),
            Some(InputKind::Enter)
        );
        assert_eq!(
            classify_pane_events(&[ClientPaneInputEvent::TextCommit("hello".into())]),
            Some(InputKind::Burst)
        );
        assert_eq!(
            classify_pane_events(&[ClientPaneInputEvent::Paste("x".into())]),
            Some(InputKind::Paste)
        );
        assert_eq!(
            classify_pane_events(&[key(ClientKeyCode::PageUp, ClientKeyKind::Press)]),
            Some(InputKind::Scroll)
        );
        assert_eq!(
            classify_raw_input(b"\x1b[200~hi\x1b[201~"),
            Some(InputKind::Paste)
        );
        assert_eq!(classify_raw_input(b"\r"), Some(InputKind::Enter));
        assert_eq!(classify_raw_input(b"abc"), Some(InputKind::Burst));
        assert_eq!(classify_raw_input(b"\x1b[A"), Some(InputKind::Key));
        assert_eq!(classify_raw_input(b""), None);
    }

    #[test]
    fn completes_on_first_frame_carrying_the_pane_for_that_client() {
        let epoch = Instant::now();
        let mut meter = LatencyMeter::with_epoch(epoch);
        meter.input_applied(
            1,
            7,
            InputKind::Key,
            at(epoch, 1_000),
            at(epoch, 1_200),
            at(epoch, 1_500),
        );

        // Another client's frame, and a frame without pane 7's output, do not count.
        meter.set_frame_sources([7]);
        meter.frame_sent(2, at(epoch, 2_000), |_| panic!("wrong client"));
        meter.set_frame_sources([8]);
        meter.frame_sent(1, at(epoch, 3_000), |_| panic!("wrong pane"));

        meter.set_frame_sources([8, 7]);
        let mut completed = Vec::new();
        meter.frame_sent(1, at(epoch, 9_000), |done| completed.push(*done));
        meter.clear_frame_sources();
        assert_eq!(
            completed,
            vec![Completed {
                kind: InputKind::Key,
                pane: 7,
                client_id: 1,
                queue_us: 200,
                handle_us: 300,
                present_us: 7_500,
                total_us: 8_000,
            }]
        );

        let report = meter.report(at(epoch, 10_000));
        assert_eq!(report.in_flight, 0);
        let summary = total(&meter, at(epoch, 10_000), InputKind::Key).expect("key row");
        assert_eq!(summary.count, 1);
        assert_eq!(summary.max_us, 8_000);
        assert_eq!(report.panes.len(), 1);
        assert_eq!(report.panes[0].pane, 7);
    }

    #[test]
    fn scroll_completes_on_any_frame_to_the_client() {
        let epoch = Instant::now();
        let mut meter = LatencyMeter::with_epoch(epoch);
        meter.input_applied(
            1,
            7,
            InputKind::Scroll,
            at(epoch, 0),
            at(epoch, 10),
            at(epoch, 20),
        );
        meter.set_frame_sources(std::iter::empty());
        let mut count = 0;
        meter.frame_sent(1, at(epoch, 500), |_| count += 1);
        assert_eq!(count, 1);
    }

    #[test]
    fn keys_within_burst_gap_on_one_pane_are_a_burst() {
        let epoch = Instant::now();
        let mut meter = LatencyMeter::with_epoch(epoch);
        let mut kinds = Vec::new();
        for arrive in [0u64, 10_000, 25_000, 200_000] {
            meter.input_applied(
                1,
                7,
                InputKind::Key,
                at(epoch, arrive),
                at(epoch, arrive),
                at(epoch, arrive),
            );
            meter.set_frame_sources([7]);
            meter.frame_sent(1, at(epoch, arrive + 1), |done| kinds.push(done.kind));
        }
        assert_eq!(
            kinds,
            vec![
                InputKind::Key,
                InputKind::Burst,
                InputKind::Burst,
                InputKind::Key
            ]
        );
    }

    #[test]
    fn window_forgets_inputs_older_than_five_minutes() {
        let epoch = Instant::now();
        let mut meter = LatencyMeter::with_epoch(epoch);
        meter.input_applied(1, 7, InputKind::Enter, epoch, epoch, epoch);
        meter.set_frame_sources([7]);
        meter.frame_sent(1, at(epoch, 1_000), |_| {});

        let four_minutes = 4 * 60 * 1_000_000;
        assert!(total(&meter, at(epoch, four_minutes), InputKind::Enter).is_some());
        let six_minutes = 6 * 60 * 1_000_000;
        assert!(total(&meter, at(epoch, six_minutes), InputKind::Enter).is_none());
        assert!(meter.report(at(epoch, six_minutes)).panes.is_empty());
    }

    #[test]
    fn quantiles_track_the_distribution() {
        let epoch = Instant::now();
        let mut meter = LatencyMeter::with_epoch(epoch);
        for i in 0..100u64 {
            // 1 ms .. 100 ms totals, one at a time.
            let start = i * 1_000_000;
            meter.input_applied(
                1,
                7,
                InputKind::Paste,
                at(epoch, start),
                at(epoch, start),
                at(epoch, start),
            );
            meter.set_frame_sources([7]);
            meter.frame_sent(1, at(epoch, start + (i + 1) * 1_000), |_| {});
        }
        let summary = total(&meter, at(epoch, 100_000_000), InputKind::Paste).expect("row");
        assert_eq!(summary.count, 100);
        assert_eq!(summary.max_us, 100_000);
        assert!((50_000..=62_500).contains(&summary.p50_us), "{summary:?}");
        assert!((95_000..=100_000).contains(&summary.p95_us), "{summary:?}");
    }

    #[test]
    fn in_flight_capacity_and_expiry_are_bounded() {
        let epoch = Instant::now();
        let mut meter = LatencyMeter::with_epoch(epoch);
        for i in 0..(PENDING_CAP as u64 + 5) {
            meter.input_applied(
                1,
                7,
                InputKind::Enter,
                at(epoch, i),
                at(epoch, i),
                at(epoch, i),
            );
        }
        let report = meter.report(at(epoch, 100));
        assert_eq!(report.in_flight, PENDING_CAP as u64);
        assert_eq!(report.dropped, 5);

        // No frame for more than the TTL: every waiting input expires.
        meter.set_frame_sources(std::iter::empty());
        meter.frame_sent(1, at(epoch, PENDING_TTL_US + 1_000), |_| {});
        let report = meter.report(at(epoch, PENDING_TTL_US + 1_000));
        assert_eq!(report.in_flight, 0);
        assert_eq!(report.unframed, PENDING_CAP as u64);
    }

    #[test]
    fn pane_table_evicts_the_longest_idle_pane() {
        let epoch = Instant::now();
        let mut meter = LatencyMeter::with_epoch(epoch);
        for pane in 0..(PANE_SLOTS as u32 + 1) {
            meter.input_applied(1, pane, InputKind::Enter, epoch, epoch, epoch);
            meter.set_frame_sources([pane]);
            meter.frame_sent(1, at(epoch, 10), |_| {});
        }
        let panes = meter.report(at(epoch, 20)).panes;
        assert_eq!(panes.len(), PANE_SLOTS);
        assert!(panes.iter().any(|row| row.pane == PANE_SLOTS as u32));
    }

    #[test]
    fn meter_fits_the_memory_budget() {
        let size = std::mem::size_of::<LatencyMeter>();
        assert!(size < 256 * 1024, "latency meter is {size} bytes");
    }
}
