//! The Soundcore [`Driver`]: serialises requests into transactions, one in flight at a
//! time, and sorts everything the speaker sends into replies and events.

use alloc::collections::VecDeque;

use junk_core::{
    Bytes, Channel, ChannelSet, Driver, Duration, Input, Outputs, ProtoError, ReqId, TimerId,
};

use crate::gatt::{GATT, RFCOMM};
use crate::proto::info::battery_from_fifths;
use crate::proto::txn::{Started, Step, Txn};
use crate::proto::{Ev, Req, Resp};
use crate::wire::{Command, Direction, Packet};

/// The one timer the driver uses: it guards the active transaction.
///
/// One id is enough because of invariant 3: the pump applies every output of a step,
/// including [`Output::CancelTimer`](junk_core::Output::CancelTimer), before it selects the
/// next input, so a timer cancelled in the same step as the reply that made it moot never
/// reaches the driver, and re-arming the id for the next transaction replaces the old
/// deadline. The pump's tests pin this.
pub const TIMEOUT_TIMER: TimerId = TimerId(0);
/// How long a transaction may wait for the speaker. The probe of 2026-09-15 was answered
/// in 47 ms; five seconds is the same allowance the ring gets.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// Where the link is, as far as the driver knows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Phase {
    /// No link.
    #[default]
    Down,
    /// A link without the byte stream: nothing can be sent, disconnect was requested.
    Unusable,
    /// A usable link.
    Ready {
        /// The RFCOMM channel's MTU, 668 on the speaker here. Recorded for the curious:
        /// the transport chunks a write for itself, and a frame never approaches it.
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

/// A [`Driver`] for Soundcore speakers.
///
/// State is `{ phase, active, queue }` as SPEC §3.3 prescribes: one transaction in flight,
/// the rest queued in request order. [`Input::Disconnected`] fails everything and resets to
/// fresh, so one value serves any number of connections and each starts from what its own
/// speaker says (invariant 6).
///
/// The speaker speaks over one [`Dir::Stream`](junk_core::Dir::Stream) channel, so this
/// driver is fed whole packets only because a `Framed` sits between it and the transport
/// with `SoundcoreFraming` as its rule. Without one it would be fed whatever chunks the
/// radio delivered, and would report most of them as [`Ev::Unparsed`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SoundcoreDriver {
    phase: Phase,
    active: Option<Active>,
    queue: VecDeque<Pending>,
}

impl SoundcoreDriver {
    /// A driver that has never seen a link.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The MTU of the current link, or `None` without a usable one.
    #[must_use]
    pub fn mtu(&self) -> Option<u16> {
        match self.phase {
            Phase::Down | Phase::Unusable => None,
            Phase::Ready { mtu } => Some(mtu),
        }
    }

    fn on_connected(&mut self, resolved: &ChannelSet, mtu: u16, out: &mut Outputs<Resp, Ev>) {
        // A `Connected` on top of a live link is a new link: nothing survives from the old one.
        self.on_disconnected(out);
        if !GATT.satisfied_by(resolved) {
            out.event(Ev::MissingChannels(GATT.required().difference(resolved)));
            out.disconnect();
            self.phase = Phase::Unusable;
            return;
        }
        // No subscribe: a byte stream pushes without being asked, which is what
        // `Dir::Stream` means. The link would answer `Ok(())` and do nothing.
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
        let Started { txn, bytes } = match Txn::start(req) {
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

    /// One whole packet off the stream: the active transaction's reply, something the
    /// speaker says on its own, or neither.
    fn on_rx(&mut self, chan: Channel, bytes: Bytes, out: &mut Outputs<Resp, Ev>) {
        if chan != RFCOMM {
            out.event(Ev::Unparsed { bytes });
            return;
        }
        let Ok(packet) = Packet::decode(&bytes) else {
            out.event(Ev::Unparsed { bytes });
            return;
        };
        if packet.direction != Direction::DeviceToHost {
            // A host-to-device packet coming back is not a reply to anything.
            out.event(Ev::Unexpected(bytes));
            return;
        }
        // The transaction is asked first, and only ever takes a packet carrying the command
        // it asked for: a notification arriving mid-transaction falls through to the
        // classification below, and a `Req::Raw` for one of the notification commands still
        // gets its answer.
        if self.feed(&packet, &bytes, out) {
            return;
        }
        out.event(unsolicited(&packet, &bytes));
    }

    /// Feeds one decoded packet to the active transaction. Returns `false` when there is
    /// none, or it did not want the packet; nothing changed then.
    fn feed(&mut self, packet: &Packet<'_>, raw: &[u8], out: &mut Outputs<Resp, Ev>) -> bool {
        let Some(active) = self.active.take() else {
            return false;
        };
        match active.txn.feed(packet, raw) {
            Step::Ignored => {
                self.active = Some(active);
                false
            }
            Step::Done(resp) => {
                out.cancel_timer(TIMEOUT_TIMER);
                self.finish(active.id, Ok(resp), out);
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

    /// Answers `id` and starts the next queued request, if any, in the same step.
    fn finish(&mut self, id: ReqId, result: Result<Resp, ProtoError>, out: &mut Outputs<Resp, Ev>) {
        out.done(id, result);
        if let Some(pending) = self.queue.pop_front() {
            self.begin(pending, out);
        }
    }

    /// Makes `pending` the active transaction: writes its packet and arms the timer.
    fn begin(&mut self, pending: Pending, out: &mut Outputs<Resp, Ev>) {
        let Pending { id, txn, bytes } = pending;
        out.tx(RFCOMM, bytes);
        out.set_timer(TIMEOUT_TIMER, TIMEOUT);
        self.active = Some(Active { id, txn });
    }
}

/// What a well-formed packet no transaction wanted is: one of the four things the speaker
/// volunteers, a notification whose payload is not the shape its command takes, or a reply
/// nobody asked for.
fn unsolicited(packet: &Packet<'_>, raw: &[u8]) -> Ev {
    let unparsed = || Ev::Unparsed {
        bytes: raw.to_vec(),
    };
    match packet.command {
        // The same fifths the device-info payload carries, so the same rule reads them.
        Command::NotifyBatteryInfo => one(packet.payload)
            .and_then(battery_from_fifths)
            .map_or_else(unparsed, Ev::Battery),
        Command::NotifyChargingInfo => {
            one(packet.payload).map_or_else(unparsed, |raw| Ev::Charging(raw != 0))
        }
        Command::NotifyVolumeInfo => one(packet.payload).map_or_else(unparsed, Ev::Volume),
        Command::NotifyPlaybackInfo => Ev::Playback(packet.payload.to_vec()),
        Command::GetDeviceInfo
        | Command::GetLdacMode
        | Command::PowerOff
        | Command::SetVoicePrompts
        | Command::SetAutoPowerOff
        | Command::GetButtonBrightness
        | Command::SetButtonBrightness
        | Command::SetLdacMode
        | Command::GetCurrentDirection
        | Command::SetAdaptiveDirection
        | Command::GetEqualizer
        | Command::SetEqualizerPreset
        | Command::SetEqualizerCustom
        | Command::NotifyBassMode
        | Command::Other(_) => Ev::Unexpected(raw.to_vec()),
    }
}

/// The single byte of a one-byte notification payload.
fn one(payload: &[u8]) -> Option<u8> {
    match payload {
        [byte] => Some(*byte),
        _ => None,
    }
}

impl Driver for SoundcoreDriver {
    type Req = Req;
    type Resp = Resp;
    type Ev = Ev;

    const GATT: &'static junk_core::GattMap = &GATT;

    fn handle(&mut self, input: Input<Req>, out: &mut Outputs<Resp, Ev>) {
        match input {
            Input::Connected { resolved, mtu } => self.on_connected(&resolved, mtu, out),
            Input::Disconnected => self.on_disconnected(out),
            Input::Rx { chan, bytes } => self.on_rx(chan, bytes, out),
            Input::Tick { .. } => {}
            Input::Timer(id) => self.on_timer(id, out),
            Input::Request { id, req, .. } => self.on_request(id, req, out),
        }
    }
}
