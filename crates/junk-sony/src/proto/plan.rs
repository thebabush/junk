//! The order things are asked in: init, and then the read phase.
//!
//! Every list here is Sony's app's (the init order) or a read of the group tables of the
//! app, and each call says which.

use alloc::vec::Vec;

use crate::payload::{FunctionType, MdrLanguage};
use crate::proto::slot::Slot;
use crate::proto::status::{GeneralSetting, Reading, Status};
use crate::proto::txn::Step;

/// Where init is. Each stage is planned when the one before it has been answered, because
/// what comes next depends on what was said: the version decides whether `04 04` is asked,
/// and the function list decides step 5.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum InitStage {
    /// Step 1: `00 00`.
    Protocol,
    /// Step 2: `02 00`.
    CapabilityInfo,
    /// Step 3: `04 01` to `04 03`, and `04 04` from protocol version `0x5000`.
    DeviceInfo,
    /// Step 4: `06 00`.
    Functions,
    /// Step 5: one capability GET per listed function.
    Capabilities,
}

impl InitStage {
    /// The stage after this one, or `None` after the last. Step 6 of Sony's init, the
    /// action log, is deliberately never asked.
    pub(crate) const fn next(self) -> Option<Self> {
        match self {
            Self::Protocol => Some(Self::CapabilityInfo),
            Self::CapabilityInfo => Some(Self::DeviceInfo),
            Self::DeviceInfo => Some(Self::Functions),
            Self::Functions => Some(Self::Capabilities),
            Self::Capabilities => None,
        }
    }
}

/// The protocol version from which Sony's app also asks `04 04`: `0x5000`. Sony's app's
/// version list prints it "5000"; the app's gate is `>= 20480`, which is hexadecimal `5000` (see
/// [`SUPPORTED_PROTOCOL_VERSIONS`](super::SUPPORTED_PROTOCOL_VERSIONS)).
const GUIDANCE_FROM_VERSION: u16 = 0x5000;

/// The steps of `stage`. `session` holds what earlier stages found, and gets the entries
/// the later ones will fill in.
pub(crate) fn init_stage(
    stage: InitStage,
    session: &mut Status,
    language: MdrLanguage,
) -> Vec<Step> {
    match stage {
        InitStage::Protocol => {
            alloc::vec![Step::one(&[0x00, 0x00], 0x01, &[], Slot::ProtocolVersion)]
        }
        InitStage::CapabilityInfo => {
            alloc::vec![Step::one(&[0x02, 0x00], 0x03, &[], Slot::CapabilityInfo)]
        }
        InitStage::DeviceInfo => {
            let mut steps = alloc::vec![
                Step::one(&[0x04, 0x01], 0x05, &[0x01], Slot::Model),
                Step::one(&[0x04, 0x02], 0x05, &[0x02], Slot::Firmware),
                Step::one(&[0x04, 0x03], 0x05, &[0x03], Slot::ModelInfo),
            ];
            // Only a version the headset reported can say it is `0x5000` or more; one that never
            // answered leaves `04 04` unasked and its reading `NotSupported`.
            if session
                .device
                .protocol_version
                .value()
                .is_some_and(|&version| version >= GUIDANCE_FROM_VERSION)
            {
                steps.push(Step::one(
                    &[0x04, 0x04],
                    0x05,
                    &[0x04],
                    Slot::GuidanceCategories,
                ));
            }
            steps
        }
        InitStage::Functions => alloc::vec![Step::one(&[0x06, 0x00], 0x07, &[], Slot::Functions)],
        InitStage::Capabilities => capability_steps(session, language),
    }
}

/// Step 5: the capability GET of each listed function, in the app's order.
///
/// The order is that of the app's init code, where the capability GETs form a table whose
/// reading order is not stated; it was checked against the code: VPT,
/// sound position, the equalizers, Extra Bass, the three NC/ASM, NC optimizer, playback,
/// connection mode, upscaling, the six SYSTEM functions, training mode, the general
/// settings (in the order the function list names them), BLE set-up, pairing and voice
/// guidance. Three functions can be listed and are **not asked**: `14` `BLE_SETUP`, `30`
/// `FW_UPDATE` and `B1` `TRAINING_MODE`. Their capabilities describe BLE set-up, firmware
/// update and training mode, none of which is a setting this driver reads. That is a choice,
/// not something the headset needs; Sony's app does ask them.
fn capability_steps(session: &mut Status, language: MdrLanguage) -> Vec<Step> {
    let lang = language.raw();
    let functions = session
        .device
        .functions
        .value()
        .cloned()
        .unwrap_or_default();
    let listed = |f: FunctionType| functions.contains(&f);

    let mut steps = Vec::new();
    sound_capabilities(&functions, lang, &mut steps);
    system_capabilities(&functions, &mut steps);
    let general: Vec<u8> = functions
        .iter()
        .copied()
        .filter(|f| {
            matches!(
                f,
                FunctionType::GeneralSetting1
                    | FunctionType::GeneralSetting2
                    | FunctionType::GeneralSetting3
            )
        })
        .map(FunctionType::raw)
        .collect();
    for &slot in &general {
        steps.push(Step::one(
            &[0xd0, slot, lang],
            0xd1,
            &[slot],
            Slot::CapGeneral(slot),
        ));
    }
    if listed(FunctionType::PairingDeviceManagementClassicBt) {
        steps.push(Step::two(&[0x30, 0x01], 0x31, &[0x01], Slot::CapPairing));
    }
    if listed(FunctionType::VoiceGuidance) {
        steps.push(Step::two(
            &[0x40, 0x01],
            0x41,
            &[0x01],
            Slot::CapVoiceGuidance,
        ));
        // `46 01 05`: the voice-guidance update method, asked in the same breath as its
        // capability. Its reply layout is not known, so it is kept as raw bytes.
        steps.push(Step::two(
            &[0x46, 0x01, 0x05],
            0x47,
            &[0x01, 0x05],
            Slot::CapRaw,
        ));
    }
    for slot in general {
        session.general_settings.push(GeneralSetting {
            slot,
            capability: Reading::NoReply,
            value: Reading::NotSupported,
        });
    }
    steps
}

/// The capability GETs from VPT to upscaling: sound shaping and the audio group.
fn sound_capabilities(functions: &[FunctionType], lang: u8, steps: &mut Vec<Step>) {
    let listed = |f: FunctionType| functions.contains(&f);
    let nc_preferred = nc_asm_function(functions);
    let eq_preferred = eq_function(functions);
    if listed(FunctionType::Vpt) {
        steps.push(Step::one(&[0x40, 0x01, lang], 0x41, &[0x01], Slot::CapRaw));
    }
    if listed(FunctionType::SoundPosition) {
        steps.push(Step::one(&[0x40, 0x02, lang], 0x41, &[0x02], Slot::CapRaw));
    }
    for (function, inquired) in [
        (FunctionType::PresetEq, 0x01),
        (FunctionType::PresetEqNoncustomizable, 0x03),
    ] {
        if listed(function) {
            let slot = if Some(function) == eq_preferred {
                Slot::CapEq
            } else {
                Slot::CapRaw
            };
            steps.push(Step::one(&[0x50, inquired, lang], 0x51, &[inquired], slot));
        }
    }
    if listed(FunctionType::Ebb) {
        steps.push(Step::one(&[0x50, 0x02, lang], 0x51, &[0x02], Slot::CapEbb));
    }
    for (function, inquired) in [
        (FunctionType::NoiseCancelling, 0x01),
        (FunctionType::NoiseCancellingAndAmbientSoundMode, 0x02),
        (FunctionType::AmbientSoundMode, 0x03),
    ] {
        if listed(function) {
            let slot = if Some(function) == nc_preferred {
                Slot::CapNcAsm
            } else {
                Slot::CapRaw
            };
            steps.push(Step::one(&[0x60, inquired], 0x61, &[inquired], slot));
        }
    }
    for (function, request, reply) in [
        (FunctionType::NcOptimizer, 0x80, 0x81),
        (FunctionType::PlaybackController, 0xa0, 0xa1),
    ] {
        if listed(function) {
            steps.push(Step::one(&[request, 0x01], reply, &[0x01], Slot::CapRaw));
        }
    }
    for (function, inquired) in [
        (FunctionType::ConnectionMode, 0x01),
        (FunctionType::Upscaling, 0x02),
    ] {
        if listed(function) {
            steps.push(Step::one(
                &[0xe0, inquired],
                0xe1,
                &[inquired],
                Slot::CapRaw,
            ));
        }
    }
}

/// The capability GETs of the six SYSTEM functions, `F0 01` to `F0 06`.
fn system_capabilities(functions: &[FunctionType], steps: &mut Vec<Step>) {
    for function in [
        FunctionType::Vibrator,
        FunctionType::PowerSavingMode,
        FunctionType::ControlByWearing,
        FunctionType::AutoPowerOff,
        FunctionType::SmartTalkingMode,
        FunctionType::AssignableSettings,
    ] {
        if functions.contains(&function) {
            let inquired = function.raw() - 0xf0;
            let slot = if function == FunctionType::AutoPowerOff {
                Slot::CapAutoPowerOff
            } else {
                Slot::CapRaw
            };
            steps.push(Step::one(&[0xf0, inquired], 0xf1, &[inquired], slot));
        }
    }
}

/// The function whose NC/ASM capability the status keeps: `62`, else `61`, else `63`.
fn nc_asm_function(functions: &[FunctionType]) -> Option<FunctionType> {
    [
        FunctionType::NoiseCancellingAndAmbientSoundMode,
        FunctionType::NoiseCancelling,
        FunctionType::AmbientSoundMode,
    ]
    .into_iter()
    .find(|f| functions.contains(f))
}

/// The function whose equalizer the status reads: `51`, else `53`.
fn eq_function(functions: &[FunctionType]) -> Option<FunctionType> {
    [
        FunctionType::PresetEq,
        FunctionType::PresetEqNoncustomizable,
    ]
    .into_iter()
    .find(|f| functions.contains(f))
}

/// The read phase: the GET of every listed function that has a readable setting, in the
/// order Sony's app lists the groups (battery and the indicators, then noise
/// cancelling, equalizer, audio, voice guidance, pairing, system, general settings, optimizer,
/// serial).
///
/// `functions` is the function list; the caller has dealt with there not being one.
pub(crate) fn status_steps(functions: &[FunctionType]) -> Vec<Step> {
    let mut steps = Vec::new();
    indicator_reads(functions, &mut steps);
    sound_reads(functions, &mut steps);
    pairing_reads(functions, &mut steps);
    system_reads(functions, &mut steps);
    steps
}

/// Battery, codec, DSEE indicator and bud connection.
fn indicator_reads(functions: &[FunctionType], steps: &mut Vec<Step>) {
    let listed = |f: FunctionType| functions.contains(&f);
    for (function, inquired, slot) in [
        (FunctionType::BatteryLevel, 0x00, Slot::Battery),
        (
            FunctionType::LeftRightBatteryLevel,
            0x01,
            Slot::BatteryLeftRight,
        ),
        (FunctionType::CradleBatteryLevel, 0x02, Slot::BatteryCradle),
    ] {
        if listed(function) {
            steps.push(Step::one(&[0x10, inquired], 0x11, &[inquired], slot));
        }
    }
    if listed(FunctionType::CodecIndicator) {
        steps.push(Step::one(&[0x18, 0x00], 0x19, &[0x00], Slot::Codec));
    }
    if listed(FunctionType::UpscalingIndicator) {
        steps.push(Step::one(&[0x14, 0x00], 0x15, &[0x00], Slot::Upscaling));
    }
    if listed(FunctionType::LeftRightConnectionStatus) {
        steps.push(Step::one(
            &[0x24, 0x01],
            0x25,
            &[0x01],
            Slot::ConnectionStatus,
        ));
    }
}

/// Noise cancelling, the equalizer, Extra Bass, DSEE, connection mode and voice guidance.
fn sound_reads(functions: &[FunctionType], steps: &mut Vec<Step>) {
    let listed = |f: FunctionType| functions.contains(&f);
    if let Some(function) = nc_asm_function(functions) {
        let inquired = function.raw() - 0x60;
        steps.push(Step::one(&[0x66, inquired], 0x67, &[inquired], Slot::NcAsm));
    }
    if let Some(function) = eq_function(functions) {
        let inquired = if function == FunctionType::PresetEq {
            0x01
        } else {
            0x03
        };
        steps.push(Step::one(&[0x56, inquired], 0x57, &[inquired], Slot::Eq));
        steps.push(Step::one(
            &[0x5a, inquired],
            0x5b,
            &[inquired],
            Slot::EqBands,
        ));
    }
    if listed(FunctionType::Ebb) {
        steps.push(Step::one(&[0x56, 0x02], 0x57, &[0x02], Slot::Ebb));
    }
    if listed(FunctionType::Upscaling) {
        steps.push(Step::one(&[0xe6, 0x02], 0xe7, &[0x02], Slot::Dsee));
    }
    if listed(FunctionType::ConnectionMode) {
        steps.push(Step::one(
            &[0xe6, 0x01],
            0xe7,
            &[0x01],
            Slot::ConnectionMode,
        ));
    }
    if listed(FunctionType::VoiceGuidance) {
        steps.push(Step::two(
            &[0x46, 0x01, 0x01],
            0x47,
            &[0x01, 0x01],
            Slot::VoiceGuidance,
        ));
        steps.push(Step::two(
            &[0x46, 0x01, 0x02],
            0x47,
            &[0x01, 0x02],
            Slot::VoiceGuidanceLanguage,
        ));
    }
}

/// The pairing mode and the paired-device list (`32 01`, `36 01`, table two), when function
/// `38` is listed. Reads only: the commands that connect, disconnect or unpair a device
/// (`3C`) or enter pairing mode (`34`) are not sent by anything here.
fn pairing_reads(functions: &[FunctionType], steps: &mut Vec<Step>) {
    if functions.contains(&FunctionType::PairingDeviceManagementClassicBt) {
        steps.push(Step::two(&[0x32, 0x01], 0x33, &[0x01], Slot::PairingMode));
        steps.push(Step::two(&[0x36, 0x01], 0x37, &[0x01], Slot::PairedDevices));
    }
}

/// The SYSTEM group, the general settings, the optimizer and the serial number.
fn system_reads(functions: &[FunctionType], steps: &mut Vec<Step>) {
    let listed = |f: FunctionType| functions.contains(&f);
    for (function, inquired, slot) in [
        (
            FunctionType::ControlByWearing,
            0x03,
            Slot::PauseWhenTakenOff,
        ),
        (FunctionType::AutoPowerOff, 0x04, Slot::AutoPowerOff),
        (FunctionType::SmartTalkingMode, 0x05, Slot::SpeakToChat),
    ] {
        if listed(function) {
            steps.push(Step::one(&[0xf6, inquired], 0xf7, &[inquired], slot));
        }
    }
    if listed(FunctionType::SmartTalkingMode) {
        steps.push(Step::one(
            &[0xfa, 0x05],
            0xfb,
            &[0x05],
            Slot::SpeakToChatConfig,
        ));
    }
    if listed(FunctionType::AssignableSettings) {
        steps.push(Step::one(
            &[0xf6, 0x06],
            0xf7,
            &[0x06],
            Slot::AssignableSettings,
        ));
    }
    for function in [
        FunctionType::GeneralSetting1,
        FunctionType::GeneralSetting2,
        FunctionType::GeneralSetting3,
    ] {
        if listed(function) {
            let slot = function.raw();
            steps.push(Step::one(&[0xd6, slot], 0xd7, &[slot], Slot::General(slot)));
        }
    }
    if listed(FunctionType::NcOptimizer) {
        steps.push(Step::one(&[0x86, 0x01], 0x87, &[0x01], Slot::NcOptimizer));
    }
    // `36 06` belongs to the UPDT group, whose function type is `30`. The app sends it and
    // what gates it is not known; this gates it on `30`.
    if listed(FunctionType::FwUpdate) {
        steps.push(Step::one(&[0x36, 0x06], 0x37, &[0x06], Slot::Serial));
    }
}
