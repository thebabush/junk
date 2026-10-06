//! The 16-byte frame of the V1 channels, its command byte, and what else V1 notify carries.

use alloc::string::String;

use binrw::io::Cursor;
use binrw::{BinRead, BinWrite, binrw};

use crate::wire::WireError;
use crate::wire::crc::sum8;

/// Bytes after the command byte and before the checksum.
const BODY_LEN: usize = 14;

/// The command byte of a 16-byte frame.
///
/// Every byte has a value: the commands `docs/colmi-protocol.md` names or knows the role
/// of get a variant, the rest are [`Cmd::Other`]. Build one with [`Cmd::from_byte`],
/// which never yields an `Other` for a named byte, and get the byte back with
/// [`Cmd::byte`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum Cmd {
    /// `0x01`: set the clock. The ack is the ring's capability bitmap.
    SetTime,
    /// `0x03`: battery level, and whether it is charging.
    Battery,
    /// `0x04`: tell the ring the phone's name.
    PhoneName,
    /// `0x0a`: read or write preferences.
    Prefs,
    /// `0x15`: the periodic HR log of one day, in several packets.
    HrLog,
    /// `0x16`: automatic HR measurement on/off and its interval.
    AutoHrPref,
    /// `0x19`: firmware and hardware version. The strings follow as raw ASCII, not frames.
    Version,
    /// `0x21`: daily goals.
    Goals,
    /// `0x2c`: automatic `SpO2` measurement on/off and its interval.
    AutoSpo2Pref,
    /// `0x2f`: the ring's packet size, volunteered after connect.
    PacketSize,
    /// `0x36`: automatic stress measurement on/off.
    AutoStressPref,
    /// `0x37`: the stress series of one day, in several packets.
    StressLog,
    /// `0x38`: automatic HRV measurement on/off.
    AutoHrvPref,
    /// `0x39`: the HRV series of one day, in several packets.
    HrvLog,
    /// `0x43`: activity (steps, calories, distance) in 15-minute buckets.
    Activity,
    /// `0x48`: today's running totals: steps, calories, distance, active minutes.
    TodayTotals,
    /// `0x50`: make the ring signal itself.
    FindDevice,
    /// `0x69`: start a one-shot manual HR measurement.
    ManualHrStart,
    /// `0x6a`: stop it.
    ManualHrStop,
    /// `0x73`: ring → phone notification: new data, battery, live activity, workout stored.
    Notify,
    /// `0x77`: phone-initiated workout control: start, pause, stop.
    WorkoutCtl,
    /// `0x78`: the live workout stream, about once a second.
    WorkoutData,
    /// `0xff`: factory reset. Never send it.
    FactoryReset,
    /// A byte with no named variant, passed through undecoded (`0x3a`, `0x3b` and `0x3c`
    /// among those the fixtures exchange).
    Other(u8),
}

impl Cmd {
    /// The command with this byte. Total: an unnamed byte is [`Cmd::Other`].
    #[must_use]
    pub const fn from_byte(byte: u8) -> Cmd {
        match byte {
            0x01 => Cmd::SetTime,
            0x03 => Cmd::Battery,
            0x04 => Cmd::PhoneName,
            0x0a => Cmd::Prefs,
            0x15 => Cmd::HrLog,
            0x16 => Cmd::AutoHrPref,
            0x19 => Cmd::Version,
            0x21 => Cmd::Goals,
            0x2c => Cmd::AutoSpo2Pref,
            0x2f => Cmd::PacketSize,
            0x36 => Cmd::AutoStressPref,
            0x37 => Cmd::StressLog,
            0x38 => Cmd::AutoHrvPref,
            0x39 => Cmd::HrvLog,
            0x43 => Cmd::Activity,
            0x48 => Cmd::TodayTotals,
            0x50 => Cmd::FindDevice,
            0x69 => Cmd::ManualHrStart,
            0x6a => Cmd::ManualHrStop,
            0x73 => Cmd::Notify,
            0x77 => Cmd::WorkoutCtl,
            0x78 => Cmd::WorkoutData,
            0xff => Cmd::FactoryReset,
            other => Cmd::Other(other),
        }
    }

    /// The byte on the wire.
    #[must_use]
    pub const fn byte(self) -> u8 {
        match self {
            Cmd::SetTime => 0x01,
            Cmd::Battery => 0x03,
            Cmd::PhoneName => 0x04,
            Cmd::Prefs => 0x0a,
            Cmd::HrLog => 0x15,
            Cmd::AutoHrPref => 0x16,
            Cmd::Version => 0x19,
            Cmd::Goals => 0x21,
            Cmd::AutoSpo2Pref => 0x2c,
            Cmd::PacketSize => 0x2f,
            Cmd::AutoStressPref => 0x36,
            Cmd::StressLog => 0x37,
            Cmd::AutoHrvPref => 0x38,
            Cmd::HrvLog => 0x39,
            Cmd::Activity => 0x43,
            Cmd::TodayTotals => 0x48,
            Cmd::FindDevice => 0x50,
            Cmd::ManualHrStart => 0x69,
            Cmd::ManualHrStop => 0x6a,
            Cmd::Notify => 0x73,
            Cmd::WorkoutCtl => 0x77,
            Cmd::WorkoutData => 0x78,
            Cmd::FactoryReset => 0xff,
            Cmd::Other(byte) => byte,
        }
    }
}

/// The checksum byte of a frame: the wrapping sum of the command byte and the body.
///
/// Takes the byte, not the [`Cmd`]: inside the `binrw` write directives below a mapped
/// field is visible as its wire value.
fn checksum_of(cmd: u8, body: &[u8; BODY_LEN]) -> u8 {
    cmd.wrapping_add(sum8(body))
}

/// The 16-byte frame of the V1 channels: `[cmd][body; 14][checksum]`, same both ways.
///
/// The checksum is the wrapping sum of the first 15 bytes; it is computed on write and
/// verified on read, so a `Frame` in hand always has a valid one. The body is whatever
/// the command needs, zero-padded; its meaning is the next layer's business.
#[binrw]
#[brw(little)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct Frame {
    /// The command byte.
    #[br(map = Cmd::from_byte)]
    #[bw(map = |cmd: &Cmd| cmd.byte())]
    pub cmd: Cmd,
    /// The 14 bytes after it.
    pub body: [u8; BODY_LEN],
    #[br(temp, assert(
        checksum == checksum_of(cmd.byte(), &body),
        WireError::Checksum { expected: checksum_of(cmd.byte(), &body), got: checksum }
    ))]
    #[bw(calc = checksum_of(cmd, body))]
    checksum: u8,
}

impl Frame {
    /// Bytes in a frame.
    pub const LEN: usize = 16;
    /// The most payload a frame carries.
    pub const BODY_LEN: usize = BODY_LEN;

    /// A frame for `cmd` carrying `payload`, zero-padded to the full body.
    ///
    /// # Errors
    ///
    /// [`WireError::PayloadTooLong`] if `payload` exceeds [`Frame::BODY_LEN`] bytes.
    pub fn new(cmd: Cmd, payload: &[u8]) -> Result<Frame, WireError> {
        if payload.len() > BODY_LEN {
            return Err(WireError::PayloadTooLong {
                max: BODY_LEN,
                got: payload.len(),
            });
        }
        let mut body = [0; BODY_LEN];
        for (dst, src) in body.iter_mut().zip(payload) {
            *dst = *src;
        }
        Ok(Frame { cmd, body })
    }

    /// Decodes exactly one frame from `bytes`.
    ///
    /// # Errors
    ///
    /// [`WireError::Length`] unless there are exactly [`Frame::LEN`] bytes, then
    /// [`WireError::Checksum`] if the last byte is not the sum of the others.
    pub fn parse(bytes: &[u8]) -> Result<Frame, WireError> {
        if bytes.len() != Self::LEN {
            return Err(WireError::Length {
                expected: Self::LEN,
                got: bytes.len(),
            });
        }
        Frame::read_le(&mut Cursor::new(bytes)).map_err(|err| WireError::from_binrw(&err))
    }

    /// The frame on the wire, checksum included.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        let mut bytes = [0; Self::LEN];
        // Sixteen bytes of fixed layout into a sixteen-byte buffer cannot fail: nothing
        // here is fallible but the buffer running out, and it is exactly the right size.
        let _ = self.write_le(&mut Cursor::new(&mut bytes[..]));
        bytes
    }
}

/// What one notification on [`V1_NOTIFY`](crate::V1_NOTIFY) turned out to be.
///
/// The channel carries 16-byte frames, but the two replies to [`Cmd::Version`] are raw
/// ASCII (`RT03CR_1.00.02_260319`, `RT03CR_V1.0`), so length alone tells them apart. The
/// one ambiguity: a 16-byte printable string whose last byte happens to be the sum of
/// the other 15 reads as a frame. No version string seen is 16 bytes long.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum V1Rx {
    /// Sixteen bytes with a valid checksum.
    Frame(Frame),
    /// Any other length, every byte printable ASCII (`0x20..=0x7e`): a version string.
    Text(String),
    /// Sixteen bytes with a bad checksum, or another length that is not all printable.
    Invalid(WireError),
}

impl V1Rx {
    /// Sorts `bytes` by the rules above. Never fails; the third variant is the catch-all.
    #[must_use]
    pub fn classify(bytes: &[u8]) -> V1Rx {
        if bytes.len() == Frame::LEN {
            return match Frame::parse(bytes) {
                Ok(frame) => V1Rx::Frame(frame),
                Err(err) => V1Rx::Invalid(err),
            };
        }
        if !bytes.is_empty() && bytes.iter().all(|b| (0x20..=0x7e).contains(b)) {
            return V1Rx::Text(bytes.iter().map(|&b| char::from(b)).collect());
        }
        V1Rx::Invalid(WireError::Length {
            expected: Frame::LEN,
            got: bytes.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// The first line of the `QRing` fixture: `04 01 12`, checksum `0x17`.
    const PHONE_NAME: [u8; 16] = [0x04, 0x01, 0x12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x17];

    fn frame(cmd: Cmd, payload: &[u8]) -> Frame {
        Frame::new(cmd, payload).unwrap_or_else(|err| panic!("{err}"))
    }

    #[test]
    fn cmd_bytes_round_trip_over_every_value() {
        let mut others = 0;
        for byte in 0..=u8::MAX {
            let cmd = Cmd::from_byte(byte);
            assert_eq!(cmd.byte(), byte);
            if let Cmd::Other(inner) = cmd {
                assert_eq!(inner, byte);
                others += 1;
            }
        }
        assert_eq!(others, 256 - 23);
    }

    #[test]
    fn named_cmds_have_the_documented_bytes() {
        let named = [
            (Cmd::SetTime, 0x01),
            (Cmd::Battery, 0x03),
            (Cmd::PhoneName, 0x04),
            (Cmd::Prefs, 0x0a),
            (Cmd::HrLog, 0x15),
            (Cmd::AutoHrPref, 0x16),
            (Cmd::Version, 0x19),
            (Cmd::Goals, 0x21),
            (Cmd::AutoSpo2Pref, 0x2c),
            (Cmd::PacketSize, 0x2f),
            (Cmd::AutoStressPref, 0x36),
            (Cmd::StressLog, 0x37),
            (Cmd::AutoHrvPref, 0x38),
            (Cmd::HrvLog, 0x39),
            (Cmd::Activity, 0x43),
            (Cmd::TodayTotals, 0x48),
            (Cmd::FindDevice, 0x50),
            (Cmd::ManualHrStart, 0x69),
            (Cmd::ManualHrStop, 0x6a),
            (Cmd::Notify, 0x73),
            (Cmd::WorkoutCtl, 0x77),
            (Cmd::WorkoutData, 0x78),
            (Cmd::FactoryReset, 0xff),
        ];
        assert_eq!(named.len(), 23);
        for (cmd, byte) in named {
            assert_eq!(cmd.byte(), byte, "{cmd:?}");
            assert_eq!(Cmd::from_byte(byte), cmd);
        }
        for byte in [0x3a, 0x3b, 0x3c] {
            assert_eq!(Cmd::from_byte(byte), Cmd::Other(byte));
        }
    }

    #[test]
    fn new_pads_with_zeros() {
        let short = frame(Cmd::PhoneName, &[0x01, 0x12]);
        assert_eq!(short.cmd, Cmd::PhoneName);
        assert_eq!(short.body, [0x01, 0x12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(short.to_bytes(), PHONE_NAME);

        let empty = frame(Cmd::Battery, &[]);
        assert_eq!(empty.body, [0; 14]);
        assert_eq!(empty.to_bytes()[15], 0x03);

        let full = frame(Cmd::Other(0x3c), &[7; 14]);
        assert_eq!(full.body, [7; 14]);
        assert_eq!(full.to_bytes()[15], 0x3cu8.wrapping_add(7 * 14));
    }

    #[test]
    fn new_refuses_a_long_payload() {
        assert_eq!(
            Frame::new(Cmd::SetTime, &[0; 15]),
            Err(WireError::PayloadTooLong { max: 14, got: 15 })
        );
        assert_eq!(
            Frame::new(Cmd::SetTime, &[0; 40]),
            Err(WireError::PayloadTooLong { max: 14, got: 40 })
        );
    }

    #[test]
    fn parse_round_trips_a_fixture_frame() {
        let parsed = Frame::parse(&PHONE_NAME).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(parsed, frame(Cmd::PhoneName, &[0x01, 0x12]));
        assert_eq!(parsed.to_bytes(), PHONE_NAME);

        // 3c 00 ac 27 …: the reply to the undecoded 0x3c.
        let other: [u8; 16] = [
            0x3c, 0x00, 0xac, 0x27, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x0f,
        ];
        let parsed = Frame::parse(&other).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(parsed.cmd, Cmd::Other(0x3c));
        assert_eq!(parsed.to_bytes(), other);
    }

    #[test]
    fn parse_wants_exactly_sixteen_bytes() {
        assert_eq!(
            Frame::parse(&PHONE_NAME[..15]),
            Err(WireError::Length {
                expected: 16,
                got: 15
            })
        );
        let mut long: Vec<u8> = PHONE_NAME.to_vec();
        long.push(0);
        assert_eq!(
            Frame::parse(&long),
            Err(WireError::Length {
                expected: 16,
                got: 17
            })
        );
        assert_eq!(
            Frame::parse(&[]),
            Err(WireError::Length {
                expected: 16,
                got: 0
            })
        );
    }

    #[test]
    fn checksum_error_carries_expected_and_got() {
        let mut bad = PHONE_NAME;
        bad[15] = 0x18;
        assert_eq!(
            Frame::parse(&bad),
            Err(WireError::Checksum {
                expected: 0x17,
                got: 0x18
            })
        );
        // A wrapping sum: ff + 01 + … must come out as 0x00, not 0x100.
        let mut wrapped = [0; 16];
        wrapped[0] = 0xff;
        wrapped[1] = 0x01;
        assert!(Frame::parse(&wrapped).is_ok());
        wrapped[15] = 0x01;
        assert_eq!(
            Frame::parse(&wrapped),
            Err(WireError::Checksum {
                expected: 0x00,
                got: 0x01
            })
        );
    }

    #[test]
    fn classify_tells_frames_text_and_garbage_apart() {
        assert_eq!(
            V1Rx::classify(&PHONE_NAME),
            V1Rx::Frame(frame(Cmd::PhoneName, &[0x01, 0x12]))
        );

        let mut bad = PHONE_NAME;
        bad[15] ^= 0xff;
        assert_eq!(
            V1Rx::classify(&bad),
            V1Rx::Invalid(WireError::Checksum {
                expected: 0x17,
                got: 0xe8
            })
        );

        assert_eq!(
            V1Rx::classify(b"RT03CR_1.00.02_260319"),
            V1Rx::Text(String::from("RT03CR_1.00.02_260319"))
        );
        assert_eq!(
            V1Rx::classify(b"RT03CR_V1.0"),
            V1Rx::Text(String::from("RT03CR_V1.0"))
        );
        assert_eq!(V1Rx::classify(b" ~"), V1Rx::Text(String::from(" ~")));

        let length = |got| V1Rx::Invalid(WireError::Length { expected: 16, got });
        assert_eq!(V1Rx::classify(&[]), length(0));
        assert_eq!(V1Rx::classify(b"RT03CR\n"), length(7));
        assert_eq!(V1Rx::classify(&[0x7f]), length(1));
        assert_eq!(V1Rx::classify(&[0x1f, b'A']), length(2));
        assert_eq!(V1Rx::classify(&[0x80; 17]), length(17));
    }

    // The documented ambiguity, pinned so a change in the rule is noticed.
    #[test]
    fn a_sixteen_byte_string_with_a_lucky_checksum_is_a_frame() {
        let mut text = *b"ABCDEFGHIJKLMNO ";
        text[15] = sum8(&text[..15]);
        assert!(matches!(V1Rx::classify(&text), V1Rx::Frame(_)));
        text[15] = b' ';
        assert!(matches!(V1Rx::classify(&text), V1Rx::Invalid(_)));
    }
}
