//! The decoded shapes of what a headset reports, and [`Report`], the one enum of them all.

use alloc::string::String;
use alloc::vec::Vec;

use super::enums::{
    AlertMessageType, AsmId, AsmSettingType, AssignableSettingsPreset, AudioCodec,
    AutoPowerOffElementId, BatteryChargingStatus, CommonStatus, ConnectionMode, ConnectionState,
    DseeSetting, EqBandInformationType, EqEbbInquiredType, EqPresetId, FileTransferSupport,
    FunctionType, GsSettingType, GsStringFormat, MdrLanguage, ModeOutTime, ModelSeries,
    NcAsmEffect, NcAsmSettingType, NcDualSingleValue, OnOff, PairingMode, StcSensitivity,
    UpscalingEffectStatus, UpscalingEffectType,
};

/// One battery, as laid out in Sony's app.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Battery {
    /// The level, as the headset says it. Whether that is a percentage is not known; a headset is expected to report `0..=100`.
    pub level: u8,
    /// Whether it is charging.
    pub charging: BatteryChargingStatus,
}

/// The left and right batteries of a headset that has two (`11|13 01`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct LeftRightBattery {
    /// The left bud.
    pub left: Battery,
    /// The right bud.
    pub right: Battery,
}

/// The read-only "is DSEE active" indicator (`15|17`), distinct from the DSEE setting.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct UpscalingIndicator {
    /// Which flavour of DSEE.
    pub kind: UpscalingEffectType,
    /// Whether it is active.
    pub status: UpscalingEffectStatus,
}

/// Which of the two buds is connected (`25|27 01`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct LeftRightConnection {
    /// The left bud.
    pub left: ConnectionState,
    /// The right bud.
    pub right: ConnectionState,
}

/// The noise-cancelling and ambient-sound parameter (`67|69`), in the three shapes
/// Sony's app gives, selected by the byte after the command id.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub enum NcAsmState {
    /// Type `01`: `6x 01 <NcSettingType> <00 off | 01 on>`.
    NoiseCancelling {
        /// `NcSettingType`. Its values are not known; kept raw.
        setting_type: u8,
        /// Whether noise cancelling is on.
        on: OnOff,
    },
    /// Type `02`, the one the XM4 is expected to use: eight bytes, command id included.
    NoiseCancellingAndAmbient {
        /// What the headset is doing about the setting.
        effect: NcAsmEffect,
        /// Copied from the capability; a notify that differs from it is ignored by Sony's
        /// own app.
        nc_setting_type: NcAsmSettingType,
        /// With [`NcAsmSettingType::DualSingleOff`], which noise-cancelling mode.
        nc_value: NcDualSingleValue,
        /// Copied from the capability.
        asm_setting_type: AsmSettingType,
        /// Which ambient mode `asm_level` belongs to.
        asm_id: AsmId,
        /// The ambient level, `1..=asmStep` of that [`AsmId`] in the capability; `0` when
        /// ambient is not what the headset is doing.
        asm_level: u8,
    },
    /// Type `03`: `6x 03 <NcAsmEffect> <AsmSettingType> <AsmId> <level>`.
    Ambient {
        /// What the headset is doing about the setting.
        effect: NcAsmEffect,
        /// Copied from the capability.
        asm_setting_type: AsmSettingType,
        /// Which ambient mode `level` belongs to.
        asm_id: AsmId,
        /// The ambient level.
        level: u8,
    },
}

/// One ambient mode of an [`NcAsmCapability`] and how many levels it has.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct AsmStep {
    /// The mode.
    pub id: AsmId,
    /// Its `asmStep`: the highest level. Device-reported per mode; never assume 20.
    pub step: u8,
}

/// The `61` capability: what the noise-cancelling control looks like.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub enum NcAsmCapability {
    /// Type `01`: `61 01 <NcSettingType>`.
    NoiseCancelling {
        /// `NcSettingType`. Its values are not known; kept raw.
        setting_type: u8,
    },
    /// Type `02`: `61 02 <NcAsmSettingType> <ncStep> <AsmSettingType> <n> {<AsmId> <asmStep>} x n`.
    NoiseCancellingAndAmbient {
        /// How noise cancelling is controlled.
        nc_setting_type: NcAsmSettingType,
        /// `ncStep`. Its meaning is not known.
        nc_step: u8,
        /// How ambient sound is controlled.
        asm_setting_type: AsmSettingType,
        /// The ambient modes and their level counts.
        asm: Vec<AsmStep>,
    },
    /// Type `03`: `61 03 <AsmSettingType> <n> {<AsmId> <asmStep>} x n`.
    Ambient {
        /// How ambient sound is controlled.
        asm_setting_type: AsmSettingType,
        /// The ambient modes and their level counts.
        asm: Vec<AsmStep>,
    },
}

impl NcAsmCapability {
    /// The `asmStep` the headset reported for `id`: the highest ambient level that mode
    /// accepts. `None` when the capability has no such mode (or no ambient part).
    #[must_use]
    pub fn asm_step(&self, id: AsmId) -> Option<u8> {
        match self {
            Self::NoiseCancelling { .. } => None,
            Self::NoiseCancellingAndAmbient { asm, .. } | Self::Ambient { asm, .. } => {
                asm.iter().find(|a| a.id == id).map(|a| a.step)
            }
        }
    }
}

/// The preset equalizer's current setting (`57|59` types `01` and `03`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct EqState {
    /// Whether this is the customizable equalizer (`01`) or the non-customizable one (`03`).
    pub kind: EqEbbInquiredType,
    /// The preset in force. [`EqPresetId::Custom`] means the bands in `values` are.
    pub preset: EqPresetId,
    /// The band levels, `0..levelSteps` of the capability with the middle at
    /// `(levelSteps - 1) / 2`. The count byte on the wire is a count, not a constant.
    pub values: Vec<u8>,
}

/// One preset named in the `51` capability.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct EqPreset {
    /// Its id.
    pub id: EqPresetId,
    /// Its name in the language the host asked for. Empty when the length byte was over 128,
    /// as Sony's app treats it.
    pub name: String,
}

/// The `51` capability of the preset equalizer (types `01` and `03`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct EqCapability {
    /// Which equalizer this describes.
    pub kind: EqEbbInquiredType,
    /// How many bands.
    pub band_count: u8,
    /// How many levels a band has: values run `0..level_steps`.
    pub level_steps: u8,
    /// The presets, in the order the headset lists them.
    pub presets: Vec<EqPreset>,
}

/// The `51 02` capability of Extra Bass: the range of its signed level.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct EbbCapability {
    /// The lowest level.
    pub min: i8,
    /// The highest level.
    pub max: i8,
}

/// One entry of the `5B` extended-info reply: what band `i` of the equalizer is.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct EqBand {
    /// What `value` is.
    pub kind: EqBandInformationType,
    /// The centre frequency for `Hz` and `Khz`; for `SpecificInformation`, `1` is
    /// `CLEAR_BASS`.
    pub value: u16,
}

impl EqBand {
    /// Whether this entry is the Clear Bass band.
    #[must_use]
    pub fn is_clear_bass(&self) -> bool {
        self.kind == EqBandInformationType::SpecificInformation && self.value == 1
    }
}

/// The `5B` reply: one [`EqBand`] per equalizer value, in order.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct EqBands {
    /// Which equalizer the bands belong to.
    pub kind: EqEbbInquiredType,
    /// The bands.
    pub bands: Vec<EqBand>,
}

impl EqBands {
    /// The index of the Clear Bass band, which is where the headset says it is rather than
    /// a fixed position. `None` when no entry declares one.
    #[must_use]
    pub fn clear_bass_index(&self) -> Option<usize> {
        self.bands.iter().position(EqBand::is_clear_bass)
    }
}

/// The auto-power-off setting (`F7|F9 04 01 <active> <timer>`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct AutoPowerOff {
    /// The entry in force: a timer, "when removed from ears", or disabled.
    pub active: AutoPowerOffElementId,
    /// The timer remembered for when it is switched on again.
    pub timer: AutoPowerOffElementId,
}

/// The Speak-to-Chat configuration (`FB|FD 05 00 <sensitivity> <focus on voice> <timeout>`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SpeakToChatConfig {
    /// How easily speech triggers it.
    pub sensitivity: StcSensitivity,
    /// Whether it focuses on the wearer's voice.
    pub focus_on_voice: OnOff,
    /// How long it stays in talking mode after speech stops.
    pub timeout: ModeOutTime,
}

/// A string of a general-setting capability: the format byte, and the text it applies to.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct GsString {
    /// Whether `text` is the literal text or the name of a title.
    pub format: GsStringFormat,
    /// The UTF-8 text.
    pub text: String,
}

impl GsString {
    /// The title this string names, if its format says it names one.
    ///
    /// In Sony's app an `ENUM_NAME` title is one of `GsTitleTitle`, whose constants are
    /// known, but not how the constant is spelled inside the string. This assumes
    /// the string is the constant's own name; a headset that spells it otherwise gives
    /// [`GsTitle::Unknown`] with the text, so nothing is lost.
    #[must_use]
    pub fn title(&self) -> GsTitle {
        if self.format != GsStringFormat::EnumName {
            return GsTitle::Raw(self.text.clone());
        }
        match self.text.as_str() {
            "ASSIGNABLE_KEY_SETTING" => GsTitle::AssignableKeySetting,
            "ASSIGNABLE_KEY_SETTING_FOR_SEPARATED" => GsTitle::AssignableKeySettingForSeparated,
            "ASSIGNABLE_KEY_SETTING_NC" => GsTitle::AssignableKeySettingNc,
            "ASSIGNABLE_KEY_SETTING_NCAMB" => GsTitle::AssignableKeySettingNcamb,
            "ASSIGNABLE_KEY_SETTING_CUSTOM" => GsTitle::AssignableKeySettingCustom,
            "ASSIGNABLE_KEY_SETTING_C" => GsTitle::AssignableKeySettingC,
            "ASSIGNABLE_KEY_SETTING_FOR_SEPARATED_R" => GsTitle::AssignableKeySettingForSeparatedR,
            "VOICE_GUIDANCE_SETTING" => GsTitle::VoiceGuidanceSetting,
            "TOUCH_PANEL_SETTING" => GsTitle::TouchPanelSetting,
            "MULTIPOINT_SETTING" => GsTitle::MultipointSetting,
            "FACETAP_SETTING" => GsTitle::FacetapSetting,
            "SELECTABLE_FACETAP_SETTING" => GsTitle::SelectableFacetapSetting,
            "SIDETONE_SETTING" => GsTitle::SidetoneSetting,
            "TWS_ONE_SIDE_USE_NCASM_SETTING" => GsTitle::TwsOneSideUseNcasmSetting,
            "TAP_SENSITIVITY_SETTING" => GsTitle::TapSensitivitySetting,
            other => GsTitle::Unknown(other.into()),
        }
    }
}

/// What a general-setting slot is called: literal text, or one of Sony's `GsTitleTitle`
/// constants.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub enum GsTitle {
    /// A `RAW_NAME` string: the text to show.
    Raw(String),
    /// `ASSIGNABLE_KEY_SETTING`.
    AssignableKeySetting,
    /// `ASSIGNABLE_KEY_SETTING_FOR_SEPARATED`.
    AssignableKeySettingForSeparated,
    /// `ASSIGNABLE_KEY_SETTING_NC`.
    AssignableKeySettingNc,
    /// `ASSIGNABLE_KEY_SETTING_NCAMB`.
    AssignableKeySettingNcamb,
    /// `ASSIGNABLE_KEY_SETTING_CUSTOM`.
    AssignableKeySettingCustom,
    /// `ASSIGNABLE_KEY_SETTING_C`.
    AssignableKeySettingC,
    /// `ASSIGNABLE_KEY_SETTING_FOR_SEPARATED_R`.
    AssignableKeySettingForSeparatedR,
    /// `VOICE_GUIDANCE_SETTING`.
    VoiceGuidanceSetting,
    /// `TOUCH_PANEL_SETTING`.
    TouchPanelSetting,
    /// `MULTIPOINT_SETTING`.
    MultipointSetting,
    /// `FACETAP_SETTING`.
    FacetapSetting,
    /// `SELECTABLE_FACETAP_SETTING`.
    SelectableFacetapSetting,
    /// `SIDETONE_SETTING`.
    SidetoneSetting,
    /// `TWS_ONE_SIDE_USE_NCASM_SETTING`.
    TwsOneSideUseNcasmSetting,
    /// `TAP_SENSITIVITY_SETTING`.
    TapSensitivitySetting,
    /// An `ENUM_NAME` string that is none of the above.
    Unknown(String),
}

/// One choice of a list-valued general setting: its name and its description.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct GsListItem {
    /// The choice's name.
    pub name: GsString,
    /// What it does.
    pub description: GsString,
}

/// The `D1` capability of one general-setting slot: the headset describes the setting
/// itself, and Sony's app draws it from this.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct GsCapability {
    /// The slot, `D1`, `D2` or `D3`.
    pub slot: u8,
    /// What the setting is called.
    pub title: GsString,
    /// What it does.
    pub description: GsString,
    /// Whether it is a switch or a list.
    pub setting_type: GsSettingType,
    /// For a list, its choices in index order; empty for anything else.
    pub items: Vec<GsListItem>,
}

/// The value of a general-setting slot (`D7|D9 <slot> <GsSettingType> <value>`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub enum GsValue {
    /// A switch.
    Boolean(OnOff),
    /// An index into the capability's list, `0..=63`.
    List(u8),
    /// A setting type this crate does not name, and the byte that followed it.
    Other {
        /// The setting-type byte.
        setting_type: u8,
        /// The value byte.
        value: u8,
    },
}

/// The pressure half of an NC optimizer reading.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub enum AtmosphericPressure {
    /// `00`: not measured.
    Unmeasured,
    /// `07..=0A`: tenths of an atmosphere, so `Atm(7)` is 0.7 atm and `Atm(10)` is 1.0.
    Atm(u8),
    /// Any other byte.
    Unknown(u8),
}

/// The NC optimizer parameter (`87|89 01 <PersonalMeasureType> <PersonalValue>
/// <BarometricMeasureType> <pressure>`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct NcOptimizer {
    /// `PersonalMeasureType`. Its values are not known; kept raw.
    pub personal_measure_type: u8,
    /// The personal measurement. Its scale is not given; kept raw.
    pub personal_value: u8,
    /// `BarometricMeasureType`. Its values are not known; kept raw.
    pub barometric_measure_type: u8,
    /// The pressure.
    pub pressure: AtmosphericPressure,
}

/// The `41 01` voice-guidance capability (table two). Its length is validated exactly.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct VoiceGuidanceCapability {
    /// Whether voice guidance can be switched on and off.
    pub on_off_switch: bool,
    /// Whether its language can be chosen.
    pub language_switch: bool,
    /// The languages on offer; empty when the reply has no list.
    pub languages: Vec<MdrLanguage>,
}

/// The `31 01` pairing capability (table two, function `38`): how many devices the headset
/// pairs with and how many it keeps connected at once.
///
/// A real WH-1000XM4 (firmware 2.7.1) replied `31 01 08 02 01`: eight and two, file transfer
/// impossible.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct PairingCapability {
    /// How many devices can be paired.
    pub max_paired: u8,
    /// How many can be connected at the same time.
    pub max_connected: u8,
    /// Whether file transfer is possible.
    pub file_transfer: FileTransferSupport,
}

/// The pairing mode and whether the headset says it can be in it (`33|35 01 <mode> <status>`).
///
/// Read from Sony's app and seen on one real WH-1000XM4 (`33 01 00 00`: normal, enabled).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct PairingModeState {
    /// Normal, or accepting new pairings.
    pub mode: PairingMode,
    /// Whether the mode is available: **`00` is [`CommonStatus::Enable`]**.
    pub status: CommonStatus,
}

/// Whether a paired device is connected, and in which order it connected: the byte after a
/// paired device's address.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub enum ConnectionOrder {
    /// `00`: paired, not connected.
    NotConnected,
    /// `01` or more: connected, and the n-th to connect.
    Connected(u8),
}

impl ConnectionOrder {
    /// The value for a wire byte.
    #[must_use]
    pub const fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Self::NotConnected,
            order => Self::Connected(order),
        }
    }

    /// The wire byte.
    #[must_use]
    pub const fn raw(self) -> u8 {
        match self {
            Self::NotConnected => 0,
            Self::Connected(order) => order,
        }
    }
}

/// One entry of the paired-device list.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct PairedDevice {
    /// The Bluetooth address as the 17 ASCII characters `XX:XX:XX:XX:XX:XX` the headset sends.
    /// Whatever 17 bytes came are kept, lossily decoded, if they are not that.
    pub address: String,
    /// Whether it is connected.
    pub connection: ConnectionOrder,
    /// Its name, as the headset stores it (UTF-8, lossily decoded).
    pub name: String,
}

/// Which paired device holds the playback right: the one whose connection order is this.
///
/// The final byte of the list is not an index into it. Sony's app marks the device whose
/// connection order (the byte after its address) equals it, and a real WH-1000XM4 sent `01`
/// with three devices listed, of which the only connected one had order 1.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub enum PlaybackHolder {
    /// The device whose [`ConnectionOrder`] is `Connected` with this number. The headset's
    /// byte is kept as it came, so it can name an order no listed device has (`0` among them,
    /// which is how a device that is not connected reads).
    Order(u8),
    /// The reply had no such byte.
    Unknown,
}

/// The paired-device list (`37|39 01`).
///
/// `<n> { <17 ASCII address> <connection order> <nameLen> <name> } x n <playback order>`. Read on
/// one real WH-1000XM4 (firmware 2.7.1), which listed three devices and a final byte of `01`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct PairedDevices {
    /// The devices, in the order the headset lists them.
    pub devices: Vec<PairedDevice>,
    /// Which of them holds the playback right.
    pub playback: PlaybackHolder,
}

impl PairedDevices {
    /// The device that holds the playback right, if the headset named one that is in the list.
    #[must_use]
    pub fn playback_device(&self) -> Option<&PairedDevice> {
        match self.playback {
            PlaybackHolder::Order(order) => self
                .devices
                .iter()
                .find(|device| device.connection == ConnectionOrder::Connected(order)),
            PlaybackHolder::Unknown => None,
        }
    }
}

/// The CONNECT capability-info reply (`03`): the key of Sony's capability cache.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct CapabilityInfo {
    /// `capabilityCounter`.
    pub counter: u8,
    /// `uniqueId`, a UTF-8 string.
    pub unique_id: String,
}

/// The colour and series of the model (`05 03`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ModelInfo {
    /// The series.
    pub series: ModelSeries,
    /// `ModelColor`. Its values are not known; kept raw.
    pub color: u8,
}

/// Something the headset said about itself: the reply to a read, or a notification, decoded.
///
/// Replies and notifications of one member share a layout, so one decoder serves both
/// and a notification reuses the same variant as the reply to the GET.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub enum Report {
    /// `01`: the protocol version, the 16-bit big-endian of bytes 2 and 3.
    ProtocolVersion(u16),
    /// `03`.
    CapabilityInfo(CapabilityInfo),
    /// `05 01`: the model name.
    Model(String),
    /// `05 02`: the firmware version.
    Firmware(String),
    /// `05 03`.
    ModelInfo(ModelInfo),
    /// `05 04`: the `GuidanceCategory` bytes. Their values are not known.
    GuidanceCategories(Vec<u8>),
    /// `07`: the function list, which is the capability model.
    Functions(Vec<FunctionType>),
    /// `61`.
    NcAsmCapability(NcAsmCapability),
    /// `51 01|03`.
    EqCapability(EqCapability),
    /// `51 02`.
    EbbCapability(EbbCapability),
    /// `F1 04`: the auto-power-off ids the headset accepts.
    AutoPowerOffCapability(Vec<AutoPowerOffElementId>),
    /// `41 01` on table two.
    VoiceGuidanceCapability(VoiceGuidanceCapability),
    /// `D1`.
    GeneralSettingCapability(GsCapability),
    /// `11|13 00`: the battery.
    Battery(Battery),
    /// `11|13 01`: the left and right batteries.
    BatteryLeftRight(LeftRightBattery),
    /// `11|13 02`: the cradle's battery.
    BatteryCradle(Battery),
    /// `19|1B`: the codec in use.
    Codec(AudioCodec),
    /// `15|17`.
    UpscalingIndicator(UpscalingIndicator),
    /// `25|27 01`.
    ConnectionStatus(LeftRightConnection),
    /// `67|69`.
    NcAsm(NcAsmState),
    /// `57|59 01|03`.
    Eq(EqState),
    /// `57|59 02`: the signed Extra Bass level.
    Ebb(i8),
    /// `5B`.
    EqBands(EqBands),
    /// `E7|E9 02`.
    Dsee(DseeSetting),
    /// `E7|E9 01`.
    ConnectionMode(ConnectionMode),
    /// `47|49 01 01` on table two.
    VoiceGuidance(OnOff),
    /// `47|49 01 02` on table two.
    VoiceGuidanceLanguage(MdrLanguage),
    /// `F7|F9 03`: whether playback pauses when the headset is taken off.
    PauseWhenTakenOff(OnOff),
    /// `F7|F9 04`.
    AutoPowerOff(AutoPowerOff),
    /// `F7 05` (the reply) or `F9 05 01` (the notification): Speak-to-Chat on or off.
    SpeakToChat(OnOff),
    /// `F9 05 02`: Speak-to-Chat's preview mode on or off.
    SpeakToChatPreview(OnOff),
    /// `FB|FD 05`.
    SpeakToChatConfig(SpeakToChatConfig),
    /// `F7|F9 06`: what each key is assigned to.
    AssignableSettings(Vec<AssignableSettingsPreset>),
    /// `D7|D9`.
    GeneralSetting {
        /// The slot, `D1`, `D2` or `D3`.
        slot: u8,
        /// Its value.
        value: GsValue,
    },
    /// `87|89 01`.
    NcOptimizer(NcOptimizer),
    /// `37 06`: the serial number.
    Serial(String),
    /// `31 01` on table two.
    PairingCapability(PairingCapability),
    /// `33|35 01` on table two.
    PairingMode(PairingModeState),
    /// `37|39 01` on table two.
    PairedDevices(PairedDevices),
    /// `99 01`: the headset raises an alert. Sony's app answers `98 01 <type> <00|01>`;
    /// this driver does not.
    Alert {
        /// Which alert.
        message: AlertMessageType,
        /// Whether the headset wants a yes or no (`01`) or only tells (`00`).
        wants_answer: bool,
    },
}
