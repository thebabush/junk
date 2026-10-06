//! [`decode_report`]: payload bytes to a [`Report`], layout by layout.

use alloc::string::String;
use alloc::vec::Vec;

use junk_core::ProtoError;

use super::enums::{
    AlertMessageType, AsmId, AsmSettingType, AssignableSettingsPreset, AudioCodec,
    AutoPowerOffElementId, BatteryChargingStatus, CommonStatus, ConnectionMode, ConnectionState,
    DseeSetting, EqBandInformationType, EqEbbInquiredType, EqPresetId, FileTransferSupport,
    FunctionType, GsSettingType, GsStringFormat, MdrLanguage, ModeOutTime, ModelSeries,
    NcAsmEffect, NcAsmSettingType, NcDualSingleValue, OnOff, PairingMode, StcSensitivity,
    UpscalingEffectStatus, UpscalingEffectType,
};
use super::types::{
    AsmStep, AtmosphericPressure, AutoPowerOff, Battery, CapabilityInfo, ConnectionOrder,
    EbbCapability, EqBand, EqBands, EqCapability, EqPreset, EqState, GsCapability, GsListItem,
    GsString, GsValue, LeftRightBattery, LeftRightConnection, ModelInfo, NcAsmCapability,
    NcAsmState, NcOptimizer, PairedDevice, PairedDevices, PairingCapability, PairingModeState,
    PlaybackHolder, Report, SpeakToChatConfig, UpscalingIndicator, VoiceGuidanceCapability,
};
use super::{Reader, Table};

/// Decodes the payload of one frame that `table` carries.
///
/// `Ok(None)` is a command this crate has no decoder for, or an inquired type it does not
/// know: not an error, but not something it can say anything about. `Err` is a command it
/// knows whose payload is shorter than its layout, or, for voice guidance, not the exact
/// length the layout has. Bytes past a layout are ignored, as Sony's own app ignores them.
///
/// # Errors
///
/// [`ProtoError::Malformed`] with a message naming the layout that was too short.
pub fn decode_report(table: Table, payload: &[u8]) -> Result<Option<Report>, ProtoError> {
    let Some((&id, rest)) = payload.split_first() else {
        return Err(ProtoError::Malformed("empty payload"));
    };
    match (table, id) {
        (Table::One, 0x01) => protocol_info(rest),
        (Table::One, 0x03) => capability_info(rest),
        (Table::One, 0x05) => device_info(rest),
        (Table::One, 0x07) => support_function(rest),
        (Table::One, 0x11 | 0x13) => battery(rest),
        (Table::One, 0x15 | 0x17) => upscaling_indicator(rest),
        (Table::One, 0x19 | 0x1b) => codec(rest),
        (Table::One, 0x25 | 0x27) => connection_status(rest),
        (Table::One, 0x37) => serial(rest),
        (Table::One, 0x51) => eq_capability(rest),
        (Table::One, 0x57 | 0x59) => eq_param(rest),
        (Table::One, 0x5b) => eq_bands(rest),
        (Table::One, 0x61) => nc_asm_capability(rest),
        (Table::One, 0x67 | 0x69) => nc_asm_param(rest),
        (Table::One, 0x87 | 0x89) => nc_optimizer(rest),
        (Table::One, 0x99) => alert(rest),
        (Table::One, 0xd1) => general_setting_capability(rest),
        (Table::One, 0xd7 | 0xd9) => general_setting_param(rest),
        (Table::One, 0xe7 | 0xe9) => audio_param(rest),
        (Table::One, 0xf1) => system_capability(rest),
        (Table::One, 0xf7 | 0xf9) => system_param(id, rest),
        (Table::One, 0xfb | 0xfd) => speak_to_chat_config(rest),
        (Table::Two, 0x31) => pairing_capability(rest),
        (Table::Two, 0x33 | 0x35) => pairing_mode(rest),
        (Table::Two, 0x37 | 0x39) => paired_devices(rest),
        (Table::Two, 0x41) => voice_guidance_capability(rest),
        (Table::Two, 0x47 | 0x49) => voice_guidance_param(rest),
        _ => Ok(None),
    }
}

/// `01 <inq> <ver_hi> <ver_lo>`: the version is the big-endian 16 bits of bytes 2 and 3.
fn protocol_info(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    let mut r = Reader::new(rest, "protocol info reply is too short");
    let _inquired = r.u8()?;
    let version = u16::from_be_bytes([r.u8()?, r.u8()?]);
    Ok(Some(Report::ProtocolVersion(version)))
}

/// `03 <inq> <capabilityCounter> <len> <uniqueId>`.
fn capability_info(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    let mut r = Reader::new(rest, "capability info reply is too short");
    let _inquired = r.u8()?;
    let counter = r.u8()?;
    let unique_id = r.text()?;
    Ok(Some(Report::CapabilityInfo(CapabilityInfo {
        counter,
        unique_id,
    })))
}

/// `05 01 <len> <model>`, `05 02 <len> <firmware>`, `05 03 <series> <color>`,
/// `05 04 <n> <category x n>`.
fn device_info(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    let mut r = Reader::new(rest, "device info reply is too short");
    match r.u8()? {
        0x01 => Ok(Some(Report::Model(r.text()?))),
        0x02 => Ok(Some(Report::Firmware(r.text()?))),
        0x03 => {
            let series = ModelSeries::from_raw(r.u8()?);
            let color = r.u8()?;
            Ok(Some(Report::ModelInfo(ModelInfo { series, color })))
        }
        0x04 => Ok(Some(Report::GuidanceCategories(r.counted()?))),
        _ => Ok(None),
    }
}

/// `07 <inq> <n> <FunctionType x n>`.
fn support_function(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    let mut r = Reader::new(rest, "support function reply is too short");
    let _inquired = r.u8()?;
    let functions = r
        .counted()?
        .into_iter()
        .map(FunctionType::from_raw)
        .collect();
    Ok(Some(Report::Functions(functions)))
}

fn one_battery(r: &mut Reader<'_>) -> Result<Battery, ProtoError> {
    let level = r.u8()?;
    let charging = BatteryChargingStatus::from_raw(r.u8()?);
    Ok(Battery { level, charging })
}

/// `11|13 00 <lvl> <chg>`; `11|13 01 <L lvl> <L chg> <R lvl> <R chg>`; `11|13 02 <lvl> <chg>`.
/// An unknown type is ignored, as in the app.
fn battery(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    let mut r = Reader::new(rest, "battery reply is too short");
    match r.u8()? {
        0x00 => Ok(Some(Report::Battery(one_battery(&mut r)?))),
        0x01 => {
            let left = one_battery(&mut r)?;
            let right = one_battery(&mut r)?;
            Ok(Some(Report::BatteryLeftRight(LeftRightBattery {
                left,
                right,
            })))
        }
        0x02 => Ok(Some(Report::BatteryCradle(one_battery(&mut r)?))),
        _ => Ok(None),
    }
}

/// `15|17 <inq> <UpscalingEffectType> <UpscalingEffectStatus>`.
fn upscaling_indicator(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    let mut r = Reader::new(rest, "upscaling indicator reply is too short");
    let _inquired = r.u8()?;
    let kind = UpscalingEffectType::from_raw(r.u8()?);
    let status = UpscalingEffectStatus::from_raw(r.u8()?);
    Ok(Some(Report::UpscalingIndicator(UpscalingIndicator {
        kind,
        status,
    })))
}

/// `19|1B <inq> <AudioCodec>`.
fn codec(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    let mut r = Reader::new(rest, "codec reply is too short");
    let _inquired = r.u8()?;
    Ok(Some(Report::Codec(AudioCodec::from_raw(r.u8()?))))
}

/// `25|27 01 <L> <R>`.
fn connection_status(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    let mut r = Reader::new(rest, "connection status reply is too short");
    if r.u8()? != 0x01 {
        return Ok(None);
    }
    let left = ConnectionState::from_raw(r.u8()?);
    let right = ConnectionState::from_raw(r.u8()?);
    Ok(Some(Report::ConnectionStatus(LeftRightConnection {
        left,
        right,
    })))
}

/// `37 06 <len> <ascii>`.
fn serial(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    let mut r = Reader::new(rest, "serial number reply is too short");
    if r.u8()? != 0x06 {
        return Ok(None);
    }
    Ok(Some(Report::Serial(r.text()?)))
}

/// `51 <01|03> <bandCount> <levelSteps> <nPresets> {<EqPresetId> <nameLen> <name>} x n`
/// and `51 02 <min s8> <max s8>`. A name length over 128 is read as 0, as the app does.
fn eq_capability(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    let mut r = Reader::new(rest, "equalizer capability reply is too short");
    let kind = r.u8()?;
    match kind {
        0x01 | 0x03 => {
            let band_count = r.u8()?;
            let level_steps = r.u8()?;
            let n = r.u8()?;
            let mut presets = Vec::new();
            for _ in 0..n {
                let id = EqPresetId::from_raw(r.u8()?);
                let len = usize::from(r.u8()?);
                let len = if len > 128 { 0 } else { len };
                let name = String::from_utf8_lossy(r.take(len)?).into_owned();
                presets.push(EqPreset { id, name });
            }
            Ok(Some(Report::EqCapability(EqCapability {
                kind: EqEbbInquiredType::from_raw(kind),
                band_count,
                level_steps,
                presets,
            })))
        }
        0x02 => {
            let min = i8::from_ne_bytes([r.u8()?]);
            let max = i8::from_ne_bytes([r.u8()?]);
            Ok(Some(Report::EbbCapability(EbbCapability { min, max })))
        }
        _ => Ok(None),
    }
}

/// `57|59 <01|03> <EqPresetId> <n> <v1> .. <vn>` and `57|59 02 <level s8>`.
fn eq_param(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    let mut r = Reader::new(rest, "equalizer reply is too short");
    let kind = r.u8()?;
    match kind {
        0x01 | 0x03 => {
            let preset = EqPresetId::from_raw(r.u8()?);
            let values = r.counted()?;
            Ok(Some(Report::Eq(EqState {
                kind: EqEbbInquiredType::from_raw(kind),
                preset,
                values,
            })))
        }
        0x02 => Ok(Some(Report::Ebb(i8::from_ne_bytes([r.u8()?])))),
        _ => Ok(None),
    }
}

/// `5B <type> <n> {<EqBandInformationType> <value u16 BE>} x n`.
fn eq_bands(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    let mut r = Reader::new(rest, "equalizer band info reply is too short");
    let kind = EqEbbInquiredType::from_raw(r.u8()?);
    let n = r.u8()?;
    let mut bands = Vec::new();
    for _ in 0..n {
        let band_kind = EqBandInformationType::from_raw(r.u8()?);
        let value = u16::from_be_bytes([r.u8()?, r.u8()?]);
        bands.push(EqBand {
            kind: band_kind,
            value,
        });
    }
    Ok(Some(Report::EqBands(EqBands { kind, bands })))
}

fn asm_steps(r: &mut Reader<'_>) -> Result<Vec<AsmStep>, ProtoError> {
    let n = r.u8()?;
    let mut steps = Vec::new();
    for _ in 0..n {
        let id = AsmId::from_raw(r.u8()?);
        let step = r.u8()?;
        steps.push(AsmStep { id, step });
    }
    Ok(steps)
}

/// `61 01 <NcSettingType>`; `61 02 <NcAsmSettingType> <ncStep> <AsmSettingType> <n> {<AsmId>
/// <asmStep>} x n`; `61 03 <AsmSettingType> <n> {<AsmId> <asmStep>} x n`.
fn nc_asm_capability(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    let mut r = Reader::new(rest, "noise cancelling capability reply is too short");
    let capability = match r.u8()? {
        0x01 => NcAsmCapability::NoiseCancelling {
            setting_type: r.u8()?,
        },
        0x02 => {
            let nc_setting_type = NcAsmSettingType::from_raw(r.u8()?);
            let nc_step = r.u8()?;
            let asm_setting_type = AsmSettingType::from_raw(r.u8()?);
            let asm = asm_steps(&mut r)?;
            NcAsmCapability::NoiseCancellingAndAmbient {
                nc_setting_type,
                nc_step,
                asm_setting_type,
                asm,
            }
        }
        0x03 => {
            let asm_setting_type = AsmSettingType::from_raw(r.u8()?);
            let asm = asm_steps(&mut r)?;
            NcAsmCapability::Ambient {
                asm_setting_type,
                asm,
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(Report::NcAsmCapability(capability)))
}

/// `67|69 02 <NcAsmEffect> <NcAsmSettingType> <ncValue> <AsmSettingType> <AsmId> <level>`
/// (eight bytes with the id; extra bytes ignored), `6x 01 <NcSettingType> <00|01>`,
/// `6x 03 <NcAsmEffect> <AsmSettingType> <AsmId> <level>`.
fn nc_asm_param(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    let mut r = Reader::new(rest, "noise cancelling reply is too short");
    let state = match r.u8()? {
        0x01 => NcAsmState::NoiseCancelling {
            setting_type: r.u8()?,
            on: OnOff::from_raw(r.u8()?),
        },
        0x02 => NcAsmState::NoiseCancellingAndAmbient {
            effect: NcAsmEffect::from_raw(r.u8()?),
            nc_setting_type: NcAsmSettingType::from_raw(r.u8()?),
            nc_value: NcDualSingleValue::from_raw(r.u8()?),
            asm_setting_type: AsmSettingType::from_raw(r.u8()?),
            asm_id: AsmId::from_raw(r.u8()?),
            asm_level: r.u8()?,
        },
        0x03 => NcAsmState::Ambient {
            effect: NcAsmEffect::from_raw(r.u8()?),
            asm_setting_type: AsmSettingType::from_raw(r.u8()?),
            asm_id: AsmId::from_raw(r.u8()?),
            level: r.u8()?,
        },
        _ => return Ok(None),
    };
    Ok(Some(Report::NcAsm(state)))
}

/// `E7|E9 02 <UpscalingSettingType> <00 off|01 auto>` is DSEE;
/// `E7|E9 01 <ConnectionModeSettingType> <00|01>` is the connection-quality priority.
fn audio_param(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    let mut r = Reader::new(rest, "audio setting reply is too short");
    let kind = r.u8()?;
    if !matches!(kind, 0x01 | 0x02) {
        return Ok(None);
    }
    let _setting_type = r.u8()?;
    let value = r.u8()?;
    if kind == 0x02 {
        Ok(Some(Report::Dsee(DseeSetting::from_raw(value))))
    } else {
        Ok(Some(Report::ConnectionMode(ConnectionMode::from_raw(
            value,
        ))))
    }
}

/// `F1 04 <n> <id x n>`: the auto-power-off ids the headset accepts. The other system
/// capabilities (`F1 05`, `F1 06`, ...) are kept as raw bytes only.
fn system_capability(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    let mut r = Reader::new(rest, "system capability reply is too short");
    if r.u8()? != 0x04 {
        return Ok(None);
    }
    let ids = r
        .counted()?
        .into_iter()
        .map(AutoPowerOffElementId::from_raw)
        .collect();
    Ok(Some(Report::AutoPowerOffCapability(ids)))
}

/// `F7|F9 03 00 <v>`; `F7|F9 04 01 <active> <timer>`; Speak-to-Chat: `F7 05 00 <v>` as a reply
/// but `F9 05 <01|02> <v>` as a notification; `F7|F9 06 <n> <preset x n>`.
fn system_param(id: u8, rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    let mut r = Reader::new(rest, "system setting reply is too short");
    match r.u8()? {
        0x03 => {
            let _setting_type = r.u8()?;
            Ok(Some(Report::PauseWhenTakenOff(OnOff::from_raw(r.u8()?))))
        }
        0x04 => {
            let _setting_type = r.u8()?;
            let active = AutoPowerOffElementId::from_raw(r.u8()?);
            let timer = AutoPowerOffElementId::from_raw(r.u8()?);
            Ok(Some(Report::AutoPowerOff(AutoPowerOff { active, timer })))
        }
        0x05 => {
            let setting = r.u8()?;
            let value = OnOff::from_raw(r.u8()?);
            // The RET's byte 2 is the setting type (always 00); the NTFY's is which
            // setting changed. They share the slot, not the meaning.
            match (id, setting) {
                (0xf7, _) | (_, 0x01) => Ok(Some(Report::SpeakToChat(value))),
                (_, 0x02) => Ok(Some(Report::SpeakToChatPreview(value))),
                _ => Ok(None),
            }
        }
        0x06 => {
            let presets = r
                .counted()?
                .into_iter()
                .map(AssignableSettingsPreset::from_raw)
                .collect();
            Ok(Some(Report::AssignableSettings(presets)))
        }
        _ => Ok(None),
    }
}

/// `FB|FD 05 00 <sensitivity> <focus on voice> <timeout>`.
fn speak_to_chat_config(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    let mut r = Reader::new(rest, "speak-to-chat config reply is too short");
    if r.u8()? != 0x05 {
        return Ok(None);
    }
    let _setting_type = r.u8()?;
    let sensitivity = StcSensitivity::from_raw(r.u8()?);
    let focus_on_voice = OnOff::from_raw(r.u8()?);
    let timeout = ModeOutTime::from_raw(r.u8()?);
    Ok(Some(Report::SpeakToChatConfig(SpeakToChatConfig {
        sensitivity,
        focus_on_voice,
        timeout,
    })))
}

/// A general-setting string: a length, then that many UTF-8 bytes. A title is 1 to 128 bytes
/// long. A description may be empty: a real WH-1000XM4 sends `TOUCH_PANEL_SETTING` with no
/// description at all (length 0), the name alone saying what the setting is.
fn gs_string(
    r: &mut Reader<'_>,
    format: GsStringFormat,
    may_be_empty: bool,
) -> Result<GsString, ProtoError> {
    let len = usize::from(r.u8()?);
    if (len == 0 && !may_be_empty) || len > 128 {
        return Err(ProtoError::Malformed(
            "general setting string length is out of range",
        ));
    }
    let text = String::from_utf8_lossy(r.take(len)?).into_owned();
    Ok(GsString { format, text })
}

/// `D1 <slot> <fmt> <len> <title> <len> <description> <GsSettingType> [LIST: <n 1..64>
/// {<fmt> <len> <str> <len> <str>} x n]`.
fn general_setting_capability(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    let mut r = Reader::new(rest, "general setting capability reply is too short");
    let slot = r.u8()?;
    let format = GsStringFormat::from_raw(r.u8()?);
    let title = gs_string(&mut r, format, false)?;
    let description = gs_string(&mut r, format, true)?;
    let setting_type = GsSettingType::from_raw(r.u8()?);
    let mut items = Vec::new();
    if setting_type == GsSettingType::List {
        let n = r.u8()?;
        if n == 0 || n > 64 {
            return Err(ProtoError::Malformed(
                "general setting list length is out of range",
            ));
        }
        for _ in 0..n {
            let format = GsStringFormat::from_raw(r.u8()?);
            let name = gs_string(&mut r, format, false)?;
            let description = gs_string(&mut r, format, true)?;
            items.push(GsListItem { name, description });
        }
    }
    Ok(Some(Report::GeneralSettingCapability(GsCapability {
        slot,
        title,
        description,
        setting_type,
        items,
    })))
}

/// `D7|D9 <slot> <GsSettingType> <value>`: `00|01` for a switch, an index for a list.
fn general_setting_param(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    let mut r = Reader::new(rest, "general setting reply is too short");
    let slot = r.u8()?;
    let setting_type = r.u8()?;
    let value = r.u8()?;
    let value = match GsSettingType::from_raw(setting_type) {
        GsSettingType::Boolean => GsValue::Boolean(OnOff::from_raw(value)),
        GsSettingType::List => GsValue::List(value),
        GsSettingType::Unknown(setting_type) => GsValue::Other {
            setting_type,
            value,
        },
    };
    Ok(Some(Report::GeneralSetting { slot, value }))
}

/// `87|89 01 <PersonalMeasureType> <PersonalValue> <BarometricMeasureType> <pressure>`.
fn nc_optimizer(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    let mut r = Reader::new(rest, "NC optimizer reply is too short");
    if r.u8()? != 0x01 {
        return Ok(None);
    }
    let personal_measure_type = r.u8()?;
    let personal_value = r.u8()?;
    let barometric_measure_type = r.u8()?;
    let pressure = match r.u8()? {
        0x00 => AtmosphericPressure::Unmeasured,
        tenths @ 0x07..=0x0a => AtmosphericPressure::Atm(tenths),
        other => AtmosphericPressure::Unknown(other),
    };
    Ok(Some(Report::NcOptimizer(NcOptimizer {
        personal_measure_type,
        personal_value,
        barometric_measure_type,
        pressure,
    })))
}

/// `99 01 <AlertMessageType> <00 confirmation only|01 positive/negative>`.
fn alert(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    let mut r = Reader::new(rest, "alert is too short");
    if r.u8()? != 0x01 {
        return Ok(None);
    }
    let message = AlertMessageType::from_raw(r.u8()?);
    let wants_answer = r.u8()? != 0;
    Ok(Some(Report::Alert {
        message,
        wants_answer,
    }))
}

/// `41 01 <on/off switch> <language switch> [<n> <MdrLanguage x n>]`, table two, exact
/// lengths: the app validates them.
fn voice_guidance_capability(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    const WRONG_LENGTH: ProtoError =
        ProtoError::Malformed("voice guidance capability has the wrong length");
    let mut r = Reader::new(rest, "voice guidance capability has the wrong length");
    if r.u8()? != 0x01 {
        return Ok(None);
    }
    let on_off_switch = r.u8()? != 0;
    let language_switch = r.u8()? != 0;
    let languages = if r.remaining() == 0 {
        Vec::new()
    } else {
        let list = r.counted()?;
        if r.remaining() != 0 {
            return Err(WRONG_LENGTH);
        }
        list.into_iter().map(MdrLanguage::from_raw).collect()
    };
    Ok(Some(Report::VoiceGuidanceCapability(
        VoiceGuidanceCapability {
            on_off_switch,
            language_switch,
            languages,
        },
    )))
}

/// `47|49 01 01 <00|01>` and `47|49 01 02 <MdrLanguage>`, table two, exactly four bytes.
/// The other detailed types (`03..05`) are parsed but not acted on by the app, and are not
/// decoded here (`Ok(None)`).
fn voice_guidance_param(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    const WRONG_LENGTH: ProtoError =
        ProtoError::Malformed("voice guidance reply has the wrong length");
    match rest {
        [0x01, detail @ (0x01 | 0x02), tail @ ..] => {
            let [value] = tail else {
                return Err(WRONG_LENGTH);
            };
            if *detail == 0x01 {
                Ok(Some(Report::VoiceGuidance(OnOff::from_raw(*value))))
            } else {
                Ok(Some(Report::VoiceGuidanceLanguage(MdrLanguage::from_raw(
                    *value,
                ))))
            }
        }
        [0x01] | [] => Err(WRONG_LENGTH),
        _ => Ok(None),
    }
}

/// `31 01 <max paired> <max connected> <00 file transfer possible | 01 impossible>`, table two.
fn pairing_capability(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    let mut r = Reader::new(rest, "pairing capability reply is too short");
    if r.u8()? != 0x01 {
        return Ok(None);
    }
    let max_paired = r.u8()?;
    let max_connected = r.u8()?;
    let file_transfer = FileTransferSupport::from_raw(r.u8()?);
    Ok(Some(Report::PairingCapability(PairingCapability {
        max_paired,
        max_connected,
        file_transfer,
    })))
}

/// `33|35 01 <00 normal | 01 inquiry scan> <CommonStatus>`, table two.
fn pairing_mode(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    let mut r = Reader::new(rest, "pairing mode reply is too short");
    if r.u8()? != 0x01 {
        return Ok(None);
    }
    let mode = PairingMode::from_raw(r.u8()?);
    let status = CommonStatus::from_raw(r.u8()?);
    Ok(Some(Report::PairingMode(PairingModeState { mode, status })))
}

/// The length of a Bluetooth address written out: `XX:XX:XX:XX:XX:XX`.
const ADDRESS_LEN: usize = 17;

/// `37|39 01 <n> {<17 ASCII address> <connection order> <nameLen> <name>} x n [<playback order>]`,
/// table two.
///
/// Read out of Sony's app and seen once on a real WH-1000XM4, and decoded defensively: a
/// shortfall anywhere is a typed failure, the 17 address bytes are kept as text whatever they
/// are, and the trailing playback byte (the connection order of the device that holds playback,
/// not an index) may be absent ([`PlaybackHolder::Unknown`]).
fn paired_devices(rest: &[u8]) -> Result<Option<Report>, ProtoError> {
    let mut r = Reader::new(rest, "paired device list reply is too short");
    if r.u8()? != 0x01 {
        return Ok(None);
    }
    let n = r.u8()?;
    let mut devices = Vec::new();
    for _ in 0..n {
        let address = String::from_utf8_lossy(r.take(ADDRESS_LEN)?).into_owned();
        let connection = ConnectionOrder::from_raw(r.u8()?);
        let name = r.text()?;
        devices.push(PairedDevice {
            address,
            connection,
            name,
        });
    }
    let playback = if r.remaining() == 0 {
        PlaybackHolder::Unknown
    } else {
        PlaybackHolder::Order(r.u8()?)
    };
    Ok(Some(Report::PairedDevices(PairedDevices {
        devices,
        playback,
    })))
}
