//! The fakering [`Driver`]: serialises requests into transactions, one in flight at a time.

use alloc::collections::VecDeque;

use junk_core::{
    Battery, Bytes, Channel, ChannelSet, Driver, Duration, Input, Outputs, ProtoError, ReqId,
    TimerId,
};

use crate::gatt::{CMD, EVT, GATT};
use crate::txn::{Step, Txn};
use crate::wire::{Command, Frame, Unsolicited};

/// The one timer the driver uses: it guards the active transaction.
///
/// One id is enough because of invariant 3: the pump applies every output of a step,
/// including [`Output::CancelTimer`](junk_core::Output::CancelTimer), before it selects the
/// next input, so a timer cancelled in the same step as the reply that made it moot never
/// reaches the driver, and re-arming the id for the next transaction replaces the old
/// deadline. The pump's tests pin this.
pub const TIMEOUT_TIMER: TimerId = TimerId(0);
/// How long a transaction may wait for the device.
pub const TIMEOUT: Duration = Duration::from_secs(1);

/// What a user can ask a fakering device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Req {
    /// Are you there?
    Ping,
    /// Read the value stored under this key.
    Get(u8),
    /// Have these bytes sent back; at most [`MAX_ECHO`](crate::MAX_ECHO) of them.
    Echo(Bytes),
}

/// The answer to a [`Req`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resp {
    /// Answer to [`Req::Ping`].
    Pong,
    /// Answer to [`Req::Get`].
    Value(u32),
    /// Answer to [`Req::Echo`]: the bytes, reassembled.
    Echo(Bytes),
}

/// Things a fakering device reports on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ev {
    /// The device's heartbeat counter.
    Heartbeat(u32),
    /// The device's battery.
    Battery(Battery),
    /// Bytes that were not a valid frame in context. Nothing else happened (invariant 5).
    Unparsed {
        /// Where they arrived.
        chan: Channel,
        /// What they were.
        bytes: Bytes,
    },
    /// The link came up without these required channels; the driver asked to disconnect.
    MissingChannels(ChannelSet),
}

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
        /// The negotiated ATT MTU. Recorded for the curious; fakering frames never approach it.
        mtu: u16,
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
    bytes: Bytes,
}

/// A [`Driver`] for fakering devices.
///
/// State is `{ phase, active, queue }` as SPEC §3.3 prescribes: one transaction in flight,
/// the rest queued in request order. [`Input::Disconnected`] fails everything and resets
/// to fresh, so one value serves any number of connections (invariant 6).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FakeDriver {
    phase: Phase,
    active: Option<Active>,
    queue: VecDeque<Pending>,
}

impl FakeDriver {
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
            Phase::Ready { mtu } => Some(mtu),
        }
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
        out.subscribe(EVT);
        self.phase = Phase::Ready { mtu };
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
        match self.phase {
            Phase::Down | Phase::Unusable => {
                out.done(id, Err(ProtoError::Disconnected));
                return;
            }
            Phase::Ready { .. } => {}
        }
        let (txn, bytes) = match Txn::start(req) {
            Ok(started) => started,
            Err(err) => {
                out.done(id, Err(err));
                return;
            }
        };
        let pending = Pending { id, txn, bytes };
        if self.active.is_some() {
            self.queue.push_back(pending);
        } else {
            self.begin(pending, out);
        }
    }

    fn on_rx(&mut self, chan: Channel, bytes: Bytes, out: &mut Outputs<Resp, Ev>) {
        let frame = if chan == EVT {
            Frame::parse(&bytes)
        } else {
            None
        };
        let Some(frame) = frame else {
            out.event(Ev::Unparsed { chan, bytes });
            return;
        };
        match frame {
            Frame::Unsolicited(Unsolicited::Heartbeat { counter }) => {
                out.event(Ev::Heartbeat(counter));
            }
            Frame::Unsolicited(Unsolicited::Battery { percent }) => {
                out.event(Ev::Battery(Battery {
                    percent,
                    charging: false,
                }));
            }
            Frame::Reply(reply) => {
                let Some(mut active) = self.active.take() else {
                    out.event(Ev::Unparsed { chan, bytes });
                    return;
                };
                match active.txn.feed(&reply) {
                    Step::Continue => self.active = Some(active),
                    Step::Ignored => {
                        self.active = Some(active);
                        out.event(Ev::Unparsed { chan, bytes });
                    }
                    Step::Done(resp) => {
                        out.cancel_timer(TIMEOUT_TIMER);
                        self.finish(active.id, Ok(resp), out);
                    }
                    Step::Fail(err) => {
                        out.cancel_timer(TIMEOUT_TIMER);
                        self.finish(active.id, Err(err), out);
                    }
                }
            }
            // The device does not send commands; on the event channel they are noise.
            Frame::Command(Command::Ping | Command::Get { .. } | Command::Echo { .. }) => {
                out.event(Ev::Unparsed { chan, bytes });
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

    /// Answers `id` and starts the next queued request, if any, in the same step.
    fn finish(&mut self, id: ReqId, result: Result<Resp, ProtoError>, out: &mut Outputs<Resp, Ev>) {
        out.done(id, result);
        if let Some(pending) = self.queue.pop_front() {
            self.begin(pending, out);
        }
    }

    /// Makes `pending` the active transaction: writes its command and arms the timer.
    fn begin(&mut self, pending: Pending, out: &mut Outputs<Resp, Ev>) {
        let Pending { id, txn, bytes } = pending;
        out.tx(CMD, bytes);
        out.set_timer(TIMEOUT_TIMER, TIMEOUT);
        self.active = Some(Active { id, txn });
    }
}

impl Driver for FakeDriver {
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
