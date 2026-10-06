//! The `0xbc` big-data frame of the V2 channels and its kind byte.

use alloc::vec::Vec;
use core::cmp::Ordering;

use binrw::io::Cursor;
use binrw::{BinRead, BinWrite, binrw};

use crate::wire::WireError;
use crate::wire::crc::crc16_modbus;

/// The first byte of every big-data frame. The `magic` directive below must agree.
const MAGIC: u8 = 0xbc;
/// Magic, kind, length and CRC: what precedes the body.
const HEADER_LEN: usize = 6;
/// The length field is a `u16`.
const MAX_BODY_LEN: usize = u16::MAX as usize;

/// The kind byte of a big-data frame: which data set a request asks for and a reply
/// carries.
///
/// Every byte has a value; unknown ones are [`BigDataKind::Other`]. Build one with
/// [`BigDataKind::from_byte`] and get the byte back with [`BigDataKind::byte`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum BigDataKind {
    /// `0x25`: half-hourly skin temperature, one block per day.
    Temperature,
    /// `0x27`: sleep sessions with their stages, one block per day.
    Sleep,
    /// `0x28`: manual `SpO2`/HR spot checks. Known from the APK, not seen on this ring.
    ManualHr,
    /// `0x2a`: hourly `SpO2`, one block per day.
    Spo2,
    /// `0x30`: the list of files stored on the ring.
    FileList,
    /// `0x41`: request the workout records since a timestamp.
    WorkoutList,
    /// `0x42`: the workout records, as TLV summaries.
    WorkoutSummary,
    /// `0x43`: request the detail of one workout.
    WorkoutDetail,
    /// `0x44`: how the detail samples are laid out.
    WorkoutDescriptor,
    /// `0x45`: one package of detail samples.
    WorkoutSeries,
    /// A byte with no named variant, passed through undecoded.
    Other(u8),
}

impl BigDataKind {
    /// The kind with this byte. Total: an unnamed byte is [`BigDataKind::Other`].
    #[must_use]
    pub const fn from_byte(byte: u8) -> BigDataKind {
        match byte {
            0x25 => BigDataKind::Temperature,
            0x27 => BigDataKind::Sleep,
            0x28 => BigDataKind::ManualHr,
            0x2a => BigDataKind::Spo2,
            0x30 => BigDataKind::FileList,
            0x41 => BigDataKind::WorkoutList,
            0x42 => BigDataKind::WorkoutSummary,
            0x43 => BigDataKind::WorkoutDetail,
            0x44 => BigDataKind::WorkoutDescriptor,
            0x45 => BigDataKind::WorkoutSeries,
            other => BigDataKind::Other(other),
        }
    }

    /// The byte on the wire.
    #[must_use]
    pub const fn byte(self) -> u8 {
        match self {
            BigDataKind::Temperature => 0x25,
            BigDataKind::Sleep => 0x27,
            BigDataKind::ManualHr => 0x28,
            BigDataKind::Spo2 => 0x2a,
            BigDataKind::FileList => 0x30,
            BigDataKind::WorkoutList => 0x41,
            BigDataKind::WorkoutSummary => 0x42,
            BigDataKind::WorkoutDetail => 0x43,
            BigDataKind::WorkoutDescriptor => 0x44,
            BigDataKind::WorkoutSeries => 0x45,
            BigDataKind::Other(byte) => byte,
        }
    }
}

/// The length field for `body`.
///
/// `body` is private and [`BigData::new`] is the only way to fill it, so it is never
/// longer than [`MAX_BODY_LEN`] and the fallback is unreachable.
fn body_len(body: &[u8]) -> u16 {
    u16::try_from(body.len()).unwrap_or(u16::MAX)
}

/// A complete big-data frame: `bc <kind> <len u16le> <crc16 u16le> <body[len]>`.
///
/// The CRC is CRC-16/MODBUS over the body; it is computed on write and verified on read,
/// so a `BigData` in hand always has a valid one. The body's layout depends on the kind
/// and is the next layer's business.
///
/// The ring delivers a reply in MTU-sized notifications. This type decodes the whole
/// frame once a collector has it; [`BigData::expected_len`] tells the collector how many
/// bytes that is, from the first four.
#[binrw]
#[brw(little, magic = 0xbcu8)]
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct BigData {
    /// Which data set.
    #[br(map = BigDataKind::from_byte)]
    #[bw(map = |kind: &BigDataKind| kind.byte())]
    pub kind: BigDataKind,
    #[br(temp)]
    #[bw(calc = body_len(body))]
    len: u16,
    #[br(temp)]
    #[bw(calc = crc16_modbus(body))]
    crc: u16,
    #[br(count = len, assert(
        crc16_modbus(&body) == crc,
        WireError::Crc { expected: crc16_modbus(&body), got: crc }
    ))]
    body: Vec<u8>,
}

impl BigData {
    /// Bytes before the body: magic, kind, length, CRC.
    pub const HEADER_LEN: usize = HEADER_LEN;
    /// The most body a frame carries: the length field is a `u16`.
    pub const MAX_BODY_LEN: usize = MAX_BODY_LEN;

    /// A frame of `kind` carrying `body`.
    ///
    /// # Errors
    ///
    /// [`WireError::PayloadTooLong`] if `body` exceeds [`BigData::MAX_BODY_LEN`] bytes.
    pub fn new(kind: BigDataKind, body: Vec<u8>) -> Result<BigData, WireError> {
        if body.len() > MAX_BODY_LEN {
            return Err(WireError::PayloadTooLong {
                max: MAX_BODY_LEN,
                got: body.len(),
            });
        }
        Ok(BigData { kind, body })
    }

    /// The body: everything after the header.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// The body, taken out of the frame.
    #[must_use]
    pub fn into_body(self) -> Vec<u8> {
        self.body
    }

    /// How long the frame that starts with `prefix` will be, header included, from the
    /// magic and the length field. `None` with fewer than four bytes or the wrong magic.
    ///
    /// A collector appends notifications until it has this many bytes, then calls
    /// [`BigData::parse`].
    #[must_use]
    pub fn expected_len(prefix: &[u8]) -> Option<usize> {
        match prefix {
            [MAGIC, _kind, lo, hi, ..] => {
                Some(HEADER_LEN + usize::from(u16::from_le_bytes([*lo, *hi])))
            }
            _ => None,
        }
    }

    /// Decodes exactly one complete frame from `bytes`.
    ///
    /// # Errors
    ///
    /// [`WireError::Magic`] if the first byte is not `0xbc`; then, against the length the
    /// header promises, [`WireError::Incomplete`] with fewer bytes (a collector needs more
    /// notifications; with fewer than four bytes the promise is "at least a header") and
    /// [`WireError::Length`] with more; then [`WireError::Crc`] if the body does not match
    /// its CRC.
    pub fn parse(bytes: &[u8]) -> Result<BigData, WireError> {
        if let Some(&got) = bytes.first().filter(|&&first| first != MAGIC) {
            return Err(WireError::Magic { got });
        }
        let expected = Self::expected_len(bytes).unwrap_or(HEADER_LEN);
        let got = bytes.len();
        match got.cmp(&expected) {
            Ordering::Less => Err(WireError::Incomplete { expected, got }),
            Ordering::Greater => Err(WireError::Length { expected, got }),
            Ordering::Equal => {
                BigData::read_le(&mut Cursor::new(bytes)).map_err(|err| WireError::from_binrw(&err))
            }
        }
    }

    /// The frame on the wire, header and CRC included.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::with_capacity(HEADER_LEN + self.body.len()));
        // Writing into a `Vec` cannot fail; nothing else in the layout is fallible.
        let _ = self.write_le(&mut cursor);
        cursor.into_inner()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// The file-list reply from the `QRing` fixture: kind `0x30`, body `00`.
    const FILE_LIST: [u8; 7] = [0xbc, 0x30, 0x01, 0x00, 0xbf, 0x40, 0x00];
    /// The workout-list request from the `QRing` fixture: kind `0x41`, body `4e 74 21 69`.
    const WORKOUT_LIST: [u8; 10] = [0xbc, 0x41, 0x04, 0x00, 0x8f, 0x68, 0x4e, 0x74, 0x21, 0x69];

    fn big(kind: BigDataKind, body: &[u8]) -> BigData {
        BigData::new(kind, body.to_vec()).unwrap_or_else(|err| panic!("{err}"))
    }

    #[test]
    fn kind_bytes_round_trip_over_every_value() {
        let mut others = 0;
        for byte in 0..=u8::MAX {
            let kind = BigDataKind::from_byte(byte);
            assert_eq!(kind.byte(), byte);
            if let BigDataKind::Other(inner) = kind {
                assert_eq!(inner, byte);
                others += 1;
            }
        }
        assert_eq!(others, 256 - 10);
        assert_eq!(BigDataKind::from_byte(0x27), BigDataKind::Sleep);
        assert_eq!(BigDataKind::from_byte(0x45), BigDataKind::WorkoutSeries);
        assert_eq!(BigDataKind::from_byte(0x47), BigDataKind::Other(0x47));
    }

    #[test]
    fn fixture_frames_round_trip() {
        let list = BigData::parse(&FILE_LIST).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(list, big(BigDataKind::FileList, &[0x00]));
        assert_eq!(list.body(), [0x00]);
        assert_eq!(list.to_bytes(), FILE_LIST);

        let workouts = BigData::parse(&WORKOUT_LIST).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(workouts.kind, BigDataKind::WorkoutList);
        assert_eq!(workouts.body(), [0x4e, 0x74, 0x21, 0x69]);
        assert_eq!(workouts.to_bytes(), WORKOUT_LIST);
        assert_eq!(workouts.into_body(), vec![0x4e, 0x74, 0x21, 0x69]);

        let empty = big(BigDataKind::FileList, &[]);
        assert_eq!(empty.to_bytes(), [0xbc, 0x30, 0x00, 0x00, 0xff, 0xff]);
        assert_eq!(BigData::parse(&empty.to_bytes()), Ok(empty));
    }

    #[test]
    fn to_bytes_starts_with_the_magic() {
        assert_eq!(big(BigDataKind::Other(0x47), &[1, 2]).to_bytes()[0], MAGIC);
    }

    #[test]
    fn new_refuses_a_body_longer_than_u16() {
        assert!(BigData::new(BigDataKind::Sleep, vec![0; MAX_BODY_LEN]).is_ok());
        assert_eq!(
            BigData::new(BigDataKind::Sleep, vec![0; MAX_BODY_LEN + 1]),
            Err(WireError::PayloadTooLong {
                max: MAX_BODY_LEN,
                got: MAX_BODY_LEN + 1,
            })
        );
    }

    #[test]
    fn parse_rejects_a_bad_magic_first() {
        assert_eq!(BigData::parse(&[0xbd]), Err(WireError::Magic { got: 0xbd }));
        let mut wrong = FILE_LIST;
        wrong[0] = 0x00;
        assert_eq!(BigData::parse(&wrong), Err(WireError::Magic { got: 0x00 }));
    }

    #[test]
    fn parse_reports_incomplete_and_over_long_frames() {
        let incomplete = |got, expected| Err(WireError::Incomplete { expected, got });
        // Not even a whole header: the promise is at least six bytes.
        assert_eq!(BigData::parse(&[]), incomplete(0, HEADER_LEN));
        assert_eq!(BigData::parse(&[0xbc]), incomplete(1, HEADER_LEN));
        assert_eq!(
            BigData::parse(&[0xbc, 0x30, 0x01]),
            incomplete(3, HEADER_LEN)
        );
        // A header, so the exact promise; the QRing log's header-only lines look like this.
        assert_eq!(BigData::parse(&[0xbc, 0x2a, 0x31, 0x00]), incomplete(4, 55));
        assert_eq!(BigData::parse(&FILE_LIST[..6]), incomplete(6, 7));

        let mut long = FILE_LIST.to_vec();
        long.push(0);
        assert_eq!(
            BigData::parse(&long),
            Err(WireError::Length {
                expected: 7,
                got: 8
            })
        );
    }

    #[test]
    fn crc_error_carries_expected_and_got() {
        let mut bad = FILE_LIST;
        bad[4] = 0xbe;
        assert_eq!(
            BigData::parse(&bad),
            Err(WireError::Crc {
                expected: 0x40bf,
                got: 0x40be
            })
        );
        let mut bad_body = WORKOUT_LIST;
        bad_body[9] ^= 0x01;
        assert_eq!(
            BigData::parse(&bad_body),
            Err(WireError::Crc {
                expected: crc16_modbus(&bad_body[6..]),
                got: 0x688f
            })
        );
    }

    #[test]
    fn expected_len_needs_four_bytes_and_the_magic() {
        assert_eq!(BigData::expected_len(&[]), None);
        assert_eq!(BigData::expected_len(&[0xbc]), None);
        assert_eq!(BigData::expected_len(&[0xbc, 0x2a]), None);
        assert_eq!(BigData::expected_len(&[0xbc, 0x2a, 0x31]), None);
        assert_eq!(BigData::expected_len(&[0xbd, 0x2a, 0x31, 0x00]), None);
        assert_eq!(BigData::expected_len(&[0xbc, 0x2a, 0x31, 0x00]), Some(55));
        assert_eq!(BigData::expected_len(&[0xbc, 0x25, 0x32, 0x00]), Some(56));
        assert_eq!(BigData::expected_len(&FILE_LIST), Some(7));
        assert_eq!(
            BigData::expected_len(&[0xbc, 0x00, 0xff, 0xff]),
            Some(HEADER_LEN + MAX_BODY_LEN)
        );
    }
}
