//! [`RfcommLink`]: a [`Link`] over one Bluetooth Classic RFCOMM channel.

use std::collections::VecDeque;
use std::sync::mpsc;
use std::thread::JoinHandle;

use junk_core::{Bytes, Channel, ChannelSet, GattMap, Link, LinkError, LinkEvent};
use tokio::sync::mpsc as tokio_mpsc;
use tokio::sync::oneshot;

use crate::runloop::{Command, Report};
use crate::{RfcommError, address, resolve, runloop};

/// One connection: the thread that holds it and the ends of the two channels to it.
struct Session {
    /// The one channel the map declared as [`Dir::Stream`](junk_core::Dir::Stream).
    chan: Channel,
    /// What the thread does next.
    commands: mpsc::Sender<Command>,
    /// What the thread has to say.
    reports: tokio_mpsc::UnboundedReceiver<Report>,
    /// Data that arrived before the channel finished opening. Cannot happen — `IOBluetooth`
    /// completes an open before it delivers on it — but bytes are not worth losing to that
    /// assumption, so `connect` parks any here and [`Link::next`] hands them over first.
    early: VecDeque<Bytes>,
    /// The thread itself, joined by [`Link::disconnect`].
    thread: JoinHandle<()>,
}

/// A [`Link`] over one Bluetooth Classic RFCOMM channel. See the [crate docs](crate).
///
/// Built by [`RfcommLink::open`] from a paired device's address; connected, used and
/// dropped by the pump like any other link. One link may be connected more than once: the
/// pump runs again with the same link after a disconnect, and each connection is a fresh
/// thread, device lookup, SDP query and channel.
pub struct RfcommLink {
    address: String,
    name: Option<String>,
    session: Option<Session>,
}

impl RfcommLink {
    /// A link to the device with this address, e.g. `"F4:2B:7D:A1:B2:C3"`. Not yet
    /// connected.
    ///
    /// This only parses the address. Everything that needs the radio waits for
    /// [`Link::connect`], which is also where [`RfcommError::NotPaired`] and
    /// [`RfcommError::NoService`] can come from: nothing here touches `IOBluetooth`, so
    /// nothing here can bind the framework to a thread that has no run loop.
    ///
    /// # Errors
    ///
    /// [`RfcommError::BadAddress`] if `address` is not six separated hex octets.
    pub fn open(address: &str) -> Result<RfcommLink, RfcommError> {
        Ok(RfcommLink {
            address: address::normalise(address)?,
            name: None,
            session: None,
        })
    }

    /// The device's address, normalised to `XX:XX:XX:XX:XX:XX`.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// The device's name as the Mac knows it, learned on the first connection.
    ///
    /// `None` before then, and `None` afterwards for a device the Mac has never got a name
    /// out of.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The session, if there is one and `chan` is its stream.
    fn stream(&mut self, chan: Channel) -> Result<&mut Session, LinkError> {
        let session = self.session.as_mut().ok_or(LinkError::NotConnected)?;
        if session.chan == chan {
            Ok(session)
        } else {
            Err(LinkError::UnknownChannel(chan))
        }
    }
}

impl Link for RfcommLink {
    async fn connect(&mut self, gatt: &GattMap) -> Result<(ChannelSet, u16), LinkError> {
        // Whatever an earlier session left behind is over.
        self.disconnect().await;
        let endpoint = resolve(gatt)?;
        let (commands, orders) = mpsc::channel();
        let (sender, mut reports) = tokio_mpsc::unbounded_channel();
        let address = self.address.clone();
        let service = endpoint.service;
        let thread = std::thread::Builder::new()
            .name(format!("junk-rfcomm {address}"))
            .spawn(move || runloop::run(&address, service, &orders, &sender))
            .map_err(|err| LinkError::Io(format!("no thread for the RFCOMM channel: {err}")))?;

        let mut early = VecDeque::new();
        let (mtu, name) = loop {
            match reports.recv().await {
                Some(Report::Opened { mtu, name }) => break (mtu, name),
                Some(Report::Data(bytes)) => early.push_back(bytes),
                Some(Report::Failed(why)) => return Err(why.into()),
                // The thread only stops before opening if it failed, and it says so when
                // it does; a silent end is a bug rather than a device being unreachable.
                Some(Report::Closed) | None => {
                    return Err(LinkError::Io(
                        "the RFCOMM thread stopped without opening the channel".into(),
                    ));
                }
            }
        };
        if name.is_some() {
            self.name = name;
        }
        self.session = Some(Session {
            chan: endpoint.chan,
            commands,
            reports,
            early,
            thread,
        });
        Ok(([endpoint.chan].into_iter().collect(), mtu))
    }

    async fn write(&mut self, chan: Channel, bytes: &[u8]) -> Result<(), LinkError> {
        let session = self.stream(chan)?;
        let gone = || LinkError::Io("the RFCOMM thread is gone".into());
        let (done, sent) = oneshot::channel();
        session
            .commands
            .send(Command::Write {
                bytes: bytes.to_vec(),
                done,
            })
            .map_err(|_| gone())?;
        match sent.await {
            Ok(result) => result.map_err(LinkError::Io),
            Err(_) => Err(gone()),
        }
    }

    /// Nothing to do: the device pushes on the stream without being asked
    /// ([`Dir::Stream`](junk_core::Dir::Stream)).
    async fn subscribe(&mut self, chan: Channel) -> Result<(), LinkError> {
        self.stream(chan).map(|_| ())
    }

    /// Always [`LinkError::UnknownChannel`]: a byte stream has no attribute to read, so
    /// there is no channel here a read can name.
    async fn read(&mut self, chan: Channel) -> Result<Bytes, LinkError> {
        Err(LinkError::UnknownChannel(chan))
    }

    async fn disconnect(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        let Session {
            commands, thread, ..
        } = session;
        let _ = commands.send(Command::Close);
        // Dropping the sender is the same order, for a thread already past its `try_recv`.
        drop(commands);
        // The thread closes the channel and clears the delegate before it ends, and it is
        // never more than a run-loop turn from noticing; joining it off the reactor keeps
        // that off the async side and makes a second `connect` start from a clean slate.
        let _ = tokio::task::spawn_blocking(move || thread.join()).await;
    }

    // Cancel-safe, for the same reason `ChannelLink`'s is: `recv` on an mpsc receiver
    // loses nothing when its future is dropped early, and the run-loop thread goes on
    // filling the channel meanwhile, so nothing the device said can be missed. The queue
    // of early bytes is drained before the first `await`, and a report this consumes and
    // discards carries nothing.
    async fn next(&mut self) -> LinkEvent {
        let Some(session) = &mut self.session else {
            return LinkEvent::Disconnected;
        };
        let chan = session.chan;
        if let Some(bytes) = session.early.pop_front() {
            return LinkEvent::Rx { chan, bytes };
        }
        loop {
            match session.reports.recv().await {
                Some(Report::Data(bytes)) => return LinkEvent::Rx { chan, bytes },
                // Neither can arrive after `connect` took one of them.
                Some(Report::Opened { .. } | Report::Failed(_)) => {}
                Some(Report::Closed) | None => break,
            }
        }
        // The thread said its last word and is ending on its own; there is nothing to
        // join here, where the pump is waiting on this future in a `select!`.
        self.session = None;
        LinkEvent::Disconnected
    }
}
