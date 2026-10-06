//! The framing rule `junk-pump`'s `Framed` needs to cut a Soundcore byte stream back into
//! packets.
//!
//! RFCOMM delivers whatever chunks the radio felt like — the probe's 39-byte reply came in
//! one, but nothing promises that — so the driver above the link is fed whole packets only
//! because `junk-pump`'s `Framed` sits in between with this rule. The rule
//! is pure and knows nothing but the bytes in front of it, which is what keeps the link
//! itself ignorant of packets (invariant 2).

use core::num::NonZeroUsize;

use junk_core::{FrameLen, Framing};

use crate::wire::{
    HEADER_LEN_WITH_CHECKSUM, HEADER_LEN_WITHOUT_CHECKSUM, START_DEVICE, START_HOST,
};

/// The Soundcore framing rule: how long the packet at the front of a buffer is.
///
/// `Incomplete` under nine bytes, `Invalid` when the first two bytes are neither start
/// word or the length field is below the ten-byte minimum, and otherwise the length the
/// frame declares. Pure, so a stream joined mid-packet walks forward to the next real
/// packet rather than wedging on the rubbish before it.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct SoundcoreFraming;

impl Framing for SoundcoreFraming {
    /// Under nine bytes there is no length field yet, so nothing can be said. Once there
    /// is one, the packet is exactly that long — the field counts the whole packet,
    /// checksum included.
    ///
    /// Two things are [`FrameLen::Invalid`], and both make `Framed` drop a byte and look
    /// again rather than wait for ever: a start-of-packet that is neither direction's, and
    /// a declared length below the ten bytes a packet cannot go under. The second matters
    /// because a corrupt length of `0` or `3` would otherwise be answered with a `Len` the
    /// buffer already exceeds, or with `Len(0)`, which the type does not even allow.
    fn frame_len(&self, buffered: &[u8]) -> FrameLen {
        let start = match *buffered {
            [first, second, ..] => [first, second],
            // One byte is not yet enough to rule the start out, let alone to read a length.
            [] | [_] => return FrameLen::Incomplete,
        };
        if start != START_HOST && start != START_DEVICE {
            return FrameLen::Invalid;
        }
        if buffered.len() < HEADER_LEN_WITHOUT_CHECKSUM {
            return FrameLen::Incomplete;
        }
        let declared = usize::from(u16::from_le_bytes([buffered[7], buffered[8]]));
        match NonZeroUsize::new(declared) {
            Some(len) if declared >= HEADER_LEN_WITH_CHECKSUM => FrameLen::Len(len),
            _ => FrameLen::Invalid,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::captured::{REPLY, REQUEST};
    use alloc::vec::Vec;

    fn len(bytes: &[u8]) -> FrameLen {
        SoundcoreFraming.frame_len(bytes)
    }

    fn some(n: usize) -> FrameLen {
        FrameLen::Len(NonZeroUsize::new(n).expect("a packet is never nothing"))
    }

    /// The reply the speaker sent on 2026-09-15 says its own length, and it is the whole
    /// 39 bytes.
    #[test]
    fn the_captured_reply_is_thirty_nine_bytes() {
        assert_eq!(len(&REPLY), some(REPLY.len()));
        assert_eq!(len(&REQUEST), some(REQUEST.len()));
    }

    #[test]
    fn under_nine_bytes_nothing_can_be_said() {
        for take in 0..HEADER_LEN_WITHOUT_CHECKSUM {
            assert_eq!(len(&REPLY[..take]), FrameLen::Incomplete, "{take} bytes");
        }
        assert_eq!(
            len(&REPLY[..HEADER_LEN_WITHOUT_CHECKSUM]),
            some(REPLY.len())
        );
    }

    #[test]
    fn a_start_of_packet_that_is_neither_direction_is_invalid() {
        // The reversed reading this family used to have: not a packet, and dropping a byte
        // is how a reader that joined mid-stream walks forward to one.
        assert_eq!(
            len(&[0xee, 0x08, 0, 0, 0, 1, 1, 10, 0, 2]),
            FrameLen::Invalid
        );
        assert_eq!(len(&[0xff, 0x09]), FrameLen::Invalid);
        assert_eq!(len(&[0x08, 0xff]), FrameLen::Invalid);
        assert_eq!(len(&[0x00, 0x00, 0x00]), FrameLen::Invalid);
        // A single byte could still be the first of either start word.
        assert_eq!(len(&[0x08]), FrameLen::Incomplete);
        assert_eq!(len(&[0x99]), FrameLen::Incomplete);
    }

    #[test]
    fn a_length_below_the_minimum_resynchronises_rather_than_waits() {
        let mut bytes: Vec<u8> = REPLY.to_vec();
        for declared in [0u16, 1, 9] {
            bytes[7..9].copy_from_slice(&declared.to_le_bytes());
            assert_eq!(len(&bytes), FrameLen::Invalid, "declared {declared}");
        }
        let smallest = u16::try_from(HEADER_LEN_WITH_CHECKSUM).expect("ten");
        bytes[7..9].copy_from_slice(&smallest.to_le_bytes());
        assert_eq!(len(&bytes), some(HEADER_LEN_WITH_CHECKSUM));
    }

    /// A length longer than what has arrived waits, exactly as `Incomplete` does; a length
    /// shorter than it cuts the packet out and leaves the rest for the next one.
    #[test]
    fn a_declared_length_is_taken_at_its_word() {
        let mut two = REPLY.to_vec();
        two.extend_from_slice(&REQUEST);
        assert_eq!(len(&two), some(REPLY.len()));
        assert_eq!(len(&REPLY[..20]), some(REPLY.len()));
    }
}
