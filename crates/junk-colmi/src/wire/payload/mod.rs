//! Typed payloads of the 16-byte frames: what a [`Frame`]'s body means, per command, in
//! both directions.
//!
//! [`HostFrame`] is what the host writes on V1 write and [`RingFrame`] what the ring
//! notifies on V1 notify. Each is an enum with one variant per documented command shape,
//! decoded *from* a [`Frame`] and encoded back into one. The `match` on [`Cmd`] in each
//! decoder is exhaustive, so a command byte that gains a [`Cmd`] variant is a compile
//! error here until it is given a payload (SPEC §3.2). Decoding is lossless on the
//! fixtures: every frame the two captures carry re-encodes byte for byte, and a byte the
//! docs do not explain is kept in its variant rather than dropped.
//!
//! No sequencing lives here: a multi-packet log is one variant per packet, and pairing a
//! reply with its request, or the packets of a log with each other, is the protocol
//! layer's job. Nor do model types: an `HrSample` needs a day and a UTC offset that the
//! protocol layer has and a frame does not.
//!
//! Byte layouts in the docs below are the body, what follows the command byte;
//! unspecified trailing bytes are zero. Dates and times are BCD; multi-byte numbers are
//! little-endian unless a layout says `be`.

mod host;
mod ring;

use core::fmt;

use crate::wire::{Cmd, Frame, WireError};

pub use host::HostFrame;
pub use ring::{
    ActivityPacket, Capabilities, HrLogPacket, Notification, RingFrame, SeriesCmd, SeriesPacket,
};

/// The body of a frame.
type Body = [u8; Frame::BODY_LEN];

/// The largest value three bytes hold.
const U24_MAX: u32 = 0x00ff_ffff;

/// Why a frame with a named [`Cmd`] did not decode as a payload.
///
/// A frame whose command byte is [`Cmd::Other`] never fails: it decodes as the `Raw`
/// variant of either enum.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum DecodeError {
    /// The body does not fit any layout the command has in this direction.
    Malformed {
        /// The command byte.
        cmd: Cmd,
        /// What did not fit: a sub-command byte, a BCD field, a percentage, …
        what: &'static str,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Malformed { cmd, what } => {
                write!(f, "malformed {what} in {cmd:?} frame ({:#04x})", cmd.byte())
            }
        }
    }
}

impl core::error::Error for DecodeError {}

/// The user profile the ring keeps: the payload of a `0a 02` write and of the `0a 01`
/// read reply, in this byte order (R09 decompilation).
///
/// Layout: `<hour12> <imperial> <sex> <age> <height_cm> <weight_kg> <sbp> <dbp> <hr_warn>`.
/// The fixture write is `00 00 00 1e af 46 78 50 96`: 24-hour clock, metric, sex 0, 30
/// years, 175 cm, 70 kg, 120/80 mmHg, warn above 150 bpm. The read reply carries the same
/// fields with the last three zero.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct Prefs {
    /// A 12-hour clock (`01`) rather than 24-hour (`00`).
    pub hour12: bool,
    /// Imperial units (`01`) rather than metric (`00`).
    pub imperial: bool,
    /// Sex, as the app encodes it; `0` in the fixture.
    pub sex: u8,
    /// Age in years.
    pub age: u8,
    /// Height in centimetres.
    pub height_cm: u8,
    /// Weight in kilograms.
    pub weight_kg: u8,
    /// Systolic blood pressure, mmHg.
    pub sbp: u8,
    /// Diastolic blood pressure, mmHg.
    pub dbp: u8,
    /// The heart rate above which the ring warns, bpm.
    pub hr_warn: u8,
}

impl Prefs {
    /// The profile at `body[1..10]`, after the sub-command byte.
    fn from_body(body: &Body) -> Prefs {
        let [
            _,
            hour12,
            imperial,
            sex,
            age,
            height_cm,
            weight_kg,
            sbp,
            dbp,
            hr_warn,
            ..,
        ] = *body;
        Prefs {
            hour12: is_on(hour12),
            imperial: is_on(imperial),
            sex,
            age,
            height_cm,
            weight_kg,
            sbp,
            dbp,
            hr_warn,
        }
    }

    /// The nine profile bytes.
    fn to_bytes(self) -> [u8; 9] {
        [
            on_byte(self.hour12),
            on_byte(self.imperial),
            self.sex,
            self.age,
            self.height_cm,
            self.weight_kg,
            self.sbp,
            self.dbp,
            self.hr_warn,
        ]
    }
}

/// The phone's platform, the first byte of a `0x04` phone-name frame.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum Platform {
    /// `01`: iOS. `QRing` on a Mac sends this.
    Ios,
    /// `02`: Android. Gadgetbridge sends this.
    Android,
    /// Any other byte, passed through.
    Other(u8),
}

impl Platform {
    /// The platform with this byte. Total: an unnamed byte is [`Platform::Other`].
    #[must_use]
    pub const fn from_byte(byte: u8) -> Platform {
        match byte {
            0x01 => Platform::Ios,
            0x02 => Platform::Android,
            other => Platform::Other(other),
        }
    }

    /// The byte on the wire.
    #[must_use]
    pub const fn byte(self) -> u8 {
        match self {
            Platform::Ios => 0x01,
            Platform::Android => 0x02,
            Platform::Other(byte) => byte,
        }
    }
}

/// What a `0x77` workout-control frame asks for, and what its ack echoes.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum WorkoutAction {
    /// `01`: start a workout.
    Start,
    /// `02`: pause it.
    Pause,
    /// `03`: resume it (R09 decompilation; not seen on this ring).
    Continue,
    /// `04`: stop it. The ring then notifies `73 07` once the record is stored.
    Stop,
    /// Any other byte, passed through. The ring acks pause and stop with `00`.
    Other(u8),
}

impl WorkoutAction {
    /// The action with this byte. Total: an unnamed byte is [`WorkoutAction::Other`].
    #[must_use]
    pub const fn from_byte(byte: u8) -> WorkoutAction {
        match byte {
            0x01 => WorkoutAction::Start,
            0x02 => WorkoutAction::Pause,
            0x03 => WorkoutAction::Continue,
            0x04 => WorkoutAction::Stop,
            other => WorkoutAction::Other(other),
        }
    }

    /// The byte on the wire.
    #[must_use]
    pub const fn byte(self) -> u8 {
        match self {
            WorkoutAction::Start => 0x01,
            WorkoutAction::Pause => 0x02,
            WorkoutAction::Continue => 0x03,
            WorkoutAction::Stop => 0x04,
            WorkoutAction::Other(byte) => byte,
        }
    }
}

/// A one-byte flag: `01` is on, anything else off.
fn is_on(byte: u8) -> bool {
    byte == 0x01
}

/// The byte of a one-byte flag: `01` on, `00` off.
fn on_byte(on: bool) -> u8 {
    u8::from(on)
}

/// The value of a BCD byte (`0x26` → 26), or `None` if either nibble is above 9.
fn bcd_to_u8(byte: u8) -> Option<u8> {
    let (high, low) = (byte >> 4, byte & 0x0f);
    if high > 9 || low > 9 {
        return None;
    }
    Some(high * 10 + low)
}

/// `value` as a BCD byte (26 → `0x26`), or `None` above 99.
fn u8_to_bcd(value: u8) -> Option<u8> {
    if value > 99 {
        return None;
    }
    Some(((value / 10) << 4) | (value % 10))
}

/// The year a two-digit BCD byte stands for: `0x26` → 2026.
fn year_of_bcd(byte: u8) -> Option<u16> {
    bcd_to_u8(byte).map(|year| 2000 + u16::from(year))
}

/// A BCD byte for `value`, or [`WireError::Value`] naming `what` if it is above 99.
fn bcd(value: u8, what: &'static str) -> Result<u8, WireError> {
    u8_to_bcd(value).ok_or(WireError::Value { what })
}

/// The two-digit BCD byte for `year`, which must be `2000..=2099`.
fn bcd_year(year: u16) -> Result<u8, WireError> {
    let what = "year";
    let since_2000 = year
        .checked_sub(2000)
        .and_then(|years| u8::try_from(years).ok())
        .ok_or(WireError::Value { what })?;
    bcd(since_2000, what)
}

/// Three little-endian bytes as a number.
fn u24_le([b0, b1, b2]: [u8; 3]) -> u32 {
    u32::from_le_bytes([b0, b1, b2, 0])
}

/// Three big-endian bytes as a number.
fn u24_be([b0, b1, b2]: [u8; 3]) -> u32 {
    u32::from_be_bytes([0, b0, b1, b2])
}

/// `value` as three little-endian bytes, or [`WireError::Value`] naming `what` if it
/// needs a fourth.
fn u24_to_le(value: u32, what: &'static str) -> Result<[u8; 3], WireError> {
    if value > U24_MAX {
        return Err(WireError::Value { what });
    }
    let [b0, b1, b2, _] = value.to_le_bytes();
    Ok([b0, b1, b2])
}

/// `value` as three big-endian bytes, or [`WireError::Value`] naming `what` if it needs
/// a fourth.
fn u24_to_be(value: u32, what: &'static str) -> Result<[u8; 3], WireError> {
    if value > U24_MAX {
        return Err(WireError::Value { what });
    }
    let [_, b0, b1, b2] = value.to_be_bytes();
    Ok([b0, b1, b2])
}

/// A frame for `cmd` whose body is `parts` laid end to end, zero-padded.
///
/// Every layout here is fixed and fits, so the error is reachable only through a
/// caller-sized part (a phone name).
fn frame(cmd: Cmd, parts: &[&[u8]]) -> Result<Frame, WireError> {
    let got: usize = parts.iter().map(|part| part.len()).sum();
    if got > Frame::BODY_LEN {
        return Err(WireError::PayloadTooLong {
            max: Frame::BODY_LEN,
            got,
        });
    }
    let mut body = [0; Frame::BODY_LEN];
    let bytes = parts.iter().flat_map(|part| part.iter());
    for (dst, src) in body.iter_mut().zip(bytes) {
        *dst = *src;
    }
    Ok(Frame { cmd, body })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn bcd_round_trips_every_two_digit_value() {
        for value in 0..=99 {
            let byte = u8_to_bcd(value).unwrap_or_else(|| panic!("{value}"));
            assert_eq!(byte >> 4, value / 10);
            assert_eq!(byte & 0x0f, value % 10);
            assert_eq!(bcd_to_u8(byte), Some(value));
        }
        assert_eq!(u8_to_bcd(26), Some(0x26));
        assert_eq!(bcd_to_u8(0x26), Some(26));
        assert_eq!(u8_to_bcd(100), None);
        assert_eq!(u8_to_bcd(255), None);
        assert_eq!(bcd_to_u8(0x0a), None);
        assert_eq!(bcd_to_u8(0xa0), None);
        assert_eq!(bcd_to_u8(0xff), None);
    }

    #[test]
    fn years_are_two_bcd_digits_since_2000() {
        assert_eq!(year_of_bcd(0x26), Some(2026));
        assert_eq!(year_of_bcd(0x00), Some(2000));
        assert_eq!(year_of_bcd(0x99), Some(2099));
        assert_eq!(year_of_bcd(0x9a), None);
        assert_eq!(bcd_year(2026), Ok(0x26));
        assert_eq!(bcd_year(2000), Ok(0x00));
        assert_eq!(bcd_year(2099), Ok(0x99));
        assert_eq!(bcd_year(1999), Err(WireError::Value { what: "year" }));
        assert_eq!(bcd_year(2100), Err(WireError::Value { what: "year" }));
        assert_eq!(bcd(7, "month"), Ok(0x07));
        assert_eq!(bcd(100, "month"), Err(WireError::Value { what: "month" }));
    }

    #[test]
    fn u24_readers_and_writers_agree_on_both_orders() {
        assert_eq!(u24_le([0x88, 0x13, 0x00]), 5000);
        assert_eq!(u24_le([0xe0, 0x93, 0x04]), 300_000);
        assert_eq!(u24_be([0x00, 0x05, 0x79]), 1401);
        assert_eq!(u24_be([0x00, 0xf7, 0xaf]), 63407);
        assert_eq!(u24_to_le(5000, "steps"), Ok([0x88, 0x13, 0x00]));
        assert_eq!(u24_to_be(1401, "steps"), Ok([0x00, 0x05, 0x79]));
        assert_eq!(u24_to_le(U24_MAX, "steps"), Ok([0xff, 0xff, 0xff]));
        assert_eq!(u24_to_be(U24_MAX, "steps"), Ok([0xff, 0xff, 0xff]));
        assert_eq!(
            u24_to_le(U24_MAX + 1, "steps"),
            Err(WireError::Value { what: "steps" })
        );
        assert_eq!(
            u24_to_be(u32::MAX, "cal"),
            Err(WireError::Value { what: "cal" })
        );
    }

    #[test]
    fn flags_are_exactly_one() {
        assert!(is_on(1));
        assert!(!is_on(0));
        assert!(!is_on(2));
        assert_eq!(on_byte(true), 1);
        assert_eq!(on_byte(false), 0);
    }

    #[test]
    fn frame_lays_parts_end_to_end_and_refuses_overflow() {
        let built = frame(Cmd::HrLog, &[&[0x01], &[0xaa, 0xbb], &[]]);
        assert_eq!(
            built,
            Ok(Frame {
                cmd: Cmd::HrLog,
                body: [0x01, 0xaa, 0xbb, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            })
        );
        assert_eq!(frame(Cmd::Battery, &[]).map(|f| f.body), Ok([0; 14]));
        assert_eq!(
            frame(Cmd::Battery, &[&[7; 14]]).map(|f| f.body),
            Ok([7; 14])
        );
        assert_eq!(
            frame(Cmd::Battery, &[&[0; 10], &[0; 5]]),
            Err(WireError::PayloadTooLong { max: 14, got: 15 })
        );
    }

    #[test]
    fn platform_and_action_bytes_round_trip() {
        for byte in 0..=u8::MAX {
            assert_eq!(Platform::from_byte(byte).byte(), byte);
            assert_eq!(WorkoutAction::from_byte(byte).byte(), byte);
        }
        assert_eq!(Platform::from_byte(1), Platform::Ios);
        assert_eq!(Platform::from_byte(2), Platform::Android);
        assert_eq!(Platform::from_byte(3), Platform::Other(3));
        assert_eq!(WorkoutAction::from_byte(1), WorkoutAction::Start);
        assert_eq!(WorkoutAction::from_byte(2), WorkoutAction::Pause);
        assert_eq!(WorkoutAction::from_byte(3), WorkoutAction::Continue);
        assert_eq!(WorkoutAction::from_byte(4), WorkoutAction::Stop);
        assert_eq!(WorkoutAction::from_byte(0), WorkoutAction::Other(0));
    }

    #[test]
    fn prefs_round_trip_through_a_body() {
        let prefs = Prefs {
            hour12: true,
            imperial: false,
            sex: 1,
            age: 30,
            height_cm: 175,
            weight_kg: 70,
            sbp: 120,
            dbp: 80,
            hr_warn: 150,
        };
        let bytes = prefs.to_bytes();
        assert_eq!(bytes, [1, 0, 1, 30, 175, 70, 120, 80, 150]);
        let mut body = [0; 14];
        body[0] = 0x02;
        for (dst, src) in body[1..].iter_mut().zip(bytes) {
            *dst = src;
        }
        assert_eq!(Prefs::from_body(&body), prefs);
    }

    #[test]
    fn decode_error_names_the_command_and_the_field() {
        let err = DecodeError::Malformed {
            cmd: Cmd::Battery,
            what: "battery percent",
        };
        let text = err.to_string();
        assert!(text.contains("Battery"), "{text}");
        assert!(text.contains("0x03"), "{text}");
        assert!(text.contains("battery percent"), "{text}");
        let other = DecodeError::Malformed {
            cmd: Cmd::Other(0x3a),
            what: "sub-command",
        }
        .to_string();
        assert!(other.contains("0x3a"), "{other}");
    }
}
