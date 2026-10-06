//! The Colmi [`Driver`]: serialises requests into transactions, one in flight at a time,
//! and sorts everything the ring sends into replies and events.

use alloc::collections::VecDeque;
use alloc::string::String;

use crate::measure::bpm_from_raw;
use junk_core::{
    Battery, Bytes, Channel, ChannelSet, Driver, Duration, Input, Outputs, ProtoError, ReqId,
    TimerId,
};

use crate::proto::txn::{Collected, Started, Step, Txn};
use crate::proto::{Dialect, Ev, Req, Resp, SleepSource};
use crate::wire::{Frame, RingFrame, V1Rx};
use crate::{DIS_FW, DIS_HW, GATT, V1_NOTIFY, V2_CMD, V2_NOTIFY};

/// The one timer the driver uses: it guards the active transaction.
///
/// One id is enough because of invariant 3: the pump applies every output of a step,
/// including [`Output::CancelTimer`](junk_core::Output::CancelTimer), before it selects the
/// next input, so a timer cancelled in the same step as the reply that made it moot never
/// reaches the driver, and re-arming the id for the next transaction replaces the old
/// deadline. The pump's tests pin this.
pub const TIMEOUT_TIMER: TimerId = TimerId(0);
/// How long a transaction may wait for the ring. The `QRing` log saw a set-time ack take
/// four seconds.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// Where the link is, as far as the driver knows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Phase {
    /// No link.
    #[default]
    Down,
    /// A link without the required channels: nothing can be sent, disconnect was requested.
    Unusable,
    /// A usable link.
    Ready {
        /// The negotiated ATT MTU. Recorded for the curious; the ring says its own packet
        /// size in `0x2f`.
        mtu: u16,
        /// Which declared channels the ring has: what decides whether a request that needs
        /// an optional channel can be made.
        resolved: ChannelSet,
    },
}

/// The transaction in flight.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Active {
    id: ReqId,
    txn: Txn,
}

/// A request waiting for its turn, already validated and encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Pending {
    id: ReqId,
    txn: Txn,
    chan: Channel,
    bytes: Bytes,
}

/// A [`Driver`] for Colmi rings.
///
/// State is `{ dialect, phase, active, queue }` as SPEC §3.3 prescribes: one transaction
/// in flight, the rest queued in request order, and the [`Dialect`] learned so far.
/// [`Input::Disconnected`] fails everything and resets to fresh, dialect included, so one
/// value serves any number of connections and each starts from what its own ring says
/// (invariant 6).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ColmiDriver {
    dialect: Dialect,
    phase: Phase,
    active: Option<Active>,
    queue: VecDeque<Pending>,
}

impl ColmiDriver {
    /// A driver that has never seen a link.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The ATT MTU of the current link, or `None` without a usable one.
    #[must_use]
    pub fn mtu(&self) -> Option<u16> {
        match self.phase {
            Phase::Down | Phase::Unusable => None,
            Phase::Ready { mtu, .. } => Some(mtu),
        }
    }

    /// What the ring on the current link speaks, as far as the session has shown; the
    /// conservative default without a link.
    #[must_use]
    pub fn dialect(&self) -> &Dialect {
        &self.dialect
    }

    fn on_connected(&mut self, resolved: ChannelSet, mtu: u16, out: &mut Outputs<Resp, Ev>) {
        // A `Connected` on top of a live link is a new link: nothing survives from the old one.
        self.on_disconnected(out);
        if !GATT.satisfied_by(&resolved) {
            out.event(Ev::MissingChannels(GATT.required().difference(&resolved)));
            out.disconnect();
            self.phase = Phase::Unusable;
            return;
        }
        self.dialect = Dialect::from_channels(&resolved);
        out.subscribe(V1_NOTIFY);
        if resolved.contains(V2_NOTIFY) {
            out.subscribe(V2_NOTIFY);
        }
        self.phase = Phase::Ready { mtu, resolved };
    }

    fn on_disconnected(&mut self, out: &mut Outputs<Resp, Ev>) {
        if let Some(active) = self.active.take() {
            out.cancel_timer(TIMEOUT_TIMER);
            out.done(active.id, Err(ProtoError::Disconnected));
        }
        for pending in self.queue.drain(..) {
            out.done(pending.id, Err(ProtoError::Disconnected));
        }
        *self = Self::new();
    }

    fn on_request(&mut self, id: ReqId, req: Req, out: &mut Outputs<Resp, Ev>) {
        let resolved = match &self.phase {
            Phase::Down | Phase::Unusable => {
                out.done(id, Err(ProtoError::Disconnected));
                return;
            }
            Phase::Ready { resolved, .. } => *resolved,
        };
        if let Some(what) = refused_by(&self.dialect, &resolved, &req) {
            out.done(id, Err(ProtoError::Unsupported(what)));
            return;
        }
        let Started { txn, chan, bytes } = match Txn::start(req) {
            Ok(started) => started,
            Err(err) => {
                out.done(id, Err(err));
                return;
            }
        };
        if chan == V2_CMD && !self.dialect.big_data {
            out.done(
                id,
                Err(ProtoError::Unsupported(
                    "big data: the ring has no V2 service",
                )),
            );
            return;
        }
        let pending = Pending {
            id,
            txn,
            chan,
            bytes,
        };
        if self.active.is_some() {
            self.queue.push_back(pending);
        } else {
            self.begin(pending, out);
        }
    }

    fn on_rx(&mut self, chan: Channel, bytes: Bytes, out: &mut Outputs<Resp, Ev>) {
        if chan == V1_NOTIFY {
            self.on_v1(bytes, out);
        } else if chan == V2_NOTIFY {
            self.on_v2(bytes, out);
        } else if chan == DIS_FW || chan == DIS_HW {
            self.on_dis(chan, &bytes, out);
        } else {
            out.event(Ev::Unparsed { chan, bytes });
        }
    }

    /// One notification on V1 notify: a frame, a raw string, or neither.
    fn on_v1(&mut self, bytes: Bytes, out: &mut Outputs<Resp, Ev>) {
        let frame = match V1Rx::classify(&bytes) {
            V1Rx::Frame(frame) => frame,
            // News: the version strings are read from the Device Information service,
            // never notified here, so a string that arrives anyway is reported as it came.
            V1Rx::Text(text) => {
                out.event(Ev::Text(text));
                return;
            }
            V1Rx::Invalid(_) => {
                out.event(Ev::Unparsed {
                    chan: V1_NOTIFY,
                    bytes,
                });
                return;
            }
        };
        let Ok(reply) = RingFrame::decode(&frame) else {
            out.event(Ev::Unparsed {
                chan: V1_NOTIFY,
                bytes,
            });
            return;
        };
        match reply {
            // The capability bitmap is news whenever it comes, and answers a set-time
            // request if one is waiting; an ack nobody asked for is reported once, here.
            RingFrame::SetTimeAck(caps) => {
                self.dialect.apply_capabilities(&caps);
                out.event(Ev::Capabilities(caps));
                self.feed_frame(&frame, &reply, out);
            }
            // Unsolicited by nature: never a transaction's business.
            RingFrame::PacketSize(size) => out.event(Ev::PacketSize(size)),
            RingFrame::Notify(notification) => out.event(Ev::Notification(notification)),
            RingFrame::WorkoutData {
                sport_type,
                flag,
                seq,
                bpm,
            } => out.event(Ev::Workout {
                sport_type,
                flag,
                seq,
                bpm: bpm_from_raw(bpm),
            }),
            // Replies: the active transaction's, or nobody's.
            RingFrame::Battery { .. }
            | RingFrame::PhoneNameAck
            | RingFrame::Prefs(_)
            | RingFrame::PrefsWriteAck
            | RingFrame::HrLog(_)
            | RingFrame::AutoHrPref { .. }
            | RingFrame::VersionAck
            | RingFrame::Goals { .. }
            | RingFrame::AutoSpo2Pref { .. }
            | RingFrame::AutoStressPref { .. }
            | RingFrame::Series { .. }
            | RingFrame::AutoHrvPref { .. }
            | RingFrame::Activity(_)
            | RingFrame::TodayTotals { .. }
            | RingFrame::WorkoutCtlAck { .. }
            | RingFrame::Raw { .. } => {
                if !self.feed_frame(&frame, &reply, out) {
                    out.event(unsolicited(reply));
                }
            }
        }
    }

    /// The value of a Device Information characteristic, read at the version
    /// transaction's request: its next string, or news.
    fn on_dis(&mut self, chan: Channel, bytes: &[u8], out: &mut Outputs<Resp, Ev>) {
        let text = dis_text(bytes);
        let Some(mut active) = self.active.take() else {
            out.event(Ev::Text(text));
            return;
        };
        let step = active.txn.feed_text(chan, &text);
        if !self.settle(active, step, out) {
            out.event(Ev::Text(text));
        }
    }

    /// One notification on V2 notify: a piece of the big-data frame the active transaction
    /// is collecting, or noise.
    fn on_v2(&mut self, bytes: Bytes, out: &mut Outputs<Resp, Ev>) {
        let Some(mut active) = self.active.take() else {
            out.event(Ev::Unparsed {
                chan: V2_NOTIFY,
                bytes,
            });
            return;
        };
        match active.txn.collect_v2(&bytes) {
            Collected::NotWanted => {
                self.active = Some(active);
                out.event(Ev::Unparsed {
                    chan: V2_NOTIFY,
                    bytes,
                });
            }
            Collected::Incomplete => self.active = Some(active),
            Collected::Fail(err) => {
                out.cancel_timer(TIMEOUT_TIMER);
                self.finish(active.id, Err(err), out);
            }
            Collected::Frame(big) => {
                let step = active.txn.feed_big(&big);
                if !self.settle(active, step, out) {
                    out.event(Ev::UnexpectedBigData(big));
                }
            }
        }
    }

    /// Feeds one typed reply to the active transaction. Returns `false` when there is
    /// none, or it did not want the reply; nothing changed then.
    fn feed_frame(
        &mut self,
        frame: &Frame,
        reply: &RingFrame,
        out: &mut Outputs<Resp, Ev>,
    ) -> bool {
        let Some(mut active) = self.active.take() else {
            return false;
        };
        let step = active.txn.feed_frame(frame, reply);
        self.settle(active, step, out)
    }

    /// Applies what the active transaction made of one input: keeps it in flight, reading
    /// what it asked for, or answers its request and starts the next. Returns `false` if
    /// it ignored the input.
    fn settle(&mut self, active: Active, step: Step, out: &mut Outputs<Resp, Ev>) -> bool {
        match step {
            Step::Continue => {
                self.active = Some(active);
                true
            }
            // The timer stays armed: the read is part of the request it guards.
            Step::Read(chan) => {
                out.read(chan);
                self.active = Some(active);
                true
            }
            Step::Ignored => {
                self.active = Some(active);
                false
            }
            Step::Done(resp) => {
                out.cancel_timer(TIMEOUT_TIMER);
                self.complete(active.id, resp, out);
                true
            }
            Step::Fail(err) => {
                out.cancel_timer(TIMEOUT_TIMER);
                self.finish(active.id, Err(err), out);
                true
            }
        }
    }

    fn on_timer(&mut self, id: TimerId, out: &mut Outputs<Resp, Ev>) {
        if id != TIMEOUT_TIMER {
            return;
        }
        let Some(active) = self.active.take() else {
            return;
        };
        self.finish(active.id, Err(ProtoError::Timeout), out);
    }

    /// Answers `id` with `resp`, first letting the dialect learn from it: the firmware
    /// string picks a dialect row, and one no row matches is reported.
    fn complete(&mut self, id: ReqId, resp: Resp, out: &mut Outputs<Resp, Ev>) {
        if let Resp::Version { firmware, .. } = &resp
            && !self.dialect.apply_firmware(firmware)
        {
            out.event(Ev::UnknownFirmware(firmware.clone()));
        }
        self.finish(id, Ok(resp), out);
    }

    /// Answers `id` and starts the next queued request, if any, in the same step.
    fn finish(&mut self, id: ReqId, result: Result<Resp, ProtoError>, out: &mut Outputs<Resp, Ev>) {
        out.done(id, result);
        if let Some(pending) = self.queue.pop_front() {
            self.begin(pending, out);
        }
    }

    /// Makes `pending` the active transaction: writes its request and arms the timer.
    fn begin(&mut self, pending: Pending, out: &mut Outputs<Resp, Ev>) {
        let Pending {
            id,
            txn,
            chan,
            bytes,
        } = pending;
        out.tx(chan, bytes);
        out.set_timer(TIMEOUT_TIMER, TIMEOUT);
        self.active = Some(Active { id, txn });
    }
}

/// Why the ring cannot be asked `req`, if it cannot: temperature needs the capability bit,
/// sleep over `bc 27` needs the ring to have announced the new sleep protocol (the legacy
/// `0x44` frames have no request here), and the version needs the Device Information
/// channels among `resolved`, since its strings are read from them. Whether the ring has a
/// V2 service at all is checked once the request has been encoded and its channel is known.
fn refused_by(dialect: &Dialect, resolved: &ChannelSet, req: &Req) -> Option<&'static str> {
    match req {
        Req::Temperature { .. } if !dialect.temperature => {
            Some("temperature: the ring does not measure it")
        }
        Req::Sleep { .. } if dialect.sleep != SleepSource::BigData27 => {
            Some("sleep: the ring does not send it as big data")
        }
        Req::Version if !(resolved.contains(DIS_FW) && resolved.contains(DIS_HW)) => {
            Some("version: no device information service")
        }
        Req::SetTime { .. }
        | Req::Battery
        | Req::PhoneName { .. }
        | Req::ReadPrefs
        | Req::WritePrefs(_)
        | Req::Version
        | Req::ReadGoals
        | Req::ReadAutoHrPref
        | Req::ReadAutoSpo2Pref
        | Req::ReadAutoStressPref
        | Req::ReadAutoHrvPref
        | Req::TodayTotals
        | Req::WorkoutCtl { .. }
        | Req::ManualHrStart { .. }
        | Req::ManualHrStop { .. }
        | Req::Raw { .. }
        | Req::RawBigData { .. }
        | Req::HrLog { .. }
        | Req::Stress { .. }
        | Req::Hrv { .. }
        | Req::Activity { .. }
        | Req::Sleep { .. }
        | Req::Spo2 { .. }
        | Req::Temperature { .. }
        | Req::WorkoutList { .. }
        | Req::WorkoutDetail { .. } => None,
    }
}

/// The value of a Device Information characteristic as text: UTF-8, lossily, with the
/// trailing NULs some firmwares pad the string with trimmed.
fn dis_text(bytes: &[u8]) -> String {
    String::from(String::from_utf8_lossy(bytes).trim_end_matches('\0'))
}

/// The event for a well-formed reply no transaction wanted: a battery reading is news in
/// its own right, anything else is reported as it came.
fn unsolicited(reply: RingFrame) -> Ev {
    match reply {
        RingFrame::Battery { percent, charging } => Ev::Battery(Battery { percent, charging }),
        other => Ev::UnexpectedReply(other),
    }
}

impl Driver for ColmiDriver {
    type Req = Req;
    type Resp = Resp;
    type Ev = Ev;

    const GATT: &'static junk_core::GattMap = &GATT;

    fn handle(&mut self, input: Input<Req>, out: &mut Outputs<Resp, Ev>) {
        match input {
            Input::Connected { resolved, mtu } => self.on_connected(resolved, mtu, out),
            Input::Disconnected => self.on_disconnected(out),
            Input::Rx { chan, bytes } => self.on_rx(chan, bytes, out),
            Input::Tick { .. } => {}
            Input::Timer(id) => self.on_timer(id, out),
            Input::Request { id, req, .. } => self.on_request(id, req, out),
        }
    }
}
