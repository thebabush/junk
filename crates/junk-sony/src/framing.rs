//! The framing rule `junk-pump`'s `Framed` needs to cut a Sony byte stream back into frames.
//!
//! RFCOMM delivers whatever chunks the radio felt like, so the driver above the link is fed
//! whole frames only because `junk-pump`'s `Framed` sits in between with this rule. The
//! rule is pure and knows nothing but the bytes in front of it, which is what keeps the
//! link itself ignorant of frames (invariant 2).

use core::num::NonZeroUsize;

use junk_core::{FrameLen, Framing};

use crate::wire::{END, START};

/// The Sony framing rule: how long the frame at the front of a buffer is.
///
/// A frame is `0x3E`, an escaped body that contains neither marker, and `0x3C`; the length
/// field inside is deliberately not consulted. It is 32 bits, nothing bounds it, and a
/// corrupt one would make the buffer wait for gigabytes. The end marker is the boundary,
/// and escaping is what makes that safe.
///
/// There is no protocol maximum on a frame, but `junk-pump`'s `Framed` caps its buffer at
/// 64 KiB, so a frame longer than that is never delivered whatever this rule says.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct SonyFraming;

impl Framing for SonyFraming {
    /// `Incomplete` for an empty buffer or a frame whose end marker has not arrived;
    /// `Len(index + 1)` of the first `0x3C` once it has.
    ///
    /// [`FrameLen::Invalid`] when the first byte is not `0x3E`, which is how a reader that
    /// joined mid-frame walks forward to the next start marker, and when a second `0x3E`
    /// turns up before any `0x3C`. Escaping removes every marker from a body, so an
    /// unescaped `0x3E` inside a frame means the end marker of the previous one was lost:
    /// `Framed` drops a byte and looks again, and lands on the start of the next frame.
    fn frame_len(&self, buffered: &[u8]) -> FrameLen {
        match buffered.split_first() {
            Some((&START, rest)) => {
                for (i, &byte) in rest.iter().enumerate() {
                    match byte {
                        END => {
                            // `i` is an index into `rest`, so the frame is `i + 2` bytes.
                            return NonZeroUsize::new(i + 2)
                                .map_or(FrameLen::Invalid, FrameLen::Len);
                        }
                        START => return FrameLen::Invalid,
                        _ => {}
                    }
                }
                FrameLen::Incomplete
            }
            Some(_) => FrameLen::Invalid,
            None => FrameLen::Incomplete,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Frame;
    use alloc::vec::Vec;

    const INIT: [u8; 11] = [
        0x3e, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x0e, 0x3c,
    ];
    const ACK0: [u8; 9] = [0x3e, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x3c];

    fn len(bytes: &[u8]) -> FrameLen {
        SonyFraming.frame_len(bytes)
    }

    fn some(n: usize) -> FrameLen {
        FrameLen::Len(NonZeroUsize::new(n).expect("a frame is never nothing"))
    }

    /// What `Framed` does with a buffer, without the I/O: cut a frame whenever the rule
    /// says `Len`, drop a byte on `Invalid`, stop on `Incomplete`.
    fn drain(mut buf: &[u8]) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        while !buf.is_empty() {
            match len(buf) {
                FrameLen::Len(n) if n.get() <= buf.len() => {
                    frames.push(buf[..n.get()].to_vec());
                    buf = &buf[n.get()..];
                }
                FrameLen::Len(_) | FrameLen::Incomplete => break,
                FrameLen::Invalid => buf = &buf[1..],
            }
        }
        frames
    }

    #[test]
    fn an_empty_buffer_is_incomplete() {
        assert_eq!(len(&[]), FrameLen::Incomplete);
    }

    #[test]
    fn a_whole_frame_says_its_own_length() {
        assert_eq!(len(&INIT), some(INIT.len()));
        assert_eq!(len(&ACK0), some(ACK0.len()));
    }

    /// Fed byte by byte, nothing is said until the end marker is in, and then it is the
    /// whole frame.
    #[test]
    fn a_chunked_frame_waits_for_its_end_marker() {
        for take in 1..INIT.len() {
            assert_eq!(len(&INIT[..take]), FrameLen::Incomplete, "{take} bytes");
        }
        assert_eq!(len(&INIT), some(INIT.len()));
    }

    #[test]
    fn two_frames_back_to_back_are_cut_one_at_a_time() {
        let mut two = INIT.to_vec();
        two.extend_from_slice(&ACK0);
        assert_eq!(len(&two), some(INIT.len()));
        assert_eq!(len(&two[INIT.len()..]), some(ACK0.len()));
        assert_eq!(drain(&two), [INIT.to_vec(), ACK0.to_vec()]);
    }

    #[test]
    fn garbage_before_a_frame_is_walked_past() {
        assert_eq!(len(&[0x00, 0x3e]), FrameLen::Invalid);
        assert_eq!(len(&[0x3c]), FrameLen::Invalid);
        assert_eq!(len(&[0x3d, 0x2c]), FrameLen::Invalid);
        let mut bytes = alloc::vec![0x00, 0x3c, 0xff, 0x12];
        bytes.extend_from_slice(&INIT);
        assert_eq!(drain(&bytes), [INIT.to_vec()]);
    }

    /// A second start marker before any end marker means the end was lost: `Invalid`, so the
    /// reader drops a byte at a time until it stands on the next, good frame.
    #[test]
    fn a_lost_end_marker_resynchronises_on_the_next_frame() {
        assert_eq!(len(&[0x3e, 0x0c, 0x00, 0x3e]), FrameLen::Invalid);
        let mut bytes = INIT[..INIT.len() - 1].to_vec();
        bytes.extend_from_slice(&ACK0);
        assert_eq!(drain(&bytes), [ACK0.to_vec()]);
    }

    /// Escaping keeps every marker out of a body, so a frame whose payload is all markers
    /// is still cut at its real end.
    #[test]
    fn escaped_markers_in_a_body_do_not_end_or_split_the_frame() {
        let frame = Frame::new(
            crate::DataType::DataMdr,
            0,
            alloc::vec![0x3c, 0x3d, 0x3e, 0x3c],
        )
        .encode()
        .expect("small");
        assert!(
            frame[1..frame.len() - 1]
                .iter()
                .all(|b| ![0x3c, 0x3e].contains(b))
        );
        assert_eq!(len(&frame), some(frame.len()));
        let mut two = frame.clone();
        two.extend_from_slice(&ACK0);
        assert_eq!(drain(&two), [frame, ACK0.to_vec()]);
    }
}
