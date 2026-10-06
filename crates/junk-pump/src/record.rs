//! A [`Link`] that writes down everything that crosses it, as trace lines.
//!
//! [`RecordingLink`] wraps any `Link` and records, in order, the outcome of every
//! `connect`, every write that went through, every subscription, every read that went
//! through, every notification `next` delivered and every disconnect, as [`Line`]s that
//! [`Trace::render`](junk_trace::Trace::render)
//! turns into the v1 text under `fixtures/`. Payloads are copied, never read (invariant 2
//! holds for the wrapper as for the link inside). A pump over one of these is how a live
//! session becomes a fixture (SPEC §5, Stage B4).
//!
//! Stamps come from a closure, so a recorder runs on the wall clock in a CLI and on a fixed
//! one in a test. Channel names come from the device family's naming function
//! (`junk_colmi::channel_name`, say), so a recording uses the names the fixtures use; a
//! channel the family does not name is recorded as `unknown.<id>`.
//!
//! What each operation records:
//!
//! - `connect`: `! connected mtu=<mtu> channels=<names, comma-separated, by id>` on
//!   success, `! connect failed: <error>` otherwise;
//! - `write`, when it succeeded: a `tx` data line;
//! - `subscribe`, when it succeeded: `! subscribe <name>`;
//! - `read`, when it succeeded: an `rx` data line with the value, as a notification on
//!   that channel would be (the fixtures file reads that way);
//! - `next`: an `rx` data line for a notification, `! disconnected` for a disconnect;
//! - `disconnect`: `! disconnect requested`.
//!
//! A data line needs at least one byte, so an empty payload, either way, is recorded as
//! the event `! empty <tx|rx> <name>` instead.

use std::mem;

use junk_core::{Bytes, Channel, ChannelSet, GattMap, Link, LinkError, LinkEvent};
use junk_trace::{DataLine, Direction, EventLine, Line, Stamp};

/// A [`Link`] that records what crosses it. See the [module docs](self).
pub struct RecordingLink<L: Link, S: FnMut() -> Stamp, N: Fn(Channel) -> Option<&'static str>> {
    inner: L,
    stamp: S,
    channel_name: N,
    lines: Vec<Line>,
}

impl<L, S, N> RecordingLink<L, S, N>
where
    L: Link,
    S: FnMut() -> Stamp,
    N: Fn(Channel) -> Option<&'static str>,
{
    /// Records what crosses `inner`, stamping each line with `stamp()` as it is written and
    /// naming channels with `channel_name`.
    pub fn new(inner: L, stamp: S, channel_name: N) -> Self {
        RecordingLink {
            inner,
            stamp,
            channel_name,
            lines: Vec::new(),
        }
    }

    /// Everything recorded so far, in order.
    #[must_use]
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    /// Everything recorded so far, in order; the recorder starts over empty.
    pub fn take_lines(&mut self) -> Vec<Line> {
        mem::take(&mut self.lines)
    }

    /// The link inside and everything recorded.
    #[must_use]
    pub fn into_inner(self) -> (L, Vec<Line>) {
        (self.inner, self.lines)
    }

    /// The name `chan` is recorded under.
    fn name(&self, chan: Channel) -> String {
        (self.channel_name)(chan).map_or_else(|| format!("unknown.{}", chan.0), str::to_owned)
    }

    /// Records an event line, stamped now.
    fn event(&mut self, text: String) {
        let at = (self.stamp)();
        self.lines.push(Line::Event(EventLine { at, text }));
    }

    /// Records a data line, stamped now; an empty payload becomes an event, since a data
    /// line cannot carry one.
    fn data(&mut self, dir: Direction, chan: Channel, bytes: &[u8]) {
        let name = self.name(chan);
        if bytes.is_empty() {
            self.event(format!("empty {dir} {name}"));
            return;
        }
        let at = (self.stamp)();
        self.lines.push(Line::Data(DataLine {
            at,
            dir,
            chan: name,
            bytes: bytes.to_vec(),
        }));
    }

    /// The resolved channels' names, comma-separated, in id order.
    fn names(&self, resolved: &ChannelSet) -> String {
        resolved
            .iter()
            .map(|chan| self.name(chan))
            .collect::<Vec<_>>()
            .join(",")
    }
}

impl<L, S, N> Link for RecordingLink<L, S, N>
where
    L: Link,
    S: FnMut() -> Stamp,
    N: Fn(Channel) -> Option<&'static str>,
{
    async fn connect(&mut self, gatt: &GattMap) -> Result<(ChannelSet, u16), LinkError> {
        let result = self.inner.connect(gatt).await;
        match &result {
            Ok((resolved, mtu)) => {
                let names = self.names(resolved);
                self.event(format!("connected mtu={mtu} channels={names}"));
            }
            Err(err) => self.event(format!("connect failed: {err}")),
        }
        result
    }

    async fn write(&mut self, chan: Channel, bytes: &[u8]) -> Result<(), LinkError> {
        let result = self.inner.write(chan, bytes).await;
        if result.is_ok() {
            self.data(Direction::Tx, chan, bytes);
        }
        result
    }

    async fn subscribe(&mut self, chan: Channel) -> Result<(), LinkError> {
        let result = self.inner.subscribe(chan).await;
        if result.is_ok() {
            let name = self.name(chan);
            self.event(format!("subscribe {name}"));
        }
        result
    }

    async fn read(&mut self, chan: Channel) -> Result<Bytes, LinkError> {
        let result = self.inner.read(chan).await;
        if let Ok(bytes) = &result {
            self.data(Direction::Rx, chan, bytes);
        }
        result
    }

    async fn disconnect(&mut self) {
        self.event("disconnect requested".to_owned());
        self.inner.disconnect().await;
    }

    // As cancel-safe as the link inside: nothing is awaited between its answer and the
    // line being written.
    async fn next(&mut self) -> LinkEvent {
        let event = self.inner.next().await;
        match &event {
            LinkEvent::Rx { chan, bytes } => self.data(Direction::Rx, *chan, bytes),
            LinkEvent::Disconnected => self.event("disconnected".to_owned()),
        }
        event
    }
}
