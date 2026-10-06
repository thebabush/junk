//! `Framed` over a `ChannelLink`: what a driver above it sees when the transport delivers
//! a byte stream in whatever chunks it likes.
//!
//! The framing under test is a toy the device families do not have: `a5 <len> <payload>`,
//! so `frame_len` is `Incomplete` under two bytes, `Invalid` when the first byte is not
//! `0xa5`, and `Len(2 + len)` otherwise.

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};

use junk_core::{Bytes, Channel, ChannelSet, GattMap, Link, LinkError, LinkEvent};
use junk_fake::{EVT, EXTRA, GATT};
use junk_pump::channel_link::{ChannelLink, PeerSide};
use junk_pump::framed::{FrameLen, Framed, Framing};

/// What a frame starts with.
const MAGIC: u8 = 0xa5;

/// The toy framing. See the [module docs](self).
struct Toy;

impl Framing for Toy {
    fn frame_len(&self, buffered: &[u8]) -> FrameLen {
        match buffered {
            [] | [_] => FrameLen::Incomplete,
            [MAGIC, len, ..] => FrameLen::Len(
                (2 + usize::from(*len))
                    .try_into()
                    .expect("a header and a payload is never nothing"),
            ),
            _ => FrameLen::Invalid,
        }
    }
}

/// A framing that never has enough, whatever it is given.
struct Never;

impl Framing for Never {
    fn frame_len(&self, _buffered: &[u8]) -> FrameLen {
        FrameLen::Incomplete
    }
}

/// One toy frame carrying `payload`.
fn frame(payload: &[u8]) -> Bytes {
    let len = u8::try_from(payload.len()).expect("the toy payloads are small");
    let mut bytes = vec![MAGIC, len];
    bytes.extend_from_slice(payload);
    bytes
}

/// A [`Link`] that counts the `next` calls that reached the link inside, so a test can tell
/// a frame served out of the buffer from one that waited on the transport.
struct Counting<L: Link> {
    inner: L,
    nexts: Arc<AtomicUsize>,
}

impl<L: Link> Counting<L> {
    /// Counts what reaches `inner`, and the counter to read it by.
    fn new(inner: L) -> (Counting<L>, Arc<AtomicUsize>) {
        let nexts = Arc::new(AtomicUsize::new(0));
        let link = Counting {
            inner,
            nexts: Arc::clone(&nexts),
        };
        (link, nexts)
    }
}

impl<L: Link> Link for Counting<L> {
    async fn connect(&mut self, gatt: &GattMap) -> Result<(ChannelSet, u16), LinkError> {
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
        self.inner.disconnect().await;
    }

    // Counted before the await, so a `next` that is dropped unfinished still counts as
    // having touched the link.
    async fn next(&mut self) -> LinkEvent {
        self.nexts.fetch_add(1, Ordering::Relaxed);
        self.inner.next().await
    }
}

/// A connected `Framed` over a `ChannelLink`, the device end, and the count of `next` calls
/// that reached the transport.
async fn connected<F: Framing>(
    framing: F,
) -> (Framed<Counting<ChannelLink>, F>, PeerSide, Arc<AtomicUsize>) {
    let (channel_link, peer) = ChannelLink::pair();
    let (counting, nexts) = Counting::new(channel_link);
    let mut link = Framed::new(counting, framing);
    link.connect(&GATT).await.expect("the peer answers");
    (link, peer, nexts)
}

/// Whether `next` is still waiting: polled once with a waker that does nothing and then
/// dropped, exactly as the pump's `select!` drops it when another branch wins. Whatever the
/// poll took out of the link stays in the buffers.
fn still_waiting<L: Link, F: Framing>(link: &mut Framed<L, F>) -> bool {
    let mut future = pin!(link.next());
    let mut cx = Context::from_waker(Waker::noop());
    matches!(future.as_mut().poll(&mut cx), Poll::Pending)
}

/// The `Rx` a whole frame arrives as.
fn rx(chan: Channel, bytes: &[u8]) -> LinkEvent {
    LinkEvent::Rx {
        chan,
        bytes: bytes.to_vec(),
    }
}

#[tokio::test]
async fn one_notification_of_one_frame_is_one_rx() {
    let (mut link, peer, nexts) = connected(Toy).await;
    let whole = frame(&[1, 2, 3]);
    assert!(peer.notify(EVT, whole.clone()));
    assert_eq!(link.next().await, rx(EVT, &whole));
    assert_eq!(nexts.load(Ordering::Relaxed), 1);
    assert_eq!(link.resynced(), 0);
    assert!(still_waiting(&mut link), "the stream said nothing else");
}

#[tokio::test]
async fn a_frame_split_across_notifications_arrives_once_it_is_whole() {
    let (mut link, peer, _) = connected(Toy).await;
    let whole = frame(&[4, 5, 6, 7]);
    // Every fragment is polled for, and the poll dropped, before the next one is sent: the
    // bytes each poll took out of the link have to survive its future.
    assert!(peer.notify(EVT, whole[..1].to_vec()));
    assert!(still_waiting(&mut link));
    assert!(peer.notify(EVT, whole[1..3].to_vec()));
    assert!(still_waiting(&mut link));
    assert!(peer.notify(EVT, whole[3..].to_vec()));
    assert_eq!(link.next().await, rx(EVT, &whole));
    assert_eq!(link.resynced(), 0);
}

#[tokio::test]
async fn two_frames_in_one_notification_are_two_rxs_in_order() {
    let (mut link, peer, nexts) = connected(Toy).await;
    let first = frame(&[8]);
    let second = frame(&[9, 10]);
    let mut both = first.clone();
    both.extend_from_slice(&second);
    assert!(peer.notify(EVT, both));

    assert_eq!(link.next().await, rx(EVT, &first));
    assert_eq!(nexts.load(Ordering::Relaxed), 1);
    assert_eq!(link.next().await, rx(EVT, &second));
    assert_eq!(
        nexts.load(Ordering::Relaxed),
        1,
        "the second frame was already whole in the buffer"
    );
}

#[tokio::test]
async fn leading_garbage_is_dropped_a_byte_at_a_time_and_counted() {
    let (mut link, peer, _) = connected(Toy).await;
    let whole = frame(&[11, 12]);
    let mut rubbish = vec![0x00, 0x01, 0xff];
    rubbish.extend_from_slice(&whole);
    assert!(peer.notify(EVT, rubbish));
    assert_eq!(link.next().await, rx(EVT, &whole));
    assert_eq!(link.resynced(), 3);

    // One byte on its own is not garbage yet, only not enough to tell; what arrives after
    // it says so, and it is dropped then.
    assert!(peer.notify(EVT, vec![0x00]));
    assert!(still_waiting(&mut link));
    assert_eq!(link.resynced(), 3);
    assert!(peer.notify(EVT, whole.clone()));
    assert_eq!(link.next().await, rx(EVT, &whole));
    assert_eq!(
        link.resynced(),
        4,
        "the count is the link's life, not a frame"
    );
}

#[tokio::test]
async fn buffers_are_per_channel() {
    let (mut link, peer, _) = connected(Toy).await;
    let on_evt = frame(&[0x11, 0x11]);
    let on_extra = frame(&[0x22]);
    assert!(peer.notify(EVT, on_evt[..2].to_vec()));
    assert!(peer.notify(EXTRA, on_extra[..1].to_vec()));
    assert!(peer.notify(EVT, on_evt[2..].to_vec()));
    assert!(peer.notify(EXTRA, on_extra[1..].to_vec()));

    // One buffer per channel, so neither stream is spliced into the other; the channels are
    // served in ascending order, so `EVT` (1) comes before `EXTRA` (2).
    assert_eq!(link.next().await, rx(EVT, &on_evt));
    assert_eq!(link.next().await, rx(EXTRA, &on_extra));
    assert_eq!(link.resynced(), 0);
    assert!(still_waiting(&mut link));
}

/// A framing that is never satisfied, so a buffer only ever grows.
struct NeverWhole;

impl Framing for NeverWhole {
    fn frame_len(&self, _buffered: &[u8]) -> FrameLen {
        FrameLen::Incomplete
    }
}

/// A stream that never completes a frame must not buffer for as long as the device talks.
#[tokio::test]
async fn a_buffer_past_the_limit_is_dropped_rather_than_grown() {
    let (channel_link, peer) = ChannelLink::pair();
    let (counting, _) = Counting::new(channel_link);
    let mut link = Framed::with_limit(counting, NeverWhole, 8);
    link.connect(&GATT).await.expect("the peer answers");

    // Nine bytes, one past the limit, none of which will ever be a frame.
    assert!(peer.notify(EVT, vec![0; 9]));
    assert!(still_waiting(&mut link), "nothing is ever whole");
    assert_eq!(link.resynced(), 9, "the whole buffer was given up");

    // And the channel starts over rather than carrying the rubbish forward.
    assert!(peer.notify(EVT, vec![0; 3]));
    assert!(still_waiting(&mut link));
    assert_eq!(
        link.resynced(),
        9,
        "three bytes are under the limit and still waiting"
    );
}

#[tokio::test]
async fn disconnect_passes_through_and_a_reconnect_starts_empty() {
    let (mut link, peer, _) = connected(Toy).await;
    let half = frame(&[13, 14]);
    assert!(peer.notify(EVT, half[..3].to_vec()));
    peer.disconnect();
    assert_eq!(link.next().await, LinkEvent::Disconnected);

    link.connect(&GATT).await.expect("the peer is still there");
    let whole = frame(&[15]);
    assert!(peer.notify(EVT, whole.clone()));
    // The half frame of the last connection is gone, not the head of this one's first.
    assert_eq!(link.next().await, rx(EVT, &whole));
    assert_eq!(link.resynced(), 0);
}

#[tokio::test]
async fn connect_alone_empties_the_buffers() {
    let (mut link, peer, _) = connected(Toy).await;
    let half = frame(&[16, 17]);
    assert!(peer.notify(EVT, half[..3].to_vec()));
    assert!(
        still_waiting(&mut link),
        "the fragment is buffered, not whole"
    );

    link.connect(&GATT).await.expect("the peer is still there");
    let whole = frame(&[18]);
    assert!(peer.notify(EVT, whole.clone()));
    assert_eq!(link.next().await, rx(EVT, &whole));
    assert_eq!(link.resynced(), 0);
}

#[tokio::test]
async fn a_framing_that_is_never_ready_never_yields() {
    let (mut link, peer, _) = connected(Never).await;
    assert!(still_waiting(&mut link));
    assert!(peer.notify(EVT, frame(&[19, 20])));
    // A poll that returned at all is the other half of the claim: a `Framed` that spun on
    // `Incomplete` instead of waiting would never come back from here.
    assert!(still_waiting(&mut link), "nothing is ever whole");
    assert!(peer.notify(EVT, frame(&[21])));
    assert!(still_waiting(&mut link));
    assert_eq!(link.resynced(), 0, "`Incomplete` drops nothing");
}
