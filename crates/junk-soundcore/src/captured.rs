//! The exchange the probe captured from a real Motion 300 on 2026-09-15, for the unit
//! tests that would otherwise each write it out again.
//!
//! The same bytes are in `fixtures/soundcore-motion-300/probe-2026-09-15.trace`, which
//! `tests/recorded.rs` replays through the driver; these are the copy the `no_std` unit
//! tests can reach without a file system.

/// The ten bytes sent: command `0x0101`, no payload.
pub(crate) const REQUEST: [u8; 10] = [0x08, 0xee, 0, 0, 0, 0x01, 0x01, 0x0a, 0x00, 0x02];

/// The 39 bytes that came back, in one RFCOMM chunk.
pub(crate) const REPLY: [u8; 39] = [
    0x09, 0xff, 0x00, 0x00, 0x01, 0x01, 0x01, 0x27, 0x00, 0x19, 0x04, 0x00, 0x00, 0x00, 0x01, 0x02,
    0x33, 0x2e, 0x30, 0x2e, 0x34, 0x41, 0x43, 0x43, 0x4c, 0x58, 0x58, 0x30, 0x30, 0x30, 0x30, 0x30,
    0x30, 0x30, 0x30, 0x30, 0x30, 0x78, 0x60,
];

/// The reply's 29 payload bytes: everything between the nine-byte header and the checksum.
pub(crate) const PAYLOAD: [u8; 29] = [
    0x19, 0x04, 0x00, 0x00, 0x00, 0x01, 0x02, 0x33, 0x2e, 0x30, 0x2e, 0x34, 0x41, 0x43, 0x43, 0x4c,
    0x58, 0x58, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x78,
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::checksum;

    /// The three constants are one capture, not three: the payload is the reply's middle
    /// and both frames carry the checksum the speaker and the probe computed.
    #[test]
    fn the_constants_agree_with_each_other() {
        assert_eq!(REPLY[9..REPLY.len() - 1], PAYLOAD);
        assert_eq!(checksum(&REPLY[..REPLY.len() - 1]), REPLY[REPLY.len() - 1]);
        assert_eq!(
            checksum(&REQUEST[..REQUEST.len() - 1]),
            REQUEST[REQUEST.len() - 1]
        );
    }
}
