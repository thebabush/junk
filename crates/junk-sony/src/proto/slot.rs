//! Where a step's reply goes: [`Slot`], and [`Status::record`], which puts it there.

use junk_core::ReqId;

use crate::payload::Report;
use crate::proto::status::{GeneralSetting, Reading, Status};

/// The field of [`Status`] (or the request) a step's reply belongs to.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    // Init steps 1 to 4.
    ProtocolVersion,
    CapabilityInfo,
    Model,
    Firmware,
    ModelInfo,
    GuidanceCategories,
    Functions,
    // Init step 5: capabilities.
    CapNcAsm,
    CapEq,
    CapEbb,
    CapAutoPowerOff,
    CapVoiceGuidance,
    CapPairing,
    CapGeneral(u8),
    /// A capability that is kept as raw bytes only.
    CapRaw,
    // The read phase.
    Battery,
    BatteryLeftRight,
    BatteryCradle,
    Codec,
    Upscaling,
    ConnectionStatus,
    NcAsm,
    Eq,
    EqBands,
    Ebb,
    Dsee,
    ConnectionMode,
    VoiceGuidance,
    VoiceGuidanceLanguage,
    PairingMode,
    PairedDevices,
    PauseWhenTakenOff,
    AutoPowerOff,
    SpeakToChat,
    SpeakToChatConfig,
    AssignableSettings,
    General(u8),
    NcOptimizer,
    Serial,
    /// A `Req::Raw`: the next data reply, undecoded.
    Raw(ReqId),
}

impl Slot {
    /// Whether the step belongs to init rather than to a status read.
    pub(crate) const fn is_init(self) -> bool {
        matches!(
            self,
            Self::ProtocolVersion
                | Self::CapabilityInfo
                | Self::Model
                | Self::Firmware
                | Self::ModelInfo
                | Self::GuidanceCategories
                | Self::Functions
                | Self::CapNcAsm
                | Self::CapEq
                | Self::CapEbb
                | Self::CapAutoPowerOff
                | Self::CapVoiceGuidance
                | Self::CapPairing
                | Self::CapGeneral(_)
                | Self::CapRaw
        )
    }

    /// Whether the reply is only kept, never decoded.
    pub(crate) const fn is_raw_only(self) -> bool {
        matches!(self, Self::CapRaw | Self::Raw(_))
    }
}

/// What came of a step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Nothing arrived in time.
    NoReply,
    /// Something arrived that is not what the slot reads.
    Malformed,
    /// The decoded reply.
    Report(Report),
}

/// The reading for `outcome`, taking the value out of the report with `f`.
fn pick<T>(outcome: Outcome, f: impl FnOnce(Report) -> Option<T>) -> Reading<T> {
    match outcome {
        Outcome::NoReply => Reading::NoReply,
        Outcome::Malformed => Reading::Malformed,
        Outcome::Report(report) => f(report).map_or(Reading::Malformed, Reading::Value),
    }
}

impl Status {
    /// Puts `outcome` in the field `slot` names.
    pub(crate) fn record(&mut self, slot: Slot, outcome: Outcome) {
        if slot.is_init() {
            self.record_init(slot, outcome);
        } else {
            self.record_read(slot, outcome);
        }
    }

    /// The init half: device info, capabilities and the general settings' capabilities.
    fn record_init(&mut self, slot: Slot, outcome: Outcome) {
        match slot {
            Slot::ProtocolVersion => {
                self.device.protocol_version = pick(outcome, |r| match r {
                    Report::ProtocolVersion(v) => Some(v),
                    _ => None,
                });
            }
            Slot::CapabilityInfo => {
                self.device.capability_info = pick(outcome, |r| match r {
                    Report::CapabilityInfo(v) => Some(v),
                    _ => None,
                });
            }
            Slot::Model => {
                self.device.model = pick(outcome, |r| match r {
                    Report::Model(v) => Some(v),
                    _ => None,
                });
            }
            Slot::Firmware => {
                self.device.firmware = pick(outcome, |r| match r {
                    Report::Firmware(v) => Some(v),
                    _ => None,
                });
            }
            Slot::ModelInfo => {
                self.device.model_info = pick(outcome, |r| match r {
                    Report::ModelInfo(v) => Some(v),
                    _ => None,
                });
            }
            Slot::GuidanceCategories => {
                self.device.guidance_categories = pick(outcome, |r| match r {
                    Report::GuidanceCategories(v) => Some(v),
                    _ => None,
                });
            }
            Slot::Functions => {
                self.device.functions = pick(outcome, |r| match r {
                    Report::Functions(v) => Some(v),
                    _ => None,
                });
            }
            Slot::CapNcAsm => {
                self.capabilities.nc_asm = pick(outcome, |r| match r {
                    Report::NcAsmCapability(v) => Some(v),
                    _ => None,
                });
            }
            Slot::CapEq => {
                self.capabilities.eq = pick(outcome, |r| match r {
                    Report::EqCapability(v) => Some(v),
                    _ => None,
                });
            }
            Slot::CapEbb => {
                self.capabilities.ebb = pick(outcome, |r| match r {
                    Report::EbbCapability(v) => Some(v),
                    _ => None,
                });
            }
            Slot::CapAutoPowerOff => {
                self.capabilities.auto_power_off = pick(outcome, |r| match r {
                    Report::AutoPowerOffCapability(v) => Some(v),
                    _ => None,
                });
            }
            Slot::CapVoiceGuidance => {
                self.capabilities.voice_guidance = pick(outcome, |r| match r {
                    Report::VoiceGuidanceCapability(v) => Some(v),
                    _ => None,
                });
            }
            Slot::CapPairing => {
                self.capabilities.pairing = pick(outcome, |r| match r {
                    Report::PairingCapability(v) => Some(v),
                    _ => None,
                });
            }
            Slot::CapGeneral(slot) => {
                let capability = pick(outcome, |r| match r {
                    Report::GeneralSettingCapability(v) if v.slot == slot => Some(v),
                    _ => None,
                });
                self.general(slot).capability = capability;
            }
            // Kept as raw bytes only, or not an init slot: `record` routes those elsewhere.
            _ => {}
        }
    }

    /// The read half: indicators, noise cancelling, equalizer, audio, voice guidance and pairing.
    fn record_read(&mut self, slot: Slot, outcome: Outcome) {
        match slot {
            Slot::Battery => {
                self.battery = pick(outcome, |r| match r {
                    Report::Battery(v) => Some(v),
                    _ => None,
                });
            }
            Slot::BatteryLeftRight => {
                self.battery_left_right = pick(outcome, |r| match r {
                    Report::BatteryLeftRight(v) => Some(v),
                    _ => None,
                });
            }
            Slot::BatteryCradle => {
                self.battery_cradle = pick(outcome, |r| match r {
                    Report::BatteryCradle(v) => Some(v),
                    _ => None,
                });
            }
            Slot::Codec => {
                self.codec = pick(outcome, |r| match r {
                    Report::Codec(v) => Some(v),
                    _ => None,
                });
            }
            Slot::Upscaling => {
                self.upscaling_indicator = pick(outcome, |r| match r {
                    Report::UpscalingIndicator(v) => Some(v),
                    _ => None,
                });
            }
            Slot::ConnectionStatus => {
                self.connection_status = pick(outcome, |r| match r {
                    Report::ConnectionStatus(v) => Some(v),
                    _ => None,
                });
            }
            Slot::NcAsm => {
                self.nc_asm = pick(outcome, |r| match r {
                    Report::NcAsm(v) => Some(v),
                    _ => None,
                });
            }
            Slot::Eq => {
                self.eq = pick(outcome, |r| match r {
                    Report::Eq(v) => Some(v),
                    _ => None,
                });
            }
            Slot::EqBands => {
                self.eq_bands = pick(outcome, |r| match r {
                    Report::EqBands(v) => Some(v),
                    _ => None,
                });
            }
            Slot::Ebb => {
                self.ebb = pick(outcome, |r| match r {
                    Report::Ebb(v) => Some(v),
                    _ => None,
                });
            }
            Slot::Dsee => {
                self.dsee = pick(outcome, |r| match r {
                    Report::Dsee(v) => Some(v),
                    _ => None,
                });
            }
            Slot::ConnectionMode => {
                self.connection_mode = pick(outcome, |r| match r {
                    Report::ConnectionMode(v) => Some(v),
                    _ => None,
                });
            }
            Slot::VoiceGuidance => {
                self.voice_guidance = pick(outcome, |r| match r {
                    Report::VoiceGuidance(v) => Some(v),
                    _ => None,
                });
            }
            Slot::VoiceGuidanceLanguage => {
                self.voice_guidance_language = pick(outcome, |r| match r {
                    Report::VoiceGuidanceLanguage(v) => Some(v),
                    _ => None,
                });
            }
            Slot::PairingMode => {
                self.pairing_mode = pick(outcome, |r| match r {
                    Report::PairingMode(v) => Some(v),
                    _ => None,
                });
            }
            Slot::PairedDevices => {
                self.paired_devices = pick(outcome, |r| match r {
                    Report::PairedDevices(v) => Some(v),
                    _ => None,
                });
            }
            _ => self.record_system(slot, outcome),
        }
    }

    /// The read half again: the SYSTEM group, the general settings, the optimizer and the
    /// serial number.
    fn record_system(&mut self, slot: Slot, outcome: Outcome) {
        match slot {
            Slot::PauseWhenTakenOff => {
                self.pause_when_taken_off = pick(outcome, |r| match r {
                    Report::PauseWhenTakenOff(v) => Some(v),
                    _ => None,
                });
            }
            Slot::AutoPowerOff => {
                self.auto_power_off = pick(outcome, |r| match r {
                    Report::AutoPowerOff(v) => Some(v),
                    _ => None,
                });
            }
            Slot::SpeakToChat => {
                self.speak_to_chat = pick(outcome, |r| match r {
                    Report::SpeakToChat(v) => Some(v),
                    _ => None,
                });
            }
            Slot::SpeakToChatConfig => {
                self.speak_to_chat_config = pick(outcome, |r| match r {
                    Report::SpeakToChatConfig(v) => Some(v),
                    _ => None,
                });
            }
            Slot::AssignableSettings => {
                self.assignable_settings = pick(outcome, |r| match r {
                    Report::AssignableSettings(v) => Some(v),
                    _ => None,
                });
            }
            Slot::NcOptimizer => {
                self.nc_optimizer = pick(outcome, |r| match r {
                    Report::NcOptimizer(v) => Some(v),
                    _ => None,
                });
            }
            Slot::Serial => {
                self.serial = pick(outcome, |r| match r {
                    Report::Serial(v) => Some(v),
                    _ => None,
                });
            }
            Slot::General(slot) => {
                let value = pick(outcome, |r| match r {
                    Report::GeneralSetting { slot: got, value } if got == slot => Some(value),
                    _ => None,
                });
                self.general(slot).value = value;
            }
            // An init slot, or a raw request: `record` routes those elsewhere.
            _ => {}
        }
    }

    /// The entry for general-setting `slot`, made if there is none yet.
    pub(crate) fn general(&mut self, slot: u8) -> &mut GeneralSetting {
        let index = self
            .general_settings
            .iter()
            .position(|g| g.slot == slot)
            .unwrap_or_else(|| {
                self.general_settings.push(GeneralSetting {
                    slot,
                    capability: Reading::NotSupported,
                    value: Reading::NotSupported,
                });
                self.general_settings.len() - 1
            });
        &mut self.general_settings[index]
    }

    /// This status with every read-phase feature `NoReply`, for a function list that never
    /// came: nothing was listed, so nothing was asked, and "not supported" would be a lie.
    pub(crate) fn all_no_reply(self) -> Self {
        Self {
            battery: Reading::NoReply,
            battery_left_right: Reading::NoReply,
            battery_cradle: Reading::NoReply,
            codec: Reading::NoReply,
            upscaling_indicator: Reading::NoReply,
            connection_status: Reading::NoReply,
            nc_asm: Reading::NoReply,
            eq: Reading::NoReply,
            eq_bands: Reading::NoReply,
            ebb: Reading::NoReply,
            dsee: Reading::NoReply,
            connection_mode: Reading::NoReply,
            voice_guidance: Reading::NoReply,
            voice_guidance_language: Reading::NoReply,
            pairing_mode: Reading::NoReply,
            paired_devices: Reading::NoReply,
            pause_when_taken_off: Reading::NoReply,
            auto_power_off: Reading::NoReply,
            speak_to_chat: Reading::NoReply,
            speak_to_chat_config: Reading::NoReply,
            assignable_settings: Reading::NoReply,
            nc_optimizer: Reading::NoReply,
            serial: Reading::NoReply,
            ..self
        }
    }
}
