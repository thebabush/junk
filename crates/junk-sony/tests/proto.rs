//! The `SonyDriver` against SPEC §3.1's invariants and the link and init of Sony's app, without any
//! I/O: a scripted fake headset on the other end of the driver's outputs.
//!
//! **Every byte the fake headset says here is synthetic.** The frames are built with this
//! crate's own `Frame::encode` and the payloads are written out from the layouts of
//! Sony's own app (the topic is named on each); nothing was captured from an
//! XM4, and the function list, the capabilities and the values are made up to exercise the
//! driver, not to describe a real headset.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt::Write as _;

use junk_core::{
    Channel, ChannelSet, Driver, Duration, Input, Instant, Output, Outputs, ProtoError, ReqId,
    TimerId,
};
use junk_sony::payload::{
    AudioCodec, AutoPowerOffElementId, Battery, BatteryChargingStatus, CommonStatus,
    ConnectionMode, ConnectionOrder, ConnectionState, DseeSetting, EqEbbInquiredType, EqPresetId,
    FileTransferSupport, FunctionType, GsSettingType, GsTitle, GsValue, MdrLanguage, ModeOutTime,
    ModelSeries, NcAsmEffect, NcAsmSettingType, NcDualSingleValue, OnOff, PairingCapability,
    PairingMode, PairingModeState, PlaybackHolder, Report, StcSensitivity,
};
use junk_sony::proto::Reading;
use junk_sony::proto::{
    ACK_TIMEOUT, ACK_TIMER, Ev, MAX_RESENDS, REPLY_TIMEOUT, REPLY_TIMER, Req, Resp, SonyDriver,
    Status,
};
use junk_sony::{DataType, Frame, FrameError, RFCOMM};

type Out = Output<Resp, Ev>;

fn hex(text: &str) -> Vec<u8> {
    text.split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).expect("hex"))
        .collect()
}

fn encode(data_type: DataType, seq: u8, payload: &[u8]) -> Vec<u8> {
    Frame::new(data_type, seq, payload.to_vec())
        .encode()
        .expect("small")
}

fn decode(bytes: &[u8]) -> Frame {
    Frame::decode(bytes).unwrap_or_else(|err| panic!("{err}: {bytes:02x?}"))
}

fn step(driver: &mut SonyDriver, input: Input<Req>) -> Vec<Out> {
    let mut out = Outputs::new();
    driver.handle(input, &mut out);
    out.into_vec()
}

fn connect(driver: &mut SonyDriver) -> Vec<Out> {
    step(
        driver,
        Input::Connected {
            resolved: [RFCOMM].into_iter().collect(),
            mtu: 668,
        },
    )
}

fn request(id: u32, req: Req) -> Input<Req> {
    Input::Request {
        id: ReqId(id),
        req,
        now: Instant::ZERO,
    }
}

fn rx(bytes: &[u8]) -> Input<Req> {
    Input::Rx {
        chan: RFCOMM,
        bytes: bytes.to_vec(),
    }
}

fn tx(bytes: &[u8]) -> Out {
    Output::Tx {
        chan: RFCOMM,
        bytes: bytes.to_vec(),
    }
}

fn arm_ack() -> Out {
    Output::SetTimer {
        id: ACK_TIMER,
        after: ACK_TIMEOUT,
    }
}

fn arm_reply() -> Out {
    Output::SetTimer {
        id: REPLY_TIMER,
        after: REPLY_TIMEOUT,
    }
}

fn done(id: u32, result: Result<Resp, ProtoError>) -> Out {
    Output::Done {
        id: ReqId(id),
        result,
    }
}

/// The device's ACK of a command we sent with `seq`: `1 - seq`.
fn ack_of(seq: u8) -> Vec<u8> {
    encode(DataType::Ack, 1 - seq, &[])
}

/// The first command of init, `00 00` on table one with sequence number 0.
fn first_command() -> Vec<u8> {
    encode(DataType::DataMdr, 0, &[0x00, 0x00])
}

/// A fake headset: what it answers to each command, and nothing it was not told to say.
/// A command with no entry is acknowledged and not answered.
#[derive(Default)]
struct Headset {
    answers: HashMap<(u8, Vec<u8>), Vec<u8>>,
    /// The sequence number of the next frame the headset sends.
    seq: u8,
}

impl Headset {
    fn on(&mut self, data_type: DataType, request: &str, reply: &str) -> &mut Self {
        self.answers
            .insert((data_type.raw(), hex(request)), hex(reply));
        self
    }

    fn one(&mut self, request: &str, reply: &str) -> &mut Self {
        self.on(DataType::DataMdr, request, reply)
    }

    fn two(&mut self, request: &str, reply: &str) -> &mut Self {
        self.on(DataType::DataMdrNo2, request, reply)
    }

    /// A frame of the headset's own, with the next sequence number.
    fn frame(&mut self, data_type: DataType, payload: &[u8]) -> Vec<u8> {
        let bytes = encode(data_type, self.seq, payload);
        self.seq = 1 - self.seq;
        bytes
    }
}

/// The driver, the fake headset wired to its outputs, and everything that came out.
struct World {
    driver: SonyDriver,
    headset: Headset,
    /// Whether the headset ACKs and answers on its own.
    auto: bool,
    /// Commands the driver wrote, in order.
    sent: Vec<Frame>,
    /// ACKs the driver wrote, in order.
    acks: Vec<Frame>,
    events: Vec<Ev>,
    dones: Vec<(ReqId, Result<Resp, ProtoError>)>,
    disconnects: usize,
    /// The timers currently armed.
    timers: BTreeMap<TimerId, Duration>,
    /// Every frame on the wire, in order: `true` for the driver's writes, `false` for what
    /// the headset said.
    wire: Vec<(bool, Vec<u8>)>,
}

impl World {
    fn new(headset: Headset) -> Self {
        Self::with(SonyDriver::new(), headset)
    }

    fn with(driver: SonyDriver, headset: Headset) -> Self {
        World {
            driver,
            headset,
            auto: true,
            sent: Vec::new(),
            acks: Vec::new(),
            events: Vec::new(),
            dones: Vec::new(),
            disconnects: 0,
            timers: BTreeMap::new(),
            wire: Vec::new(),
        }
    }

    /// Feeds `input`, and everything the headset says back, until nothing more is said.
    fn feed(&mut self, input: Input<Req>) {
        let mut queue = VecDeque::from([input]);
        while let Some(input) = queue.pop_front() {
            if let Input::Rx { bytes, .. } = &input {
                self.wire.push((false, bytes.clone()));
            }
            for output in step(&mut self.driver, input) {
                match output {
                    Output::Tx { bytes, .. } => {
                        self.wire.push((true, bytes.clone()));
                        let frame = decode(&bytes);
                        if frame.data_type == DataType::Ack {
                            self.acks.push(frame);
                            continue;
                        }
                        self.sent.push(frame.clone());
                        if !self.auto {
                            continue;
                        }
                        queue.push_back(rx(&ack_of(frame.seq)));
                        let key = (frame.data_type.raw(), frame.payload.clone());
                        if let Some(reply) = self.headset.answers.get(&key).cloned() {
                            let bytes = self.headset.frame(frame.data_type, &reply);
                            queue.push_back(rx(&bytes));
                        }
                    }
                    Output::SetTimer { id, after } => {
                        self.timers.insert(id, after);
                    }
                    Output::CancelTimer(id) => {
                        self.timers.remove(&id);
                    }
                    Output::Event(ev) => self.events.push(ev),
                    Output::Done { id, result } => self.dones.push((id, result)),
                    Output::Disconnect => self.disconnects += 1,
                    Output::Subscribe(_) | Output::Read(_) => {
                        panic!("a stream subscribes to nothing")
                    }
                }
            }
        }
    }

    fn connect(&mut self) {
        self.feed(Input::Connected {
            resolved: [RFCOMM].into_iter().collect(),
            mtu: 668,
        });
    }

    fn request(&mut self, id: u32, req: Req) {
        self.feed(request(id, req));
    }

    fn rx(&mut self, bytes: &[u8]) {
        self.feed(rx(bytes));
    }

    fn timer(&mut self, id: TimerId) {
        self.timers.remove(&id);
        self.feed(Input::Timer(id));
    }

    /// The payloads of the commands sent so far, as `data type: hex`.
    fn sent_payloads(&self) -> Vec<String> {
        self.sent
            .iter()
            .map(|frame| {
                let bytes: Vec<String> = frame.payload.iter().map(|b| format!("{b:02x}")).collect();
                let table = if frame.data_type == DataType::DataMdrNo2 {
                    "0e: "
                } else {
                    ""
                };
                format!("{table}{}", bytes.join(" "))
            })
            .collect()
    }

    /// The `Status` the one request answered, which must have been answered with one.
    fn status(&self, id: u32) -> Status {
        let (_, result) = self
            .dones
            .iter()
            .find(|(got, _)| *got == ReqId(id))
            .unwrap_or_else(|| panic!("request {id} was not answered: {:?}", self.dones));
        match result {
            Ok(Resp::Status(status)) => (**status).clone(),
            other => panic!("expected a status, got {other:?}"),
        }
    }
}

/// A `D1` capability, general-settings layout: `D1 <slot> <fmt> <len> <title> <len> <description>
/// <GsSettingType> [list]`.
fn gs_capability(slot: u8, title: &str, setting: &[u8]) -> String {
    let mut bytes = vec![0xd1, slot, 0x02, u8::try_from(title.len()).expect("short")];
    bytes.extend_from_slice(title.as_bytes());
    bytes.extend_from_slice(&[0x01, b'x']);
    bytes.extend_from_slice(setting);
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The `37 01` reply of the fake headset: a connected phone that holds playback, and a
/// laptop that is paired and not connected. The final byte is the connection order of the
/// device that holds playback (1: the phone), the shape a real WH-1000XM4 sent.
fn paired_devices_reply() -> String {
    let device = |address: &str, order: u8, name: &str| {
        let mut bytes = address.as_bytes().to_vec();
        bytes.push(order);
        bytes.push(u8::try_from(name.len()).expect("short"));
        bytes.extend_from_slice(name.as_bytes());
        bytes
    };
    let mut bytes = vec![0x37, 0x01, 0x02];
    bytes.extend(device("AA:BB:CC:DD:EE:01", 1, "Phone"));
    bytes.extend(device("AA:BB:CC:DD:EE:02", 0, "Laptop"));
    bytes.push(0x01);
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The functions the fake headset lists: every one the driver reads, plus three it must not
/// ask the capability of (`14`, `30` is asked only for the serial, `B1`), plus `C1`, whose
/// action log it must not switch on.
const FUNCTIONS: &str = "11 13 12 17 62 51 52 81 e1 e2 f3 f4 f5 f6 d2 d3 39 38 30 14 b1 c1";

/// A headset that answers every question of init and of `Req::Status`. The topics
/// are named on the lines.
fn full_headset() -> Headset {
    let mut h = Headset::default();
    // Init steps 1 to 4.
    h.one("00 00", "01 00 40 10");
    h.one("02 00", "03 00 07 04 41 42 43 44");
    h.one("04 01", "05 01 0a 57 48 2d 31 30 30 30 58 4d 34");
    h.one("04 02", "05 02 05 31 2e 32 2e 33");
    h.one("04 03", "05 03 10 02");
    h.one("06 00", &format!("07 00 16 {FUNCTIONS}"));
    // Init step 5, in the app's order; the language byte is English.
    h.one("50 01 01", "51 01 06 15 02 00 00 a0 06 43 75 73 74 6f 6d");
    h.one("50 02 01", "51 02 fb 05");
    h.one("60 02", "61 02 02 01 01 02 00 0a 01 05");
    h.one("80 01", "81 01 00 01 00 01 00");
    h.one("e0 01", "e1 01 00");
    h.one("e0 02", "e1 02 00 00");
    h.one("f0 03", "f1 03 00");
    h.one("f0 04", "f1 04 03 00 01 11");
    h.one("f0 05", "f1 05 00 01 00");
    h.one("f0 06", "f1 06 01 00 00 00 00");
    h.one(
        "d0 d2 01",
        &gs_capability(0xd2, "TOUCH_PANEL_SETTING", &[0x01]),
    );
    h.one(
        "d0 d3 01",
        &gs_capability(
            0xd3,
            "MULTIPOINT_SETTING",
            &[
                0x02, 0x02, 0x01, 0x03, b'O', b'f', b'f', 0x01, b'x', 0x01, 0x02, b'O', b'n', 0x01,
                b'y',
            ],
        ),
    );
    // `31 01 08 02 01` is what a real WH-1000XM4 sent.
    h.two("30 01", "31 01 08 02 01");
    h.two("40 01", "41 01 01 01 02 01 0b");
    h.two("46 01 05", "47 01 05 01");
    // The read phase, in the order the driver asks them.
    h.one("10 00", "11 00 50 00");
    h.one("18 00", "19 00 10");
    h.one("14 00", "15 00 02 01");
    h.one("24 01", "25 01 01 01");
    h.one("66 02", "67 02 01 02 00 01 00 05");
    h.one("56 01", "57 01 a0 06 0a 0a 0a 0a 0a 0a");
    h.one(
        "5a 01",
        "5b 01 06 10 00 01 01 01 90 02 00 01 02 00 02 02 00 06 02 00 10",
    );
    h.one("56 02", "57 02 fd");
    h.one("e6 02", "e7 02 00 01");
    h.one("e6 01", "e7 01 00 01");
    h.two("46 01 01", "47 01 01 01");
    h.two("46 01 02", "47 01 02 01");
    h.two("32 01", "33 01 00 00");
    h.two("36 01", &paired_devices_reply());
    h.one("f6 03", "f7 03 00 01");
    h.one("f6 04", "f7 04 01 10 02");
    h.one("f6 05", "f7 05 00 01");
    h.one("fa 05", "fb 05 00 01 01 02");
    h.one("f6 06", "f7 06 02 00 31");
    h.one("d6 d2", "d7 d2 01 01");
    h.one("d6 d3", "d7 d3 02 01");
    h.one("86 01", "87 01 01 05 01 08");
    h.one("36 06", "37 06 05 41 42 43 44 45");
    h
}

/// The commands init sends for [`FUNCTIONS`], init steps 1 to 5. `04 04` is absent because
/// the version, `0x4010`, is under `0x5000`; `1C` (BLE set-up), `36 07` (firmware update) and
/// `B0 01` (training mode) are absent because they are deliberately not asked; `C4 01 00`
/// (the action log) is absent because it makes the headset stream JSON.
const INIT_COMMANDS: [&str; 21] = [
    "00 00",
    "02 00",
    "04 01",
    "04 02",
    "04 03",
    "06 00",
    "50 01 01",
    "50 02 01",
    "60 02",
    "80 01",
    "e0 01",
    "e0 02",
    "f0 03",
    "f0 04",
    "f0 05",
    "f0 06",
    "d0 d2 01",
    "d0 d3 01",
    "0e: 30 01",
    "0e: 40 01",
    "0e: 46 01 05",
];

const STATUS_COMMANDS: [&str; 23] = [
    "10 00",
    "18 00",
    "14 00",
    "24 01",
    "66 02",
    "56 01",
    "5a 01",
    "56 02",
    "e6 02",
    "e6 01",
    "0e: 46 01 01",
    "0e: 46 01 02",
    "0e: 32 01",
    "0e: 36 01",
    "f6 03",
    "f6 04",
    "f6 05",
    "fa 05",
    "f6 06",
    "d6 d2",
    "d6 d3",
    "86 01",
    "36 06",
];

#[test]
fn connecting_starts_init_on_its_own_with_the_first_command_at_sequence_zero() {
    let mut driver = SonyDriver::new();
    assert_eq!(
        connect(&mut driver),
        [tx(&first_command()), arm_ack()],
        "00 00 on table one, seq 0, no request needed"
    );
}

/// The whole of init and the read phase, against a headset that answers
/// everything.
#[test]
fn init_and_a_status_read_run_end_to_end() {
    let mut world = World::new(full_headset());
    world.connect();
    world.request(1, Req::Status);

    let mut expected: Vec<&str> = INIT_COMMANDS.to_vec();
    expected.extend(STATUS_COMMANDS);
    assert_eq!(world.sent_payloads(), expected);
    // Sequence numbers toggle: each command is sent with the number its predecessor's ACK
    // carried.
    for (i, frame) in world.sent.iter().enumerate() {
        assert_eq!(
            frame.seq,
            u8::try_from(i % 2).expect("0 or 1"),
            "command {i}"
        );
    }
    // Every frame of the headset's was ACKed, with `1 - seq`, immediately.
    assert_eq!(world.acks.len(), world.sent.len());
    for (i, ack) in world.acks.iter().enumerate() {
        assert_eq!(ack.data_type, DataType::Ack);
        assert_eq!(ack.seq, 1 - u8::try_from(i % 2).expect("0 or 1"), "ack {i}");
        assert_eq!(ack.payload, [] as [u8; 0]);
    }
    assert!(world.timers.is_empty(), "{:?}", world.timers);
    assert_eq!(world.disconnects, 0);
    assert_eq!(
        world.events,
        [Ev::ProtocolVersion {
            version: 0x4010,
            supported: true
        }]
    );

    let status = world.status(1);
    assert_device_and_capabilities(&status);
    assert_reads(&status);
}

/// What init found, in the status of a headset that lists everything.
fn assert_device_and_capabilities(status: &Status) {
    // Device info.
    assert_eq!(status.device.protocol_version, Reading::Value(0x4010));
    assert_eq!(
        status.device.model,
        Reading::Value("WH-1000XM4".to_string())
    );
    assert_eq!(status.device.firmware, Reading::Value("1.2.3".to_string()));
    assert_eq!(
        status.device.model_info.value().map(|m| m.series),
        Some(ModelSeries::ExtraBass)
    );
    assert_eq!(
        status
            .device
            .capability_info
            .value()
            .map(|c| c.unique_id.as_str()),
        Some("ABCD")
    );
    assert_eq!(
        status.device.guidance_categories,
        Reading::NotSupported,
        "`04 04` is asked from 0x5000 only"
    );
    let functions = status.device.functions.value().expect("a function list");
    assert_eq!(functions.len(), 22);
    assert_eq!(functions[0], FunctionType::BatteryLevel);

    // Capabilities.
    let nc = status.capabilities.nc_asm.value().expect("nc capability");
    assert_eq!(nc.asm_step(junk_sony::payload::AsmId::Normal), Some(10));
    assert_eq!(nc.asm_step(junk_sony::payload::AsmId::Voice), Some(5));
    let eq = status.capabilities.eq.value().expect("eq capability");
    assert_eq!(
        (eq.band_count, eq.level_steps, eq.presets.len()),
        (6, 21, 2)
    );
    assert_eq!(
        status.capabilities.ebb.value().map(|e| (e.min, e.max)),
        Some((-5, 5))
    );
    assert_eq!(
        status.capabilities.auto_power_off.value().map(Vec::len),
        Some(3)
    );
    assert_eq!(
        status
            .capabilities
            .voice_guidance
            .value()
            .map(|v| v.languages.len()),
        Some(2)
    );
    assert_eq!(
        status.capabilities.pairing,
        Reading::Value(PairingCapability {
            max_paired: 8,
            max_connected: 2,
            file_transfer: FileTransferSupport::Impossible,
        })
    );
    // 22 listed, of which these have a capability GET that was sent and kept.
    let raw: Vec<u8> = status.raw_replies.iter().map(|r| r.payload[0]).collect();
    assert_eq!(raw.len(), 21, "one raw reply per init step: {raw:02x?}");
    assert_eq!(raw[0], 0x01);
    assert_eq!(raw[5], 0x07);
    assert_eq!(status.raw_replies[18].data_type, DataType::DataMdrNo2);
    assert_eq!(status.raw_replies[18].payload, hex("31 01 08 02 01"));
    assert_eq!(status.raw_replies[19].data_type, DataType::DataMdrNo2);

    // General settings: the slots come with their titles.
    assert_eq!(status.general_settings.len(), 2);
    let touch = &status.general_settings[0];
    assert_eq!(touch.slot, 0xd2);
    let capability = touch.capability.value().expect("capability");
    assert_eq!(capability.title.title(), GsTitle::TouchPanelSetting);
    assert_eq!(capability.setting_type, GsSettingType::Boolean);
    assert_eq!(touch.value, Reading::Value(GsValue::Boolean(OnOff::On)));
    let multipoint = &status.general_settings[1];
    assert_eq!(multipoint.slot, 0xd3);
    let capability = multipoint.capability.value().expect("capability");
    assert_eq!(capability.title.title(), GsTitle::MultipointSetting);
    assert_eq!(capability.items.len(), 2);
    assert_eq!(multipoint.value, Reading::Value(GsValue::List(1)));
}

/// What the read phase found.
fn assert_reads(status: &Status) {
    // Reads.
    assert_eq!(
        status.battery,
        Reading::Value(Battery {
            level: 0x50,
            charging: BatteryChargingStatus::NotCharging
        })
    );
    assert_eq!(status.battery_left_right, Reading::NotSupported);
    assert_eq!(status.battery_cradle, Reading::NotSupported);
    assert_eq!(status.codec, Reading::Value(AudioCodec::Ldac));
    assert!(status.upscaling_indicator.value().is_some());
    assert_eq!(
        status.connection_status.value().map(|c| (c.left, c.right)),
        Some((ConnectionState::Connected, ConnectionState::Connected))
    );
    let junk_sony::payload::NcAsmState::NoiseCancellingAndAmbient {
        effect,
        nc_setting_type,
        nc_value,
        asm_level,
        ..
    } = status.nc_asm.value().expect("nc/asm").clone()
    else {
        panic!("type 02");
    };
    assert_eq!(effect, NcAsmEffect::On);
    assert_eq!(nc_setting_type, NcAsmSettingType::DualSingleOff);
    assert_eq!(nc_value, NcDualSingleValue::Off);
    assert_eq!(asm_level, 5);
    let eq = status.eq.value().expect("eq");
    assert_eq!(eq.kind, EqEbbInquiredType::PresetEq);
    assert_eq!(eq.preset, EqPresetId::Custom);
    assert_eq!(eq.values.len(), 6);
    assert_eq!(
        status
            .eq_bands
            .value()
            .and_then(junk_sony::payload::EqBands::clear_bass_index),
        Some(0)
    );
    assert_eq!(status.ebb, Reading::Value(-3));
    assert_eq!(status.dsee, Reading::Value(DseeSetting::Auto));
    assert_eq!(
        status.connection_mode,
        Reading::Value(ConnectionMode::ConnectionQualityPrior)
    );
    assert_eq!(status.voice_guidance, Reading::Value(OnOff::On));
    assert_eq!(
        status.voice_guidance_language,
        Reading::Value(MdrLanguage::English)
    );
    assert_eq!(
        status.pairing_mode,
        Reading::Value(PairingModeState {
            mode: PairingMode::Normal,
            status: CommonStatus::Enable,
        })
    );
    let paired = status.paired_devices.value().expect("paired devices");
    assert_eq!(paired.devices.len(), 2);
    assert_eq!(paired.devices[0].address, "AA:BB:CC:DD:EE:01");
    assert_eq!(paired.devices[0].connection, ConnectionOrder::Connected(1));
    assert_eq!(paired.devices[0].name, "Phone");
    assert_eq!(paired.devices[1].connection, ConnectionOrder::NotConnected);
    assert_eq!(paired.playback, PlaybackHolder::Order(1));
    assert_eq!(
        paired.playback_device().map(|d| d.name.as_str()),
        Some("Phone")
    );
    assert_eq!(status.pause_when_taken_off, Reading::Value(OnOff::On));
    let apo = status.auto_power_off.value().expect("auto power off");
    assert_eq!(apo.active, AutoPowerOffElementId::WhenRemovedFromEars);
    assert_eq!(apo.timer, AutoPowerOffElementId::Minutes60);
    assert_eq!(status.speak_to_chat, Reading::Value(OnOff::On));
    let stc = status.speak_to_chat_config.value().expect("stc config");
    assert_eq!(stc.sensitivity, StcSensitivity::High);
    assert_eq!(stc.timeout, ModeOutTime::Slow);
    assert_eq!(status.assignable_settings.value().map(Vec::len), Some(2));
    assert!(status.nc_optimizer.value().is_some());
    assert_eq!(status.serial, Reading::Value("ABCDE".to_string()));
}

/// A headset that lists pairing management and says nothing about it, or something unreadable.
#[test]
fn pairing_reads_that_get_no_reply_or_a_bad_one_say_so() {
    let mut h = full_headset();
    h.answers
        .remove(&(DataType::DataMdrNo2.raw(), hex("32 01")));
    h.two("36 01", "37 01 02 41 42");
    let mut world = World::new(h);
    world.connect();
    world.request(1, Req::Status);
    world.timer(REPLY_TIMER);
    let status = world.status(1);
    assert_eq!(status.pairing_mode, Reading::NoReply);
    assert_eq!(status.paired_devices, Reading::Malformed);
    assert!(status.serial.value().is_some(), "the session went on");
    assert!(world.events.iter().any(
        |ev| matches!(ev, Ev::Unparsed { bytes } if decode(bytes).payload == hex("37 01 02 41 42"))
    ));
}

/// Without the trailing playback byte the list still reads, with the holder unknown.
#[test]
fn a_paired_device_list_without_the_playback_byte_reads() {
    let mut h = full_headset();
    let reply = paired_devices_reply();
    h.two("36 01", reply.strip_suffix(" 01").expect("a playback byte"));
    let mut world = World::new(h);
    world.connect();
    world.request(1, Req::Status);
    let status = world.status(1);
    let paired = status.paired_devices.value().expect("paired devices");
    assert_eq!(paired.devices.len(), 2);
    assert_eq!(paired.playback, PlaybackHolder::Unknown);
}

/// The pairing reads are `32 01` and `36 01` only: nothing that connects, disconnects,
/// unpairs or enters pairing mode (`34`, `3C`) is ever sent, and the capability read is
/// asked once, in init.
#[test]
fn the_pairing_reads_never_send_a_write() {
    let mut world = World::new(full_headset());
    world.connect();
    world.request(1, Req::Status);
    world.request(2, Req::Status);
    let peripheral: Vec<&Frame> = world
        .sent
        .iter()
        .filter(|f| f.data_type == DataType::DataMdrNo2 && (0x30..=0x3d).contains(&f.payload[0]))
        .collect();
    let payloads: Vec<&[u8]> = peripheral.iter().map(|f| f.payload.as_slice()).collect();
    assert_eq!(
        payloads,
        [
            &[0x30, 0x01][..],
            &[0x32, 0x01],
            &[0x36, 0x01],
            &[0x32, 0x01],
            &[0x36, 0x01]
        ]
    );
}

/// A second `Req::Status` re-reads: init does not run again.
#[test]
fn a_second_status_request_reads_again_without_init() {
    let mut world = World::new(full_headset());
    world.connect();
    world.request(1, Req::Status);
    let after_first = world.sent.len();
    world.request(2, Req::Status);
    assert_eq!(world.sent.len() - after_first, STATUS_COMMANDS.len());
    assert_eq!(world.sent_payloads()[after_first], "10 00");
    assert_eq!(world.status(2).device, world.status(1).device);
}

#[test]
fn a_request_made_before_init_is_done_waits_for_it() {
    let mut world = World::new(full_headset());
    world.auto = false;
    world.connect();
    world.request(1, Req::Status);
    assert_eq!(world.sent.len(), 1, "only init's first command is out");
    assert_eq!(
        world.dones,
        [] as [(
            junk_core::ReqId,
            std::result::Result<junk_sony::proto::Resp, junk_core::ProtoError>
        ); 0]
    );
}

#[test]
fn the_requested_language_goes_out_in_the_capability_gets() {
    // The headset answers the capability GETs that carry a language in Japanese.
    let mut h = Headset::default();
    for ((data_type, mut request), reply) in full_headset().answers {
        if matches!(request[0], 0x40 | 0x50 | 0xd0) && request.len() == 3 && data_type == 0x0c {
            request[2] = 0x0b;
        }
        h.answers.insert((data_type, request), reply);
    }
    let mut world = World::with(SonyDriver::with_language(MdrLanguage::Japanese), h);
    world.connect();
    let payloads = world.sent_payloads();
    assert!(payloads.contains(&"50 01 0b".to_string()), "{payloads:?}");
    assert!(payloads.contains(&"d0 d2 0b".to_string()), "{payloads:?}");
}

/// With version `0x5000`, `04 04` is asked too.
#[test]
fn guidance_categories_are_asked_from_protocol_version_0x5000() {
    let mut h = full_headset();
    h.one("00 00", "01 00 50 00");
    h.one("04 04", "05 04 02 01 02");
    let mut world = World::new(h);
    world.connect();
    world.request(1, Req::Status);
    assert_eq!(world.sent_payloads()[5], "04 04");
    assert_eq!(
        world.status(1).device.guidance_categories,
        Reading::Value(vec![1, 2])
    );
}

/// An ACK carries `1 - txSeq`: it is adopted and releases the waiter; one that carries the
/// current `txSeq` is "invalid ack, ignore" and the step goes on waiting.
#[test]
fn an_ack_is_adopted_and_an_invalid_ack_is_ignored() {
    let mut driver = SonyDriver::new();
    connect(&mut driver);
    // seq 0 is outstanding; an ACK of seq 0 is invalid.
    assert_eq!(step(&mut driver, rx(&encode(DataType::Ack, 0, &[]))), []);
    // `1 - 0` is the one: adopted, and the reply timer starts.
    assert_eq!(
        step(&mut driver, rx(&encode(DataType::Ack, 1, &[]))),
        [Output::CancelTimer(ACK_TIMER), arm_reply()]
    );
    // The reply releases the step, ACKs it with `1 - 0`, and the next command goes out with
    // the seq the ACK carried.
    let reply = encode(DataType::DataMdr, 0, &hex("01 00 40 10"));
    let outs = step(&mut driver, rx(&reply));
    assert_eq!(
        outs,
        [
            tx(&encode(DataType::Ack, 1, &[])),
            Output::CancelTimer(REPLY_TIMER),
            Output::Event(Ev::ProtocolVersion {
                version: 0x4010,
                supported: true
            }),
            tx(&encode(DataType::DataMdr, 1, &hex("02 00"))),
            arm_ack(),
        ]
    );
}

/// An ACK need not carry 0 or 1 for the driver to adopt it, as the app does.
#[test]
fn an_ack_whose_seq_differs_is_adopted_whatever_it_is() {
    let mut driver = SonyDriver::new();
    connect(&mut driver);
    step(&mut driver, rx(&encode(DataType::Ack, 7, &[])));
    let reply = encode(DataType::DataMdr, 0, &hex("01 00 40 10"));
    let outs = step(&mut driver, rx(&reply));
    assert_eq!(outs.last(), Some(&arm_ack()), "{outs:?}");
    assert!(outs.contains(&tx(&encode(DataType::DataMdr, 7, &hex("02 00")))));
}

#[test]
fn no_ack_in_750_ms_resends_the_identical_frame() {
    let mut driver = SonyDriver::new();
    connect(&mut driver);
    assert_eq!(ACK_TIMEOUT, Duration::from_millis(750));
    for _ in 0..3 {
        assert_eq!(
            step(&mut driver, Input::Timer(ACK_TIMER)),
            [tx(&first_command()), arm_ack()],
            "same bytes, same seq"
        );
    }
}

/// Ten resends, eleven transmissions; the twelfth tick gives up: the request in flight
/// fails with a timeout, the link is dropped.
#[test]
fn ten_resends_then_the_request_times_out_and_the_link_is_dropped() {
    let mut world = World::new(Headset::default());
    // Init, against a headset that has nothing to say, and then a raw request.
    world.auto = false;
    world.connect();
    // Walk init to its end by acknowledging and abandoning each step: ACK, then the reply
    // timer.
    let mut seq = 0u8;
    loop {
        let before = world.sent.len();
        world.rx(&ack_of(seq));
        seq = 1 - seq;
        world.timer(REPLY_TIMER);
        if world.sent.len() == before {
            break;
        }
        if world.driver.init_finished() {
            break;
        }
    }
    assert!(world.driver.init_finished());
    world.request(
        9,
        Req::Raw {
            data_type: DataType::DataMdr,
            payload: hex("10 00"),
        },
    );
    let written = world.sent.len();
    let command = world.sent.last().expect("the raw command").clone();
    assert_eq!(command.payload, hex("10 00"));

    for resend in 1..=MAX_RESENDS {
        world.timer(ACK_TIMER);
        assert_eq!(world.sent.len(), written + usize::from(resend));
        assert_eq!(world.sent.last(), Some(&command), "resend {resend}");
        assert_eq!(
            world.dones,
            [] as [(
                junk_core::ReqId,
                std::result::Result<junk_sony::proto::Resp, junk_core::ProtoError>
            ); 0]
        );
    }
    assert_eq!(world.disconnects, 0);
    // 11 transmissions in all; the next expiry gives up.
    world.timer(ACK_TIMER);
    assert_eq!(world.sent.len(), written + usize::from(MAX_RESENDS));
    assert_eq!(world.dones, [(ReqId(9), Err(ProtoError::Timeout))]);
    assert_eq!(world.disconnects, 1);
    // The driver is down: a request is refused and the link's own `Disconnected` is harmless.
    world.request(
        10,
        Req::Raw {
            data_type: DataType::DataMdr,
            payload: vec![0],
        },
    );
    assert_eq!(world.dones[1], (ReqId(10), Err(ProtoError::Disconnected)));
    world.feed(Input::Disconnected);
    assert_eq!(world.dones.len(), 2);
}

/// If init is what the wire gave up on, the request waiting behind it fails with the link,
/// and there is still a `Disconnect`.
#[test]
fn giving_up_during_init_fails_the_queued_request_with_the_link() {
    let mut world = World::new(Headset::default());
    world.auto = false;
    world.connect();
    world.request(5, Req::Status);
    for _ in 0..MAX_RESENDS {
        world.timer(ACK_TIMER);
    }
    assert_eq!(
        world.dones,
        [] as [(
            junk_core::ReqId,
            std::result::Result<junk_sony::proto::Resp, junk_core::ProtoError>
        ); 0]
    );
    world.timer(ACK_TIMER);
    assert_eq!(world.dones, [(ReqId(5), Err(ProtoError::Disconnected))]);
    assert_eq!(world.disconnects, 1);
    assert_eq!(world.sent.len(), 1 + usize::from(MAX_RESENDS));
}

/// A device frame with the sequence number the driver last processed is a retransmission:
/// `ACK`ed again, not processed again.
#[test]
fn a_duplicate_rx_seq_is_acked_but_not_processed_twice() {
    let mut driver = SonyDriver::new();
    connect(&mut driver);
    let news = encode(DataType::DataMdr, 0, &hex("13 00 3c 00"));
    let ack = tx(&encode(DataType::Ack, 1, &[]));
    let first = step(&mut driver, rx(&news));
    assert_eq!(first.len(), 2);
    assert_eq!(first[0], ack, "the ACK comes before anything else");
    assert!(matches!(
        first[1],
        Output::Event(Ev::Report(Report::Battery(_)))
    ));
    // The same seq again: ACKed, and nothing else.
    assert_eq!(step(&mut driver, rx(&news)), std::slice::from_ref(&ack));
    assert_eq!(step(&mut driver, rx(&news)), std::slice::from_ref(&ack));
    // The other seq is new.
    let next = encode(DataType::DataMdr, 1, &hex("13 00 3b 00"));
    let outs = step(&mut driver, rx(&next));
    assert_eq!(outs[0], tx(&encode(DataType::Ack, 0, &[])));
    assert!(matches!(
        outs[1],
        Output::Event(Ev::Report(Report::Battery(_)))
    ));
}

#[test]
fn a_frame_that_fails_to_decode_is_dropped_without_an_ack_and_surfaced() {
    let mut driver = SonyDriver::new();
    connect(&mut driver);
    let good = encode(DataType::DataMdr, 0, &hex("13 00 3c 00"));

    // Checksum: flip a payload byte.
    let mut bad = good.clone();
    bad[8] ^= 0x01;
    let outs = step(&mut driver, rx(&bad));
    let [Output::Event(Ev::Dropped { error, bytes })] = &outs[..] else {
        panic!("{outs:?}");
    };
    assert!(matches!(error, FrameError::Checksum { .. }), "{error:?}");
    assert_eq!(bytes, &bad);

    // Length, markers, escape, nothing at all: each is dropped, and not one is ACKed.
    let mut length = good.clone();
    length[5] = 0x09;
    let mut no_end = good.clone();
    no_end.pop();
    let escape_last = vec![0x3e, 0x0c, 0x00, 0x3d, 0x3c];
    for bytes in [length, no_end, escape_last, vec![], vec![0x3e]] {
        let outs = step(&mut driver, rx(&bytes));
        assert!(
            matches!(&outs[..], [Output::Event(Ev::Dropped { .. })]),
            "{bytes:02x?}: {outs:?}"
        );
    }
    // The dropped frame did not use up seq 0: the retransmission is processed.
    let outs = step(&mut driver, rx(&good));
    assert_eq!(outs.len(), 2);
    assert!(matches!(
        outs[1],
        Output::Event(Ev::Report(Report::Battery(_)))
    ));
}

/// An unsolicited frame while a request is in flight is `ACK`ed, reported, and not taken for
/// the reply; the reply that follows is still the reply.
#[test]
fn an_unsolicited_frame_mid_step_is_acked_and_not_mistaken_for_the_reply() {
    let mut driver = SonyDriver::new();
    connect(&mut driver);
    step(&mut driver, rx(&encode(DataType::Ack, 1, &[])));
    // Waiting for the `01` reply of init step 1. A battery notification arrives instead.
    let news = encode(DataType::DataMdr, 0, &hex("13 00 3c 00"));
    let outs = step(&mut driver, rx(&news));
    assert_eq!(outs.len(), 2, "{outs:?}");
    assert_eq!(outs[0], tx(&encode(DataType::Ack, 1, &[])));
    assert!(matches!(
        outs[1],
        Output::Event(Ev::Report(Report::Battery(_)))
    ));
    // Still waiting: no next command went out and the reply timer was not touched.
    assert!(
        !outs
            .iter()
            .any(|o| matches!(o, Output::Tx { .. } if *o != outs[0]))
    );
    // The real reply (device seq 1, since 0 was the notification) is taken.
    let reply = encode(DataType::DataMdr, 1, &hex("01 00 40 10"));
    let outs = step(&mut driver, rx(&reply));
    assert!(outs.contains(&tx(&encode(DataType::DataMdr, 1, &hex("02 00")))));
}

/// A reply that arrives before the ACK of its command is held until the ACK, so the next
/// command never goes out under a sequence number the headset has not released.
#[test]
fn a_reply_that_beats_its_ack_waits_for_the_ack() {
    let mut driver = SonyDriver::new();
    connect(&mut driver);
    let reply = encode(DataType::DataMdr, 0, &hex("01 00 40 10"));
    let outs = step(&mut driver, rx(&reply));
    assert_eq!(
        outs,
        [tx(&encode(DataType::Ack, 1, &[]))],
        "ACKed, and nothing more"
    );
    let outs = step(&mut driver, rx(&encode(DataType::Ack, 1, &[])));
    assert_eq!(outs[0], Output::CancelTimer(ACK_TIMER));
    assert!(outs.contains(&tx(&encode(DataType::DataMdr, 1, &hex("02 00")))));
}

/// A step the headset does not answer fails with a timeout of its own, and the session goes
/// on to the next one; the reading says so.
#[test]
fn a_step_with_no_reply_does_not_hang_the_session() {
    let mut h = full_headset();
    h.answers.remove(&(DataType::DataMdr.raw(), hex("18 00")));
    h.answers.remove(&(DataType::DataMdr.raw(), hex("f0 04")));
    let mut world = World::new(h);
    world.connect();
    world.request(1, Req::Status);
    // Stuck at the first unanswered step: ACKed, the reply timer running.
    assert_eq!(
        world.dones,
        [] as [(
            junk_core::ReqId,
            std::result::Result<junk_sony::proto::Resp, junk_core::ProtoError>
        ); 0]
    );
    assert_eq!(world.timers.get(&REPLY_TIMER), Some(&REPLY_TIMEOUT));
    assert_eq!(
        world.sent_payloads().last().map(String::as_str),
        Some("f0 04")
    );
    world.timer(REPLY_TIMER);
    // Init goes on: the rest of the capabilities, then the status read, and stuck again at
    // the codec.
    assert_eq!(
        world.sent_payloads().last().map(String::as_str),
        Some("18 00")
    );
    world.timer(REPLY_TIMER);
    let status = world.status(1);
    assert_eq!(status.codec, Reading::NoReply);
    assert_eq!(status.capabilities.auto_power_off, Reading::NoReply);
    assert_eq!(status.battery.value().map(|b| b.level), Some(0x50));
    assert_eq!(status.serial, Reading::Value("ABCDE".to_string()));
    assert_eq!(world.disconnects, 0);
}

#[test]
fn an_unlisted_function_is_not_supported_and_not_asked() {
    let mut h = full_headset();
    h.one("06 00", "07 00 02 11 13");
    let mut world = World::new(h);
    world.connect();
    world.request(1, Req::Status);
    let status = world.status(1);
    assert!(status.battery.value().is_some());
    assert!(status.codec.value().is_some());
    assert_eq!(status.nc_asm, Reading::NotSupported);
    assert_eq!(status.eq, Reading::NotSupported);
    assert_eq!(status.speak_to_chat, Reading::NotSupported);
    assert_eq!(status.capabilities.pairing, Reading::NotSupported);
    assert_eq!(status.pairing_mode, Reading::NotSupported);
    assert_eq!(status.paired_devices, Reading::NotSupported);
    assert_eq!(
        status.general_settings,
        [] as [junk_sony::proto::GeneralSetting; 0]
    );
    assert_eq!(
        world.sent_payloads(),
        [
            "00 00", "02 00", "04 01", "04 02", "04 03", "06 00", "10 00", "18 00"
        ]
    );
}

#[test]
fn no_function_list_means_nothing_was_asked_and_every_feature_reads_no_reply() {
    let mut h = full_headset();
    h.answers.remove(&(DataType::DataMdr.raw(), hex("06 00")));
    let mut world = World::new(h);
    world.connect();
    world.request(1, Req::Status);
    world.timer(REPLY_TIMER);
    let status = world.status(1);
    assert_eq!(status.device.functions, Reading::NoReply);
    assert_eq!(status.battery, Reading::NoReply);
    assert_eq!(status.serial, Reading::NoReply);
    assert_eq!(status.pairing_mode, Reading::NoReply);
    assert_eq!(status.paired_devices, Reading::NoReply);
    assert_eq!(status.eq, Reading::NoReply);
    assert_eq!(
        world.sent.len(),
        6,
        "init up to the function list, and nothing after"
    );
}

#[test]
fn an_unsupported_protocol_version_fails_init_and_surfaces_the_version() {
    let mut h = full_headset();
    h.one("00 00", "01 00 12 34");
    let mut world = World::new(h);
    world.connect();
    world.request(1, Req::Status);
    assert_eq!(
        world.events,
        [Ev::ProtocolVersion {
            version: 0x1234,
            supported: false
        }]
    );
    let Some((_, Err(ProtoError::Unsupported(why)))) = world.dones.first() else {
        panic!("{:?}", world.dones);
    };
    assert!(why.contains("protocol version"), "{why}");
    // Nothing further was asked, and later requests are refused with the same error.
    assert_eq!(world.sent_payloads(), ["00 00"]);
    world.request(
        2,
        Req::Raw {
            data_type: DataType::DataMdr,
            payload: vec![0],
        },
    );
    assert!(matches!(
        world.dones[1],
        (ReqId(2), Err(ProtoError::Unsupported(_)))
    ));
    // The link is left alone, and what the headset still says is still ACKed.
    assert_eq!(world.disconnects, 0);
    let before = world.acks.len();
    world.rx(&encode(DataType::DataMdr, 1, &hex("13 00 3c 00")));
    assert_eq!(world.acks.len(), before + 1);
}

#[test]
fn every_whitelisted_version_is_accepted_and_the_neighbours_are_not() {
    for version in [
        0x1000_u16, 0x2000, 0x3000, 0x4000, 0x4010, 0x5000, 0x6000, 0x7000, 0x7010,
    ] {
        assert!(junk_sony::proto::SUPPORTED_PROTOCOL_VERSIONS.contains(&version));
    }
    // The decimal reading of the version list is not it: 4010 and 5000 decimal are not
    // in the app's array, which holds hexadecimal values.
    for version in [4010_u16, 5000, 0x0fff, 0x4011, 0x7011, 0] {
        assert!(!junk_sony::proto::SUPPORTED_PROTOCOL_VERSIONS.contains(&version));
    }
}

#[test]
fn a_disconnect_fails_everything_and_resets_the_link_state() {
    let mut world = World::new(full_headset());
    world.auto = false;
    world.connect();
    world.request(1, Req::Status);
    world.request(
        2,
        Req::Raw {
            data_type: DataType::DataMdr,
            payload: vec![0x10, 0x00],
        },
    );
    // Device frame seq 0 processed, so `last_rx` is 0.
    world.rx(&encode(DataType::DataMdr, 0, &hex("13 00 3c 00")));
    assert_eq!(world.events.len(), 1);
    world.feed(Input::Disconnected);
    assert_eq!(
        world.dones,
        [
            (ReqId(1), Err(ProtoError::Disconnected)),
            (ReqId(2), Err(ProtoError::Disconnected)),
        ]
    );
    assert!(world.timers.is_empty(), "{:?}", world.timers);
    // And a request without a link is refused rather than queued.
    world.request(3, Req::Status);
    assert_eq!(world.dones[2], (ReqId(3), Err(ProtoError::Disconnected)));
    // Frames with no link are not a driver's business.
    let before = world.events.len();
    world.rx(&encode(DataType::DataMdr, 1, &hex("13 00 3c 00")));
    assert_eq!(world.events.len(), before);
    assert_eq!(world.acks.len(), 1, "only the one before the disconnect");

    // A reconnect starts from nothing: init again from `00 00` at seq 0, and the same device
    // seq 0 that was a duplicate before is news now.
    world.connect();
    assert_eq!(
        world.sent.last().map(|f| (f.seq, f.payload.clone())),
        Some((0, hex("00 00")))
    );
    world.rx(&encode(DataType::DataMdr, 0, &hex("13 00 3c 00")));
    assert_eq!(world.events.len(), before + 1);
}

#[test]
fn a_connect_on_a_live_link_is_a_new_link() {
    let mut world = World::new(full_headset());
    world.auto = false;
    world.connect();
    world.request(1, Req::Status);
    world.connect();
    assert_eq!(world.dones, [(ReqId(1), Err(ProtoError::Disconnected))]);
    assert_eq!(world.sent.len(), 2);
    assert_eq!(world.sent[1].seq, 0);
}

#[test]
fn a_link_without_the_byte_stream_is_refused() {
    let mut driver = SonyDriver::new();
    let outs = step(
        &mut driver,
        Input::Connected {
            resolved: ChannelSet::EMPTY,
            mtu: 668,
        },
    );
    let missing: ChannelSet = [RFCOMM].into_iter().collect();
    assert_eq!(
        outs,
        [
            Output::Event(Ev::MissingChannels(missing)),
            Output::Disconnect
        ]
    );
    assert_eq!(
        step(&mut driver, request(1, Req::Status)),
        [done(1, Err(ProtoError::Disconnected))]
    );
}

#[test]
fn the_timers_are_two_different_ids() {
    assert_ne!(ACK_TIMER, REPLY_TIMER);
    assert_eq!(ACK_TIMER, TimerId(0));
    assert_eq!(REPLY_TIMER, TimerId(1));
    // A timer the driver never armed is not its business, and neither is one that has
    // nothing to guard.
    let mut driver = SonyDriver::new();
    connect(&mut driver);
    assert_eq!(step(&mut driver, Input::Timer(TimerId(9))), []);
    assert_eq!(step(&mut driver, Input::Timer(REPLY_TIMER)), []);
    assert_eq!(
        step(&mut driver, Input::Tick { now: Instant::ZERO }),
        [],
        "the headset is never asked the time"
    );
}

#[test]
fn raw_sends_what_it_is_given_and_answers_with_the_next_reply_payload() {
    let mut h = full_headset();
    h.one("10 00", "11 00 50 00");
    let mut world = World::new(h);
    world.connect();
    world.request(1, Req::Status);
    world.dones.clear();
    world.request(
        2,
        Req::Raw {
            data_type: DataType::DataMdr,
            payload: hex("10 00"),
        },
    );
    assert_eq!(
        world.sent.last().map(|f| f.payload.clone()),
        Some(hex("10 00"))
    );
    assert_eq!(world.dones, [(ReqId(2), Ok(Resp::Raw(hex("11 00 50 00"))))]);
}

#[test]
fn raw_on_a_data_type_that_is_not_acknowledged_is_refused() {
    let mut world = World::new(full_headset());
    world.connect();
    for data_type in [DataType::Ack, DataType::ShotMdr, DataType::Other(0x55)] {
        world.request(
            1,
            Req::Raw {
                data_type,
                payload: vec![1],
            },
        );
        let (_, result) = world.dones.pop().expect("answered at once");
        assert!(
            matches!(result, Err(ProtoError::Unsupported(_))),
            "{result:?}"
        );
    }
}

#[test]
fn a_raw_request_that_is_never_answered_times_out_without_ending_the_session() {
    let mut world = World::new(full_headset());
    world.connect();
    world.request(
        1,
        Req::Raw {
            data_type: DataType::DataMdr,
            payload: hex("ee ee"),
        },
    );
    assert_eq!(
        world.dones,
        [] as [(
            junk_core::ReqId,
            std::result::Result<junk_sony::proto::Resp, junk_core::ProtoError>
        ); 0]
    );
    world.timer(REPLY_TIMER);
    assert_eq!(world.dones, [(ReqId(1), Err(ProtoError::Timeout))]);
    assert_eq!(world.disconnects, 0);
    // The session is fine afterwards.
    world.request(2, Req::Status);
    assert!(matches!(world.dones[1], (ReqId(2), Ok(Resp::Status(_)))));
}

/// What the headset says that the driver does not read is still `ACK`ed and reported.
#[test]
fn what_the_driver_does_not_understand_is_acked_and_reported_as_unparsed() {
    let mut driver = SonyDriver::new();
    connect(&mut driver);
    let ack0 = tx(&encode(DataType::Ack, 1, &[]));
    let ack1 = tx(&encode(DataType::Ack, 0, &[]));

    // A command id nobody decodes.
    let unknown = encode(DataType::DataMdr, 0, &hex("ee 01 02"));
    assert_eq!(
        step(&mut driver, rx(&unknown)),
        [
            ack0.clone(),
            Output::Event(Ev::Unparsed {
                bytes: unknown.clone()
            })
        ]
    );
    // A known one too short for its layout.
    let short = encode(DataType::DataMdr, 1, &hex("11 00 50"));
    assert_eq!(
        step(&mut driver, rx(&short)),
        [
            ack1.clone(),
            Output::Event(Ev::Unparsed {
                bytes: short.clone()
            })
        ]
    );
    // A data type a v1 link does not handle: ACKed (it asks for one) and reported.
    let common = encode(DataType::DataCommon, 0, &hex("01 02"));
    assert_eq!(
        step(&mut driver, rx(&common)),
        [
            ack0.clone(),
            Output::Event(Ev::Unparsed {
                bytes: common.clone()
            })
        ]
    );
    // No payload at all.
    let empty = encode(DataType::DataMdr, 1, &[]);
    assert_eq!(
        step(&mut driver, rx(&empty)),
        [
            ack1.clone(),
            Output::Event(Ev::Unparsed {
                bytes: empty.clone()
            })
        ]
    );
    // The action log is ACKed and ignored, with no event.
    let log = encode(DataType::DataMdr, 0, &hex("c9 01 00 02 7b 7d"));
    assert_eq!(step(&mut driver, rx(&log)), std::slice::from_ref(&ack0));
    // A frame whose sequence number is neither 0 nor 1 gets no ACK, and is still read.
    let odd = encode(DataType::DataMdr, 5, &hex("13 00 3c 00"));
    let outs = step(&mut driver, rx(&odd));
    assert!(
        matches!(&outs[..], [Output::Event(Ev::Report(Report::Battery(_)))]),
        "{outs:?}"
    );
    // SHOT frames are never ACKed and are read like their DATA twins.
    let shot_frame = encode(DataType::ShotMdr, 0, &hex("13 00 3b 00"));
    let outs = step(&mut driver, rx(&shot_frame));
    assert!(
        matches!(&outs[..], [Output::Event(Ev::Report(Report::Battery(_)))]),
        "{outs:?}"
    );
    // A channel this family does not declare cannot carry one of its frames.
    let outs = step(
        &mut driver,
        Input::Rx {
            chan: Channel(7),
            bytes: unknown.clone(),
        },
    );
    assert_eq!(outs, [Output::Event(Ev::Unparsed { bytes: unknown })]);
}

/// A reply of the awaited command that is too short is `ACK`ed, reported, and the step moves
/// on with a reading that says so.
#[test]
fn a_reply_too_short_for_its_layout_is_malformed_and_unparsed() {
    let mut h = full_headset();
    h.one("10 00", "11 00 50");
    let mut world = World::new(h);
    world.connect();
    world.request(1, Req::Status);
    let status = world.status(1);
    assert_eq!(status.battery, Reading::Malformed);
    assert!(status.codec.value().is_some(), "the session went on");
    assert!(world.events.iter().any(
        |ev| matches!(ev, Ev::Unparsed { bytes } if decode(bytes).payload == hex("11 00 50"))
    ));
}

#[test]
fn a_notification_is_decoded_like_a_reply() {
    let mut world = World::new(full_headset());
    world.connect();
    world.request(1, Req::Status);
    world.events.clear();
    for (payload, check) in [
        ("69 02 11 02 02 01 00 00", "nc"),
        ("13 00 32 01", "battery"),
        ("1b 00 02", "codec"),
        ("f9 05 02 01", "stc preview"),
        ("f9 05 01 00", "stc"),
        ("e9 02 00 00", "dsee"),
        ("99 01 08 00", "alert"),
    ] {
        let bytes = world.headset.frame(DataType::DataMdr, &hex(payload));
        world.rx(&bytes);
        assert!(
            matches!(world.events.last(), Some(Ev::Report(_))),
            "{check}: {:?}",
            world.events.last()
        );
    }
    assert_eq!(world.events.len(), 7);
    assert!(matches!(
        &world.events[0],
        Ev::Report(Report::NcAsm(
            junk_sony::payload::NcAsmState::NoiseCancellingAndAmbient {
                effect: NcAsmEffect::AdjustmentCompletion,
                nc_value: NcDualSingleValue::Dual,
                ..
            }
        ))
    ));
    // The pairing notifications are table two, and decode like their replies.
    let before = world.events.len();
    let bytes = world
        .headset
        .frame(DataType::DataMdrNo2, &hex("35 01 01 00"));
    world.rx(&bytes);
    let bytes = world.headset.frame(
        DataType::DataMdrNo2,
        &hex(&paired_devices_reply().replacen("37", "39", 1)),
    );
    world.rx(&bytes);
    assert_eq!(world.events.len(), before + 2);
    assert!(matches!(
        &world.events[before],
        Ev::Report(Report::PairingMode(PairingModeState {
            mode: PairingMode::InquiryScan,
            status: CommonStatus::Enable,
        }))
    ));
    assert!(matches!(
        &world.events[before + 1],
        Ev::Report(Report::PairedDevices(list)) if list.devices.len() == 2
    ));
    // Voice guidance is table two.
    let bytes = world
        .headset
        .frame(DataType::DataMdrNo2, &hex("49 01 01 00"));
    world.rx(&bytes);
    assert_eq!(
        world.events.last(),
        Some(&Ev::Report(Report::VoiceGuidance(OnOff::Off)))
    );
}

/// Nothing the headset could send, in whatever state the driver is in, panics; and every
/// request is answered exactly once (invariant 4).
#[test]
fn nothing_the_headset_could_send_panics() {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as u32
    };
    let ids: [u8; 31] = [
        0x01, 0x03, 0x05, 0x07, 0x11, 0x13, 0x15, 0x19, 0x25, 0x31, 0x33, 0x35, 0x37, 0x39, 0x41,
        0x47, 0x51, 0x57, 0x5b, 0x61, 0x67, 0x87, 0x99, 0xc9, 0xd1, 0xd7, 0xe7, 0xf1, 0xf7, 0xfb,
        0x30,
    ];
    let types = [
        DataType::DataMdr,
        DataType::DataMdr,
        DataType::DataMdrNo2,
        DataType::ShotMdr,
        DataType::Data,
        DataType::DataCommon,
        DataType::Ack,
        DataType::Other(0x55),
    ];
    let mut driver = SonyDriver::new();
    let mut asked = 0u32;
    let mut answered: HashMap<u32, usize> = HashMap::new();
    let mut count = |outs: Vec<Out>| {
        for out in outs {
            if let Output::Done { id, .. } = out {
                *answered.entry(id.0).or_default() += 1;
            }
        }
    };
    for round in 0..30_000_u32 {
        let input = match next() % 10 {
            0 => Input::Connected {
                resolved: [RFCOMM].into_iter().collect(),
                mtu: 668,
            },
            1 if next() % 8 == 0 => Input::Disconnected,
            2 => Input::Timer(TimerId(u8::try_from(next() % 3).expect("small"))),
            3 => {
                asked += 1;
                let req = if next() % 2 == 0 {
                    Req::Status
                } else {
                    Req::Raw {
                        data_type: types[(next() as usize) % types.len()],
                        payload: (0..next() % 5).map(|_| next().to_le_bytes()[0]).collect(),
                    }
                };
                request(asked, req)
            }
            4 => {
                // Garbage.
                let len = next() % 30;
                rx(&(0..len)
                    .map(|_| next().to_le_bytes()[0])
                    .collect::<Vec<u8>>())
            }
            _ => {
                // A well-formed frame with a payload that may or may not be a layout.
                let mut payload: Vec<u8> =
                    (0..next() % 20).map(|_| next().to_le_bytes()[0]).collect();
                if let Some(first) = payload.first_mut()
                    && next() % 3 != 0
                {
                    *first = ids[(next() as usize) % ids.len()];
                }
                let data_type = types[(next() as usize) % types.len()];
                let seq = u8::try_from(next() % 3).expect("small");
                rx(&encode(data_type, seq, &payload))
            }
        };
        let _ = round;
        count(step(&mut driver, input));
    }
    // Close the link: whatever is still outstanding is failed.
    count(step(&mut driver, Input::Disconnected));
    for id in 1..=asked {
        assert_eq!(answered.get(&id), Some(&1), "request {id}");
    }
}

/// Every reply of the full headset, mangled in a way no firmware is expected to: cut short,
/// padded, bytes changed. The session still ends, every time, with its one answer.
#[test]
fn mangled_replies_neither_hang_nor_panic_a_session() {
    let mut state = 0x0123_4567_89ab_cdef_u64;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as u32
    };
    for seed in 0..300 {
        let mut h = Headset::default();
        for ((data_type, request), mut reply) in full_headset().answers {
            match next() % 5 {
                0 => reply.truncate((next() as usize) % (reply.len() + 1)),
                1 => reply.extend((0..next() % 6).map(|_| next().to_le_bytes()[0])),
                2 => {
                    let at = (next() as usize) % reply.len();
                    reply[at] = next().to_le_bytes()[0];
                }
                _ => {}
            }
            h.answers.insert((data_type, request), reply);
        }
        let mut world = World::new(h);
        world.connect();
        world.request(1, Req::Status);
        for _ in 0..200 {
            if !world.dones.is_empty() {
                break;
            }
            world.timer(REPLY_TIMER);
        }
        assert_eq!(world.dones.len(), 1, "seed {seed}: {:?}", world.dones);
        assert!(
            matches!(
                world.dones[0].1,
                Ok(Resp::Status(_)) | Err(ProtoError::Unsupported(_))
            ),
            "seed {seed}: {:?}",
            world.dones[0]
        );
        assert_eq!(world.disconnects, 0, "seed {seed}");
    }
}

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/sony-wh1000xm4/synthetic-init-status.trace"
);

/// The text of the synthetic trace: the whole of init and one `Req::Status` against the fake
/// headset above, as a `junk-trace` file. **Not captured from hardware.**
fn synthetic_trace() -> String {
    let mut world = World::new(full_headset());
    world.connect();
    world.request(1, Req::Status);
    let mut text = String::new();
    for line in [
        "# junk trace v1 \u{2014} SYNTHETIC init and status read, Sony WH-1000XM4",
        "# source: NOT CAPTURED FROM HARDWARE. Generated by crates/junk-sony/tests/proto.rs",
        "#   (`synthetic_trace`), from payloads written out from the layouts of Sony's own app",
        "#   (an analysis of it). Every reply here is made up to exercise",
        "#   the driver: the function list, the capabilities and the values are not an XM4's.",
        "#   Regenerate with: JUNK_SONY_REGEN=1 cargo test -p junk-sony --test proto synthetic",
        "# columns: <iso-ts> <tx|rx> <channel> <hex>",
        "# lines starting with # are comments; \"! \" lines are link events kept for orientation",
        "# transport: Bluetooth Classic RFCOMM, as the real device would be; the timestamps are",
        "#   fabricated, 7 ms apart",
        "! 2026-10-05T12:00:00.000+00:00 connected mtu=668 channels=rfcomm",
    ] {
        text.push_str(line);
        text.push('\n');
    }
    for (i, (is_tx, bytes)) in world.wire.iter().enumerate() {
        let dir = if *is_tx { "tx" } else { "rx" };
        let ms = (i + 1) * 7;
        let _ = write!(
            text,
            "2026-10-05T12:00:{:02}.{:03}+00:00 {dir} rfcomm ",
            ms / 1000,
            ms % 1000
        );
        for byte in bytes {
            let _ = write!(text, "{byte:02x}");
        }
        text.push('\n');
    }
    text
}

/// The committed trace is what the generator says, so the fixture cannot drift from the
/// payloads in this file.
#[test]
fn the_synthetic_trace_is_what_the_fake_headset_says() {
    let text = synthetic_trace();
    if std::env::var_os("JUNK_SONY_REGEN").is_some() {
        std::fs::create_dir_all(std::path::Path::new(FIXTURE).parent().expect("a directory"))
            .expect("create the fixture directory");
        std::fs::write(FIXTURE, &text).expect("write the fixture");
    }
    let committed = std::fs::read_to_string(FIXTURE).expect("the fixture exists");
    assert_eq!(committed, text);
}
