//! The replay harness: drives any [`Driver`] from a [`Trace`] and records what it did.
//!
//! A trace is the record of one session between an app and a device. Replaying it hands
//! the driver that session from the device's side: every `rx` line is fed as
//! [`Input::Rx`], and every `tx` line is turned back into the request that would have
//! produced it and fed as [`Input::Request`], so the driver writes what the app wrote. The
//! result, a [`Replay`], keeps both sides, the trace's writes next to the driver's, and
//! [`Replay::writes_match`] compares them byte for byte (SPEC §5, Stage A1); the answers
//! and events are there for the value checks of Stage A2.
//!
//! The harness knows nothing about any device. An [`Adapter`] maps the trace's channel
//! names to the family's [`Channel`]s, says what the link looks like when it comes up, and
//! turns each `tx` line into the family's request.
//!
//! # Order
//!
//! [`Input::Connected`] first, with what [`Adapter::connected`] gives; then the data lines
//! in trace order, comment and event lines skipped; then [`Input::Disconnected`], so every
//! request still in flight gets its answer (SPEC invariant 4). Every output of one input is
//! applied before the next input, as the pump does (invariant 3). A read the driver asks
//! for ([`Output::Read`]) is recorded in [`Replay::reads`] and feeds nothing: the value it
//! would have returned is the trace's next `rx` line on that channel, which is fed in its
//! turn like any other.
//!
//! # Time
//!
//! The driver's clock is the trace's: [`Instant::ZERO`] is the first data line's stamp and
//! every later line is at its [`Stamp::delta_millis`] from it (a stamp before the first
//! clamps to zero). A timer the driver arms is due at the clock of the input that armed it
//! plus its delay. Before a line is fed, every timer due at or before that line's clock
//! fires as [`Input::Timer`], earliest first; the outputs of a firing are applied at the
//! deadline itself, so a timer armed by a firing is placed as the pump would place it, and
//! fires in turn if it is also due. Timers still armed after the last line never fire, as a
//! pump forgets them when a session ends.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::fmt;

use junk_core::{
    Channel, ChannelSet, Driver, Input, Instant, Output, Outputs, ProtoError, ReqId, TimerId,
};

use crate::{DataLine, Direction, Stamp, Trace};

/// What a device family tells the harness: its channels by the trace's names, what its
/// link looks like when it comes up, and which request each write in the trace was.
pub trait Adapter {
    /// The driver being replayed.
    type Driver: Driver;

    /// The channel a trace calls `name`, or `None` if the family declares no such channel.
    fn channel(&self, name: &str) -> Option<Channel>;

    /// What [`Input::Connected`] carries: the resolved channels and the ATT MTU.
    fn connected(&self) -> (ChannelSet, u16);

    /// The request that makes the driver write `line`, a `tx` line, or `None` if none does
    /// (the bytes do not decode, or nothing asks for that write). A `None` line is skipped:
    /// the driver never writes it, and it is listed in [`Replay::skipped`].
    fn request(&self, line: &DataLine) -> Option<<Self::Driver as Driver>::Req>;
}

/// One [`Output::Done`] the driver emitted, with the request it answers.
pub struct Answer<D: Driver> {
    /// The index, among the trace's data lines, of the `tx` line that became the request;
    /// `None` if no request of that id was in flight (a second answer to one request, or
    /// an id the harness never handed out).
    pub line: Option<usize>,
    /// The id the request was fed with.
    pub id: ReqId,
    /// The outcome.
    pub result: Result<D::Resp, ProtoError>,
}

/// Everything a replay recorded.
///
/// Line indices count the trace's data lines only, from zero, in the order
/// [`Trace::data`] gives them.
pub struct Replay<D: Driver> {
    /// The `tx` lines the adapter turned into requests, in trace order: what the driver
    /// was expected to write.
    pub expected: Vec<(Channel, Vec<u8>)>,
    /// Every [`Output::Tx`] the driver emitted, in order.
    pub written: Vec<(Channel, Vec<u8>)>,
    /// Every [`Output::Done`], in order.
    pub answers: Vec<Answer<D>>,
    /// Every [`Output::Event`], in order.
    pub events: Vec<D::Ev>,
    /// The `tx` lines the adapter did not turn into requests.
    pub skipped: Vec<usize>,
    /// The lines whose channel the adapter did not know. An `rx` line was fed nowhere; a
    /// `tx` line was not offered to [`Adapter::request`].
    pub unknown_channels: Vec<usize>,
    /// Every timer that fired: the index of the line it fired before, and its id.
    pub timers_fired: Vec<(usize, TimerId)>,
    /// Every [`Output::Subscribe`], in order.
    pub subscribed: Vec<Channel>,
    /// Every [`Output::Read`], in order. Nothing was fed for them: the trace's `rx` lines
    /// carry the values.
    pub reads: Vec<Channel>,
    /// Whether the driver emitted [`Output::Disconnect`]. The replay goes on regardless:
    /// the trace is what happened.
    pub disconnect_requested: bool,
}

/// Where the driver's writes first departed from the trace's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mismatch {
    /// The index into [`Replay::expected`] and [`Replay::written`] that differs.
    pub index: usize,
    /// What the trace has there, or `None` if it has fewer writes.
    pub expected: Option<(Channel, Vec<u8>)>,
    /// What the driver wrote there, or `None` if it wrote fewer.
    pub written: Option<(Channel, Vec<u8>)>,
}

impl<D: Driver> Replay<D> {
    /// A replay that has recorded nothing.
    fn empty() -> Self {
        Replay {
            expected: Vec::new(),
            written: Vec::new(),
            answers: Vec::new(),
            events: Vec::new(),
            skipped: Vec::new(),
            unknown_channels: Vec::new(),
            timers_fired: Vec::new(),
            subscribed: Vec::new(),
            reads: Vec::new(),
            disconnect_requested: false,
        }
    }

    /// Whether the driver wrote exactly what the trace has, channel and bytes, in order.
    ///
    /// # Errors
    ///
    /// The first index at which [`Replay::written`] and [`Replay::expected`] differ, or
    /// the length of the shorter one if one is a prefix of the other.
    pub fn writes_match(&self) -> Result<(), Mismatch> {
        let len = self.expected.len().max(self.written.len());
        (0..len)
            .find(|&index| self.expected.get(index) != self.written.get(index))
            .map_or(Ok(()), |index| {
                Err(Mismatch {
                    index,
                    expected: self.expected.get(index).cloned(),
                    written: self.written.get(index).cloned(),
                })
            })
    }
}

// Not derived: the derive would demand `D: Debug`, which a driver need not be, and could
// not see through `Vec<Answer<D>>` to ask for `D::Resp: Debug`, which it must.
impl<D: Driver> fmt::Debug for Replay<D>
where
    D::Resp: fmt::Debug,
    D::Ev: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Replay")
            .field("expected", &self.expected)
            .field("written", &self.written)
            .field("answers", &self.answers)
            .field("events", &self.events)
            .field("skipped", &self.skipped)
            .field("unknown_channels", &self.unknown_channels)
            .field("timers_fired", &self.timers_fired)
            .field("subscribed", &self.subscribed)
            .field("reads", &self.reads)
            .field("disconnect_requested", &self.disconnect_requested)
            .finish()
    }
}

impl<D: Driver> fmt::Debug for Answer<D>
where
    D::Resp: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Answer")
            .field("line", &self.line)
            .field("id", &self.id)
            .field("result", &self.result)
            .finish()
    }
}

impl<D: Driver> Clone for Answer<D>
where
    D::Resp: Clone,
{
    fn clone(&self) -> Self {
        Answer {
            line: self.line,
            id: self.id,
            result: self.result.clone(),
        }
    }
}

impl<D: Driver> PartialEq for Answer<D>
where
    D::Resp: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.line == other.line && self.id == other.id && self.result == other.result
    }
}

impl<D: Driver> Eq for Answer<D> where D::Resp: Eq {}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "write {}: the trace has ", self.index)?;
        write_hex(f, self.expected.as_ref())?;
        f.write_str(", the driver wrote ")?;
        write_hex(f, self.written.as_ref())
    }
}

impl core::error::Error for Mismatch {}

/// `<channel id> <hex>`, or `nothing`.
fn write_hex(f: &mut fmt::Formatter<'_>, write: Option<&(Channel, Vec<u8>)>) -> fmt::Result {
    let Some((Channel(id), bytes)) = write else {
        return f.write_str("nothing");
    };
    write!(f, "channel {id} ")?;
    for byte in bytes {
        write!(f, "{byte:02x}")?;
    }
    Ok(())
}

/// The clock of a line stamped `at` in a trace whose first data line is stamped `origin`.
fn clock(origin: &Stamp, at: &Stamp) -> Instant {
    Instant(u64::try_from(at.delta_millis(origin)).unwrap_or(0))
}

/// One replay in progress: the record so far and the pump's bookkeeping.
struct Run<D: Driver> {
    out: Replay<D>,
    /// Armed timers and when they are due.
    timers: BTreeMap<TimerId, Instant>,
    /// Requests fed and not yet answered, by id, with the line each came from.
    requests: BTreeMap<ReqId, usize>,
    /// The next [`ReqId`] to hand out.
    next_id: u32,
    /// The driver's sink, kept between inputs so its buffer is reused.
    outputs: Outputs<D::Resp, D::Ev>,
}

impl<D: Driver> Run<D> {
    fn new() -> Self {
        Run {
            out: Replay::empty(),
            timers: BTreeMap::new(),
            requests: BTreeMap::new(),
            next_id: 0,
            outputs: Outputs::new(),
        }
    }

    /// Feeds `input` at clock `now` and applies every output, in order.
    fn feed(&mut self, driver: &mut D, input: Input<D::Req>, now: Instant) {
        driver.handle(input, &mut self.outputs);
        let mut outputs = core::mem::take(&mut self.outputs);
        for output in outputs.drain() {
            self.apply(output, now);
        }
        self.outputs = outputs;
    }

    /// Records one output; `now` is the clock a timer counts from.
    fn apply(&mut self, output: Output<D::Resp, D::Ev>, now: Instant) {
        match output {
            Output::Tx { chan, bytes } => self.out.written.push((chan, bytes)),
            Output::Subscribe(chan) => self.out.subscribed.push(chan),
            Output::Read(chan) => self.out.reads.push(chan),
            Output::SetTimer { id, after } => {
                self.timers.insert(id, now + after);
            }
            Output::CancelTimer(id) => {
                self.timers.remove(&id);
            }
            Output::Event(ev) => self.out.events.push(ev),
            Output::Done { id, result } => {
                let line = self.requests.remove(&id);
                self.out.answers.push(Answer { line, id, result });
            }
            Output::Disconnect => self.out.disconnect_requested = true,
        }
    }

    /// Fires every timer due at or before `now`, earliest first (lowest id among equals),
    /// each at its own deadline, before line `index` is fed.
    fn fire_due(&mut self, driver: &mut D, index: usize, now: Instant) {
        while let Some((&id, &deadline)) = self.timers.iter().min_by_key(|(_, at)| **at) {
            if deadline > now {
                return;
            }
            self.timers.remove(&id);
            self.out.timers_fired.push((index, id));
            self.feed(driver, Input::Timer(id), deadline);
        }
    }

    /// Feeds the request for `line`, at index `index`, as the next id.
    fn request(
        &mut self,
        driver: &mut D,
        index: usize,
        chan: Channel,
        line: &DataLine,
        req: D::Req,
        now: Instant,
    ) {
        let id = ReqId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        self.requests.insert(id, index);
        self.out.expected.push((chan, line.bytes.clone()));
        self.feed(driver, Input::Request { id, req, now }, now);
    }
}

/// Drives `driver` through `trace` as the module doc describes and returns what it did.
///
/// The driver is left in whatever state [`Input::Disconnected`] leaves it, which for a
/// well-behaved driver is fresh (SPEC invariant 6).
pub fn replay<A: Adapter>(trace: &Trace, adapter: &A, driver: &mut A::Driver) -> Replay<A::Driver> {
    let mut run = Run::new();
    let (resolved, mtu) = adapter.connected();
    run.feed(driver, Input::Connected { resolved, mtu }, Instant::ZERO);

    let mut now = Instant::ZERO;
    let mut origin = None;
    for (index, line) in trace.data().enumerate() {
        let origin = *origin.get_or_insert(line.at);
        now = clock(&origin, &line.at);
        run.fire_due(driver, index, now);
        let Some(chan) = adapter.channel(&line.chan) else {
            run.out.unknown_channels.push(index);
            continue;
        };
        match line.dir {
            Direction::Tx => match adapter.request(line) {
                Some(req) => run.request(driver, index, chan, line, req, now),
                None => run.out.skipped.push(index),
            },
            Direction::Rx => run.feed(
                driver,
                Input::Rx {
                    chan,
                    bytes: line.bytes.clone(),
                },
                now,
            ),
        }
    }

    run.feed(driver, Input::Disconnected, now);
    run.out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::VecDeque;
    use alloc::string::ToString;
    use alloc::vec;

    use junk_core::{Bytes, CharDecl, Dir as GattDir, Duration, GattMap, ServiceDecl, Uuid};

    const CMD: Channel = Channel(0);
    const EVT: Channel = Channel(1);
    const VER: Channel = Channel(2);
    const TIMER: TimerId = TimerId(0);
    const TIMEOUT: Duration = Duration::from_millis(100);

    const GATT: GattMap = GattMap {
        services: &[ServiceDecl {
            uuid: Uuid::from_u128(1),
            required: true,
            chars: &[
                CharDecl {
                    id: CMD,
                    uuid: Uuid::from_u128(2),
                    dir: GattDir::Write,
                    required: true,
                },
                CharDecl {
                    id: EVT,
                    uuid: Uuid::from_u128(3),
                    dir: GattDir::Notify,
                    required: true,
                },
                CharDecl {
                    id: VER,
                    uuid: Uuid::from_u128(4),
                    dir: GattDir::Read,
                    required: false,
                },
            ],
        }],
    };

    /// A driver that echoes: a request writes its bytes on `CMD` and arms `TIMER` for
    /// `TIMEOUT`; the next notification on `EVT` answers it with the bytes; the timer
    /// answers it with a timeout. Requests are serialised; a notification with nothing in
    /// flight is an event. On connect it subscribes to `EVT` and reads `VER`, whose value
    /// (with nothing in flight) is an event too.
    #[derive(Debug, Default)]
    struct Echo {
        active: Option<ReqId>,
        queue: VecDeque<(ReqId, Bytes)>,
    }

    impl Echo {
        fn begin_next(&mut self, out: &mut Outputs<Bytes, Bytes>) {
            if let Some((id, bytes)) = self.queue.pop_front() {
                out.tx(CMD, bytes);
                out.set_timer(TIMER, TIMEOUT);
                self.active = Some(id);
            }
        }
    }

    impl Driver for Echo {
        type Req = Bytes;
        type Resp = Bytes;
        type Ev = Bytes;

        const GATT: &'static GattMap = &GATT;

        fn handle(&mut self, input: Input<Bytes>, out: &mut Outputs<Bytes, Bytes>) {
            match input {
                Input::Connected { .. } => {
                    out.subscribe(EVT);
                    out.read(VER);
                }
                Input::Disconnected => {
                    if let Some(id) = self.active.take() {
                        out.cancel_timer(TIMER);
                        out.done(id, Err(ProtoError::Disconnected));
                    }
                    for (id, _) in self.queue.drain(..) {
                        out.done(id, Err(ProtoError::Disconnected));
                    }
                }
                Input::Rx { chan, bytes } => match self.active {
                    Some(id) if chan == EVT => {
                        self.active = None;
                        out.cancel_timer(TIMER);
                        out.done(id, Ok(bytes));
                        self.begin_next(out);
                    }
                    _ => out.event(bytes),
                },
                Input::Timer(TIMER) => {
                    if let Some(id) = self.active.take() {
                        out.done(id, Err(ProtoError::Timeout));
                        self.begin_next(out);
                    }
                }
                Input::Tick { .. } | Input::Timer(_) => {}
                Input::Request { id, req, .. } => {
                    self.queue.push_back((id, req));
                    if self.active.is_none() {
                        self.begin_next(out);
                    }
                }
            }
        }
    }

    /// Knows `cmd` and `evt`; every write is its own request except `ff`.
    struct EchoAdapter;

    impl Adapter for EchoAdapter {
        type Driver = Echo;

        fn channel(&self, name: &str) -> Option<Channel> {
            match name {
                "cmd" => Some(CMD),
                "evt" => Some(EVT),
                "ver" => Some(VER),
                _ => None,
            }
        }

        fn connected(&self) -> (ChannelSet, u16) {
            ([CMD, EVT].into_iter().collect(), 23)
        }

        fn request(&self, line: &DataLine) -> Option<Bytes> {
            (line.bytes != [0xff]).then(|| line.bytes.clone())
        }
    }

    fn run(text: &str) -> Replay<Echo> {
        let trace = Trace::parse(text).unwrap_or_else(|err| panic!("{err}"));
        let mut driver = Echo::default();
        let out = replay(&trace, &EchoAdapter, &mut driver);
        // Invariant 6: `Disconnected` left the driver fresh.
        assert!(
            driver.active.is_none() && driver.queue.is_empty(),
            "{driver:?}"
        );
        out
    }

    fn answer(line: usize, id: u32, result: Result<&[u8], ProtoError>) -> Answer<Echo> {
        Answer {
            line: Some(line),
            id: ReqId(id),
            result: result.map(<[u8]>::to_vec),
        }
    }

    const SESSION: &str = "\
# echo session
2026-01-01T00:00:00.000 tx cmd 01
2026-01-01T00:00:00.050 rx evt 81
2026-01-01T00:00:00.100 tx cmd 02
2026-01-01T00:00:00.250 rx evt 82
! 2026-01-01T00:00:00.300 an event line, skipped
2026-01-01T00:00:00.300 tx cmd 03
2026-01-01T00:00:00.310 rx weird 99
2026-01-01T00:00:00.320 tx cmd ff
2026-01-01T00:00:00.330 rx evt 83
2026-01-01T00:00:00.340 tx cmd 04
";

    #[test]
    fn writes_answers_and_events_are_recorded_in_order() {
        let out = run(SESSION);
        let writes = vec![
            (CMD, vec![0x01]),
            (CMD, vec![0x02]),
            (CMD, vec![0x03]),
            (CMD, vec![0x04]),
        ];
        assert_eq!(out.expected, writes);
        assert_eq!(out.written, writes);
        assert_eq!(out.writes_match(), Ok(()));
        assert_eq!(
            out.answers,
            vec![
                // Answered 50 ms later.
                answer(0, 0, Ok(&[0x81])),
                // The reply came 150 ms later: the timer fired first.
                answer(2, 1, Err(ProtoError::Timeout)),
                answer(4, 2, Ok(&[0x83])),
                // Never answered: `Disconnected` drained it.
                answer(8, 3, Err(ProtoError::Disconnected)),
            ]
        );
        // The late reply arrived with nothing in flight.
        assert_eq!(out.events, vec![vec![0x82]]);
        assert_eq!(out.skipped, [6]);
        assert_eq!(out.unknown_channels, [5]);
        assert_eq!(out.timers_fired, [(3, TIMER)]);
        assert_eq!(out.subscribed, [EVT]);
        assert_eq!(out.reads, [VER]);
        assert!(!out.disconnect_requested);
    }

    #[test]
    fn a_read_is_recorded_and_feeds_nothing() {
        // The value of the read is the trace's `rx ver` line, fed in its turn: the only
        // input the driver sees besides connect and disconnect, and so the only event.
        let out = run("\
2026-01-01T00:00:00.000 rx ver 77
2026-01-01T00:00:00.010 tx cmd 01
2026-01-01T00:00:00.020 rx evt 81
");
        assert_eq!(out.reads, [VER]);
        assert_eq!(out.events, vec![vec![0x77]]);
        assert_eq!(out.answers, vec![answer(1, 0, Ok(&[0x81]))]);
        assert_eq!(out.writes_match(), Ok(()));
    }

    #[test]
    fn a_timer_due_exactly_at_a_line_fires_before_it() {
        let out = run("\
2026-01-01T00:00:00.000 tx cmd 01
2026-01-01T00:00:00.100 rx evt 81
");
        assert_eq!(out.answers, vec![answer(0, 0, Err(ProtoError::Timeout))]);
        assert_eq!(out.events, vec![vec![0x81]]);
        assert_eq!(out.timers_fired, [(1, TIMER)]);

        let out = run("\
2026-01-01T00:00:00.000 tx cmd 01
2026-01-01T00:00:00.099 rx evt 81
");
        assert_eq!(out.answers, vec![answer(0, 0, Ok(&[0x81]))]);
        assert!(out.events.is_empty());
        assert!(out.timers_fired.is_empty());
    }

    #[test]
    fn a_timer_armed_by_a_firing_counts_from_the_deadline() {
        // The second request is queued behind the first; each times out 100 ms after it
        // was written, and both are due before the stray reply at 250 ms.
        let out = run("\
2026-01-01T00:00:00.000 tx cmd 01
2026-01-01T00:00:00.010 tx cmd 02
2026-01-01T00:00:00.250 rx evt 81
");
        assert_eq!(out.writes_match(), Ok(()));
        assert_eq!(
            out.answers,
            vec![
                answer(0, 0, Err(ProtoError::Timeout)),
                answer(1, 1, Err(ProtoError::Timeout)),
            ]
        );
        assert_eq!(out.timers_fired, [(2, TIMER), (2, TIMER)]);
        assert_eq!(out.events, vec![vec![0x81]]);

        // With the reply at 150 ms only the first is due; the reply answers the second.
        let out = run("\
2026-01-01T00:00:00.000 tx cmd 01
2026-01-01T00:00:00.010 tx cmd 02
2026-01-01T00:00:00.150 rx evt 81
");
        assert_eq!(
            out.answers,
            vec![
                answer(0, 0, Err(ProtoError::Timeout)),
                answer(1, 1, Ok(&[0x81])),
            ]
        );
        assert_eq!(out.timers_fired, [(2, TIMER)]);
    }

    #[test]
    fn a_stamp_before_the_first_clamps_to_zero() {
        let out = run("\
2026-01-01T00:00:01.000 tx cmd 01
2026-01-01T00:00:00.000 rx evt 81
");
        assert_eq!(out.answers, vec![answer(0, 0, Ok(&[0x81]))]);
        assert!(out.timers_fired.is_empty());
    }

    #[test]
    fn timers_still_armed_at_the_end_never_fire() {
        let out = run("2026-01-01T00:00:00.000 tx cmd 01\n");
        assert_eq!(
            out.answers,
            vec![answer(0, 0, Err(ProtoError::Disconnected))]
        );
        assert!(out.timers_fired.is_empty());
    }

    #[test]
    fn an_empty_trace_only_connects_and_disconnects() {
        let out = run("# nothing\n! 2026-01-01T00:00:00.000 connected\n");
        assert!(out.expected.is_empty());
        assert!(out.written.is_empty());
        assert!(out.answers.is_empty());
        assert_eq!(out.subscribed, [EVT]);
        assert_eq!(out.reads, [VER]);
        assert!(out.events.is_empty());
        assert_eq!(out.writes_match(), Ok(()));
    }

    #[test]
    fn writes_match_reports_the_first_difference() {
        let mut out = run(SESSION);
        assert_eq!(out.writes_match(), Ok(()));

        out.written[2] = (EVT, vec![0x03]);
        let mismatch = out.writes_match().unwrap_err();
        assert_eq!(
            mismatch,
            Mismatch {
                index: 2,
                expected: Some((CMD, vec![0x03])),
                written: Some((EVT, vec![0x03])),
            }
        );
        assert_eq!(
            mismatch.to_string(),
            "write 2: the trace has channel 0 03, the driver wrote channel 1 03"
        );

        out.written[2] = (CMD, vec![0x03]);
        out.written.pop();
        assert_eq!(
            out.writes_match(),
            Err(Mismatch {
                index: 3,
                expected: Some((CMD, vec![0x04])),
                written: None,
            })
        );
        assert_eq!(
            out.writes_match().unwrap_err().to_string(),
            "write 3: the trace has channel 0 04, the driver wrote nothing"
        );

        out.written.push((CMD, vec![0x04]));
        out.written.push((CMD, vec![0xab, 0xcd]));
        assert_eq!(
            out.writes_match().unwrap_err().to_string(),
            "write 4: the trace has nothing, the driver wrote channel 0 abcd"
        );
    }
}
