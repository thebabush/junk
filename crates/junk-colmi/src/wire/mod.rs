//! Bytes ↔ frames for the Colmi rings, with no knowledge of sequencing.
//!
//! Two frame formats, one per service:
//!
//! - [`Frame`], the 16-byte `[cmd][body; 14][sum8]` of the V1 channels, both directions.
//!   [`V1Rx`] sorts what arrives on V1 notify into a frame, one of the raw ASCII version
//!   strings, or neither.
//! - [`BigData`], the `bc <kind> <len u16le> <crc16 u16le> <body[len]>` of the V2
//!   channels. The ring delivers replies in MTU-sized notifications; this module decodes
//!   a *complete* frame and tells a collector how long one will be
//!   ([`BigData::expected_len`]). Collecting is the protocol layer's job.
//!
//! [`Cmd`] and [`BigDataKind`] name every command and kind byte the docs know; unknown
//! bytes pass through as `Other`. Every decoder is total: bad bytes are a [`WireError`],
//! never a panic (SPEC invariant 5). The layouts are declared with `binrw`; a `binrw`
//! error never leaves this module.
//!
//! On top of the 16-byte frame sit its typed payloads: [`HostFrame`] for what the host
//! writes and [`RingFrame`] for what the ring notifies, one variant per command shape,
//! decoded from a [`Frame`] and encoded back into one. A body that does not fit a named
//! command is a [`DecodeError`]. Likewise on top of the big-data frame: [`RequestBody`]
//! for what the host writes and [`ReplyBody`] for what the ring notifies, one variant per
//! kind, with a body that does not fit its kind's layout a [`BodyError`].

mod bigdata;
mod body;
mod crc;
mod frame;
mod payload;

use core::fmt;

pub use bigdata::{BigData, BigDataKind};
pub use body::{
    BodyError, DescriptorField, ReplyBody, RequestBody, SampleTag, SleepBody, SleepDay, Spo2Body,
    Spo2Day, Spo2Hour, TemperatureBody, TemperatureDay, WorkoutDescriptor, WorkoutField,
    WorkoutRecord, WorkoutSeries, WorkoutSummary, WorkoutTag,
};
pub use crc::{crc16_modbus, sum8};
pub use frame::{Cmd, Frame, V1Rx};
pub use payload::{
    ActivityPacket, Capabilities, DecodeError, HostFrame, HrLogPacket, Notification, Platform,
    Prefs, RingFrame, SeriesCmd, SeriesPacket, WorkoutAction,
};

/// Why bytes did not decode as a frame, or a frame could not be built.
///
/// Wherever a value is compared, `expected` is what the frame's own contents call for
/// and `got` is what was actually there.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum WireError {
    /// The bytes are not the length this frame format takes.
    Length {
        /// The one length that would have decoded.
        expected: usize,
        /// How many bytes there were.
        got: usize,
    },
    /// A 16-byte frame's last byte is not the sum of the first 15.
    Checksum {
        /// The sum of the first 15 bytes.
        expected: u8,
        /// The byte the frame carried.
        got: u8,
    },
    /// More payload than the frame format can hold.
    PayloadTooLong {
        /// The most it can hold.
        max: usize,
        /// How much was offered.
        got: usize,
    },
    /// A big-data frame does not start with `0xbc`.
    Magic {
        /// The first byte.
        got: u8,
    },
    /// A big-data frame is shorter than its header says; more notifications are needed.
    Incomplete {
        /// The length the header promises, or at least a header's worth if there was
        /// not even a whole one.
        expected: usize,
        /// How many bytes there were.
        got: usize,
    },
    /// A big-data frame's CRC does not match its body.
    Crc {
        /// CRC-16/MODBUS of the body.
        expected: u16,
        /// The CRC the frame carried.
        got: u16,
    },
    /// A value a typed payload cannot put on the wire: a year outside the two BCD
    /// digits, a bucket past the end of the day, a number too big for three bytes.
    Value {
        /// Which field.
        what: &'static str,
    },
    /// The layout did not decode for a reason none of the above names.
    ///
    /// The checks above run before `binrw` sees the bytes, so this is not expected to
    /// happen; it exists so that no `binrw` error, however it arises, escapes as a panic.
    Binrw,
}

impl WireError {
    /// Our error out of a `binrw` one: the [`WireError`] an `assert` raised, if that is
    /// what failed, else [`WireError::Binrw`].
    pub(crate) fn from_binrw(err: &binrw::Error) -> WireError {
        err.custom_err::<WireError>()
            .copied()
            .unwrap_or(WireError::Binrw)
    }
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireError::Length { expected, got } => {
                write!(f, "wrong length: expected {expected} bytes, got {got}")
            }
            WireError::Checksum { expected, got } => {
                write!(
                    f,
                    "checksum mismatch: expected {expected:#04x}, got {got:#04x}"
                )
            }
            WireError::PayloadTooLong { max, got } => {
                write!(f, "payload too long: at most {max} bytes, got {got}")
            }
            WireError::Magic { got } => write!(f, "bad magic: expected 0xbc, got {got:#04x}"),
            WireError::Incomplete { expected, got } => {
                write!(f, "incomplete frame: expected {expected} bytes, got {got}")
            }
            WireError::Crc { expected, got } => {
                write!(f, "crc mismatch: expected {expected:#06x}, got {got:#06x}")
            }
            WireError::Value { what } => write!(f, "{what} does not fit the wire layout"),
            WireError::Binrw => f.write_str("frame layout did not decode"),
        }
    }
}

impl core::error::Error for WireError {}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeSet;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    #[test]
    fn display_names_expected_and_got() {
        let all = [
            WireError::Length {
                expected: 16,
                got: 15,
            },
            WireError::Checksum {
                expected: 0x17,
                got: 0x18,
            },
            WireError::PayloadTooLong { max: 14, got: 15 },
            WireError::Magic { got: 0xbd },
            WireError::Incomplete {
                expected: 55,
                got: 4,
            },
            WireError::Crc {
                expected: 0x40bf,
                got: 0x40be,
            },
            WireError::Value { what: "year" },
            WireError::Binrw,
        ];
        let strings: BTreeSet<String> = all.iter().map(ToString::to_string).collect();
        assert_eq!(strings.len(), all.len());
        assert!(strings.iter().all(|s| !s.is_empty()));

        let length = all[0].to_string();
        assert!(length.contains("16") && length.contains("15"), "{length}");
        let checksum = all[1].to_string();
        assert!(
            checksum.contains("0x17") && checksum.contains("0x18"),
            "{checksum}"
        );
        let too_long = all[2].to_string();
        assert!(
            too_long.contains("14") && too_long.contains("15"),
            "{too_long}"
        );
        let magic = all[3].to_string();
        assert!(magic.contains("0xbc") && magic.contains("0xbd"), "{magic}");
        let incomplete = all[4].to_string();
        assert!(
            incomplete.contains("55") && incomplete.contains('4'),
            "{incomplete}"
        );
        let crc = all[5].to_string();
        assert!(crc.contains("0x40bf") && crc.contains("0x40be"), "{crc}");
        let value = all[6].to_string();
        assert!(value.contains("year"), "{value}");
    }

    #[test]
    fn a_binrw_error_that_is_not_ours_becomes_binrw() {
        let err = binrw::Error::AssertFail {
            pos: 0,
            message: String::from("not a WireError"),
        };
        assert_eq!(WireError::from_binrw(&err), WireError::Binrw);

        let ours = binrw::Error::Custom {
            pos: 3,
            err: alloc::boxed::Box::new(WireError::Magic { got: 1 }),
        };
        assert_eq!(WireError::from_binrw(&ours), WireError::Magic { got: 1 });
    }

    /// Xorshift64: enough randomness to walk the decoders' branches, no dev-dependency.
    struct XorShift(u64);

    impl XorShift {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn byte(&mut self) -> u8 {
            // Truncation is the point: one byte of the state.
            (self.next() & 0xff) as u8
        }
    }

    // Invariant 5, at the wire layer: no input of length 0..=80 makes any decoder panic.
    // Every fourth case is nudged onto a success path (a 0xbc magic, or a 16-byte frame
    // with a good checksum) so those branches are walked too.
    #[test]
    fn decoders_never_panic_on_random_bytes() {
        let mut rng = XorShift(0x9e37_79b9_7f4a_7c15);
        let mut frames = 0;
        let mut big = 0;
        for case in 0..20_000 {
            let len = usize::try_from(rng.next() % 81).unwrap_or(0);
            let mut bytes: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
            match case % 4 {
                1 if !bytes.is_empty() => bytes[0] = 0xbc,
                3 => {
                    bytes.resize(Frame::LEN, 0);
                    bytes[Frame::LEN - 1] = sum8(&bytes[..Frame::LEN - 1]);
                }
                _ => {}
            }
            if Frame::parse(&bytes).is_ok() {
                frames += 1;
            }
            if BigData::parse(&bytes).is_ok() {
                big += 1;
            }
            let _ = BigData::expected_len(&bytes);
            let _ = V1Rx::classify(&bytes);
        }
        assert!(frames >= 5_000, "{frames} valid frames");
        // A random CRC matches one time in 65536; a success here would be a fluke.
        assert_eq!(big, 0);
    }
}
