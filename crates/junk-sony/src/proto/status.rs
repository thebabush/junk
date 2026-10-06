//! [`Status`]: everything the headset reports about itself, and how to tell what it did not.

use alloc::string::String;
use alloc::vec::Vec;

use junk_core::Bytes;

use crate::payload::{
    AssignableSettingsPreset, AudioCodec, AutoPowerOff, AutoPowerOffElementId, Battery,
    CapabilityInfo, ConnectionMode, DseeSetting, EbbCapability, EqBands, EqCapability, EqState,
    FunctionType, GsCapability, GsValue, LeftRightBattery, LeftRightConnection, MdrLanguage,
    ModelInfo, NcAsmCapability, NcAsmState, NcOptimizer, OnOff, PairedDevices, PairingCapability,
    PairingModeState, SpeakToChatConfig, UpscalingIndicator, VoiceGuidanceCapability,
};
use crate::wire::DataType;

/// What one feature's reading came to. Not an `Option`, because "absent" is three different
/// facts and a shell has to say which.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub enum Reading<T> {
    /// The headset does not list the function this feature belongs to, so the driver never
    /// asked. This is the state of every field of a [`Status::default`].
    #[default]
    NotSupported,
    /// The function is listed (or the function list itself never came), the driver asked,
    /// and no reply arrived before the step's reply timeout. Also what a feature reads as
    /// when it was never asked because the function list had no answer.
    NoReply,
    /// A reply arrived, was acknowledged, and could not be decoded: too short for its
    /// layout, or not the command the driver expected. The reply was also reported as an
    /// [`Ev::Unparsed`](super::Ev::Unparsed).
    Malformed,
    /// The decoded reply.
    Value(T),
}

impl<T> Reading<T> {
    /// The value, if there is one.
    #[must_use]
    pub const fn value(&self) -> Option<&T> {
        match self {
            Self::Value(value) => Some(value),
            Self::NotSupported | Self::NoReply | Self::Malformed => None,
        }
    }

    /// Whether the headset lists the function this reading belongs to: anything but
    /// [`Reading::NotSupported`].
    #[must_use]
    pub const fn is_supported(&self) -> bool {
        !matches!(self, Self::NotSupported)
    }
}

/// One reply the driver consumed during init, exactly as it came: the raw material for a
/// fixture. Frames are not kept; the payload is what the headset said, command id first.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RawReply {
    /// The frame's data type: `0C` for table one, `0E` for table two.
    pub data_type: DataType,
    /// The payload, command id first.
    pub payload: Bytes,
}

/// What the headset said about itself during init (steps 1 to 4 of Sony's init).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct DeviceInfo {
    /// The protocol version, from the `01` reply. [`Reading::Value`] does not mean the
    /// driver accepts it: see [`SUPPORTED_PROTOCOL_VERSIONS`](super::SUPPORTED_PROTOCOL_VERSIONS).
    pub protocol_version: Reading<u16>,
    /// The `03` reply.
    pub capability_info: Reading<CapabilityInfo>,
    /// The model name, `05 01`.
    pub model: Reading<String>,
    /// The firmware version, `05 02`.
    pub firmware: Reading<String>,
    /// The series and colour, `05 03`.
    pub model_info: Reading<ModelInfo>,
    /// The `GuidanceCategory` bytes, `05 04`, asked only when the protocol version is `0x5000`
    /// or more; [`Reading::NotSupported`] otherwise.
    pub guidance_categories: Reading<Vec<u8>>,
    /// The function list, `07`: **the capability model**. Every feature below is gated on
    /// it.
    pub functions: Reading<Vec<FunctionType>>,
}

impl DeviceInfo {
    /// Whether the headset listed `function`. `false` when the list is missing, too.
    #[must_use]
    pub fn lists(&self, function: FunctionType) -> bool {
        self.functions
            .value()
            .is_some_and(|functions| functions.contains(&function))
    }
}

/// The capabilities the driver decodes, from init step 5. Every other capability reply is
/// in [`Status::raw_replies`] only.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Capabilities {
    /// The noise-cancelling and ambient-sound capability (`61`) of the first of functions
    /// `62`, `61`, `63` the headset lists. It carries the `asmStep` of each ambient mode.
    pub nc_asm: Reading<NcAsmCapability>,
    /// The preset equalizer's capability (`51`) of function `51`, or of `53` when `51` is
    /// not listed: band count, level steps and the presets by name.
    pub eq: Reading<EqCapability>,
    /// The Extra Bass range (`51 02`), function `52`.
    pub ebb: Reading<EbbCapability>,
    /// The auto-power-off ids the headset accepts (`F1 04`), function `F4`.
    pub auto_power_off: Reading<Vec<AutoPowerOffElementId>>,
    /// The voice-guidance capability (`41 01` on table two), function `39`.
    pub voice_guidance: Reading<VoiceGuidanceCapability>,
    /// The pairing capability (`30 01` on table two): how many devices are paired and
    /// connected at most. Function `38`. Seen on a real headset (`31 01 08 02 01`).
    pub pairing: Reading<PairingCapability>,
}

/// One general-setting slot: the headset describes the setting, and then it has a value.
///
/// Touch panel, multipoint and the like are slots like this, not fixed commands
/// (general settings in Sony's app). Which slot is which is read from [`GsCapability::title`].
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct GeneralSetting {
    /// The slot byte, `D1`, `D2` or `D3`: also the function type that listed it.
    pub slot: u8,
    /// What the slot is: its title, description and, for a list, its choices.
    pub capability: Reading<GsCapability>,
    /// Its current value.
    pub value: Reading<GsValue>,
}

/// Everything the headset reports, as of one [`Req::Status`](super::Req::Status).
///
/// A plain struct with one field per feature, and every feature a [`Reading`]: the headset
/// either does not list it, did not answer, answered something unreadable, or said a value.
/// Nothing was invented: where the headset has no reply, the field says so.
///
/// The init half ([`device`](Self::device), [`capabilities`](Self::capabilities),
/// [`general_settings`](Self::general_settings)'s capabilities and
/// [`raw_replies`](Self::raw_replies)) is whatever the connection's init found; the rest is
/// read fresh for each request.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Status {
    /// Model, firmware, protocol version and the function list.
    pub device: DeviceInfo,
    /// The decoded capabilities.
    pub capabilities: Capabilities,
    /// Single battery (function `11`).
    pub battery: Reading<Battery>,
    /// Left and right batteries (function `15`).
    pub battery_left_right: Reading<LeftRightBattery>,
    /// Cradle battery (function `18`).
    pub battery_cradle: Reading<Battery>,
    /// The codec in use (function `13`).
    pub codec: Reading<AudioCodec>,
    /// Whether DSEE is active (function `12`).
    pub upscaling_indicator: Reading<UpscalingIndicator>,
    /// Which buds are connected (function `17`).
    pub connection_status: Reading<LeftRightConnection>,
    /// Noise cancelling and ambient sound (functions `61`, `62`, `63`).
    pub nc_asm: Reading<NcAsmState>,
    /// The preset equalizer (function `51`, or `53`).
    pub eq: Reading<EqState>,
    /// What each equalizer value is, including where Clear Bass is (`5A`/`5B`).
    pub eq_bands: Reading<EqBands>,
    /// The Extra Bass level (function `52`).
    pub ebb: Reading<i8>,
    /// The DSEE setting (function `E2`).
    pub dsee: Reading<DseeSetting>,
    /// Sound quality versus connection quality (function `E1`).
    pub connection_mode: Reading<ConnectionMode>,
    /// Whether voice guidance is on (function `39`, table two).
    pub voice_guidance: Reading<OnOff>,
    /// The voice-guidance language (function `39`, table two).
    pub voice_guidance_language: Reading<MdrLanguage>,
    /// Pause when taken off (function `F3`).
    pub pause_when_taken_off: Reading<OnOff>,
    /// Auto power off (function `F4`).
    pub auto_power_off: Reading<AutoPowerOff>,
    /// Speak-to-Chat on or off (function `F5`).
    pub speak_to_chat: Reading<OnOff>,
    /// The Speak-to-Chat configuration (function `F5`).
    pub speak_to_chat_config: Reading<SpeakToChatConfig>,
    /// What each key is assigned to (function `F6`).
    pub assignable_settings: Reading<Vec<AssignableSettingsPreset>>,
    /// One entry per general-setting slot the headset lists, in the order it lists them.
    pub general_settings: Vec<GeneralSetting>,
    /// Whether the headset is in pairing mode (`32 01`, table two, function `38`). Seen on a
    /// real headset.
    pub pairing_mode: Reading<PairingModeState>,
    /// The paired devices and which holds playback (`36 01`, table two, function `38`). Seen on
    /// a real headset.
    pub paired_devices: Reading<PairedDevices>,
    /// The NC optimizer (function `81`).
    pub nc_optimizer: Reading<NcOptimizer>,
    /// The serial number (`36 06`). Asked only when function `30` is listed; what gates it
    /// is not known, and this is a guess.
    pub serial: Reading<String>,
    /// Every reply init consumed, in the order it came: the version, the function list and
    /// every capability, undecoded. What a shell dumps so a capture can become a fixture.
    pub raw_replies: Vec<RawReply>,
}
