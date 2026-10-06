//! Wire format for Sony RFCOMM frames.
//!
//! Frame layout, big-endian where it is a number:
//!
//! ```text
//! 3E <type:u8> <seq:u8> <len:u32> <payload...> <sum:u8> 3C
//!    \_______________ escaped _______________________/
//! ```
//!
//! `len` is the payload's length before escaping. `sum` is the wrapping byte sum of the
//! type, the sequence number, the four length bytes and the payload, again before
//! escaping. Everything between the markers is escaped, the checksum byte included, so a
//! marker byte never appears inside a frame: `3C` becomes `3D 2C`, `3D` becomes `3D 2D` and
//! `3E` becomes `3D 2E`. That is what lets [`SonyFraming`](crate::SonyFraming) find the end
//! of a frame without reading its length.
//!
//! # Unescape, then verify
//!
//! [`Frame::decode`] unescapes the body first and checks the checksum over the unescaped
//! bytes, which is what Sony's own app does. A client that sums the bytes as they arrive
//! rejects every frame whose payload or checksum contains a marker byte.
//!
//! # What decoding is stricter about than Sony's app
//!
//! The app logs a length mismatch and carries on, truncating a payload that is longer than
//! the declared length. [`Frame::decode`] returns [`FrameError::Length`] instead: what to do
//! about a damaged frame is the driver's decision. Neither does decoding allocate from the
//! declared length; it compares it with the bytes it has.

use alloc::vec::Vec;
use core::fmt;

use junk_core::ProtoError;

/// The byte that opens a frame.
pub(crate) const START: u8 = 0x3e;
/// The byte that closes a frame.
pub(crate) const END: u8 = 0x3c;
/// The escape byte: the next byte, with `0x10` cleared, is the one that was escaped.
pub(crate) const ESCAPE: u8 = 0x3d;

/// Everything in a frame before the payload: type, sequence number and the 32-bit length.
const HEADER_LEN: usize = 6;
/// The shortest body: the header and the checksum, no payload.
const MIN_BODY_LEN: usize = HEADER_LEN + 1;

/// The data-type byte at the front of a frame: what kind of message it carries and whether
/// the receiver owes an ACK.
///
/// The data types of Sony's app (its `DataType` enum). A byte this crate does not name
/// is [`DataType::Other`] and round-trips, so decoding never refuses a frame over it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum DataType {
    /// `0x00`: `DATA`.
    Data,
    /// `0x01`: `ACK`, an empty-payload frame that acknowledges one received frame.
    Ack,
    /// `0x02`: `DATA_MC_NO1`.
    DataMcNo1,
    /// `0x07`: `DATA_SSH`.
    DataSsh,
    /// `0x09`: `DATA_ICD`.
    DataIcd,
    /// `0x0a`: `DATA_EV`.
    DataEv,
    /// `0x0c`: `DATA_MDR`, the v1 table-one headphone commands.
    DataMdr,
    /// `0x0d`: `DATA_COMMON`.
    DataCommon,
    /// `0x0e`: `DATA_MDR_NO2`, the v1 table-two headphone commands.
    DataMdrNo2,
    /// `0x10`: `SHOT`.
    Shot,
    /// `0x12`: `SHOT_MC_NO1`.
    ShotMcNo1,
    /// `0x19`: `SHOT_ICD`.
    ShotIcd,
    /// `0x1a`: `SHOT_EV`.
    ShotEv,
    /// `0x1c`: `SHOT_MDR`: `DATA_MDR` that is never acknowledged.
    ShotMdr,
    /// `0x1d`: `SHOT_COMMON`.
    ShotCommon,
    /// `0x1e`: `SHOT_MDR_NO2`: `DATA_MDR_NO2` that is never acknowledged.
    ShotMdrNo2,
    /// `0x27`: `LARGE_DATA_SSH`.
    LargeDataSsh,
    /// `0x2d`: `LARGE_DATA_COMMON`.
    LargeDataCommon,
    /// A data type not named by this crate.
    Other(u8),
}

impl DataType {
    /// Convert a raw type byte into a named data type when known.
    #[must_use]
    pub const fn from_raw(raw: u8) -> Self {
        match raw {
            0x00 => Self::Data,
            0x01 => Self::Ack,
            0x02 => Self::DataMcNo1,
            0x07 => Self::DataSsh,
            0x09 => Self::DataIcd,
            0x0a => Self::DataEv,
            0x0c => Self::DataMdr,
            0x0d => Self::DataCommon,
            0x0e => Self::DataMdrNo2,
            0x10 => Self::Shot,
            0x12 => Self::ShotMcNo1,
            0x19 => Self::ShotIcd,
            0x1a => Self::ShotEv,
            0x1c => Self::ShotMdr,
            0x1d => Self::ShotCommon,
            0x1e => Self::ShotMdrNo2,
            0x27 => Self::LargeDataSsh,
            0x2d => Self::LargeDataCommon,
            other => Self::Other(other),
        }
    }

    /// The raw type byte used on the wire.
    #[must_use]
    pub const fn raw(self) -> u8 {
        match self {
            Self::Data => 0x00,
            Self::Ack => 0x01,
            Self::DataMcNo1 => 0x02,
            Self::DataSsh => 0x07,
            Self::DataIcd => 0x09,
            Self::DataEv => 0x0a,
            Self::DataMdr => 0x0c,
            Self::DataCommon => 0x0d,
            Self::DataMdrNo2 => 0x0e,
            Self::Shot => 0x10,
            Self::ShotMcNo1 => 0x12,
            Self::ShotIcd => 0x19,
            Self::ShotEv => 0x1a,
            Self::ShotMdr => 0x1c,
            Self::ShotCommon => 0x1d,
            Self::ShotMdrNo2 => 0x1e,
            Self::LargeDataSsh => 0x27,
            Self::LargeDataCommon => 0x2d,
            Self::Other(raw) => raw,
        }
    }

    /// Whether the receiver of a frame of this type owes the sender an ACK.
    ///
    /// `ACK` and every `SHOT_*` variant are fire and forget; every other named type is
    /// acknowledged, including the ones a v1 link otherwise ignores. [`DataType::Other`] answers
    /// `false`: the app's table has nothing to say about a byte it does not name, and this
    /// crate does not guess.
    #[must_use]
    pub const fn needs_ack(self) -> bool {
        match self {
            Self::Data
            | Self::DataMcNo1
            | Self::DataSsh
            | Self::DataIcd
            | Self::DataEv
            | Self::DataMdr
            | Self::DataCommon
            | Self::DataMdrNo2
            | Self::LargeDataSsh
            | Self::LargeDataCommon => true,
            Self::Ack
            | Self::Shot
            | Self::ShotMcNo1
            | Self::ShotIcd
            | Self::ShotEv
            | Self::ShotMdr
            | Self::ShotCommon
            | Self::ShotMdrNo2
            | Self::Other(_) => false,
        }
    }
}

/// One decoded Sony frame.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Frame {
    /// What the frame carries.
    pub data_type: DataType,
    /// The sequence number. Zero or one in practice; the wire has a whole byte.
    pub seq: u8,
    /// The payload, unescaped, without the checksum.
    pub payload: Vec<u8>,
}

impl Frame {
    /// A frame of `data_type` and `seq` carrying `payload`.
    #[must_use]
    pub const fn new(data_type: DataType, seq: u8, payload: Vec<u8>) -> Self {
        Self {
            data_type,
            seq,
            payload,
        }
    }

    /// The ACK for a received frame whose sequence number was `received_seq`: an `ACK`
    /// with sequence number `1 - received_seq` and no payload.
    ///
    /// Only 0 and 1 are valid; Sony's app answers any other sequence number with no ACK and
    /// a log line, so `None` here.
    #[must_use]
    pub const fn ack_for(received_seq: u8) -> Option<Self> {
        match received_seq {
            0 | 1 => Some(Self::new(DataType::Ack, 1 - received_seq, Vec::new())),
            _ => None,
        }
    }

    /// Encode as the bytes that go on the wire: markers, escaping and checksum.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError::PayloadTooLong`] if the payload cannot fit in the 32-bit
    /// length field.
    pub fn encode(&self) -> Result<Vec<u8>, FrameError> {
        let len = u32::try_from(self.payload.len()).map_err(|_| FrameError::PayloadTooLong {
            got: self.payload.len(),
        })?;
        let mut body = Vec::with_capacity(MIN_BODY_LEN + self.payload.len());
        body.push(self.data_type.raw());
        body.push(self.seq);
        body.extend_from_slice(&len.to_be_bytes());
        body.extend_from_slice(&self.payload);
        body.push(checksum(&body));

        let mut out = Vec::with_capacity(body.len() + 2);
        out.push(START);
        escape_into(&mut out, &body);
        out.push(END);
        Ok(out)
    }

    /// Decode one complete frame, markers included, and verify it.
    ///
    /// The body is unescaped first and the checksum is computed over the unescaped bytes.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError`] when the markers are missing or a marker appears inside the
    /// body, the escaping is malformed, the body is shorter than a header and checksum, the
    /// declared length is not the payload's length, or the checksum is wrong.
    pub fn decode(bytes: &[u8]) -> Result<Self, FrameError> {
        let [START, body @ .., END] = bytes else {
            return Err(match bytes {
                [] | [_] => FrameError::TooShort { got: bytes.len() },
                [first, ..] if *first != START => FrameError::MissingStart,
                _ => FrameError::MissingEnd,
            });
        };
        if let Some(at) = body.iter().position(|&b| b == START || b == END) {
            return Err(FrameError::StrayMarker { at: at + 1 });
        }
        let raw = unescape(body)?;
        let [data_type, seq, l0, l1, l2, l3, rest @ ..] = raw.as_slice() else {
            return Err(FrameError::TooShort { got: raw.len() });
        };
        let Some((&got, payload)) = rest.split_last() else {
            return Err(FrameError::TooShort { got: raw.len() });
        };
        let declared = u32::from_be_bytes([*l0, *l1, *l2, *l3]);
        if usize::try_from(declared) != Ok(payload.len()) {
            return Err(FrameError::Length {
                declared,
                got: payload.len(),
            });
        }
        let expected = checksum(&raw[..raw.len() - 1]);
        if expected != got {
            return Err(FrameError::Checksum { expected, got });
        }
        Ok(Self {
            data_type: DataType::from_raw(*data_type),
            seq: *seq,
            payload: payload.to_vec(),
        })
    }
}

/// Errors while encoding or decoding a Sony frame.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub enum FrameError {
    /// Too few bytes to be a frame: under nine for a whole frame, under seven between the
    /// markers.
    TooShort {
        /// Actual byte count.
        got: usize,
    },
    /// The first byte was not the `0x3E` start marker.
    MissingStart,
    /// The last byte was not the `0x3C` end marker.
    MissingEnd,
    /// A start or end marker appeared between the markers, where escaping forbids one.
    StrayMarker {
        /// Offset into the frame, markers counted.
        at: usize,
    },
    /// An escape byte with nothing after it.
    BadEscape,
    /// The declared payload length is not the length of the payload present.
    Length {
        /// Length from the header.
        declared: u32,
        /// Payload byte count actually present.
        got: usize,
    },
    /// Checksum mismatch.
    Checksum {
        /// Computed checksum.
        expected: u8,
        /// Frame checksum byte.
        got: u8,
    },
    /// Payload too large for the 32-bit length field.
    PayloadTooLong {
        /// Payload length.
        got: usize,
    },
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort { got } => write!(f, "Sony frame too short: {got} bytes"),
            Self::MissingStart => f.write_str("Sony frame does not start with 0x3e"),
            Self::MissingEnd => f.write_str("Sony frame does not end with 0x3c"),
            Self::StrayMarker { at } => {
                write!(
                    f,
                    "unescaped marker byte inside a Sony frame at offset {at}"
                )
            }
            Self::BadEscape => f.write_str("Sony frame ends in an escape byte"),
            Self::Length { declared, got } => write!(
                f,
                "Sony length mismatch: declared {declared}, payload is {got}"
            ),
            Self::Checksum { expected, got } => write!(
                f,
                "Sony checksum mismatch: expected 0x{expected:02x}, got 0x{got:02x}"
            ),
            Self::PayloadTooLong { got } => {
                write!(f, "Sony payload too long for u32 length: {got} bytes")
            }
        }
    }
}

impl core::error::Error for FrameError {}

impl From<FrameError> for ProtoError {
    /// A bad checksum is [`ProtoError::Checksum`]; every other fault is a frame that does
    /// not have the layout, so [`ProtoError::Malformed`] with the fault's name.
    fn from(err: FrameError) -> Self {
        match err {
            FrameError::Checksum { .. } => Self::Checksum,
            FrameError::TooShort { .. } => Self::Malformed("Sony frame too short"),
            FrameError::MissingStart => Self::Malformed("Sony frame has no start marker"),
            FrameError::MissingEnd => Self::Malformed("Sony frame has no end marker"),
            FrameError::StrayMarker { .. } => Self::Malformed("marker byte inside Sony frame"),
            FrameError::BadEscape => Self::Malformed("Sony frame ends in an escape byte"),
            FrameError::Length { .. } => Self::Malformed("Sony length does not match payload"),
            FrameError::PayloadTooLong { .. } => Self::Malformed("Sony payload too long"),
        }
    }
}

/// Wrapping byte sum used as the frame checksum.
#[must_use]
pub fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |sum, byte| sum.wrapping_add(*byte))
}

/// Append `bytes` to `out`, escaping the three marker bytes.
fn escape_into(out: &mut Vec<u8>, bytes: &[u8]) {
    for &byte in bytes {
        match byte {
            START | END | ESCAPE => out.extend_from_slice(&[ESCAPE, byte & 0xef]),
            _ => out.push(byte),
        }
    }
}

/// Escape `bytes`: `3C` becomes `3D 2C`, `3D` becomes `3D 2D`, `3E` becomes `3D 2E`.
#[must_use]
pub fn escape(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    escape_into(&mut out, bytes);
    out
}

/// Undo [`escape`]: after each `3D`, the next byte with `0x10` set. Bytes that are not
/// escaped pass through, markers included; rejecting those is [`Frame::decode`]'s job.
///
/// # Errors
///
/// Returns [`FrameError::BadEscape`] when the last byte is an escape byte.
pub fn unescape(bytes: &[u8]) -> Result<Vec<u8>, FrameError> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut iter = bytes.iter();
    while let Some(&byte) = iter.next() {
        if byte == ESCAPE {
            let &next = iter.next().ok_or(FrameError::BadEscape)?;
            out.push(next | 0x10);
        } else {
            out.push(byte);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
