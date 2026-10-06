//! A [`Link`] that turns a byte stream back into whole frames.
//!
//! A serial-style transport — Bluetooth Classic RFCOMM, say — is one bidirectional byte
//! stream on a single [`Dir::Stream`](junk_core::Dir::Stream) channel, and what comes out
//! of it is chunks of whatever size the radio felt like: half a packet, then a packet and a
//! half, not one packet per [`LinkEvent::Rx`] the way a GATT notification is. [`Framed`]
//! wraps such a link, buffers what it delivers and hands the pump exactly one whole frame
//! per `next`, so the driver above it is fed what it would be fed over BLE.
//!
//! The packet knowledge stays out of the link. The device family supplies it as a
//! [`Framing`]: a pure rule answering "how long is the frame at the front of this buffer",
//! which is the shape `junk_colmi::wire::BigData::expected_len` already has. The link
//! inside goes on moving bytes it never looks at, so invariant 2 holds for the wrapper as
//! for the link inside.
//!
//! Everything but `next` passes straight through. `connect` and `disconnect` also drop the
//! buffers: a new connection is a new stream, and the tail of the old one is not the start
//! of anything on this one.
//!
//! # Not growing without bound
//!
//! A buffer that never completes a frame would otherwise grow for as long as the device
//! talks. [`Framed`] caps each channel's buffer at [`Framed::DEFAULT_LIMIT`], or whatever
//! [`Framed::with_limit`] was given: once a buffer is waiting for more and already past
//! what any frame can be, the stream is not in sync, so the buffer is dropped whole and
//! counted with the resynchronised bytes. The limit must therefore exceed the longest frame
//! the family can produce.
//!
//! # Resynchronising
//!
//! [`FrameLen::Invalid`] says the buffer cannot start a frame, whatever arrives next. The
//! buffer then drops its first byte, counts it in [`Framed::resynced`] and asks again, so a
//! reader that joined a stream mid-packet walks forward to the next thing the family
//! recognises instead of wedging on rubbish.
//!
//! # Cancel safety
//!
//! [`Link::next`] must lose nothing when the pump's `select!` drops its future, and
//! wrapping a stream is where that is easy to get wrong: a buffer living inside the future
//! would go with it and take a half-delivered frame along. This one keeps its buffers in
//! the [`Framed`], which outlives any future, and what the inner link hands over is
//! appended to them **before** anything else can be awaited — the append is the statement
//! right after the `await` that produced the bytes, with no branch and no second await in
//! between, and a future is only ever dropped at an await point. So a dropped `next` leaves
//! every byte in the buffer and the next call carries on from it. A frame is likewise cut
//! out of a buffer only on the way out of a `next` that has already stopped awaiting.
//! `Framed` is exactly as cancel-safe as the link inside it.

use std::collections::BTreeMap;
use std::mem;

use junk_core::{Bytes, Channel, ChannelSet, GattMap, Link, LinkError, LinkEvent};

pub use junk_core::{FrameLen, Framing};

/// A [`Link`] that hands whole frames on. See the [module docs](self).
#[derive(Debug)]
pub struct Framed<L: Link, F: Framing> {
    inner: L,
    framing: F,
    /// What each channel has delivered and no frame has claimed yet.
    buffers: BTreeMap<Channel, Bytes>,
    /// The most one channel may buffer before the stream is judged out of sync.
    limit: usize,
    /// Bytes dropped resynchronising, over the link's life.
    resynced: usize,
}

impl<L: Link, F: Framing> Framed<L, F> {
    /// The most one channel buffers by default: enough for any frame a 16-bit length
    /// field can describe, which is every framing seen here.
    pub const DEFAULT_LIMIT: usize = 64 * 1024;

    /// Reassembles what `inner` delivers into the frames `framing` describes, buffering at
    /// most [`Framed::DEFAULT_LIMIT`] bytes per channel.
    pub fn new(inner: L, framing: F) -> Self {
        Self::with_limit(inner, framing, Self::DEFAULT_LIMIT)
    }

    /// As [`Framed::new`], buffering at most `limit` bytes per channel.
    ///
    /// `limit` must exceed the longest frame `framing` can describe, or a legitimate frame
    /// will be thrown away while it is still arriving.
    pub fn with_limit(inner: L, framing: F, limit: usize) -> Self {
        Framed {
            inner,
            framing,
            buffers: BTreeMap::new(),
            limit,
            resynced: 0,
        }
    }

    /// How many bytes were dropped resynchronising, over the link's life: connections do
    /// not reset it.
    #[must_use]
    pub const fn resynced(&self) -> usize {
        self.resynced
    }

    /// The link inside. Whatever is buffered is dropped with the wrapper.
    #[must_use]
    pub fn into_inner(self) -> L {
        self.inner
    }

    /// The first whole frame the buffers hold, by ascending channel, dropping the bytes
    /// that cannot start one as it looks. `None` when every buffer is waiting for more.
    fn next_frame(&mut self) -> Option<LinkEvent> {
        for (chan, buffer) in &mut self.buffers {
            // An empty buffer is asked nothing: there is no byte to drop if it answered
            // `Invalid`, and `Len` is `NonZeroUsize`, so every other answer makes progress.
            while !buffer.is_empty() {
                match self.framing.frame_len(buffer) {
                    FrameLen::Incomplete => break,
                    FrameLen::Len(len) if len.get() > buffer.len() => break,
                    FrameLen::Len(len) => {
                        let rest = buffer.split_off(len.get());
                        let frame = mem::replace(buffer, rest);
                        return Some(LinkEvent::Rx {
                            chan: *chan,
                            bytes: frame,
                        });
                    }
                    FrameLen::Invalid => {
                        buffer.remove(0);
                        self.resynced += 1;
                    }
                }
            }
            // Waiting for more, and already past what any frame can be: this is not the
            // middle of a frame, it is a stream out of sync. Start the channel over rather
            // than buffer for as long as the device talks.
            if buffer.len() > self.limit {
                self.resynced += buffer.len();
                buffer.clear();
            }
        }
        None
    }
}

impl<L: Link, F: Framing> Link for Framed<L, F> {
    async fn connect(&mut self, gatt: &GattMap) -> Result<(ChannelSet, u16), LinkError> {
        // A new connection is a new stream; what the last one left half-delivered starts
        // nothing on this one. Dropped before the await, so a connect that never returns
        // leaves nothing behind either.
        self.buffers.clear();
        self.inner.connect(gatt).await
    }

    async fn write(&mut self, chan: Channel, bytes: &[u8]) -> Result<(), LinkError> {
        self.inner.write(chan, bytes).await
    }

    async fn subscribe(&mut self, chan: Channel) -> Result<(), LinkError> {
        self.inner.subscribe(chan).await
    }

    async fn read(&mut self, chan: Channel) -> Result<Bytes, LinkError> {
        self.inner.read(chan).await
    }

    async fn disconnect(&mut self) {
        self.buffers.clear();
        self.inner.disconnect().await;
    }

    // As cancel-safe as the link inside: the buffers are the wrapper's, not the future's,
    // and what the link delivers is appended to them before anything else is awaited. See
    // the module docs.
    async fn next(&mut self) -> LinkEvent {
        loop {
            // Whatever is already whole is served without touching the link again.
            if let Some(frame) = self.next_frame() {
                return frame;
            }
            match self.inner.next().await {
                LinkEvent::Rx { chan, bytes } => {
                    self.buffers
                        .entry(chan)
                        .or_default()
                        .extend_from_slice(&bytes);
                }
                LinkEvent::Disconnected => {
                    self.buffers.clear();
                    return LinkEvent::Disconnected;
                }
            }
        }
    }
}
