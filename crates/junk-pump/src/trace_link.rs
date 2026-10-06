//! A [`Link`] that serves a captured [`Trace`] back to a pump.
//!
//! [`RecordingLink`](crate::record::RecordingLink) turns a session into a trace;
//! [`TraceLink`] turns a trace back into a session, so a script that was recorded against
//! a real device can be driven again with no hardware (SPEC §5: everything the CLI does
//! offline is testable, and this is what makes the whole session, not just the driver,
//! offline).
//!
//! The trace is flattened to its data lines, in order, and served against a cursor:
//!
//! - a `write` looks forward from the cursor for the first unserved `tx` line with the
//!   same channel and bytes, moves the cursor past it, and queues every `rx` line up to
//!   the next `tx` line, which [`Link::next`] then delivers as
//!   [`LinkEvent::Rx`];
//! - a `read` takes the next unserved `rx` line on that channel at or after the cursor,
//!   since that is how a recorder writes a read down;
//! - a write the trace does not have is [`LinkError::Io`], never a wait: a script that
//!   departs from the recorded one must fail loudly.
//!
//! A queued line and a read draw from the same lines, so a line is served once whichever
//! asks for it first: the queue holds line numbers and `next` skips the ones a read took
//! meanwhile. That is what lets a recorded `Version` work, whose `dis.fw` and `dis.hw`
//! values sit among the `rx` lines of the write that preceded them and are read, not
//! notified.
//!
//! Channels are the device family's, by the trace's names for them: `new` takes the
//! family's naming function (`junk_colmi::channel_by_name`, say) and drops the lines whose
//! channel it does not know, counting them in [`TraceLink::unknown_channels`].

use std::collections::BTreeMap;

use junk_core::{Bytes, Channel, ChannelSet, GattMap, Link, LinkError, LinkEvent};
use junk_trace::{Direction, Trace};
use tokio::sync::mpsc;

/// The ATT MTU a trace without a `connected` event line is served with.
pub const DEFAULT_MTU: u16 = 247;

/// One data line of the trace, flattened.
#[derive(Debug)]
struct Recorded {
    /// Which way the bytes went.
    dir: Direction,
    /// The channel the naming function gave for the line's name.
    chan: Channel,
    /// The payload.
    bytes: Bytes,
}

/// A [`Link`] that serves a captured trace. See the [module docs](self).
#[derive(Debug)]
pub struct TraceLink {
    /// The trace's data lines whose channel resolved, in order.
    lines: Vec<Recorded>,
    /// The trace's own name for each channel, for the messages.
    names: BTreeMap<Channel, String>,
    /// What `connect` resolves and reports as the MTU.
    connected_as: (ChannelSet, u16),
    /// Data lines whose channel the naming function did not know.
    unknown_channels: usize,
    /// Whether a connection is up.
    connected: bool,
    /// The first line no write has looked past yet.
    cursor: usize,
    /// Which lines have been served, by index into `lines`.
    served: Vec<bool>,
    /// The lines a write queued, by index; `next` delivers the ones a read did not take.
    queue: mpsc::UnboundedReceiver<usize>,
    /// Where the queue is filled from.
    queued: mpsc::UnboundedSender<usize>,
    /// Every write served so far, in order.
    writes: Vec<(Channel, Bytes)>,
}

impl TraceLink {
    /// Serves `trace`, with `channel_by_name` naming its channels.
    ///
    /// Comment and event lines are read only for the `connected` event that says what the
    /// link looked like; every other line is either a data line to serve or, if its
    /// channel has no name the function knows, one more
    /// [`unknown_channels`](TraceLink::unknown_channels).
    #[must_use]
    pub fn new(trace: &Trace, channel_by_name: impl Fn(&str) -> Option<Channel>) -> TraceLink {
        let mut lines = Vec::new();
        let mut names = BTreeMap::new();
        let mut unknown_channels = 0;
        let mut seen = ChannelSet::EMPTY;
        for line in trace.data() {
            let Some(chan) = channel_by_name(&line.chan) else {
                unknown_channels += 1;
                continue;
            };
            names.entry(chan).or_insert_with(|| line.chan.clone());
            seen.insert(chan);
            lines.push(Recorded {
                dir: line.dir,
                chan,
                bytes: line.bytes.clone(),
            });
        }
        let connected_as = trace
            .events()
            .find_map(|event| connected(&event.text, &channel_by_name, &mut names))
            .unwrap_or((seen, DEFAULT_MTU));
        let (queued, queue) = mpsc::unbounded_channel();
        TraceLink {
            served: vec![false; lines.len()],
            lines,
            names,
            connected_as,
            unknown_channels,
            connected: false,
            cursor: 0,
            queue,
            queued,
            writes: Vec::new(),
        }
    }

    /// Every write served so far, in order, across connections.
    #[must_use]
    pub fn writes(&self) -> &[(Channel, Bytes)] {
        &self.writes
    }

    /// How many `tx` lines of this connection's pass over the trace nobody wrote yet.
    #[must_use]
    pub fn unserved(&self) -> usize {
        self.lines
            .iter()
            .zip(&self.served)
            .filter(|(line, served)| line.dir == Direction::Tx && !**served)
            .count()
    }

    /// How many data lines were dropped because the naming function did not know their
    /// channel.
    #[must_use]
    pub const fn unknown_channels(&self) -> usize {
        self.unknown_channels
    }

    /// The trace's name for `chan`, or `unknown.<id>` as a recorder would write it.
    fn name(&self, chan: Channel) -> String {
        self.names
            .get(&chan)
            .cloned()
            .unwrap_or_else(|| format!("unknown.{}", chan.0))
    }

    /// Fails when there is no connection.
    fn usable(&self) -> Result<(), LinkError> {
        if self.connected {
            Ok(())
        } else {
            Err(LinkError::NotConnected)
        }
    }
}

/// What a `connected mtu=<n> channels=<names>` event line says the link looked like, or
/// `None` if it is not one. Names the function does not know are ignored; every name is
/// remembered in `names` so the messages can use the trace's own.
fn connected(
    text: &str,
    channel_by_name: &impl Fn(&str) -> Option<Channel>,
    names: &mut BTreeMap<Channel, String>,
) -> Option<(ChannelSet, u16)> {
    let fields = text.strip_prefix("connected ")?;
    let mut resolved = ChannelSet::EMPTY;
    let mut mtu = DEFAULT_MTU;
    for field in fields.split_whitespace() {
        if let Some(value) = field.strip_prefix("mtu=") {
            mtu = value.parse().unwrap_or(DEFAULT_MTU);
        } else if let Some(value) = field.strip_prefix("channels=") {
            for name in value.split(',').filter(|name| !name.is_empty()) {
                if let Some(chan) = channel_by_name(name) {
                    resolved.insert(chan);
                    names.entry(chan).or_insert_with(|| name.to_owned());
                }
            }
        }
    }
    Some((resolved, mtu))
}

// Keep side effects deferred until the future is polled, just like the real links.
// `unknown_lints` supports toolchains predating this Clippy lint (including our MSRV).
#[allow(unknown_lints, clippy::unused_async_trait_impl)]
impl Link for TraceLink {
    async fn connect(&mut self, gatt: &GattMap) -> Result<(ChannelSet, u16), LinkError> {
        let (resolved, mtu) = self.connected_as;
        if !gatt.satisfied_by(&resolved) {
            return Err(LinkError::MissingRequired(
                gatt.required().difference(&resolved),
            ));
        }
        // A second connection starts over from the top: the same trace, served again, so
        // a pump can run twice.
        self.connected = true;
        self.cursor = 0;
        self.served.clear();
        self.served.resize(self.lines.len(), false);
        let (queued, queue) = mpsc::unbounded_channel();
        self.queued = queued;
        self.queue = queue;
        Ok((resolved, mtu))
    }

    async fn write(&mut self, chan: Channel, bytes: &[u8]) -> Result<(), LinkError> {
        self.usable()?;
        let found = (self.cursor..self.lines.len()).find(|&index| {
            let line = &self.lines[index];
            !self.served[index]
                && line.dir == Direction::Tx
                && line.chan == chan
                && line.bytes == bytes
        });
        let Some(index) = found else {
            return Err(LinkError::Io(format!(
                "the trace has no such write: {}",
                hex(bytes)
            )));
        };
        self.served[index] = true;
        self.cursor = index + 1;
        self.writes.push((chan, bytes.to_vec()));
        // Everything the device said before its next turn to be written to is the answer
        // to this write; a read may still take one of these first, which `next` allows for.
        for (offset, line) in self.lines[self.cursor..].iter().enumerate() {
            if line.dir == Direction::Tx {
                break;
            }
            let _ = self.queued.send(self.cursor + offset);
        }
        Ok(())
    }

    async fn subscribe(&mut self, _chan: Channel) -> Result<(), LinkError> {
        Ok(())
    }

    async fn read(&mut self, chan: Channel) -> Result<Bytes, LinkError> {
        self.usable()?;
        let found = (self.cursor..self.lines.len()).find(|&index| {
            let line = &self.lines[index];
            !self.served[index] && line.dir == Direction::Rx && line.chan == chan
        });
        let Some(index) = found else {
            return Err(LinkError::Io(format!(
                "the trace has no read left on {}",
                self.name(chan)
            )));
        };
        self.served[index] = true;
        Ok(self.lines[index].bytes.clone())
    }

    async fn disconnect(&mut self) {
        self.connected = false;
    }

    // Cancel-safe: `recv` on an mpsc receiver loses nothing when dropped early, and an
    // index whose line a read already took carries nothing to lose.
    async fn next(&mut self) -> LinkEvent {
        loop {
            let Some(index) = self.queue.recv().await else {
                return LinkEvent::Disconnected;
            };
            if self.served[index] {
                continue;
            }
            self.served[index] = true;
            let line = &self.lines[index];
            return LinkEvent::Rx {
                chan: line.chan,
                bytes: line.bytes.clone(),
            };
        }
    }
}

/// `bytes` as lowercase hex, no separators, as the traces write them.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}
