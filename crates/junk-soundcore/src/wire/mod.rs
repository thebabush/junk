//! Wire format for Soundcore RFCOMM packets.
//!
//! Packet layout, little-endian unless noted:
//!
//! ```text
//! host → device: 08 ee 00 00 00 <cmd:u16> <len:u16> <payload...> <sum:u8>
//! device → host: 09 ff 00 00 01 <cmd:u16> <len:u16> <payload...> <sum:u8>
//! ```
//!
//! `len` is the complete packet length, including the final checksum byte. The
//! checksum is the wrapping sum of every preceding byte in the packet. The vendor
//! app/Gadgetbridge code computes it when sending; Gadgetbridge currently does not
//! verify it when decoding, but this crate does by default.
//!
//! # The start-of-packet byte order
//!
//! Gadgetbridge declares `START_OF_PACKET_HOST = (short)0xee08` and
//! `START_OF_PACKET_DEVICE = (short)0xff09`, and writes both through a **little-endian**
//! `ByteBuffer`. Those constants are therefore not the wire order: the bytes that leave
//! the host are `08 ee`, and the ones that come back are `09 ff`. This crate and
//! `docs/soundcore-motion-300.md` had them the other way round until the probe of
//! 2026-09-15 sent `08 ee 00 00 00 01 01 0a 00 02` to a Motion 300 and was answered with a
//! frame beginning `09 ff 00 00 01`. The capture is
//! `fixtures/soundcore-motion-300/probe-2026-09-15.trace`.

use core::fmt;
use core::str;

/// Bluetooth Classic RFCOMM/SPP service UUID used by Soundcore Motion 300.
///
/// The same UUID as [`SERVICE`](crate::SERVICE), in the text form the SDP notes and the
/// probe's command line use.
pub const SERVICE_MOTION_300: &str = "0cf12d31-fac3-4553-bd80-d6832e7b3135";

/// The shortest a packet can be: the header and the checksum, with no payload.
pub(crate) const HEADER_LEN_WITH_CHECKSUM: usize = 10;
/// Everything before the payload: two start bytes, two reserved, direction, command,
/// length.
pub(crate) const HEADER_LEN_WITHOUT_CHECKSUM: usize = 9;
/// Start of packet, host to device: `0xee08` as a little-endian `ByteBuffer` writes it.
pub(crate) const START_HOST: [u8; 2] = [0x08, 0xee];
/// Start of packet, device to host: `0xff09`, likewise.
pub(crate) const START_DEVICE: [u8; 2] = [0x09, 0xff];
const DIRECTION_HOST: u8 = 0x00;
const DIRECTION_DEVICE: u8 = 0x01;

/// A Soundcore command identifier.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    /// `0x0101`: get full Motion 300 device info.
    GetDeviceInfo,
    /// `0x0301`: battery level notification; payload is one unit in fifths.
    NotifyBatteryInfo,
    /// `0x0401`: charging-state notification.
    NotifyChargingInfo,
    /// `0x0901`: volume notification.
    NotifyVolumeInfo,
    /// `0x2101`: playback notification.
    NotifyPlaybackInfo,
    /// `0x7f01`: get LDAC mode.
    GetLdacMode,
    /// `0x8901`: power off.
    PowerOff,
    /// `0x9001`: set voice prompts on/off.
    SetVoicePrompts,
    /// `0x8601`: set auto power-off.
    SetAutoPowerOff,
    /// `0x9310`: get button brightness.
    GetButtonBrightness,
    /// `0x9210`: set button brightness.
    SetButtonBrightness,
    /// `0xff01`: set LDAC mode.
    SetLdacMode,
    /// `0x8c02`: get current speaker direction/orientation.
    GetCurrentDirection,
    /// `0x8a02`: set adaptive direction.
    SetAdaptiveDirection,
    /// `0x8902`: get equalizer state.
    GetEqualizer,
    /// `0x8b02`: set equalizer preset.
    SetEqualizerPreset,
    /// `0x8d02`: set custom equalizer.
    SetEqualizerCustom,
    /// `0x8e02`: bass-mode notification.
    NotifyBassMode,
    /// A command not yet named by this crate.
    Other(u16),
}

impl Command {
    /// Convert a raw little-endian command value into a named command when known.
    #[must_use]
    pub const fn from_raw(raw: u16) -> Self {
        match raw {
            0x0101 => Self::GetDeviceInfo,
            0x0301 => Self::NotifyBatteryInfo,
            0x0401 => Self::NotifyChargingInfo,
            0x0901 => Self::NotifyVolumeInfo,
            0x2101 => Self::NotifyPlaybackInfo,
            0x7f01 => Self::GetLdacMode,
            0x8901 => Self::PowerOff,
            0x9001 => Self::SetVoicePrompts,
            0x8601 => Self::SetAutoPowerOff,
            0x9310 => Self::GetButtonBrightness,
            0x9210 => Self::SetButtonBrightness,
            0xff01 => Self::SetLdacMode,
            0x8c02 => Self::GetCurrentDirection,
            0x8a02 => Self::SetAdaptiveDirection,
            0x8902 => Self::GetEqualizer,
            0x8b02 => Self::SetEqualizerPreset,
            0x8d02 => Self::SetEqualizerCustom,
            0x8e02 => Self::NotifyBassMode,
            other => Self::Other(other),
        }
    }

    /// The raw command value used on the wire.
    #[must_use]
    pub const fn raw(self) -> u16 {
        match self {
            Self::GetDeviceInfo => 0x0101,
            Self::NotifyBatteryInfo => 0x0301,
            Self::NotifyChargingInfo => 0x0401,
            Self::NotifyVolumeInfo => 0x0901,
            Self::NotifyPlaybackInfo => 0x2101,
            Self::GetLdacMode => 0x7f01,
            Self::PowerOff => 0x8901,
            Self::SetVoicePrompts => 0x9001,
            Self::SetAutoPowerOff => 0x8601,
            Self::GetButtonBrightness => 0x9310,
            Self::SetButtonBrightness => 0x9210,
            Self::SetLdacMode => 0xff01,
            Self::GetCurrentDirection => 0x8c02,
            Self::SetAdaptiveDirection => 0x8a02,
            Self::GetEqualizer => 0x8902,
            Self::SetEqualizerPreset => 0x8b02,
            Self::SetEqualizerCustom => 0x8d02,
            Self::NotifyBassMode => 0x8e02,
            Self::Other(raw) => raw,
        }
    }
}

/// Direction encoded in a packet header.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Host/app to speaker.
    HostToDevice,
    /// Speaker to host/app.
    DeviceToHost,
}

impl Direction {
    const fn start(self) -> [u8; 2] {
        match self {
            Self::HostToDevice => START_HOST,
            Self::DeviceToHost => START_DEVICE,
        }
    }

    const fn byte(self) -> u8 {
        match self {
            Self::HostToDevice => DIRECTION_HOST,
            Self::DeviceToHost => DIRECTION_DEVICE,
        }
    }
}

/// A decoded Soundcore packet borrowing its payload.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Packet<'a> {
    /// Packet direction from the header.
    pub direction: Direction,
    /// Packet command.
    pub command: Command,
    /// Payload bytes, excluding the checksum.
    pub payload: &'a [u8],
}

impl<'a> Packet<'a> {
    /// Create a host→device packet.
    #[must_use]
    pub const fn host(command: Command, payload: &'a [u8]) -> Self {
        Self {
            direction: Direction::HostToDevice,
            command,
            payload,
        }
    }

    /// Create a device→host packet, useful for fixtures/tests.
    #[must_use]
    pub const fn device(command: Command, payload: &'a [u8]) -> Self {
        Self {
            direction: Direction::DeviceToHost,
            command,
            payload,
        }
    }

    /// Total encoded length.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        HEADER_LEN_WITH_CHECKSUM + self.payload.len()
    }

    /// Encode into `out`, returning the packet slice that was written.
    ///
    /// # Errors
    ///
    /// Returns [`PacketError::PayloadTooLong`] if the payload cannot fit in the
    /// protocol's `u16` length field, or [`PacketError::OutputTooSmall`] when
    /// `out` cannot hold the complete packet.
    pub fn encode_into<'b>(&self, out: &'b mut [u8]) -> Result<&'b [u8], PacketError> {
        let total_len = self.encoded_len();
        let total_len_u16 = u16::try_from(total_len).map_err(|_| PacketError::PayloadTooLong {
            got: self.payload.len(),
        })?;
        if out.len() < total_len {
            return Err(PacketError::OutputTooSmall {
                needed: total_len,
                got: out.len(),
            });
        }

        let start = self.direction.start();
        out[0] = start[0];
        out[1] = start[1];
        out[2] = 0;
        out[3] = 0;
        out[4] = self.direction.byte();
        out[5..7].copy_from_slice(&self.command.raw().to_le_bytes());
        out[7..9].copy_from_slice(&total_len_u16.to_le_bytes());
        out[9..9 + self.payload.len()].copy_from_slice(self.payload);
        out[total_len - 1] = checksum(&out[..total_len - 1]);

        Ok(&out[..total_len])
    }

    /// Decode a complete packet and verify its checksum.
    ///
    /// # Errors
    ///
    /// Returns [`PacketError`] when the header, length, or checksum is invalid.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, PacketError> {
        if bytes.len() < HEADER_LEN_WITH_CHECKSUM {
            return Err(PacketError::TooShort { got: bytes.len() });
        }

        // Matched against the constants rather than repeated literals: the two disagreeing
        // is exactly how the reversed start-of-packet survived as long as it did.
        let direction = match ([bytes[0], bytes[1]], bytes[4]) {
            (START_HOST, DIRECTION_HOST) => Direction::HostToDevice,
            (START_DEVICE, DIRECTION_DEVICE) => Direction::DeviceToHost,
            _ => {
                return Err(PacketError::Header {
                    start: [bytes[0], bytes[1]],
                    direction: bytes[4],
                });
            }
        };

        if bytes[2] != 0 || bytes[3] != 0 {
            return Err(PacketError::Reserved {
                got: [bytes[2], bytes[3]],
            });
        }

        let command = Command::from_raw(u16::from_le_bytes([bytes[5], bytes[6]]));
        let declared_len = usize::from(u16::from_le_bytes([bytes[7], bytes[8]]));
        if declared_len != bytes.len() {
            return Err(PacketError::Length {
                declared: declared_len,
                got: bytes.len(),
            });
        }
        if declared_len < HEADER_LEN_WITH_CHECKSUM {
            return Err(PacketError::TooShort { got: declared_len });
        }

        let expected = checksum(&bytes[..bytes.len() - 1]);
        let got = bytes[bytes.len() - 1];
        if expected != got {
            return Err(PacketError::Checksum { expected, got });
        }

        Ok(Self {
            direction,
            command,
            payload: &bytes[HEADER_LEN_WITHOUT_CHECKSUM..bytes.len() - 1],
        })
    }
}

/// Errors while encoding or decoding a Soundcore packet.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PacketError {
    /// Fewer than ten bytes were supplied.
    TooShort {
        /// Actual byte count.
        got: usize,
    },
    /// Start bytes or direction byte do not form a known direction.
    Header {
        /// First two bytes.
        start: [u8; 2],
        /// Direction byte at offset 4.
        direction: u8,
    },
    /// Reserved bytes at offsets 2 and 3 were nonzero.
    Reserved {
        /// The two reserved bytes.
        got: [u8; 2],
    },
    /// Header length did not equal the buffer length.
    Length {
        /// Length from the header.
        declared: usize,
        /// Actual byte count.
        got: usize,
    },
    /// Checksum mismatch.
    Checksum {
        /// Computed checksum.
        expected: u8,
        /// Packet checksum byte.
        got: u8,
    },
    /// Payload too large for the `u16` length field.
    PayloadTooLong {
        /// Payload length.
        got: usize,
    },
    /// Output buffer was not large enough for encoding.
    OutputTooSmall {
        /// Required byte count.
        needed: usize,
        /// Provided byte count.
        got: usize,
    },
}

/// Errors while decoding a command payload.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PayloadError {
    /// Payload length was not the command's expected length.
    Length {
        /// Expected byte count.
        expected: usize,
        /// Actual byte count.
        got: usize,
    },
}

impl fmt::Display for PayloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Length { expected, got } => {
                write!(f, "payload length mismatch: expected {expected}, got {got}")
            }
        }
    }
}

impl fmt::Display for PacketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort { got } => write!(f, "Soundcore packet too short: {got} bytes"),
            Self::Header { start, direction } => write!(
                f,
                "invalid Soundcore packet header: start={start:02x?} direction=0x{direction:02x}"
            ),
            Self::Reserved { got } => write!(f, "reserved bytes are not zero: {got:02x?}"),
            Self::Length { declared, got } => {
                write!(
                    f,
                    "Soundcore length mismatch: declared {declared}, got {got}"
                )
            }
            Self::Checksum { expected, got } => write!(
                f,
                "Soundcore checksum mismatch: expected 0x{expected:02x}, got 0x{got:02x}"
            ),
            Self::PayloadTooLong { got } => {
                write!(f, "Soundcore payload too long for u16 length: {got} bytes")
            }
            Self::OutputTooSmall { needed, got } => {
                write!(f, "output too small: need {needed}, got {got}")
            }
        }
    }
}

/// Wrapping byte sum used as packet checksum.
#[must_use]
pub fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |sum, byte| sum.wrapping_add(*byte))
}

/// Motion 300 `0x0101` device-info reply payload.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Motion300DeviceInfo {
    /// Current volume byte.
    pub volume: u8,
    /// Battery level in fifths; percentage is [`Motion300DeviceInfo::battery_percent`].
    pub battery_level_raw: u8,
    /// Raw charging byte: `0` normal, nonzero charging.
    pub charging_raw: u8,
    /// Raw playback byte: `0` stopped, nonzero playing.
    pub currently_playing_raw: u8,
    /// Raw voice-prompts byte: `0` disabled, nonzero enabled.
    pub voice_prompts_raw: u8,
    /// Raw auto power-off byte: `0` disabled, nonzero enabled.
    pub auto_power_off_enabled_raw: u8,
    /// Auto power-off duration selector.
    pub auto_power_off_duration: u8,
    /// Five firmware-version bytes.
    pub firmware: [u8; 5],
    /// Seventeen serial-number bytes.
    pub serial: [u8; 17],
}

impl Motion300DeviceInfo {
    /// Payload length of a Motion 300 device-info reply.
    pub const LEN: usize = 29;

    /// Parse a `0x0101` reply payload.
    ///
    /// # Errors
    ///
    /// Returns [`PayloadError::Length`] unless `payload` is exactly 29 bytes.
    pub fn parse(payload: &[u8]) -> Result<Self, PayloadError> {
        if payload.len() != Self::LEN {
            return Err(PayloadError::Length {
                expected: Self::LEN,
                got: payload.len(),
            });
        }

        let mut firmware = [0; 5];
        firmware.copy_from_slice(&payload[7..12]);
        let mut serial = [0; 17];
        serial.copy_from_slice(&payload[12..29]);

        Ok(Self {
            volume: payload[0],
            battery_level_raw: payload[1],
            charging_raw: payload[2],
            currently_playing_raw: payload[3],
            voice_prompts_raw: payload[4],
            auto_power_off_enabled_raw: payload[5],
            auto_power_off_duration: payload[6],
            firmware,
            serial,
        })
    }

    /// Battery percentage, matching Gadgetbridge's `raw * 20` interpretation.
    #[must_use]
    pub const fn battery_percent(&self) -> u8 {
        self.battery_level_raw.saturating_mul(20)
    }

    /// Whether the speaker says it is charging.
    #[must_use]
    pub const fn is_charging(&self) -> bool {
        self.charging_raw != 0
    }

    /// Whether playback is currently active.
    #[must_use]
    pub const fn is_currently_playing(&self) -> bool {
        self.currently_playing_raw != 0
    }

    /// Whether voice prompts are enabled.
    #[must_use]
    pub const fn voice_prompts_enabled(&self) -> bool {
        self.voice_prompts_raw != 0
    }

    /// Whether auto power-off is enabled.
    #[must_use]
    pub const fn auto_power_off_enabled(&self) -> bool {
        self.auto_power_off_enabled_raw != 0
    }

    /// Firmware as UTF-8 text.
    ///
    /// # Errors
    ///
    /// Returns [`str::Utf8Error`] if the bytes are not valid UTF-8.
    pub fn firmware_str(&self) -> Result<&str, str::Utf8Error> {
        str::from_utf8(&self.firmware)
    }

    /// Serial number as UTF-8 text.
    ///
    /// # Errors
    ///
    /// Returns [`str::Utf8Error`] if the bytes are not valid UTF-8.
    pub fn serial_str(&self) -> Result<&str, str::Utf8Error> {
        str::from_utf8(&self.serial)
    }
}

/// One Motion 300 custom equalizer band.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct EqualizerBand {
    /// Wire value. Gadgetbridge maps UI `0..120` to wire `60..180`.
    pub value: u8,
    /// Frequency selector byte.
    pub frequency: u8,
}

impl EqualizerBand {
    /// Build a band from Gadgetbridge/UI value `0..=120` and a frequency selector.
    ///
    /// Returns `None` if `ui_value` is outside the observed preference range.
    #[must_use]
    pub const fn from_ui(ui_value: u8, frequency: u8) -> Option<Self> {
        if ui_value <= 120 {
            Some(Self {
                value: ui_value + 60,
                frequency,
            })
        } else {
            None
        }
    }

    /// The Gadgetbridge/UI value `0..=120`, saturating if a malformed wire value is lower
    /// than the observed offset.
    #[must_use]
    pub const fn ui_value(&self) -> u8 {
        self.value.saturating_sub(60)
    }
}

/// A Motion 300 custom equalizer profile: nine value/frequency bands.
pub type EqualizerProfile = [EqualizerBand; 9];

/// Motion 300 `0x8902` equalizer-state reply payload.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Motion300Equalizer {
    /// Adaptive-direction flag.
    pub adaptive_direction: bool,
    /// Current direction/orientation byte.
    pub current_direction: u8,
    /// Active preset number.
    pub preset: u8,
    /// Three custom equalizer profiles, each with nine bands.
    pub profiles: [EqualizerProfile; 3],
}

impl Motion300Equalizer {
    /// Payload length of a Motion 300 equalizer reply.
    pub const LEN: usize = 57;

    /// Parse a `0x8902` reply payload.
    ///
    /// # Errors
    ///
    /// Returns [`PayloadError::Length`] unless `payload` is exactly 57 bytes.
    pub fn parse(payload: &[u8]) -> Result<Self, PayloadError> {
        if payload.len() != Self::LEN {
            return Err(PayloadError::Length {
                expected: Self::LEN,
                got: payload.len(),
            });
        }

        let mut profiles = [[EqualizerBand::default(); 9]; 3];
        let mut offset = 3;
        for profile in &mut profiles {
            for band in profile {
                *band = EqualizerBand {
                    value: payload[offset],
                    frequency: payload[offset + 1],
                };
                offset += 2;
            }
        }

        Ok(Self {
            adaptive_direction: payload[0] != 0,
            current_direction: payload[1],
            preset: payload[2],
            profiles,
        })
    }
}

/// Payload for a Motion 300 `0x8d02` custom equalizer set command.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct CustomEqualizerSet {
    /// Direction/profile index selected by the app, encoded as `1 << direction`.
    pub direction: u8,
    /// The nine-band profile to write.
    pub profile: EqualizerProfile,
}

impl CustomEqualizerSet {
    /// Encoded payload length.
    pub const LEN: usize = 21;

    /// Encode as `[1 << direction, 0x01, 0xff, profile...]`.
    #[must_use]
    pub fn to_payload(&self) -> [u8; Self::LEN] {
        let mut payload = [0; Self::LEN];
        payload[0] = 1u8.checked_shl(u32::from(self.direction)).unwrap_or(0);
        payload[1] = 0x01;
        payload[2] = 0xff;

        let mut offset = 3;
        for band in self.profile {
            payload[offset] = band.value;
            payload[offset + 1] = band.frequency;
            offset += 2;
        }
        payload
    }
}

/// Parse a one-byte battery notification payload into a percentage.
///
/// # Errors
///
/// Returns [`PayloadError::Length`] unless `payload` is exactly one byte.
pub fn battery_percent(payload: &[u8]) -> Result<u8, PayloadError> {
    let [raw] = expect_one(payload)?;
    Ok(raw.saturating_mul(20))
}

/// Parse a one-byte charging notification payload.
///
/// # Errors
///
/// Returns [`PayloadError::Length`] unless `payload` is exactly one byte.
pub fn charging(payload: &[u8]) -> Result<bool, PayloadError> {
    let [raw] = expect_one(payload)?;
    Ok(raw != 0)
}

fn expect_one(payload: &[u8]) -> Result<[u8; 1], PayloadError> {
    if payload.len() != 1 {
        return Err(PayloadError::Length {
            expected: 1,
            got: payload.len(),
        });
    }
    Ok([payload[0]])
}

/// Build a `0x0101` device-info request.
#[must_use]
pub const fn device_info_request<'a>() -> Packet<'a> {
    Packet::host(Command::GetDeviceInfo, &[])
}

/// Build a `0x8901` power-off request.
#[must_use]
pub const fn power_off_request<'a>() -> Packet<'a> {
    Packet::host(Command::PowerOff, &[])
}

/// Build a one-byte boolean setting payload (`0x9001`, `0xff01`, `0x8a02`).
#[must_use]
pub const fn boolean_setting_payload(enabled: bool) -> [u8; 1] {
    [if enabled { 1 } else { 0 }]
}

/// Build an auto power-off payload.
///
/// `duration` uses Gadgetbridge's preference values: `0` disables auto-off,
/// `1..=4` encode 10, 20, 30, and 60 minutes as payload durations `0..=3`.
#[must_use]
pub const fn auto_power_off_payload(duration: u8, disabled_duration: u8) -> [u8; 2] {
    if duration > 0 {
        [1, duration - 1]
    } else {
        [0, disabled_duration]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_device_info_request() {
        let mut out = [0; 10];
        let encoded = device_info_request().encode_into(&mut out).unwrap();
        // The ten bytes the probe sent the speaker on 2026-09-15, byte for byte.
        assert_eq!(encoded, &[0x08, 0xee, 0, 0, 0, 0x01, 0x01, 0x0a, 0, 0x02]);
    }

    #[test]
    fn decodes_device_info_reply() {
        let payload = [
            4, 3, 0, 1, 1, 1, 1, b'1', b'.', b'2', b'3', b'4', b'S', b'N', b'0', b'0', b'0', b'0',
            b'0', b'0', b'0', b'0', b'0', b'0', b'0', b'0', b'0', b'0', b'1',
        ];
        let packet = Packet::device(Command::GetDeviceInfo, &payload);
        let mut bytes = [0; 39];
        let encoded = packet.encode_into(&mut bytes).unwrap();
        let decoded = Packet::decode(encoded).unwrap();
        assert_eq!(decoded.direction, Direction::DeviceToHost);
        assert_eq!(decoded.command, Command::GetDeviceInfo);
        assert_eq!(decoded.payload, payload);

        let info = Motion300DeviceInfo::parse(decoded.payload).unwrap();
        assert_eq!(info.battery_percent(), 60);
        assert!(!info.is_charging());
        assert_eq!(info.firmware_str().unwrap(), "1.234");
        assert_eq!(info.serial_str().unwrap(), "SN000000000000001");
    }

    #[test]
    fn parses_equalizer_reply() {
        let mut payload = [0; Motion300Equalizer::LEN];
        payload[0] = 1;
        payload[1] = 2;
        payload[2] = 0x81;
        payload[3] = 120;
        payload[4] = 4;
        payload[55] = 121;
        payload[56] = 8;

        let eq = Motion300Equalizer::parse(&payload).unwrap();
        assert!(eq.adaptive_direction);
        assert_eq!(eq.current_direction, 2);
        assert_eq!(eq.preset, 0x81);
        assert_eq!(eq.profiles[0][0].value, 120);
        assert_eq!(eq.profiles[0][0].frequency, 4);
        assert_eq!(eq.profiles[2][8].value, 121);
        assert_eq!(eq.profiles[2][8].frequency, 8);
    }

    #[test]
    fn encodes_custom_equalizer_set() {
        let mut profile = [EqualizerBand::default(); 9];
        profile[0] = EqualizerBand::from_ui(60, 4).unwrap();
        profile[8] = EqualizerBand::from_ui(61, 8).unwrap();

        let payload = CustomEqualizerSet {
            direction: 2,
            profile,
        }
        .to_payload();

        assert_eq!(payload[0..3], [0x04, 0x01, 0xff]);
        assert_eq!(payload[3], 120);
        assert_eq!(payload[4], 4);
        assert_eq!(payload[19], 121);
        assert_eq!(payload[20], 8);
    }

    #[test]
    fn parses_one_byte_notifications() {
        assert_eq!(battery_percent(&[3]).unwrap(), 60);
        assert!(!charging(&[0]).unwrap());
        assert!(charging(&[1]).unwrap());
        assert!(matches!(
            battery_percent(&[]),
            Err(PayloadError::Length {
                expected: 1,
                got: 0
            })
        ));
    }

    #[test]
    fn rejects_bad_checksum() {
        let mut out = [0; 10];
        let encoded = device_info_request().encode_into(&mut out).unwrap();
        let mut corrupted = [0; 10];
        corrupted.copy_from_slice(encoded);
        corrupted[9] ^= 0x80;
        assert!(matches!(
            Packet::decode(&corrupted),
            Err(PacketError::Checksum { .. })
        ));
    }
}
