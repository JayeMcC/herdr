//! Client-measured input round trip.
//!
//! The client stamps each stdin chunk it forwards, sends an optional round-trip mark after the
//! input on the same ordered connection, and the server echoes the highest applied mark in the
//! same render item as its next frame. A sample is the time from the stdin read to the frame
//! being written to this client's stdout. Marks are only sent when the endpoint advertises
//! [`crate::protocol::endpoint::CLIENT_RTT_CAPABILITY`], so an old server never sees one and an
//! old client never receives an echo. Without the capability the tracker reports
//! "rtt unavailable" and does nothing else.
//!
//! Storage is fixed: a ring of pending marks and a quarter-octave histogram per kind. Nothing
//! allocates per input apart from the mark message itself.

use std::time::{Duration, Instant};

/// How often the client log receives a summary line.
pub(super) const REPORT_INTERVAL: Duration = Duration::from_secs(60);
/// Pending marks older than this are counted as lost rather than measured.
const PENDING_EXPIRY: Duration = Duration::from_secs(30);
/// Keys arriving closer together than this are a dictation burst rather than typing.
const DICTATION_GAP: Duration = Duration::from_millis(15);
const PENDING_SLOTS: usize = 128;
/// Four buckets per octave of microseconds, up to 2^25 us (about 33 s).
const BUCKETS: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InputKind {
    Key,
    Dictation,
    Paste,
    Scroll,
    Enter,
}

impl InputKind {
    const ALL: [Self; 5] = [
        Self::Key,
        Self::Dictation,
        Self::Paste,
        Self::Scroll,
        Self::Enter,
    ];

    fn index(self) -> usize {
        self as usize
    }

    fn name(self) -> &'static str {
        match self {
            Self::Key => "key",
            Self::Dictation => "dictation",
            Self::Paste => "paste",
            Self::Scroll => "scroll",
            Self::Enter => "enter",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotState {
    Free,
    Sent,
    Echoed,
}

#[derive(Clone, Copy)]
struct Pending {
    seq: u64,
    read_at: Instant,
    kind: InputKind,
    state: SlotState,
}

#[derive(Clone, Copy)]
struct Histogram {
    counts: [u32; BUCKETS],
    samples: u32,
    sum_us: u64,
    max_us: u64,
}

impl Default for Histogram {
    fn default() -> Self {
        Self {
            counts: [0; BUCKETS],
            samples: 0,
            sum_us: 0,
            max_us: 0,
        }
    }
}

fn bucket_for(us: u64) -> usize {
    if us < 4 {
        return us as usize;
    }
    let octave = 63 - u64::from(us.leading_zeros());
    let quarter = (us >> (octave - 2)) & 0b11;
    ((octave as usize) * 4 + quarter as usize).min(BUCKETS - 1)
}

/// Midpoint of a bucket in microseconds.
fn bucket_value(bucket: usize) -> u64 {
    if bucket < 4 {
        return bucket as u64;
    }
    let octave = (bucket / 4) as u32;
    let quarter = (bucket % 4) as u64;
    let low = (1u64 << octave) + quarter * (1u64 << (octave - 2));
    low + (1u64 << (octave - 2)) / 2
}

impl Histogram {
    fn record(&mut self, us: u64) {
        self.counts[bucket_for(us)] = self.counts[bucket_for(us)].saturating_add(1);
        self.samples = self.samples.saturating_add(1);
        self.sum_us = self.sum_us.saturating_add(us);
        self.max_us = self.max_us.max(us);
    }

    fn percentile(&self, fraction: f64) -> u64 {
        if self.samples == 0 {
            return 0;
        }
        let rank = ((f64::from(self.samples) * fraction).ceil() as u32).max(1);
        let mut seen = 0u32;
        for (bucket, count) in self.counts.iter().enumerate() {
            seen = seen.saturating_add(*count);
            if seen >= rank {
                return bucket_value(bucket).min(self.max_us);
            }
        }
        self.max_us
    }
}

/// One kind's summary for a reporting window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct KindSummary {
    pub(super) kind: InputKind,
    pub(super) samples: u32,
    pub(super) p50_us: u64,
    pub(super) p95_us: u64,
    pub(super) max_us: u64,
    pub(super) mean_us: u64,
}

pub(super) struct ClientRtt {
    available: bool,
    bound_to: Option<(super::endpoint::ClientEndpointId, u64)>,
    next_seq: u64,
    pending: [Pending; PENDING_SLOTS],
    next_slot: usize,
    histograms: [Histogram; 5],
    lost: u32,
    last_key_at: Option<Instant>,
    window_started: Instant,
}

impl ClientRtt {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            available: false,
            bound_to: None,
            next_seq: 0,
            pending: [Pending {
                seq: 0,
                read_at: now,
                kind: InputKind::Key,
                state: SlotState::Free,
            }; PENDING_SLOTS],
            next_slot: 0,
            histograms: [Histogram::default(); 5],
            lost: 0,
            last_key_at: None,
            window_started: now,
        }
    }

    /// Follows the active endpoint connection. A different endpoint or connection generation
    /// drops in-flight marks, because its echo cannot answer a mark sent elsewhere.
    pub(super) fn bind(&mut self, binding: Option<(super::endpoint::ClientEndpointId, u64)>) {
        if self.bound_to != binding {
            for slot in &mut self.pending {
                slot.state = SlotState::Free;
            }
        }
        self.available = binding.is_some();
        self.bound_to = binding;
    }

    #[cfg(test)]
    fn set_available(&mut self, available: bool) {
        if self.available != available {
            for slot in &mut self.pending {
                slot.state = SlotState::Free;
            }
        }
        self.available = available;
    }

    /// Classifies one stdin chunk. Called for every forwarded chunk, available or not, so the
    /// dictation gap stays accurate.
    pub(super) fn classify(&mut self, data: &[u8], read_at: Instant) -> InputKind {
        let kind = classify_bytes(data);
        if kind != InputKind::Key {
            self.last_key_at = None;
            return kind;
        }
        let burst = self
            .last_key_at
            .is_some_and(|last| read_at.saturating_duration_since(last) < DICTATION_GAP);
        self.last_key_at = Some(read_at);
        if burst || printable_run(data) > 1 {
            InputKind::Dictation
        } else {
            InputKind::Key
        }
    }

    /// Records forwarded input and returns the mark to send after it, or `None` when the
    /// endpoint does not support round trips.
    pub(super) fn input_sent(&mut self, kind: InputKind, read_at: Instant) -> Option<u64> {
        if !self.available {
            return None;
        }
        self.next_seq = self.next_seq.wrapping_add(1).max(1);
        let slot = &mut self.pending[self.next_slot];
        if slot.state != SlotState::Free {
            self.lost = self.lost.saturating_add(1);
        }
        *slot = Pending {
            seq: self.next_seq,
            read_at,
            kind,
            state: SlotState::Sent,
        };
        self.next_slot = (self.next_slot + 1) % PENDING_SLOTS;
        Some(self.next_seq)
    }

    /// The server applied every mark up to `seq`; its frame follows immediately.
    pub(super) fn echo(&mut self, seq: u64) {
        for slot in &mut self.pending {
            if slot.state == SlotState::Sent && slot.seq <= seq {
                slot.state = SlotState::Echoed;
            }
        }
    }

    /// A frame was written to stdout.
    pub(super) fn frame_presented(&mut self, now: Instant) {
        for slot in &mut self.pending {
            match slot.state {
                SlotState::Echoed => {
                    let us = now.saturating_duration_since(slot.read_at).as_micros();
                    self.histograms[slot.kind.index()]
                        .record(u64::try_from(us).unwrap_or(u64::MAX));
                    slot.state = SlotState::Free;
                }
                SlotState::Sent if now.saturating_duration_since(slot.read_at) > PENDING_EXPIRY => {
                    self.lost = self.lost.saturating_add(1);
                    slot.state = SlotState::Free;
                }
                _ => {}
            }
        }
    }

    pub(super) fn summaries(&self) -> Vec<KindSummary> {
        InputKind::ALL
            .iter()
            .map(|&kind| {
                let histogram = &self.histograms[kind.index()];
                KindSummary {
                    kind,
                    samples: histogram.samples,
                    p50_us: histogram.percentile(0.50),
                    p95_us: histogram.percentile(0.95),
                    max_us: histogram.max_us,
                    mean_us: histogram
                        .sum_us
                        .checked_div(u64::from(histogram.samples))
                        .unwrap_or(0),
                }
            })
            .collect()
    }

    /// Writes one summary line to the client log once per window, then starts a new window.
    pub(super) fn report_if_due(&mut self, now: Instant) {
        if now.saturating_duration_since(self.window_started) < REPORT_INTERVAL {
            return;
        }
        tracing::info!(target: "twodr::client_rtt", "{}", self.report_line());
        self.histograms = [Histogram::default(); 5];
        self.lost = 0;
        self.window_started = now;
    }

    pub(super) fn report_line(&self) -> String {
        if !self.available {
            return "client_rtt: rtt unavailable (endpoint does not advertise client_rtt)".into();
        }
        let mut line = format!("client_rtt: window_s={}", REPORT_INTERVAL.as_secs());
        for summary in self.summaries() {
            line.push_str(&format!(
                " {}=n:{},p50_us:{},p95_us:{},max_us:{},mean_us:{}",
                summary.kind.name(),
                summary.samples,
                summary.p50_us,
                summary.p95_us,
                summary.max_us,
                summary.mean_us
            ));
        }
        line.push_str(&format!(" lost={}", self.lost));
        line
    }
}

/// Whether a shell input outcome reaches the endpoint as pane input or an endpoint operation,
/// which is the only input a server round trip can answer.
pub(super) fn forwards_to_endpoint(outcome: &super::shell::ClientShellInput) -> bool {
    outcome.requests.iter().any(|request| {
        matches!(
            request,
            crate::protocol::ClientMessage::ClientShellPaneInput { .. }
                | crate::protocol::ClientMessage::ClientShellPopupInput { .. }
        )
    }) || outcome
        .actions
        .iter()
        .any(|action| matches!(action, super::shell::ClientShellAction::Endpoint { .. }))
}

/// Sends a round-trip mark after forwarded input when the active endpoint advertises
/// [`crate::protocol::endpoint::CLIENT_RTT_CAPABILITY`]. A failed send is not an input error;
/// the transport reports its own failures.
pub(super) fn send_mark(
    rtt: &mut ClientRtt,
    endpoints: &mut super::endpoint::EndpointRegistry,
    kind: InputKind,
    read_at: Instant,
) {
    let active = endpoints.active_id().clone();
    let binding = endpoints
        .connection(&active)
        .filter(|connection| {
            connection
                .negotiation
                .supports_capability(crate::protocol::endpoint::CLIENT_RTT_CAPABILITY)
        })
        .map(|connection| (active.clone(), connection.generation));
    rtt.bind(binding);
    let Some(seq) = rtt.input_sent(kind, read_at) else {
        return;
    };
    endpoints.send_to(
        &active,
        &crate::protocol::ClientMessage::EndpointControl {
            kind: crate::protocol::endpoint::CLIENT_RTT_MARK_KIND.into(),
            data: seq.to_string(),
        },
    );
}

fn printable_run(data: &[u8]) -> usize {
    if data.contains(&0x1b) {
        return 0;
    }
    std::str::from_utf8(data)
        .map(|text| text.chars().filter(|ch| !ch.is_control()).count())
        .unwrap_or(0)
}

fn classify_bytes(data: &[u8]) -> InputKind {
    if data.starts_with(b"\x1b[200~") {
        return InputKind::Paste;
    }
    if is_scroll_report(data) {
        return InputKind::Scroll;
    }
    if matches!(data, b"\r" | b"\n" | b"\r\n" | b"\x1b[13u" | b"\x1bOM") {
        return InputKind::Enter;
    }
    InputKind::Key
}

/// SGR (`ESC [ < b ; x ; y M`) or legacy X10 (`ESC [ M b x y`) wheel reports.
fn is_scroll_report(data: &[u8]) -> bool {
    if let Some(body) = data.strip_prefix(b"\x1b[<") {
        let end = body.iter().position(|byte| *byte == b';').unwrap_or(0);
        return std::str::from_utf8(&body[..end])
            .ok()
            .and_then(|button| button.parse::<u16>().ok())
            .is_some_and(|button| button & 0b0100_0000 != 0 && button & 0b10 == 0);
    }
    if let Some(&[button, _, _]) = data.strip_prefix(b"\x1b[M") {
        let button = u16::from(button).saturating_sub(32);
        return button & 0b0100_0000 != 0 && button & 0b10 == 0;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn client_rtt_measures_read_to_frame_after_echo() {
        let start = Instant::now();
        let mut rtt = ClientRtt::new(start);
        rtt.set_available(true);
        let seq = rtt.input_sent(InputKind::Key, start).expect("mark");
        // A frame before the echo belongs to earlier input and must not complete this mark.
        rtt.frame_presented(at(start, 3));
        assert_eq!(rtt.summaries()[0].samples, 0);
        rtt.echo(seq);
        rtt.frame_presented(at(start, 8));
        let key = rtt.summaries()[0];
        assert_eq!(key.samples, 1);
        assert_eq!(key.max_us, 8_000);
        assert!((6_000..=10_000).contains(&key.p50_us), "{key:?}");
    }

    #[test]
    fn client_rtt_echo_covers_every_earlier_mark() {
        let start = Instant::now();
        let mut rtt = ClientRtt::new(start);
        rtt.set_available(true);
        for ms in 0..3 {
            rtt.input_sent(InputKind::Dictation, at(start, ms));
        }
        rtt.echo(3);
        rtt.frame_presented(at(start, 10));
        assert_eq!(rtt.summaries()[1].samples, 3);
    }

    #[test]
    fn client_rtt_unavailable_without_server_capability() {
        let start = Instant::now();
        let mut rtt = ClientRtt::new(start);
        assert!(rtt.input_sent(InputKind::Key, start).is_none());
        rtt.echo(1);
        rtt.frame_presented(at(start, 1));
        assert!(rtt.summaries().iter().all(|summary| summary.samples == 0));
        assert_eq!(
            rtt.report_line(),
            "client_rtt: rtt unavailable (endpoint does not advertise client_rtt)"
        );
    }

    #[test]
    fn client_rtt_server_that_never_echoes_yields_no_samples_and_no_error() {
        let start = Instant::now();
        let mut rtt = ClientRtt::new(start);
        rtt.set_available(true);
        for ms in 0..200 {
            rtt.input_sent(InputKind::Key, at(start, ms));
            rtt.frame_presented(at(start, ms + 1));
        }
        rtt.frame_presented(at(start, 60_000));
        assert!(rtt.summaries().iter().all(|summary| summary.samples == 0));
        assert!(rtt.report_line().contains("lost="));
    }

    #[test]
    fn client_rtt_classifies_input_kinds() {
        let start = Instant::now();
        let mut rtt = ClientRtt::new(start);
        assert_eq!(rtt.classify(b"a", start), InputKind::Key);
        assert_eq!(rtt.classify(b"b", at(start, 3)), InputKind::Dictation);
        assert_eq!(rtt.classify(b"c", at(start, 200)), InputKind::Key);
        assert_eq!(rtt.classify(b"hello", at(start, 400)), InputKind::Dictation);
        assert_eq!(rtt.classify(b"\r", at(start, 401)), InputKind::Enter);
        assert_eq!(
            rtt.classify(b"\x1b[200~text\x1b[201~", start),
            InputKind::Paste
        );
        assert_eq!(rtt.classify(b"\x1b[<65;10;5M", start), InputKind::Scroll);
        assert_eq!(rtt.classify(b"\x1b[<64;10;5M", start), InputKind::Scroll);
        assert_eq!(rtt.classify(b"\x1b[<0;10;5M", start), InputKind::Key);
        assert_eq!(rtt.classify(b"\x1b[A", at(start, 1000)), InputKind::Key);
    }

    #[test]
    fn client_rtt_switching_endpoint_drops_inflight_marks() {
        let start = Instant::now();
        let mut rtt = ClientRtt::new(start);
        rtt.set_available(true);
        rtt.input_sent(InputKind::Key, start);
        rtt.set_available(false);
        rtt.set_available(true);
        rtt.echo(1);
        rtt.frame_presented(at(start, 5));
        assert_eq!(rtt.summaries()[0].samples, 0);
    }

    #[test]
    fn client_rtt_buckets_are_monotonic_and_bounded() {
        let mut previous = 0;
        for us in [0, 1, 3, 4, 5, 7, 8, 100, 1_000, 10_000, 1_000_000, u64::MAX] {
            let bucket = bucket_for(us);
            assert!(bucket >= previous && bucket < BUCKETS);
            previous = bucket;
        }
        for us in [5u64, 50, 500, 5_000, 50_000] {
            let value = bucket_value(bucket_for(us));
            assert!(
                value * 10 >= us * 8 && value * 10 <= us * 12,
                "{us} -> {value}"
            );
        }
    }
}

#[cfg(test)]
mod endpoint_tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::client::endpoint::{
        ClientEndpointId, EndpointNegotiation, EndpointRegistry, EndpointTransport,
    };
    use crate::protocol::ClientMessage;

    struct Recording(Arc<Mutex<Vec<ClientMessage>>>);

    impl EndpointTransport for Recording {
        fn send(&mut self, message: &ClientMessage) -> io::Result<()> {
            self.0.lock().unwrap().push(message.clone());
            Ok(())
        }
    }

    fn registry(capabilities: Vec<String>) -> (EndpointRegistry, Arc<Mutex<Vec<ClientMessage>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let registry = EndpointRegistry::new(
            Recording(sent.clone()),
            1,
            EndpointNegotiation::new(Vec::new(), capabilities),
        );
        (registry, sent)
    }

    fn is_mark(message: &ClientMessage) -> bool {
        matches!(
            message,
            ClientMessage::EndpointControl { kind, .. }
                if kind == crate::protocol::endpoint::CLIENT_RTT_MARK_KIND
        )
    }

    #[test]
    fn client_rtt_new_client_sends_nothing_to_an_old_server() {
        // An old server's welcome never lists the capability (see endpoint-welcome-v1.json).
        let welcome: crate::protocol::endpoint::EndpointServerWelcome =
            serde_json::from_str(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/endpoint-welcome-v1.json"
            )))
            .unwrap();
        let (mut endpoints, sent) = registry(welcome.capabilities);
        let start = Instant::now();
        let mut rtt = ClientRtt::new(start);
        for ms in 0..50 {
            send_mark(
                &mut rtt,
                &mut endpoints,
                InputKind::Key,
                start + Duration::from_millis(ms),
            );
            rtt.frame_presented(start + Duration::from_millis(ms + 1));
        }
        assert!(sent.lock().unwrap().is_empty());
        assert!(endpoints.take_failures().is_empty());
        assert!(rtt.report_line().contains("rtt unavailable"));
    }

    #[test]
    fn client_rtt_new_client_marks_a_new_server_and_measures_the_echo() {
        let (mut endpoints, sent) =
            registry(vec![crate::protocol::endpoint::CLIENT_RTT_CAPABILITY.into()]);
        let start = Instant::now();
        let mut rtt = ClientRtt::new(start);
        send_mark(&mut rtt, &mut endpoints, InputKind::Enter, start);
        let sent = sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        assert!(is_mark(&sent[0]));
        let ClientMessage::EndpointControl { data, .. } = &sent[0] else {
            unreachable!();
        };
        rtt.echo(data.parse().unwrap());
        rtt.frame_presented(start + Duration::from_millis(4));
        let enter = rtt.summaries()[InputKind::Enter.index()];
        assert_eq!(enter.samples, 1);
        assert_eq!(enter.max_us, 4_000);
        assert!(!rtt.report_line().contains("unavailable"));
        assert_eq!(endpoints.active_id(), &ClientEndpointId::Local);
    }
}
