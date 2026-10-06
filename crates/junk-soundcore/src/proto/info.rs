//! The typed `0x0101` device-info payload.

use alloc::string::String;

use junk_core::{Percent, ProtoError};

use crate::wire::Motion300DeviceInfo;

/// Percent per unit of the speaker's battery scale: the wire byte counts fifths.
const PERCENT_PER_FIFTH: u8 = 20;

/// What a Motion 300 says about itself, decoded and range-checked.
///
/// [`Motion300DeviceInfo`] is the byte-faithful view of the same 29 payload bytes, kept as
/// Gadgetbridge reads them; this is the one a [`Resp`](crate::proto::Resp) carries, with
/// the battery as a [`Percent`] and the two ASCII fields as text. Every field was
/// confirmed against the speaker on 2026-09-15
/// (`fixtures/soundcore-motion-300/probe-2026-09-15.trace`).
#[expect(
    clippy::struct_excessive_bools,
    reason = "the speaker reports each of these as its own payload byte; packing them into flags would invent a structure the wire does not have"
)]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DeviceInfo {
    /// Current volume, as the speaker numbers it.
    pub volume: u8,
    /// Battery charge. The wire byte is fifths, so the percentage is `value × 20`.
    pub battery: Percent,
    /// Whether the speaker says it is charging.
    pub charging: bool,
    /// Whether it says it is playing.
    pub playing: bool,
    /// Whether voice prompts are on.
    pub voice_prompts: bool,
    /// Whether auto power-off is on.
    pub auto_power_off: bool,
    /// Which auto power-off duration is selected: `0`–`3` for 10, 20, 30 and 60 minutes.
    pub auto_power_off_duration: u8,
    /// The firmware version, five ASCII bytes (`3.0.4` on the speaker here).
    pub firmware: String,
    /// The serial number, seventeen ASCII bytes (`ACCLXX0000000000x`).
    pub serial: String,
}

impl DeviceInfo {
    /// Payload length of a `0x0101` device-info reply.
    pub const LEN: usize = Motion300DeviceInfo::LEN;

    /// Reads a `0x0101` reply payload.
    ///
    /// # Errors
    ///
    /// [`ProtoError::Malformed`] unless `payload` is exactly [`DeviceInfo::LEN`] bytes
    /// whose battery byte is a count of fifths — a byte above five is a payload this
    /// decoder does not understand, not a charge above 100 % (invariant 5) — and whose
    /// firmware and serial fields are UTF-8.
    pub fn parse(payload: &[u8]) -> Result<DeviceInfo, ProtoError> {
        let raw = Motion300DeviceInfo::parse(payload)
            .map_err(|_| ProtoError::Malformed("device info: not 29 payload bytes"))?;
        let battery = battery_from_fifths(raw.battery_level_raw).ok_or(ProtoError::Malformed(
            "device info: battery is not a count of fifths",
        ))?;
        Ok(DeviceInfo {
            volume: raw.volume,
            battery,
            charging: raw.is_charging(),
            playing: raw.is_currently_playing(),
            voice_prompts: raw.voice_prompts_enabled(),
            auto_power_off: raw.auto_power_off_enabled(),
            auto_power_off_duration: raw.auto_power_off_duration,
            firmware: text(
                raw.firmware_str()
                    .map_err(|_| ProtoError::Malformed("device info: firmware is not UTF-8"))?,
            ),
            serial: text(
                raw.serial_str()
                    .map_err(|_| ProtoError::Malformed("device info: serial is not UTF-8"))?,
            ),
        })
    }
}

/// The speaker's battery scale, in the device-info payload and in the `0x0301`
/// notification alike: one byte counting fifths.
///
/// `None` for a byte that is not a count of fifths — six fifths would be 120 %, and
/// anything past twelve overflows the multiplication rather than reaching a percentage.
/// That is a payload this crate does not understand, not a charge above full (invariant 5).
pub(crate) fn battery_from_fifths(raw: u8) -> Option<Percent> {
    raw.checked_mul(PERCENT_PER_FIFTH).and_then(Percent::new)
}

/// One of the two fixed-width ASCII fields as text, with the trailing NULs a shorter value
/// would be padded with trimmed. The captured reply fills both fields exactly.
fn text(field: &str) -> String {
    String::from(field.trim_end_matches('\0'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::captured::PAYLOAD;
    use alloc::vec;
    use alloc::vec::Vec;

    /// The captured payload with one byte changed.
    fn altered(index: usize, value: u8) -> Vec<u8> {
        let mut payload = PAYLOAD.to_vec();
        payload[index] = value;
        payload
    }

    #[test]
    fn decodes_the_captured_payload() {
        let info = DeviceInfo::parse(&PAYLOAD).expect("the speaker's own reply");
        assert_eq!(info.volume, 25);
        assert_eq!(info.battery, Percent::new(80).expect("in range"));
        assert!(!info.charging);
        assert!(!info.playing);
        assert!(!info.voice_prompts);
        assert!(info.auto_power_off);
        assert_eq!(info.auto_power_off_duration, 2);
        assert_eq!(info.firmware, "3.0.4");
        assert_eq!(info.serial, "ACCLXX0000000000x");
    }

    #[test]
    fn the_battery_byte_is_fifths_and_nothing_else() {
        assert_eq!(
            DeviceInfo::parse(&altered(1, 0)).map(|info| info.battery),
            Ok(Percent::ZERO)
        );
        assert_eq!(
            DeviceInfo::parse(&altered(1, 5)).map(|info| info.battery),
            Ok(Percent::FULL)
        );
        // Six fifths is 120 %, and 0xff would overflow the multiplication: both are a
        // payload this decoder does not understand.
        for raw in [6, 0xff] {
            assert_eq!(
                DeviceInfo::parse(&altered(1, raw)),
                Err(ProtoError::Malformed(
                    "device info: battery is not a count of fifths"
                )),
                "battery byte {raw}"
            );
        }
    }

    #[test]
    fn a_payload_of_the_wrong_length_is_malformed() {
        for len in [0, DeviceInfo::LEN - 1, DeviceInfo::LEN + 1] {
            assert_eq!(
                DeviceInfo::parse(&vec![0; len]),
                Err(ProtoError::Malformed("device info: not 29 payload bytes")),
                "{len} bytes"
            );
        }
    }

    #[test]
    fn a_field_that_is_not_utf8_is_malformed() {
        assert_eq!(
            DeviceInfo::parse(&altered(7, 0xff)),
            Err(ProtoError::Malformed("device info: firmware is not UTF-8"))
        );
        assert_eq!(
            DeviceInfo::parse(&altered(12, 0xff)),
            Err(ProtoError::Malformed("device info: serial is not UTF-8"))
        );
    }

    /// A shorter serial is padded to seventeen bytes; the padding is not part of it.
    #[test]
    fn trailing_nuls_are_not_part_of_a_field() {
        assert_eq!(
            DeviceInfo::parse(&altered(28, 0)).map(|info| info.serial),
            Ok("ACCLXX0000000000".into())
        );
    }
}
