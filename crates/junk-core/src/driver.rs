//! The protocol boundary: what the world tells a driver and what a driver asks of it.

use alloc::vec::Vec;

use crate::{Channel, ChannelSet, Duration, GattMap, Instant, ProtoError};

/// Raw bytes.
///
/// This alias is the one place raw bytes cross a layer boundary by design: the payload of
/// [`Input::Rx`], [`Output::Tx`], [`LinkEvent::Rx`](crate::LinkEvent::Rx) and the value a
/// [`Link::read`](crate::Link::read) returns. Everything else that crosses a boundary is
/// typed.
pub type Bytes = Vec<u8>;

/// Identifies one user request so its [`Output::Done`] can be matched to it.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, PartialOrd, Ord)]
pub struct ReqId(pub u32);

/// Identifies a timer the driver armed with [`Output::SetTimer`].
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, PartialOrd, Ord)]
pub struct TimerId(pub u8);

/// What the world tells the driver. Nothing else exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input<Req> {
    /// The link is up and the declared channels have been resolved.
    Connected {
        /// Which declared channels the device actually has.
        resolved: ChannelSet,
        /// The negotiated ATT MTU, in bytes.
        mtu: u16,
    },
    /// The link is gone. Every in-flight transaction is void (invariant 6).
    Disconnected,
    /// One notification from the device, unmodified; or the value of a channel the driver
    /// asked to read with [`Output::Read`].
    Rx {
        /// The channel it arrived on, or was read from.
        chan: Channel,
        /// Its payload.
        bytes: Bytes,
    },
    /// The clock. This is the only way it ever reaches the driver.
    Tick {
        /// Current time.
        now: Instant,
    },
    /// A timer the driver asked for has fired.
    Timer(TimerId),
    /// User intent.
    Request {
        /// Handle to answer with in [`Output::Done`].
        id: ReqId,
        /// The request itself.
        req: Req,
        /// Current time, so the driver can arm timeouts without reading a clock.
        now: Instant,
    },
}

/// What the driver asks the world to do. Nothing else exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output<Resp, Ev> {
    /// Write these bytes to this channel.
    Tx {
        /// The channel to write to.
        chan: Channel,
        /// The payload.
        bytes: Bytes,
    },
    /// Enable notifications or indications on this channel.
    Subscribe(Channel),
    /// Read this channel's value; it comes back as [`Input::Rx`] with the same channel,
    /// as the next input after the rest of this step's outputs have been applied. A read
    /// that fails feeds nothing: the driver's own timer fails whatever request it belonged
    /// to.
    Read(Channel),
    /// Deliver [`Input::Timer`] with this id after this long. Re-arming an id replaces it.
    SetTimer {
        /// The id to fire with.
        id: TimerId,
        /// How long to wait.
        after: Duration,
    },
    /// Forget a timer that has not fired yet.
    CancelTimer(TimerId),
    /// Unsolicited data: battery, live heart rate, "new data" pings, and so on.
    Event(Ev),
    /// The one and only answer to a [`Input::Request`] (invariant 4).
    Done {
        /// The request being answered.
        id: ReqId,
        /// The outcome.
        result: Result<Resp, ProtoError>,
    },
    /// Drop the link.
    Disconnect,
}

/// An ordered sink of [`Output`]s the driver pushes into while handling one [`Input`].
///
/// The pump drains it and applies each item in order before feeding the next input
/// (invariant 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outputs<Resp, Ev> {
    items: Vec<Output<Resp, Ev>>,
}

impl<Resp, Ev> Outputs<Resp, Ev> {
    /// An empty sink.
    #[must_use]
    pub const fn new() -> Self {
        Outputs { items: Vec::new() }
    }

    /// Queues `out` after everything already queued.
    pub fn push(&mut self, out: Output<Resp, Ev>) {
        self.items.push(out);
    }

    /// Queues [`Output::Tx`].
    pub fn tx(&mut self, chan: Channel, bytes: Bytes) {
        self.push(Output::Tx { chan, bytes });
    }

    /// Queues [`Output::Subscribe`].
    pub fn subscribe(&mut self, chan: Channel) {
        self.push(Output::Subscribe(chan));
    }

    /// Queues [`Output::Read`].
    pub fn read(&mut self, chan: Channel) {
        self.push(Output::Read(chan));
    }

    /// Queues [`Output::SetTimer`].
    pub fn set_timer(&mut self, id: TimerId, after: Duration) {
        self.push(Output::SetTimer { id, after });
    }

    /// Queues [`Output::CancelTimer`].
    pub fn cancel_timer(&mut self, id: TimerId) {
        self.push(Output::CancelTimer(id));
    }

    /// Queues [`Output::Event`].
    pub fn event(&mut self, ev: Ev) {
        self.push(Output::Event(ev));
    }

    /// Queues [`Output::Done`].
    pub fn done(&mut self, id: ReqId, result: Result<Resp, ProtoError>) {
        self.push(Output::Done { id, result });
    }

    /// Queues [`Output::Disconnect`].
    pub fn disconnect(&mut self) {
        self.push(Output::Disconnect);
    }

    /// Number of queued outputs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether nothing is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Removes every queued output, oldest first.
    ///
    /// The sink is empty once the iterator is dropped, even if it was not run to the end.
    pub fn drain(&mut self) -> impl Iterator<Item = Output<Resp, Ev>> + '_ {
        self.items.drain(..)
    }

    /// The queued outputs, oldest first.
    #[must_use]
    pub fn into_vec(self) -> Vec<Output<Resp, Ev>> {
        self.items
    }
}

impl<Resp, Ev> Default for Outputs<Resp, Ev> {
    fn default() -> Self {
        Self::new()
    }
}

/// A device family's protocol state machine.
///
/// `handle` is the whole interface: one [`Input`] in, zero or more [`Output`]s out, nothing
/// else observed or touched (invariant 1). `GATT` declares the characteristics the family
/// needs; the pump hands it to [`Link::connect`](crate::Link::connect).
pub trait Driver {
    /// User requests this family understands.
    type Req;
    /// Answers to those requests.
    type Resp;
    /// Unsolicited things the family reports.
    type Ev;

    /// The characteristics this family uses.
    const GATT: &'static GattMap;

    /// Feeds one input; the driver queues whatever it wants done in `out`.
    fn handle(&mut self, input: Input<Self::Req>, out: &mut Outputs<Self::Resp, Self::Ev>);
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn drain_preserves_order_and_empties_the_sink() {
        let mut out: Outputs<u8, &str> = Outputs::new();
        assert!(out.is_empty());

        out.subscribe(Channel(1));
        out.tx(Channel(0), vec![0xaa, 0xbb]);
        out.read(Channel(4));
        out.set_timer(TimerId(3), Duration::from_millis(500));
        out.event("hello");
        out.cancel_timer(TimerId(3));
        out.done(ReqId(7), Ok(42));
        out.done(ReqId(8), Err(ProtoError::Timeout));
        out.push(Output::Disconnect);
        out.disconnect();
        assert_eq!(out.len(), 10);
        assert!(!out.is_empty());

        let drained: Vec<_> = out.drain().collect();
        assert_eq!(
            drained,
            vec![
                Output::Subscribe(Channel(1)),
                Output::Tx {
                    chan: Channel(0),
                    bytes: vec![0xaa, 0xbb],
                },
                Output::Read(Channel(4)),
                Output::SetTimer {
                    id: TimerId(3),
                    after: Duration::from_millis(500),
                },
                Output::Event("hello"),
                Output::CancelTimer(TimerId(3)),
                Output::Done {
                    id: ReqId(7),
                    result: Ok(42),
                },
                Output::Done {
                    id: ReqId(8),
                    result: Err(ProtoError::Timeout),
                },
                Output::Disconnect,
                Output::Disconnect,
            ]
        );
        assert!(out.is_empty());
        assert_eq!(out.len(), 0);
        assert_eq!(out.drain().count(), 0);
    }

    #[test]
    fn partial_drain_still_empties_the_sink() {
        let mut out: Outputs<(), ()> = Outputs::default();
        out.subscribe(Channel(1));
        out.subscribe(Channel(2));
        assert_eq!(out.drain().next(), Some(Output::Subscribe(Channel(1))));
        assert!(out.is_empty());
    }

    #[test]
    fn into_vec_keeps_order() {
        let mut out: Outputs<(), ()> = Outputs::new();
        out.subscribe(Channel(2));
        out.subscribe(Channel(1));
        assert_eq!(
            out.into_vec(),
            vec![Output::Subscribe(Channel(2)), Output::Subscribe(Channel(1))]
        );
    }
}
