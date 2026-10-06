//! The Sony [`Driver`]: the link layer, the init sequence, and one step in flight at a time.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec::Vec;

use junk_core::{
    Bytes, Channel, ChannelSet, Driver, Duration, Input, Outputs, ProtoError, ReqId, TimerId,
};

use crate::gatt::{GATT, RFCOMM};
use crate::payload::{MdrLanguage, Report, Table, decode_report};
use crate::proto::plan::{self, InitStage};
use crate::proto::slot::{Outcome, Slot};
use crate::proto::status::{RawReply, Status};
use crate::proto::txn::{Expect, Step};
use crate::proto::{Ev, Req, Resp};
use crate::wire::{DataType, Frame};

/// Guards the ACK of the command on the wire. On expiry the identical frame, same sequence
/// number, is written again (the link layer of Sony's app).
pub const ACK_TIMER: TimerId = TimerId(0);
/// Guards the reply to an acknowledged command. On expiry the step is recorded as no reply
/// and the session goes on.
pub const REPLY_TIMER: TimerId = TimerId(1);
/// How long to wait for an ACK before resending: 750 ms, Sony's app's number.
pub const ACK_TIMEOUT: Duration = Duration::from_millis(750);
/// How many times a frame is resent before the link is given up: 10, so 11 transmissions
/// in all.
pub const MAX_RESENDS: u8 = 10;
/// How long to wait for the reply to an acknowledged command. **Not Sony's number**: the app
/// has no per-step timeout, only 30 seconds for the whole init. The headset acknowledges a
/// command before it answers it, so this starts at the ACK; three seconds is a guess about
/// how long an answer takes, and the only cost of a wrong guess is a slower or emptier
/// [`Status`](super::Status).
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(3);
/// The language the init sends in the capability GETs that take one (`<lang>` in
/// Sony's init). The headset localises the names it returns in it. English, because it
/// is the only one a user of this tool can be assumed to read; [`SonyDriver::with_language`]
/// changes it.
pub const DEFAULT_LANGUAGE: MdrLanguage = MdrLanguage::English;
/// The protocol versions Sony's app goes on with (its init's whitelist); any other aborts
/// its init, and this driver's.
///
/// **These are hexadecimal**: the app's constants are decimal (4096, 8192, ... 28688), i.e. `0x1000 ... 0x7010`. Read as decimal `4010`, the
/// community's XM3 reply `01 00 40 10` (`0x4010`) would be refused by the very app it works
/// with. The gate on `04 04` is `>= 0x5000` (20480 in the app's constants).
pub const SUPPORTED_PROTOCOL_VERSIONS: [u16; 9] = [
    0x1000, 0x2000, 0x3000, 0x4000, 0x4010, 0x5000, 0x6000, 0x7000, 0x7010,
];

/// Where the link is, as far as the driver knows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Phase {
    /// No link, or one the driver gave up on.
    #[default]
    Down,
    /// A usable link.
    Ready,
}

/// How init stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Init {
    /// Running, or not started.
    #[default]
    Pending,
    /// Failed for good on this connection, with this error.
    Failed(ProtoError),
}

/// What the driver is working through, besides what is in flight.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Job {
    /// Nothing: the next queued request starts when the wire is free.
    #[default]
    Idle,
    /// Init: the next stage to plan when the steps already planned are done.
    Init(InitStage),
    /// A status read.
    Status { id: ReqId, status: Box<Status> },
}

/// A frame the headset sent in answer to the step in flight.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Reply {
    payload: Bytes,
    /// The whole frame as it came, for [`Ev::Unparsed`].
    raw: Bytes,
}

/// The step on the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
struct InFlight {
    step: Step,
    /// The encoded frame, written again on each resend.
    bytes: Bytes,
    resends: u8,
    acked: bool,
    reply: Option<Reply>,
}

/// A request waiting for the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Queued {
    id: ReqId,
    req: Req,
}

/// A [`Driver`] for the Sony WH-1000XM4.
///
/// State is the link (`tx_seq`, `last_rx`), the init's progress, the job being worked
/// through and the step in flight, and the queue of requests behind them, one step in
/// flight at a time as SPEC §3.3 prescribes. [`Input::Disconnected`] fails everything and
/// resets to fresh (invariant 6).
///
/// The headset speaks over one [`Dir::Stream`](junk_core::Dir::Stream) channel, so this
/// driver is fed whole frames only because a `Framed` sits between it and the transport
/// with [`SonyFraming`](crate::SonyFraming) as its rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SonyDriver {
    language: MdrLanguage,
    phase: Phase,
    init: Init,
    /// The sequence number the next command goes out with.
    tx_seq: u8,
    /// The sequence number of the last data frame processed, to recognise a retransmission.
    last_rx: Option<u8>,
    /// What init found. Persists until the link goes.
    session: Status,
    job: Job,
    /// Steps planned and not yet sent, in order.
    steps: VecDeque<Step>,
    inflight: Option<InFlight>,
    queue: VecDeque<Queued>,
}

impl Default for SonyDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl SonyDriver {
    /// A driver that has never seen a link, asking for names in [`DEFAULT_LANGUAGE`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_language(DEFAULT_LANGUAGE)
    }

    /// A driver that asks the headset to localise its names in `language`.
    #[must_use]
    pub fn with_language(language: MdrLanguage) -> Self {
        Self {
            language,
            phase: Phase::Down,
            init: Init::Pending,
            tx_seq: 0,
            last_rx: None,
            session: Status::default(),
            job: Job::Idle,
            steps: VecDeque::new(),
            inflight: None,
            queue: VecDeque::new(),
        }
    }

    /// Whether the init sequence has run to its end on this connection, answered or not.
    #[must_use]
    pub fn init_finished(&self) -> bool {
        self.phase == Phase::Ready
            && self.init == Init::Pending
            && self.job == Job::Idle
            && self.steps.is_empty()
            && self.inflight.is_none()
    }

    fn on_connected(&mut self, resolved: &ChannelSet, out: &mut Outputs<Resp, Ev>) {
        // A `Connected` on top of a live link is a new link: nothing survives from the old one.
        self.on_disconnected(out);
        if !GATT.satisfied_by(resolved) {
            out.event(Ev::MissingChannels(GATT.required().difference(resolved)));
            out.disconnect();
            return;
        }
        // No subscribe: a byte stream pushes without being asked.
        self.phase = Phase::Ready;
        self.job = Job::Init(InitStage::Protocol);
        self.advance(out);
    }

    fn on_disconnected(&mut self, out: &mut Outputs<Resp, Ev>) {
        self.fail_everything(ProtoError::Disconnected, out);
        *self = Self::with_language(self.language);
    }

    /// Answers every request the driver holds: the one being worked on with `active`, the
    /// ones behind it with [`ProtoError::Disconnected`], and cancels the timer in use.
    fn fail_everything(&mut self, active: ProtoError, out: &mut Outputs<Resp, Ev>) {
        if let Some(inflight) = self.inflight.take() {
            out.cancel_timer(if inflight.acked {
                REPLY_TIMER
            } else {
                ACK_TIMER
            });
            if let Slot::Raw(id) = inflight.step.slot {
                out.done(id, Err(active));
            }
        }
        if let Job::Status { id, .. } = core::mem::take(&mut self.job) {
            out.done(id, Err(active));
        }
        for queued in self.queue.drain(..) {
            out.done(queued.id, Err(ProtoError::Disconnected));
        }
    }

    fn on_request(&mut self, id: ReqId, req: Req, out: &mut Outputs<Resp, Ev>) {
        match (&self.phase, &self.init) {
            (Phase::Down, _) => {
                out.done(id, Err(ProtoError::Disconnected));
                return;
            }
            (Phase::Ready, Init::Failed(err)) => {
                out.done(id, Err(*err));
                return;
            }
            (Phase::Ready, Init::Pending) => {}
        }
        if let Req::Raw { data_type, payload } = &req {
            if !data_type.needs_ack() {
                out.done(
                    id,
                    Err(ProtoError::Unsupported(
                        "raw: only a data type that is acknowledged can be sent",
                    )),
                );
                return;
            }
            if u32::try_from(payload.len()).is_err() {
                out.done(id, Err(ProtoError::Unsupported("raw: payload too long")));
                return;
            }
        }
        self.queue.push_back(Queued { id, req });
        self.advance(out);
    }

    /// Writes the next thing, if the wire is free: the next planned step, else the next
    /// stage of init, else the end of the job and the next request.
    fn advance(&mut self, out: &mut Outputs<Resp, Ev>) {
        loop {
            if self.inflight.is_some() {
                return;
            }
            if let Some(step) = self.steps.pop_front() {
                self.send(step, out);
                continue;
            }
            match core::mem::take(&mut self.job) {
                Job::Init(stage) => {
                    let steps = plan::init_stage(stage, &mut self.session, self.language);
                    for step in &steps {
                        self.session.record(step.slot, Outcome::NoReply);
                    }
                    self.steps.extend(steps);
                    self.job = stage.next().map_or(Job::Idle, Job::Init);
                }
                Job::Status { id, status } => out.done(id, Ok(Resp::Status(status))),
                Job::Idle => match self.queue.pop_front() {
                    Some(queued) => self.start(queued),
                    None => return,
                },
            }
        }
    }

    /// Makes `queued` the job, or for a raw request, plans its one step.
    fn start(&mut self, queued: Queued) {
        let Queued { id, req } = queued;
        match req {
            Req::Status => {
                let functions = self.session.device.functions.value().cloned();
                let mut status = Status {
                    device: self.session.device.clone(),
                    capabilities: self.session.capabilities.clone(),
                    general_settings: self.session.general_settings.clone(),
                    raw_replies: self.session.raw_replies.clone(),
                    ..Status::default()
                };
                let steps = if let Some(functions) = functions {
                    plan::status_steps(&functions)
                } else {
                    status = status.all_no_reply();
                    Vec::new()
                };
                for step in &steps {
                    status.record(step.slot, Outcome::NoReply);
                }
                self.steps.extend(steps);
                self.job = Job::Status {
                    id,
                    status: Box::new(status),
                };
            }
            Req::Raw { data_type, payload } => {
                // A raw request is a job of one step, and the step answers it.
                self.steps.push_back(Step {
                    data_type,
                    payload,
                    expect: Expect::NextData,
                    slot: Slot::Raw(id),
                });
            }
        }
    }

    /// Writes `step` with the current sequence number and arms the ACK timer.
    fn send(&mut self, step: Step, out: &mut Outputs<Resp, Ev>) {
        let frame = Frame::new(step.data_type, self.tx_seq, step.payload.clone());
        let Ok(bytes) = frame.encode() else {
            // A payload that cannot be framed was refused when the request came in; a step
            // of the driver's own is a few bytes. Nothing to write is nothing answered.
            self.settle(&step, None, out);
            return;
        };
        out.tx(RFCOMM, bytes.clone());
        out.set_timer(ACK_TIMER, ACK_TIMEOUT);
        self.inflight = Some(InFlight {
            step,
            bytes,
            resends: 0,
            acked: false,
            reply: None,
        });
    }

    /// One whole frame off the stream.
    fn on_rx(&mut self, chan: Channel, bytes: Bytes, out: &mut Outputs<Resp, Ev>) {
        if self.phase != Phase::Ready {
            return;
        }
        if chan != RFCOMM {
            out.event(Ev::Unparsed { bytes });
            return;
        }
        let frame = match Frame::decode(&bytes) {
            Ok(frame) => frame,
            Err(error) => {
                // No ACK: the headset retransmits what it does not see acknowledged.
                out.event(Ev::Dropped { error, bytes });
                return;
            }
        };
        if frame.data_type == DataType::Ack {
            self.on_ack(frame.seq, out);
            return;
        }
        let needs_ack = frame.data_type.needs_ack();
        if needs_ack {
            // Before anything else, and for everything that asks: including what the driver
            // does not understand and what it has already seen.
            if let Some(ack) = Frame::ack_for(frame.seq).and_then(|ack| ack.encode().ok()) {
                out.tx(RFCOMM, ack);
            }
            if self.last_rx == Some(frame.seq) {
                return;
            }
            self.last_rx = Some(frame.seq);
        }
        self.on_data(&frame, bytes, out);
    }

    /// An ACK frame. Its sequence number is `1 - ` the command's; one that equals the
    /// current transmit number is "invalid ack, ignore" in Sony's app.
    fn on_ack(&mut self, seq: u8, out: &mut Outputs<Resp, Ev>) {
        let Some(inflight) = self.inflight.as_mut() else {
            return;
        };
        if inflight.acked || seq == self.tx_seq {
            return;
        }
        self.tx_seq = seq;
        inflight.acked = true;
        out.cancel_timer(ACK_TIMER);
        if inflight.reply.is_some() {
            // The reply came first; the step was waiting only for this.
            self.finish_inflight(out);
        } else {
            out.set_timer(REPLY_TIMER, REPLY_TIMEOUT);
        }
    }

    /// A data frame, already acknowledged and known not to be a retransmission.
    fn on_data(&mut self, frame: &Frame, raw: Bytes, out: &mut Outputs<Resp, Ev>) {
        if self.offer_reply(frame, &raw, out) {
            return;
        }
        let Some(table) = Table::of(frame.data_type) else {
            out.event(Ev::Unparsed { bytes: raw });
            return;
        };
        // The action log (`C9`) streams large JSON nobody asked for: acknowledged, ignored.
        if table == Table::One && frame.payload.first() == Some(&0xc9) {
            return;
        }
        match decode_report(table, &frame.payload) {
            Ok(Some(report)) => out.event(Ev::Report(report)),
            Ok(None) | Err(_) => out.event(Ev::Unparsed { bytes: raw }),
        }
    }

    /// Hands `frame` to the step in flight if it is that step's reply. Returns whether it
    /// was taken.
    fn offer_reply(&mut self, frame: &Frame, raw: &[u8], out: &mut Outputs<Resp, Ev>) -> bool {
        let Some(inflight) = self.inflight.as_mut() else {
            return false;
        };
        if inflight.reply.is_some() || !inflight.step.wants(frame) {
            return false;
        }
        inflight.reply = Some(Reply {
            payload: frame.payload.clone(),
            raw: raw.to_vec(),
        });
        // A reply that beats its ACK waits for it, so the sequence number is settled
        // before the next command goes out.
        if inflight.acked {
            out.cancel_timer(REPLY_TIMER);
            self.finish_inflight(out);
        }
        true
    }

    /// The step in flight has its ACK and its reply: settle it and go on.
    fn finish_inflight(&mut self, out: &mut Outputs<Resp, Ev>) {
        if let Some(inflight) = self.inflight.take() {
            self.settle(&inflight.step, inflight.reply, out);
        }
        self.advance(out);
    }

    fn on_timer(&mut self, id: TimerId, out: &mut Outputs<Resp, Ev>) {
        if id == ACK_TIMER {
            self.on_ack_timer(out);
        } else if id == REPLY_TIMER {
            let Some(inflight) = self.inflight.take_if(|f| f.acked && f.reply.is_none()) else {
                return;
            };
            self.settle(&inflight.step, None, out);
            self.advance(out);
        }
    }

    /// No ACK in time: write the identical frame again, or give up on the link.
    fn on_ack_timer(&mut self, out: &mut Outputs<Resp, Ev>) {
        let Some(inflight) = self.inflight.as_mut() else {
            return;
        };
        if inflight.acked {
            return;
        }
        if inflight.resends >= MAX_RESENDS {
            // "Remote endpoint does not respond": the app closes the connection. The request
            // in flight, if there is a user's, fails with a timeout, the rest with the link.
            self.fail_everything(ProtoError::Timeout, out);
            *self = Self::with_language(self.language);
            out.disconnect();
            return;
        }
        inflight.resends += 1;
        out.tx(RFCOMM, inflight.bytes.clone());
        out.set_timer(ACK_TIMER, ACK_TIMEOUT);
    }

    /// Applies the end of a step: `reply` is what came, `None` for nothing in time.
    fn settle(&mut self, step: &Step, reply: Option<Reply>, out: &mut Outputs<Resp, Ev>) {
        if let Slot::Raw(id) = step.slot {
            out.done(
                id,
                reply.map_or(Err(ProtoError::Timeout), |reply| {
                    Ok(Resp::Raw(reply.payload))
                }),
            );
            return;
        }
        let outcome = match &reply {
            None => Outcome::NoReply,
            Some(reply) => {
                let decoded =
                    Table::of(step.data_type).map(|table| decode_report(table, &reply.payload));
                match decoded {
                    Some(Ok(Some(report))) => Outcome::Report(report),
                    _ if step.slot.is_raw_only() => Outcome::Malformed,
                    _ => {
                        out.event(Ev::Unparsed {
                            bytes: reply.raw.clone(),
                        });
                        Outcome::Malformed
                    }
                }
            }
        };
        if step.slot.is_init() {
            if let Some(reply) = reply {
                self.session.raw_replies.push(RawReply {
                    data_type: step.data_type,
                    payload: reply.payload,
                });
            }
            let version = match (&step.slot, &outcome) {
                (Slot::ProtocolVersion, Outcome::Report(Report::ProtocolVersion(v))) => Some(*v),
                _ => None,
            };
            self.session.record(step.slot, outcome);
            if let Some(version) = version {
                self.check_version(version, out);
            }
        } else if let Job::Status { status, .. } = &mut self.job {
            status.record(step.slot, outcome);
        }
    }

    /// Surfaces the protocol version, and ends init if Sony's app would.
    fn check_version(&mut self, version: u16, out: &mut Outputs<Resp, Ev>) {
        let supported = SUPPORTED_PROTOCOL_VERSIONS.contains(&version);
        out.event(Ev::ProtocolVersion { version, supported });
        if supported {
            return;
        }
        let err = ProtoError::Unsupported("protocol version is not one Sony's app accepts");
        self.init = Init::Failed(err);
        self.steps.clear();
        self.job = Job::Idle;
        for queued in self.queue.drain(..) {
            out.done(queued.id, Err(err));
        }
    }
}

impl Driver for SonyDriver {
    type Req = Req;
    type Resp = Resp;
    type Ev = Ev;

    const GATT: &'static junk_core::GattMap = &GATT;

    fn handle(&mut self, input: Input<Req>, out: &mut Outputs<Resp, Ev>) {
        match input {
            Input::Connected { resolved, .. } => self.on_connected(&resolved, out),
            Input::Disconnected => self.on_disconnected(out),
            Input::Rx { chan, bytes } => self.on_rx(chan, bytes, out),
            Input::Tick { .. } => {}
            Input::Timer(id) => self.on_timer(id, out),
            Input::Request { id, req, .. } => self.on_request(id, req, out),
        }
    }
}
