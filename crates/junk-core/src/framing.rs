//! The rule that cuts a byte stream back into frames.
//!
//! A [`Dir::Stream`](crate::Dir::Stream) transport hands over whatever chunks the radio
//! felt like, not one frame per delivery the way a GATT notification is. Putting the frames
//! back together needs to know what a frame looks like, which is the device family's
//! knowledge and not the transport's: a link that knew would break invariant 2. So the
//! family supplies it here, as [`Framing`], and `junk-pump`'s `Framed` does the buffering
//! with it.
//!
//! This sits in `junk-core` beside [`Driver`](crate::Driver) and [`Link`](crate::Link)
//! because it is the same kind of thing: a contract a device family implements. A family
//! can then supply one without leaving `no_std`.

use core::num::NonZeroUsize;

/// How long the next frame is, given what has been buffered so far.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum FrameLen {
    /// Not enough bytes yet to tell; wait for more.
    Incomplete,
    /// The next frame is exactly this many bytes.
    Len(NonZeroUsize),
    /// These bytes cannot start a frame. The buffer drops its first byte and asks again,
    /// so a reader that joined a stream mid-packet resynchronises.
    Invalid,
}

/// The device family's framing rule: pure, and the only thing here that knows a packet
/// when it sees one.
pub trait Framing {
    /// How long the frame at the front of `buffered` is.
    ///
    /// Called with at least one byte, after the buffer gains a chunk and again
    /// after every byte [`FrameLen::Invalid`] made it drop. It must not depend on anything
    /// but `buffered`: the same bytes always answer the same way.
    ///
    /// A [`FrameLen::Len`] shorter than `buffered` is answered with that many bytes and the
    /// rest kept for the frame after it; one longer waits, exactly as
    /// [`FrameLen::Incomplete`] does.
    fn frame_len(&self, buffered: &[u8]) -> FrameLen;
}
