//! The decoders against the exact hex layouts of Sony's own app.
//!
//! The payloads are synthetic: each is written out from the layout Sony's app uses, with
//! the topic it came from, and none was captured from an XM4.

use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

use junk_core::ProtoError;

use super::*;

/// `"11 00 50 00"` to bytes.
fn hex(text: &str) -> Vec<u8> {
    text.split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).expect("hex"))
        .collect()
}

fn decode(table: Table, text: &str) -> Result<Option<Report>, ProtoError> {
    decode_report(table, &hex(text))
}

/// Table one, expecting a report.
fn one(text: &str) -> Report {
    decode(Table::One, text)
        .unwrap_or_else(|err| panic!("{text}: {err}"))
        .unwrap_or_else(|| panic!("{text}: no decoder"))
}

/// Table two, expecting a report.
fn two(text: &str) -> Report {
    decode(Table::Two, text)
        .unwrap_or_else(|err| panic!("{text}: {err}"))
        .unwrap_or_else(|| panic!("{text}: no decoder"))
}

/// Every proper prefix of `text` that still has a command id is a typed failure or an
/// "I do not know this" (`None`), never a panic; and `text` with junk after it decodes as
/// `text` alone does. That is the app's "ignore bytes past the layout".
fn prefixes_never_panic_and_tails_are_ignored(table: Table, text: &str) {
    let full = hex(text);
    let expected = decode_report(table, &full);
    for cut in 0..full.len() {
        let _ = decode_report(table, &full[..cut]);
    }
    let mut longer = full;
    longer.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
    assert_eq!(
        decode_report(table, &longer),
        expected,
        "{text} plus a tail"
    );
}

fn is_short(table: Table, text: &str) {
    assert!(
        matches!(decode(table, text), Err(ProtoError::Malformed(_))),
        "{text} should be a typed decode failure, got {:?}",
        decode(table, text)
    );
}

#[test]
fn an_empty_payload_is_malformed_and_an_unknown_command_is_not_an_error() {
    assert_eq!(
        decode(Table::One, ""),
        Err(ProtoError::Malformed("empty payload"))
    );
    assert_eq!(decode(Table::One, "c9 01 00 02 7b 7d"), Ok(None));
    assert_eq!(decode(Table::One, "ee 00"), Ok(None));
    // Command ids overlap between the tables: `47` on table one is VPT's, which is not read.
    assert_eq!(decode(Table::One, "47 01 01 01"), Ok(None));
    assert_eq!(two("47 01 01 01"), Report::VoiceGuidance(OnOff::On));
}

/// Init step 1, protocol version: `01 <inq> <verHi> <verLo>`. The XM3 reply the community quotes is `01 00 40 10`.
#[test]
fn protocol_version_is_big_endian_of_bytes_two_and_three() {
    assert_eq!(one("01 00 40 10"), Report::ProtocolVersion(0x4010));
    assert_eq!(one("01 00 10 00"), Report::ProtocolVersion(0x1000));
    // Bytes past [3] are ignored, as the app ignores them: this is the v2-length reply.
    assert_eq!(
        one("01 00 40 10 aa bb cc dd"),
        Report::ProtocolVersion(0x4010)
    );
    is_short(Table::One, "01 00 40");
    is_short(Table::One, "01");
    prefixes_never_panic_and_tails_are_ignored(Table::One, "01 00 40 10");
}

/// Init step 2, unique id: `03 <inq> <capabilityCounter> <len> <uniqueId utf8>`.
#[test]
fn capability_info_carries_a_counter_and_a_unique_id() {
    assert_eq!(
        one("03 00 07 04 41 42 43 44"),
        Report::CapabilityInfo(CapabilityInfo {
            counter: 7,
            unique_id: "ABCD".to_string()
        })
    );
    // The declared length is longer than what is there.
    is_short(Table::One, "03 00 07 05 41 42 43 44");
    prefixes_never_panic_and_tails_are_ignored(Table::One, "03 00 07 04 41 42 43 44");
}

/// Init step 3, device info: `05 01 <len> <model>`, `05 02 <len> <fw>`, `05 03 <series> <color>`,
/// `05 04 <n> <category x n>`.
#[test]
fn device_info_has_four_kinds() {
    assert_eq!(
        one("05 01 0a 57 48 2d 31 30 30 30 58 4d 34"),
        Report::Model("WH-1000XM4".to_string())
    );
    assert_eq!(
        one("05 02 05 31 2e 32 2e 33"),
        Report::Firmware("1.2.3".to_string())
    );
    assert_eq!(
        one("05 03 10 02"),
        Report::ModelInfo(ModelInfo {
            series: ModelSeries::ExtraBass,
            color: 2
        })
    );
    assert_eq!(
        one("05 03 77 09"),
        Report::ModelInfo(ModelInfo {
            series: ModelSeries::Unknown(0x77),
            color: 9
        })
    );
    assert_eq!(
        one("05 04 02 01 02"),
        Report::GuidanceCategories(vec![1, 2])
    );
    assert_eq!(decode(Table::One, "05 09 00"), Ok(None));
    is_short(Table::One, "05 01 0a 57 48");
    is_short(Table::One, "05 03 10");
    is_short(Table::One, "05 04 03 01");
    is_short(Table::One, "05");
    for text in [
        "05 01 0a 57 48 2d 31 30 30 30 58 4d 34",
        "05 03 10 02",
        "05 04 02 01 02",
    ] {
        prefixes_never_panic_and_tails_are_ignored(Table::One, text);
    }
}

/// Init step 4, function list: `07 <inq> <n> <FunctionType x n>`; an unknown byte is kept.
#[test]
fn the_function_list_keeps_unknown_function_types() {
    assert_eq!(
        one("07 00 06 11 13 62 51 f4 7e"),
        Report::Functions(vec![
            FunctionType::BatteryLevel,
            FunctionType::CodecIndicator,
            FunctionType::NoiseCancellingAndAmbientSoundMode,
            FunctionType::PresetEq,
            FunctionType::AutoPowerOff,
            FunctionType::Unknown(0x7e),
        ])
    );
    assert_eq!(one("07 00 00"), Report::Functions(vec![]));
    is_short(Table::One, "07 00 03 11 13");
    prefixes_never_panic_and_tails_are_ignored(Table::One, "07 00 02 11 13");
}

/// Battery and codec: `11|13 00 <lvl> <chg>`; `01 <L lvl> <L chg> <R lvl> <R chg>`; `02 <lvl> <chg>`.
#[test]
fn battery_has_three_shapes() {
    let battery = |level, charging| Battery { level, charging };
    assert_eq!(
        one("11 00 50 00"),
        Report::Battery(battery(0x50, BatteryChargingStatus::NotCharging))
    );
    assert_eq!(
        one("13 00 64 01"),
        Report::Battery(battery(100, BatteryChargingStatus::Charging))
    );
    assert_eq!(
        one("13 00 0a f0"),
        Report::Battery(battery(10, BatteryChargingStatus::Unknown))
    );
    assert_eq!(
        one("13 00 0a 07"),
        Report::Battery(battery(10, BatteryChargingStatus::Other(7)))
    );
    assert_eq!(
        one("11 01 32 00 28 01"),
        Report::BatteryLeftRight(LeftRightBattery {
            left: battery(0x32, BatteryChargingStatus::NotCharging),
            right: battery(0x28, BatteryChargingStatus::Charging),
        })
    );
    assert_eq!(
        one("11 02 5a 00"),
        Report::BatteryCradle(battery(0x5a, BatteryChargingStatus::NotCharging))
    );
    // An unknown inquired type is ignored by the app.
    assert_eq!(decode(Table::One, "11 09 01 02"), Ok(None));
    is_short(Table::One, "11 00 50");
    is_short(Table::One, "11 01 32 00 28");
    is_short(Table::One, "11");
    for text in ["11 00 50 00", "11 01 32 00 28 01", "13 02 5a 00"] {
        prefixes_never_panic_and_tails_are_ignored(Table::One, text);
    }
}

/// Battery and codec: `19|1B <inq> <AudioCodec>`, `15|17 <inq> <type> <status>`, `25|27 01 <L> <R>`.
#[test]
fn codec_upscaling_and_connection_status() {
    assert_eq!(one("19 00 10"), Report::Codec(AudioCodec::Ldac));
    assert_eq!(one("1b 00 21"), Report::Codec(AudioCodec::AptXHd));
    assert_eq!(one("1b 00 ff"), Report::Codec(AudioCodec::Other));
    assert_eq!(one("19 00 33"), Report::Codec(AudioCodec::Unknown(0x33)));
    assert_eq!(
        one("15 00 02 01"),
        Report::UpscalingIndicator(UpscalingIndicator {
            kind: UpscalingEffectType::DseeHxAi,
            status: UpscalingEffectStatus::Valid,
        })
    );
    assert_eq!(
        one("17 00 09 05"),
        Report::UpscalingIndicator(UpscalingIndicator {
            kind: UpscalingEffectType::Unknown(9),
            status: UpscalingEffectStatus::Unknown(5),
        })
    );
    assert_eq!(
        one("25 01 01 00"),
        Report::ConnectionStatus(LeftRightConnection {
            left: ConnectionState::Connected,
            right: ConnectionState::NotConnected,
        })
    );
    assert_eq!(decode(Table::One, "27 02 01 01"), Ok(None));
    is_short(Table::One, "19 00");
    is_short(Table::One, "15 00 02");
    is_short(Table::One, "25 01 01");
    for text in ["19 00 10", "15 00 02 01", "25 01 01 00"] {
        prefixes_never_panic_and_tails_are_ignored(Table::One, text);
    }
}

/// NC/ASM: the type-02 parameter is eight bytes with the id and the app reads exactly those.
#[test]
fn nc_asm_param_type_two_is_the_eight_byte_layout() {
    // The community's NC-on frame: effect 11 (completion), setting type 02, value 02 (dual),
    // asm setting type 01, asm id 00, level 00.
    assert_eq!(
        one("69 02 11 02 02 01 00 00"),
        Report::NcAsm(NcAsmState::NoiseCancellingAndAmbient {
            effect: NcAsmEffect::AdjustmentCompletion,
            nc_setting_type: NcAsmSettingType::DualSingleOff,
            nc_value: NcDualSingleValue::Dual,
            asm_setting_type: AsmSettingType::LevelAdjustment,
            asm_id: AsmId::Normal,
            asm_level: 0,
        })
    );
    // Single ("wind noise reduction"), still being dragged.
    assert_eq!(
        one("67 02 10 02 01 01 00 00"),
        Report::NcAsm(NcAsmState::NoiseCancellingAndAmbient {
            effect: NcAsmEffect::AdjustmentInProgress,
            nc_setting_type: NcAsmSettingType::DualSingleOff,
            nc_value: NcDualSingleValue::Single,
            asm_setting_type: AsmSettingType::LevelAdjustment,
            asm_id: AsmId::Normal,
            asm_level: 0,
        })
    );
    // Ambient: value off, a level, the voice-focus mode.
    assert_eq!(
        one("69 02 01 02 00 01 01 0c"),
        Report::NcAsm(NcAsmState::NoiseCancellingAndAmbient {
            effect: NcAsmEffect::On,
            nc_setting_type: NcAsmSettingType::DualSingleOff,
            nc_value: NcDualSingleValue::Off,
            asm_setting_type: AsmSettingType::LevelAdjustment,
            asm_id: AsmId::Voice,
            asm_level: 12,
        })
    );
    // Bytes the app does not read.
    assert_eq!(
        one("69 02 11 02 02 01 00 00 ff ff"),
        one("69 02 11 02 02 01 00 00")
    );
    // New values do not fail the decode.
    let Report::NcAsm(NcAsmState::NoiseCancellingAndAmbient {
        effect,
        nc_setting_type,
        nc_value,
        asm_setting_type,
        asm_id,
        ..
    }) = one("69 02 77 78 79 7a 7b 01")
    else {
        panic!("type 02");
    };
    assert_eq!(effect, NcAsmEffect::Unknown(0x77));
    assert_eq!(nc_setting_type, NcAsmSettingType::Unknown(0x78));
    assert_eq!(nc_value, NcDualSingleValue::Unknown(0x79));
    assert_eq!(asm_setting_type, AsmSettingType::Unknown(0x7a));
    assert_eq!(asm_id, AsmId::Unknown(0x7b));
    // One byte short.
    is_short(Table::One, "69 02 11 02 02 01 00");
    prefixes_never_panic_and_tails_are_ignored(Table::One, "69 02 11 02 02 01 00 00");
}

/// NC/ASM: types 01 and 03.
#[test]
fn nc_asm_param_types_one_and_three() {
    assert_eq!(
        one("67 01 00 01"),
        Report::NcAsm(NcAsmState::NoiseCancelling {
            setting_type: 0,
            on: OnOff::On
        })
    );
    assert_eq!(
        one("69 03 11 01 01 0c"),
        Report::NcAsm(NcAsmState::Ambient {
            effect: NcAsmEffect::AdjustmentCompletion,
            asm_setting_type: AsmSettingType::LevelAdjustment,
            asm_id: AsmId::Voice,
            level: 12,
        })
    );
    assert_eq!(decode(Table::One, "67 09 00 00 00 00 00 00"), Ok(None));
    is_short(Table::One, "67 01 00");
    is_short(Table::One, "69 03 11 01 01");
    prefixes_never_panic_and_tails_are_ignored(Table::One, "67 01 00 01");
    prefixes_never_panic_and_tails_are_ignored(Table::One, "69 03 11 01 01 0c");
}

/// NC/ASM: the capability carries `asmStep` per `AsmId`; nothing is assumed to be 20.
#[test]
fn nc_asm_capability_reports_asm_steps_per_mode() {
    let report = one("61 02 02 01 01 02 00 0a 01 05");
    assert_eq!(
        report,
        Report::NcAsmCapability(NcAsmCapability::NoiseCancellingAndAmbient {
            nc_setting_type: NcAsmSettingType::DualSingleOff,
            nc_step: 1,
            asm_setting_type: AsmSettingType::LevelAdjustment,
            asm: vec![
                AsmStep {
                    id: AsmId::Normal,
                    step: 10
                },
                AsmStep {
                    id: AsmId::Voice,
                    step: 5
                },
            ],
        })
    );
    let Report::NcAsmCapability(capability) = report else {
        panic!("a capability");
    };
    assert_eq!(capability.asm_step(AsmId::Normal), Some(10));
    assert_eq!(capability.asm_step(AsmId::Voice), Some(5));
    assert_eq!(capability.asm_step(AsmId::Unknown(9)), None);

    assert_eq!(
        one("61 01 00"),
        Report::NcAsmCapability(NcAsmCapability::NoiseCancelling { setting_type: 0 })
    );
    assert_eq!(
        one("61 03 01 01 00 0a"),
        Report::NcAsmCapability(NcAsmCapability::Ambient {
            asm_setting_type: AsmSettingType::LevelAdjustment,
            asm: vec![AsmStep {
                id: AsmId::Normal,
                step: 10
            }],
        })
    );
    // Two modes declared, one present.
    is_short(Table::One, "61 02 02 01 01 02 00 0a");
    is_short(Table::One, "61 03 01 01");
    prefixes_never_panic_and_tails_are_ignored(Table::One, "61 02 02 01 01 02 00 0a 01 05");
}

/// Equalizer: `57|59 <type> <EqPresetId> <n> <v1> .. <vn>`; `n` is a count.
#[test]
fn eq_param_has_a_count_not_a_constant() {
    assert_eq!(
        one("59 01 a0 06 0f 0a 0a 0a 0a 0a"),
        Report::Eq(EqState {
            kind: EqEbbInquiredType::PresetEq,
            preset: EqPresetId::Custom,
            values: vec![0x0f, 10, 10, 10, 10, 10],
        })
    );
    // A preset, zero values: what the app sends to select one.
    assert_eq!(
        one("57 01 01 00"),
        Report::Eq(EqState {
            kind: EqEbbInquiredType::PresetEq,
            preset: EqPresetId::Rock,
            values: vec![],
        })
    );
    assert_eq!(
        one("57 03 16 00"),
        Report::Eq(EqState {
            kind: EqEbbInquiredType::PresetEqNoncustomizable,
            preset: EqPresetId::Bass,
            values: vec![],
        })
    );
    // A count of three is three values, whatever else follows.
    let Report::Eq(state) = one("57 01 a1 03 01 02 03 04 05") else {
        panic!("an equalizer");
    };
    assert_eq!(state.values, [1, 2, 3]);
    assert_eq!(state.preset, EqPresetId::UserSetting1);
    assert_eq!(
        one("57 01 42 00"),
        Report::Eq(EqState {
            kind: EqEbbInquiredType::PresetEq,
            preset: EqPresetId::Unknown(0x42),
            values: vec![],
        })
    );
    // Six declared, two present.
    is_short(Table::One, "57 01 a0 06 0a 0a");
    is_short(Table::One, "57 01 a0");
    prefixes_never_panic_and_tails_are_ignored(Table::One, "59 01 a0 03 01 02 03");
}

/// Equalizer: Extra Bass is a signed byte.
#[test]
fn ebb_is_a_signed_byte() {
    assert_eq!(one("57 02 fd"), Report::Ebb(-3));
    assert_eq!(one("59 02 05"), Report::Ebb(5));
    assert_eq!(one("57 02 00"), Report::Ebb(0));
    is_short(Table::One, "57 02");
    assert_eq!(
        one("51 02 fb 05"),
        Report::EbbCapability(EbbCapability { min: -5, max: 5 })
    );
    is_short(Table::One, "51 02 fb");
}

/// Equalizer: `51 <01|03> <bandCount> <levelSteps> <nPresets> {<id> <nameLen> <name>} x n`.
#[test]
fn eq_capability_names_the_presets() {
    assert_eq!(
        one("51 01 06 15 03 00 00 01 04 52 6f 63 6b a0 06 43 75 73 74 6f 6d"),
        Report::EqCapability(EqCapability {
            kind: EqEbbInquiredType::PresetEq,
            band_count: 6,
            level_steps: 21,
            presets: vec![
                EqPreset {
                    id: EqPresetId::Off,
                    name: String::new()
                },
                EqPreset {
                    id: EqPresetId::Rock,
                    name: "Rock".to_string()
                },
                EqPreset {
                    id: EqPresetId::Custom,
                    name: "Custom".to_string()
                },
            ],
        })
    );
    // A name length over 128 is read as zero: no name bytes are taken.
    assert_eq!(
        one("51 03 05 0b 01 07 81"),
        Report::EqCapability(EqCapability {
            kind: EqEbbInquiredType::PresetEqNoncustomizable,
            band_count: 5,
            level_steps: 11,
            presets: vec![EqPreset {
                id: EqPresetId::Acoustic,
                name: String::new()
            }],
        })
    );
    // Three presets declared, one present.
    is_short(Table::One, "51 01 06 15 03 01 04 52 6f 63 6b");
    is_short(Table::One, "51 01 06 15 01 01 04 52 6f");
    prefixes_never_panic_and_tails_are_ignored(Table::One, "51 01 06 15 01 01 04 52 6f 63 6b");
}

/// Equalizer: `5B <type> <n> {<EqBandInformationType> <value u16 BE>} x n`; Clear Bass is where
/// the device says, not at a fixed index.
#[test]
fn eq_bands_say_where_clear_bass_is() {
    let first = one("5b 01 03 10 00 01 01 01 90 02 00 10");
    assert_eq!(
        first,
        Report::EqBands(EqBands {
            kind: EqEbbInquiredType::PresetEq,
            bands: vec![
                EqBand {
                    kind: EqBandInformationType::SpecificInformation,
                    value: 1
                },
                EqBand {
                    kind: EqBandInformationType::Hz,
                    value: 400
                },
                EqBand {
                    kind: EqBandInformationType::Khz,
                    value: 16
                },
            ],
        })
    );
    let Report::EqBands(bands) = first else {
        panic!("bands");
    };
    assert_eq!(bands.clear_bass_index(), Some(0));

    let Report::EqBands(bands) = one("5b 01 03 01 01 90 10 00 01 02 00 10") else {
        panic!("bands");
    };
    assert_eq!(bands.clear_bass_index(), Some(1));

    // `10` with another specific value is not Clear Bass; and none at all is `None`.
    let Report::EqBands(bands) = one("5b 03 02 10 00 02 01 01 90") else {
        panic!("bands");
    };
    assert_eq!(bands.clear_bass_index(), None);
    // Three entries declared, two and a half present.
    is_short(Table::One, "5b 01 03 10 00 01 01 01");
    prefixes_never_panic_and_tails_are_ignored(Table::One, "5b 01 02 10 00 01 01 01 90");
}

/// Audio: DSEE `E7|E9 02 <00> <00 off|01 auto>`; connection mode `E7|E9 01 <00> <00|01>`.
#[test]
fn dsee_and_connection_mode() {
    assert_eq!(one("e7 02 00 01"), Report::Dsee(DseeSetting::Auto));
    assert_eq!(one("e9 02 00 00"), Report::Dsee(DseeSetting::Off));
    assert_eq!(one("e9 02 00 07"), Report::Dsee(DseeSetting::Unknown(7)));
    assert_eq!(
        one("e7 01 00 00"),
        Report::ConnectionMode(ConnectionMode::SoundQualityPrior)
    );
    assert_eq!(
        one("e9 01 00 01"),
        Report::ConnectionMode(ConnectionMode::ConnectionQualityPrior)
    );
    assert_eq!(decode(Table::One, "e7 03 00 00"), Ok(None));
    is_short(Table::One, "e7 02 00");
    is_short(Table::One, "e7 01");
    prefixes_never_panic_and_tails_are_ignored(Table::One, "e7 02 00 01");
}

/// System: pause when taken off, auto power off.
#[test]
fn pause_when_taken_off_and_auto_power_off() {
    assert_eq!(one("f7 03 00 01"), Report::PauseWhenTakenOff(OnOff::On));
    assert_eq!(one("f9 03 00 00"), Report::PauseWhenTakenOff(OnOff::Off));
    assert_eq!(
        one("f9 04 01 11 01"),
        Report::AutoPowerOff(AutoPowerOff {
            active: AutoPowerOffElementId::Disable,
            timer: AutoPowerOffElementId::Minutes30,
        })
    );
    assert_eq!(
        one("f7 04 01 10 02"),
        Report::AutoPowerOff(AutoPowerOff {
            active: AutoPowerOffElementId::WhenRemovedFromEars,
            timer: AutoPowerOffElementId::Minutes60,
        })
    );
    assert_eq!(
        one("f7 04 01 55 03"),
        Report::AutoPowerOff(AutoPowerOff {
            active: AutoPowerOffElementId::Unknown(0x55),
            timer: AutoPowerOffElementId::Minutes180,
        })
    );
    assert_eq!(
        one("f1 04 03 00 01 11"),
        Report::AutoPowerOffCapability(vec![
            AutoPowerOffElementId::Minutes5,
            AutoPowerOffElementId::Minutes30,
            AutoPowerOffElementId::Disable,
        ])
    );
    // The other system capabilities are kept raw, not decoded.
    assert_eq!(decode(Table::One, "f1 05 00 01 00"), Ok(None));
    is_short(Table::One, "f7 03 00");
    is_short(Table::One, "f7 04 01 10");
    is_short(Table::One, "f1 04 03 00 01");
    for text in ["f7 03 00 01", "f9 04 01 11 01", "f1 04 02 00 01"] {
        prefixes_never_panic_and_tails_are_ignored(Table::One, text);
    }
}

/// System: the RET is `F7 05 00 <v>`; the NTFY is `F9 05 <01|02> <v>`.
#[test]
fn speak_to_chat_ret_and_ntfy_differ() {
    assert_eq!(one("f7 05 00 01"), Report::SpeakToChat(OnOff::On));
    assert_eq!(one("f7 05 00 00"), Report::SpeakToChat(OnOff::Off));
    assert_eq!(one("f9 05 01 01"), Report::SpeakToChat(OnOff::On));
    assert_eq!(one("f9 05 02 01"), Report::SpeakToChatPreview(OnOff::On));
    assert_eq!(one("f9 05 02 00"), Report::SpeakToChatPreview(OnOff::Off));
    // `F9 05 00 v` is not a thing: that byte is the setting that changed.
    assert_eq!(decode(Table::One, "f9 05 00 01"), Ok(None));
    assert_eq!(decode(Table::One, "f9 05 03 01"), Ok(None));
    is_short(Table::One, "f7 05 00");
    is_short(Table::One, "f9 05 01");
    prefixes_never_panic_and_tails_are_ignored(Table::One, "f7 05 00 01");
}

/// System: `FB|FD 05 00 <sensitivity> <focus on voice> <timeout>`.
#[test]
fn speak_to_chat_config() {
    assert_eq!(
        one("fb 05 00 01 01 02"),
        Report::SpeakToChatConfig(SpeakToChatConfig {
            sensitivity: StcSensitivity::High,
            focus_on_voice: OnOff::On,
            timeout: ModeOutTime::Slow,
        })
    );
    assert_eq!(
        one("fd 05 00 00 00 03"),
        Report::SpeakToChatConfig(SpeakToChatConfig {
            sensitivity: StcSensitivity::Auto,
            focus_on_voice: OnOff::Off,
            timeout: ModeOutTime::None,
        })
    );
    assert_eq!(
        one("fb 05 00 09 09 09"),
        Report::SpeakToChatConfig(SpeakToChatConfig {
            sensitivity: StcSensitivity::Unknown(9),
            focus_on_voice: OnOff::Unknown(9),
            timeout: ModeOutTime::Unknown(9),
        })
    );
    assert_eq!(decode(Table::One, "fb 04 00 00 00 00"), Ok(None));
    is_short(Table::One, "fb 05 00 01 01");
    prefixes_never_panic_and_tails_are_ignored(Table::One, "fb 05 00 01 01 02");
}

/// System: `F7|F9 06 <nKeys> <AssignableSettingsPreset x nKeys>`.
#[test]
fn assignable_settings() {
    assert_eq!(
        one("f7 06 02 00 31"),
        Report::AssignableSettings(vec![
            AssignableSettingsPreset::AmbientSoundControl,
            AssignableSettingsPreset::GoogleAssistant,
        ])
    );
    assert_eq!(
        one("f9 06 03 ff 20 44"),
        Report::AssignableSettings(vec![
            AssignableSettingsPreset::NoFunction,
            AssignableSettingsPreset::PlaybackControl,
            AssignableSettingsPreset::Unknown(0x44),
        ])
    );
    is_short(Table::One, "f7 06 02 00");
    prefixes_never_panic_and_tails_are_ignored(Table::One, "f7 06 02 00 31");
}

/// `<len> <text>` as bytes.
fn text(text: &str) -> Vec<u8> {
    let mut bytes = vec![u8::try_from(text.len()).expect("short")];
    bytes.extend_from_slice(text.as_bytes());
    bytes
}

/// A `D1` capability with the general-settings layout.
fn gs_capability(slot: u8, format: u8, title: &str, description: &str, rest: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0xd1, slot, format];
    bytes.extend(text(title));
    bytes.extend(text(description));
    bytes.extend_from_slice(rest);
    bytes
}

/// General settings: the headset describes the slot, and the title names what it is.
#[test]
fn a_general_setting_capability_names_its_title() {
    let touch = gs_capability(0xd2, 0x02, "TOUCH_PANEL_SETTING", "x", &[0x01]);
    let Ok(Some(Report::GeneralSettingCapability(capability))) = decode_report(Table::One, &touch)
    else {
        panic!("a capability");
    };
    assert_eq!(capability.slot, 0xd2);
    assert_eq!(capability.setting_type, GsSettingType::Boolean);
    assert_eq!(capability.title.format, GsStringFormat::EnumName);
    assert_eq!(capability.title.title(), GsTitle::TouchPanelSetting);
    assert_eq!(capability.items, Vec::new());

    // A list, with RAW_NAME choices: slot D3 as the multipoint switch.
    let mut list = vec![0x02, 0x02];
    for (name, description) in [("Off", "Single device"), ("On", "Two devices")] {
        list.push(0x01);
        list.extend(text(name));
        list.extend(text(description));
    }
    let multipoint = gs_capability(0xd3, 0x02, "MULTIPOINT_SETTING", "d", &list);
    let Ok(Some(Report::GeneralSettingCapability(capability))) =
        decode_report(Table::One, &multipoint)
    else {
        panic!("a capability");
    };
    assert_eq!(capability.title.title(), GsTitle::MultipointSetting);
    assert_eq!(capability.setting_type, GsSettingType::List);
    assert_eq!(capability.items.len(), 2);
    assert_eq!(capability.items[0].name.format, GsStringFormat::RawName);
    assert_eq!(capability.items[0].name.text, "Off");
    assert_eq!(capability.items[1].description.text, "Two devices");
    assert_eq!(
        capability.items[1].name.title(),
        GsTitle::Raw("On".to_string())
    );

    // A title this crate has no name for, and a literal one.
    let other = gs_capability(0xd1, 0x02, "SOMETHING_NEW", "d", &[0x01]);
    let Ok(Some(Report::GeneralSettingCapability(capability))) = decode_report(Table::One, &other)
    else {
        panic!("a capability");
    };
    assert_eq!(
        capability.title.title(),
        GsTitle::Unknown("SOMETHING_NEW".to_string())
    );
    let raw = gs_capability(0xd1, 0x01, "Touch panel", "d", &[0x01]);
    let Ok(Some(Report::GeneralSettingCapability(capability))) = decode_report(Table::One, &raw)
    else {
        panic!("a capability");
    };
    assert_eq!(
        capability.title.title(),
        GsTitle::Raw("Touch panel".to_string())
    );

    // A setting type this crate does not name decodes with no items.
    let odd = gs_capability(0xd1, 0x01, "t", "d", &[0x07]);
    let Ok(Some(Report::GeneralSettingCapability(capability))) = decode_report(Table::One, &odd)
    else {
        panic!("a capability");
    };
    assert_eq!(capability.setting_type, GsSettingType::Unknown(7));
}

#[test]
fn a_general_setting_capability_is_validated_like_the_apps() {
    // A title of length zero.
    let mut empty_title = vec![0xd1, 0xd2, 0x01, 0x00];
    empty_title.extend(text("d"));
    empty_title.push(0x01);
    assert!(matches!(
        decode_report(Table::One, &empty_title),
        Err(ProtoError::Malformed(_))
    ));
    // A list of zero choices, and one of sixty-five.
    for n in [0u8, 65] {
        let capability = gs_capability(0xd3, 0x01, "t", "d", &[0x02, n]);
        assert!(matches!(
            decode_report(Table::One, &capability),
            Err(ProtoError::Malformed(_))
        ));
    }
    // Cut anywhere, it is a failure or an absence and never a panic.
    let full = gs_capability(0xd2, 0x02, "TOUCH_PANEL_SETTING", "x", &[0x01]);
    for cut in 0..full.len() {
        let _ = decode_report(Table::One, &full[..cut]);
    }
    assert!(matches!(
        decode_report(Table::One, &full[..full.len() - 1]),
        Err(ProtoError::Malformed(_))
    ));
}

/// Captured from a real WH-1000XM4 (firmware 2.7.1): the touch panel slot has an `ENUM_NAME`
/// title and no description (length 0), and a byte after the setting type. Both must decode.
#[test]
fn a_real_headsets_touch_panel_capability_has_no_description() {
    let real = one("d1 d1 02 13 54 4f 55 43 48 5f 50 41 4e 45 4c 5f 53 45 54 54 49 4e 47 00 01 00");
    let Report::GeneralSettingCapability(capability) = real else {
        panic!("a capability");
    };
    assert_eq!(capability.slot, 0xd1);
    assert_eq!(capability.title.title(), GsTitle::TouchPanelSetting);
    assert_eq!(capability.description.text, "");
    assert_eq!(capability.setting_type, GsSettingType::Boolean);

    // A real RAW_NAME slot from the same headset, with a description.
    let multipoint = one(
        "d1 d2 01 12 4d 55 4c 54 49 50 4f 49 4e 54 5f 53 45 54 54 49 4e 47 1a 4d 55 4c 54 49 50 4f 49 4e 54 5f 53 45 54 54 49 4e 47 5f 53 55 4d 4d 41 52 59 01 00",
    );
    let Report::GeneralSettingCapability(capability) = multipoint else {
        panic!("a capability");
    };
    assert_eq!(capability.title.text, "MULTIPOINT_SETTING");
    assert_eq!(capability.description.text, "MULTIPOINT_SETTING_SUMMARY");
}

/// General settings: `D7|D9 <slot> <GsSettingType> <value>`.
#[test]
fn a_general_setting_value_is_a_switch_or_an_index() {
    assert_eq!(
        one("d7 d2 01 01"),
        Report::GeneralSetting {
            slot: 0xd2,
            value: GsValue::Boolean(OnOff::On)
        }
    );
    assert_eq!(
        one("d9 d3 02 03"),
        Report::GeneralSetting {
            slot: 0xd3,
            value: GsValue::List(3)
        }
    );
    assert_eq!(
        one("d7 d1 07 05"),
        Report::GeneralSetting {
            slot: 0xd1,
            value: GsValue::Other {
                setting_type: 7,
                value: 5
            }
        }
    );
    is_short(Table::One, "d7 d2 01");
    prefixes_never_panic_and_tails_are_ignored(Table::One, "d7 d2 01 01");
}

/// NC optimizer: `87|89 01 <PersonalMeasureType> <PersonalValue> <BarometricMeasureType> <pressure>`.
#[test]
fn nc_optimizer_pressure() {
    assert_eq!(
        one("87 01 01 05 01 08"),
        Report::NcOptimizer(NcOptimizer {
            personal_measure_type: 1,
            personal_value: 5,
            barometric_measure_type: 1,
            pressure: AtmosphericPressure::Atm(8),
        })
    );
    let pressure = |byte: &str| {
        let Report::NcOptimizer(optimizer) = one(&alloc::format!("89 01 00 00 00 {byte}")) else {
            panic!("an optimizer");
        };
        optimizer.pressure
    };
    assert_eq!(pressure("00"), AtmosphericPressure::Unmeasured);
    assert_eq!(pressure("07"), AtmosphericPressure::Atm(7));
    assert_eq!(pressure("0a"), AtmosphericPressure::Atm(10));
    assert_eq!(pressure("0b"), AtmosphericPressure::Unknown(0x0b));
    assert_eq!(pressure("03"), AtmosphericPressure::Unknown(3));
    assert_eq!(decode(Table::One, "87 02 00 00 00 00"), Ok(None));
    is_short(Table::One, "87 01 01 05 01");
    prefixes_never_panic_and_tails_are_ignored(Table::One, "87 01 01 05 01 08");
}

/// Voice guidance: table two, `47|49 01 01 <00|01>` and `47|49 01 02 <MdrLanguage>`, exact lengths.
#[test]
fn voice_guidance_is_exact_length() {
    assert_eq!(two("47 01 01 01"), Report::VoiceGuidance(OnOff::On));
    assert_eq!(two("49 01 01 00"), Report::VoiceGuidance(OnOff::Off));
    assert_eq!(
        two("47 01 02 0b"),
        Report::VoiceGuidanceLanguage(MdrLanguage::Japanese)
    );
    assert_eq!(
        two("49 01 02 7f"),
        Report::VoiceGuidanceLanguage(MdrLanguage::Unknown(0x7f))
    );
    // A byte too many is as wrong as a byte too few: the app validates the length.
    is_short(Table::Two, "47 01 01 01 00");
    is_short(Table::Two, "47 01 01");
    is_short(Table::Two, "47 01 02");
    is_short(Table::Two, "47 01");
    is_short(Table::Two, "47");
    // Detailed types 03 to 05 are parsed but not acted on by the app.
    assert_eq!(decode(Table::Two, "47 01 03 00 00"), Ok(None));
    assert_eq!(decode(Table::Two, "47 01 05 01"), Ok(None));
    assert_eq!(decode(Table::Two, "47 02 01 01"), Ok(None));
}

/// Voice guidance: `41 01 <on/off switch> <language switch> [<n> <MdrLanguage x n>]`.
#[test]
fn voice_guidance_capability_is_exact_length() {
    assert_eq!(
        two("41 01 01 00"),
        Report::VoiceGuidanceCapability(VoiceGuidanceCapability {
            on_off_switch: true,
            language_switch: false,
            languages: vec![],
        })
    );
    assert_eq!(
        two("41 01 01 01 02 01 0b"),
        Report::VoiceGuidanceCapability(VoiceGuidanceCapability {
            on_off_switch: true,
            language_switch: true,
            languages: vec![MdrLanguage::English, MdrLanguage::Japanese],
        })
    );
    // The count is longer than what is there, and shorter.
    is_short(Table::Two, "41 01 01 01 03 01 0b");
    is_short(Table::Two, "41 01 01 01 01 01 0b");
    is_short(Table::Two, "41 01 01");
    assert_eq!(decode(Table::Two, "41 02 01 01"), Ok(None));
    // Table one has VPT's capability under the same id, which is not decoded.
    assert_eq!(decode(Table::One, "41 01 01 01"), Ok(None));
}

/// Pairing capability: `31 01 <max paired> <max connected> <file transfer>`. The first
/// reply is what a real WH-1000XM4 (firmware 2.7.1) sent.
#[test]
fn pairing_capability_layout() {
    assert_eq!(
        two("31 01 08 02 01"),
        Report::PairingCapability(PairingCapability {
            max_paired: 8,
            max_connected: 2,
            file_transfer: FileTransferSupport::Impossible,
        })
    );
    assert_eq!(
        two("31 01 03 01 00"),
        Report::PairingCapability(PairingCapability {
            max_paired: 3,
            max_connected: 1,
            file_transfer: FileTransferSupport::Possible,
        })
    );
    assert_eq!(
        two("31 01 08 02 07"),
        Report::PairingCapability(PairingCapability {
            max_paired: 8,
            max_connected: 2,
            file_transfer: FileTransferSupport::Unknown(7),
        })
    );
    is_short(Table::Two, "31 01 08 02");
    is_short(Table::Two, "31");
    assert_eq!(decode(Table::Two, "31 02 08 02 01"), Ok(None));
    // Table one has no `31`.
    assert_eq!(decode(Table::One, "31 01 08 02 01"), Ok(None));
    prefixes_never_panic_and_tails_are_ignored(Table::Two, "31 01 08 02 01");
}

/// Pairing mode: `33|35 01 <00 normal | 01 inquiry scan> <CommonStatus>`. Not seen on a real
/// headset.
#[test]
fn pairing_mode_layout() {
    assert_eq!(
        two("33 01 00 00"),
        Report::PairingMode(PairingModeState {
            mode: PairingMode::Normal,
            status: CommonStatus::Enable,
        })
    );
    assert_eq!(
        two("35 01 01 01"),
        Report::PairingMode(PairingModeState {
            mode: PairingMode::InquiryScan,
            status: CommonStatus::Disable,
        })
    );
    assert_eq!(
        two("35 01 02 ff"),
        Report::PairingMode(PairingModeState {
            mode: PairingMode::Unknown(2),
            status: CommonStatus::OutOfRange,
        })
    );
    is_short(Table::Two, "33 01 00");
    is_short(Table::Two, "33 01");
    assert_eq!(decode(Table::Two, "33 02 00 00"), Ok(None));
    prefixes_never_panic_and_tails_are_ignored(Table::Two, "33 01 00 00");
}

/// `"AA:BB:CC:DD:EE:FF"` as the hex of its 17 ASCII bytes.
fn address_hex(address: &str) -> alloc::string::String {
    address
        .bytes()
        .map(|b| alloc::format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Paired devices: `37|39 01 <n> {<17 ASCII address> <order> <nameLen> <name>} x n <index>`.
/// Not seen on a real headset.
#[test]
fn paired_devices_layout() {
    let phone = address_hex("AA:BB:CC:DD:EE:01");
    let laptop = address_hex("AA:BB:CC:DD:EE:02");
    let full =
        alloc::format!("37 01 02 {phone} 01 05 50 68 6f 6e 65 {laptop} 00 06 4c 61 70 74 6f 70 01");
    assert_eq!(
        two(&full),
        Report::PairedDevices(PairedDevices {
            devices: vec![
                PairedDevice {
                    address: "AA:BB:CC:DD:EE:01".to_string(),
                    connection: ConnectionOrder::Connected(1),
                    name: "Phone".to_string(),
                },
                PairedDevice {
                    address: "AA:BB:CC:DD:EE:02".to_string(),
                    connection: ConnectionOrder::NotConnected,
                    name: "Laptop".to_string(),
                },
            ],
            playback: PlaybackHolder::Order(1),
        })
    );
    // The notification layout is the same.
    assert_eq!(two(&full.replacen("37", "39", 1)), two(&full));
    prefixes_never_panic_and_tails_are_ignored(Table::Two, &full);
    // Every proper prefix that cuts a device short is a typed failure.
    let bytes = hex(&full);
    for cut in 2..bytes.len() - 1 {
        assert!(
            matches!(
                decode_report(Table::Two, &bytes[..cut]),
                Err(ProtoError::Malformed(_))
            ),
            "cut at {cut}"
        );
    }
    // The playback byte is a connection order, not an index. It may be absent: unknown, not an
    // error. It may also name an order no listed device has.
    let without = full.rsplit_once(' ').expect("a byte").0;
    let Report::PairedDevices(list) = two(without) else {
        panic!("a list");
    };
    assert_eq!(list.devices.len(), 2);
    assert_eq!(list.playback, PlaybackHolder::Unknown);
    assert_eq!(list.playback_device(), None);
    let Report::PairedDevices(list) = two(&alloc::format!("{without} 01")) else {
        panic!("a list");
    };
    assert_eq!(list.playback, PlaybackHolder::Order(1));
    assert_eq!(
        list.playback_device().map(|d| d.name.as_str()),
        Some("Phone")
    );
    for order in ["00", "02", "09"] {
        let Report::PairedDevices(list) = two(&alloc::format!("{without} {order}")) else {
            panic!("a list");
        };
        assert_eq!(
            list.playback,
            PlaybackHolder::Order(u8::from_str_radix(order, 16).expect("hex"))
        );
        assert_eq!(list.playback_device(), None, "order {order}");
    }
    // No devices at all.
    assert_eq!(
        two("37 01 00"),
        Report::PairedDevices(PairedDevices {
            devices: vec![],
            playback: PlaybackHolder::Unknown,
        })
    );
    // A count that promises more than the payload holds.
    is_short(Table::Two, &alloc::format!("37 01 03 {phone} 01 00 00"));
    // A name length that runs off the end.
    is_short(Table::Two, &alloc::format!("37 01 01 {phone} 01 09 41"));
    is_short(Table::Two, "37 01");
    assert_eq!(decode(Table::Two, "37 02 00 00"), Ok(None));
}

/// The shape a real WH-1000XM4 sent: three paired devices, the first connected (order 1), and a
/// final byte of `01`. The marker belongs on the connected device, not on index 1.
#[test]
fn a_real_headsets_playback_byte_is_a_connection_order() {
    let a = address_hex("AA:BB:CC:DD:EE:01");
    let b = address_hex("AA:BB:CC:DD:EE:02");
    let c = address_hex("AA:BB:CC:DD:EE:03");
    let reply = alloc::format!(
        "37 01 03 {a} 01 03 4d 61 63 {b} 00 05 50 68 6f 6e 65 {c} 00 03 54 56 31 01"
    );
    let Report::PairedDevices(list) = two(&reply) else {
        panic!("a list");
    };
    assert_eq!(list.devices.len(), 3);
    assert_eq!(list.playback, PlaybackHolder::Order(1));
    assert_eq!(list.playback_device().map(|d| d.name.as_str()), Some("Mac"));
}

/// An address that is not ASCII text is kept lossily, and a name that is not UTF-8 too.
#[test]
fn paired_devices_keep_odd_text() {
    let Report::PairedDevices(list) =
        two("39 01 01 ff fe 00 01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 03 02 ff 41 00")
    else {
        panic!("a list");
    };
    assert_eq!(list.devices[0].address.chars().count(), 17);
    assert!(list.devices[0].address.starts_with("\u{fffd}\u{fffd}"));
    assert_eq!(list.devices[0].connection, ConnectionOrder::Connected(3));
    assert_eq!(list.devices[0].name, "\u{fffd}A");
    assert_eq!(list.playback, PlaybackHolder::Order(0));
}

/// Serial number: `37 06 <len> <ascii>`.
#[test]
fn serial_number() {
    assert_eq!(
        one("37 06 05 41 42 43 44 45"),
        Report::Serial("ABCDE".to_string())
    );
    assert_eq!(decode(Table::One, "37 02 01 00"), Ok(None));
    is_short(Table::One, "37 06 05 41");
    prefixes_never_panic_and_tails_are_ignored(Table::One, "37 06 02 41 42");
}

/// Alert: `99 01 <AlertMessageType> <00 confirmation | 01 positive/negative>`.
#[test]
fn an_alert() {
    assert_eq!(
        one("99 01 08 00"),
        Report::Alert {
            message: AlertMessageType::BatteryUseIncreasesWithEqAndDsee,
            wants_answer: false
        }
    );
    assert_eq!(
        one("99 01 02 01"),
        Report::Alert {
            message: AlertMessageType::DisconnectDueToKeyAssignChange,
            wants_answer: true
        }
    );
    assert_eq!(
        one("99 01 63 01"),
        Report::Alert {
            message: AlertMessageType::Unknown(0x63),
            wants_answer: true
        }
    );
    is_short(Table::One, "99 01 08");
}

#[test]
fn the_table_follows_the_data_type() {
    assert_eq!(Table::of(DataType::DataMdr), Some(Table::One));
    assert_eq!(Table::of(DataType::ShotMdr), Some(Table::One));
    assert_eq!(Table::of(DataType::DataMdrNo2), Some(Table::Two));
    assert_eq!(Table::of(DataType::ShotMdrNo2), Some(Table::Two));
    for other in [
        DataType::Data,
        DataType::Ack,
        DataType::DataCommon,
        DataType::Other(0x55),
    ] {
        assert_eq!(Table::of(other), None);
    }
}

/// Whatever the bytes, nothing panics and nothing allocates from a count it cannot back.
#[test]
fn nothing_can_make_a_decoder_panic() {
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as u32
    };
    for _ in 0..20_000 {
        let len = (next() % 24) as usize;
        let mut payload: Vec<u8> = (0..len).map(|_| next().to_le_bytes()[0]).collect();
        // Half of them start with a command id that has a decoder.
        if let Some(first) = payload.first_mut()
            && next() % 2 == 0
        {
            const IDS: [u8; 28] = [
                0x01, 0x03, 0x05, 0x07, 0x11, 0x13, 0x15, 0x19, 0x25, 0x31, 0x33, 0x35, 0x37, 0x39,
                0x41, 0x47, 0x51, 0x57, 0x5b, 0x61, 0x67, 0x87, 0x99, 0xd1, 0xd7, 0xe7, 0xf1, 0xf7,
            ];
            *first = IDS[(next() as usize) % IDS.len()];
        }
        // The pairing layouts only read on inquired type `01`.
        if let Some(inquired) = payload.get_mut(1)
            && next() % 2 == 0
        {
            *inquired = 0x01;
        }
        for table in [Table::One, Table::Two] {
            let _ = decode_report(table, &payload);
        }
    }
}
