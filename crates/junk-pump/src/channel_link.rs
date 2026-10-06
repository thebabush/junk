//! A [`Link`] over tokio channels, for tests and simulators.
//!
//! [`ChannelLink::pair`] gives the pump's end ([`ChannelLink`]) and the device's end
//! ([`PeerSide`]). What the pump writes comes out of [`PeerSide::next_write`]; what the
//! device notifies with [`PeerSide::notify`] comes out of [`Link::next`]; what the pump
//! reads is whatever the device end last set with [`PeerSide::set_value`], and the reads
//! are listed by [`PeerSide::reads`]. The device end decides what `connect` finds (which
//! channels, which MTU, or an error) and can drop the connection at any time. Nothing here
//! looks at a payload (invariant 2).
//!
//! `connect` never checks the map's `required` flags: it resolves whatever the device was
//! told to have and leaves a missing channel for the driver to notice, so that a driver's
//! handling of a half-there device can be exercised. A test that wants `connect` itself to
//! fail says so with [`PeerSide::set_connect_error`].

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use junk_core::{Bytes, Channel, ChannelSet, GattMap, Link, LinkError, LinkEvent};
use tokio::sync::mpsc;

/// The ATT MTU a fresh [`PeerSide`] reports.
pub const DEFAULT_MTU: u16 = 247;

/// What both ends see.
#[derive(Debug)]
struct Shared {
    /// What the device has, or `None` for every channel the map declares.
    resolved: Option<ChannelSet>,
    /// What `connect` reports as the MTU.
    mtu: u16,
    /// What `connect` fails with, if anything.
    connect_error: Option<LinkError>,
    /// Whether a connection is up.
    connected: bool,
    /// The channels of the current connection: declared and present.
    usable: ChannelSet,
    /// Every channel subscribed to so far, in order, across connections.
    subscriptions: Vec<Channel>,
    /// What a read of each channel returns, for the channels that have a value.
    values: BTreeMap<Channel, Bytes>,
    /// Every channel read so far, in order, across connections.
    reads: Vec<Channel>,
}

impl Shared {
    fn new() -> Self {
        Shared {
            resolved: None,
            mtu: DEFAULT_MTU,
            connect_error: None,
            connected: false,
            usable: ChannelSet::EMPTY,
            subscriptions: Vec::new(),
            values: BTreeMap::new(),
            reads: Vec::new(),
        }
    }
}

/// Locks `shared`; a poisoned lock is still just the state.
fn lock(shared: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The pump's end of a channel pair. See the [module docs](self).
#[derive(Debug)]
pub struct ChannelLink {
    shared: Arc<Mutex<Shared>>,
    writes: mpsc::UnboundedSender<(Channel, Bytes)>,
    events: mpsc::UnboundedReceiver<LinkEvent>,
}

/// The device's end of a channel pair. See the [module docs](self).
///
/// Dropping it is the device going away for good: the link's next `next` yields
/// [`LinkEvent::Disconnected`], and a later `connect` fails.
#[derive(Debug)]
pub struct PeerSide {
    shared: Arc<Mutex<Shared>>,
    writes: mpsc::UnboundedReceiver<(Channel, Bytes)>,
    events: mpsc::UnboundedSender<LinkEvent>,
}

impl ChannelLink {
    /// A link and its device end, not connected.
    #[must_use]
    pub fn pair() -> (ChannelLink, PeerSide) {
        let shared = Arc::new(Mutex::new(Shared::new()));
        let (write_tx, write_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let link = ChannelLink {
            shared: Arc::clone(&shared),
            writes: write_tx,
            events: event_rx,
        };
        let peer = PeerSide {
            shared,
            writes: write_rx,
            events: event_tx,
        };
        (link, peer)
    }

    /// Checks that `chan` can be used right now.
    fn usable(&self, chan: Channel) -> Result<(), LinkError> {
        let shared = lock(&self.shared);
        if !shared.connected {
            return Err(LinkError::NotConnected);
        }
        if !shared.usable.contains(chan) {
            return Err(LinkError::UnknownChannel(chan));
        }
        Ok(())
    }
}

// Keep side effects deferred until the future is polled, just like the real links.
// `unknown_lints` supports toolchains predating this Clippy lint (including our MSRV).
#[allow(unknown_lints, clippy::unused_async_trait_impl)]
impl Link for ChannelLink {
    async fn connect(&mut self, gatt: &GattMap) -> Result<(ChannelSet, u16), LinkError> {
        if self.events.is_closed() {
            return Err(LinkError::Io("the peer is gone".into()));
        }
        let mut shared = lock(&self.shared);
        if let Some(err) = &shared.connect_error {
            return Err(err.clone());
        }
        let declared: ChannelSet = gatt.chars().map(|c| c.id).collect();
        let resolved = match shared.resolved {
            Some(present) => declared.iter().filter(|c| present.contains(*c)).collect(),
            None => declared,
        };
        shared.connected = true;
        shared.usable = resolved;
        drop(shared);
        // Whatever the device said on an earlier connection is not for this one.
        while self.events.try_recv().is_ok() {}
        Ok((resolved, lock(&self.shared).mtu))
    }

    async fn write(&mut self, chan: Channel, bytes: &[u8]) -> Result<(), LinkError> {
        self.usable(chan)?;
        self.writes
            .send((chan, bytes.to_vec()))
            .map_err(|_| LinkError::Io("the peer is gone".into()))
    }

    async fn subscribe(&mut self, chan: Channel) -> Result<(), LinkError> {
        self.usable(chan)?;
        lock(&self.shared).subscriptions.push(chan);
        Ok(())
    }

    async fn read(&mut self, chan: Channel) -> Result<Bytes, LinkError> {
        self.usable(chan)?;
        let mut shared = lock(&self.shared);
        shared.reads.push(chan);
        shared
            .values
            .get(&chan)
            .cloned()
            .ok_or_else(|| LinkError::Io("no value".into()))
    }

    async fn disconnect(&mut self) {
        lock(&self.shared).connected = false;
    }

    // Cancel-safe: `recv` on an mpsc receiver loses nothing when dropped early.
    async fn next(&mut self) -> LinkEvent {
        let Some(event) = self.events.recv().await else {
            lock(&self.shared).connected = false;
            return LinkEvent::Disconnected;
        };
        event
    }
}

impl PeerSide {
    /// Which channels the device has. `connect` resolves the ones the map also declares.
    ///
    /// The default is every channel the map declares.
    pub fn set_resolved(&mut self, resolved: ChannelSet) {
        lock(&self.shared).resolved = Some(resolved);
    }

    /// What `connect` reports as the MTU. The default is [`DEFAULT_MTU`].
    pub fn set_mtu(&mut self, mtu: u16) {
        lock(&self.shared).mtu = mtu;
    }

    /// What `connect` fails with; `None` (the default) lets it succeed.
    pub fn set_connect_error(&mut self, err: Option<LinkError>) {
        lock(&self.shared).connect_error = err;
    }

    /// What a read of `chan` returns from now on, replacing any earlier value. A channel
    /// with no value fails its reads with [`LinkError::Io`].
    pub fn set_value(&mut self, chan: Channel, bytes: Vec<u8>) {
        lock(&self.shared).values.insert(chan, bytes);
    }

    /// The next thing the pump wrote, or `None` once the link is dropped and nothing is
    /// left.
    pub async fn next_write(&mut self) -> Option<(Channel, Bytes)> {
        self.writes.recv().await
    }

    /// Notifies `bytes` on `chan`; the link's `next` yields it as [`LinkEvent::Rx`].
    ///
    /// Returns `false`, and sends nothing, if the link is gone or not connected.
    #[must_use]
    pub fn notify(&self, chan: Channel, bytes: Bytes) -> bool {
        if !lock(&self.shared).connected {
            return false;
        }
        self.events.send(LinkEvent::Rx { chan, bytes }).is_ok()
    }

    /// Drops the connection from the device's side: the link's `next` yields
    /// [`LinkEvent::Disconnected`] after whatever was notified before. A no-op when there is
    /// no connection.
    pub fn disconnect(&self) {
        let mut shared = lock(&self.shared);
        if !shared.connected {
            return;
        }
        shared.connected = false;
        drop(shared);
        let _ = self.events.send(LinkEvent::Disconnected);
    }

    /// Every channel the pump subscribed to so far, in order, across connections.
    #[must_use]
    pub fn subscriptions(&self) -> Vec<Channel> {
        lock(&self.shared).subscriptions.clone()
    }

    /// Every channel the pump read so far, in order, across connections: every read that
    /// reached the device, whether or not it had a value.
    #[must_use]
    pub fn reads(&self) -> Vec<Channel> {
        lock(&self.shared).reads.clone()
    }

    /// Whether a connection is up.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        lock(&self.shared).connected
    }
}
