//! The protocol layer against the frames of `fixtures/colmi-r10`, and the six invariants
//! of SPEC §3.1 pinned on the Colmi driver the way `junk-fake` pins them on the fake one.

use std::collections::VecDeque;

use junk_colmi::proto::{Ev, LiveHr, Req, Resp, SleepSource, TIMEOUT, TIMEOUT_TIMER, Transport};
use junk_colmi::wire::{
    ActivityPacket, BigData, BigDataKind, Capabilities, Cmd, Frame, HostFrame, HrLogPacket,
    Notification, Platform, Prefs, RequestBody, RingFrame, SeriesCmd, SeriesPacket, V1Rx,
    WorkoutAction,
};
use junk_colmi::{Bpm, Source};
use junk_colmi::{ColmiDriver, DIS_FW, DIS_HW, GATT, V1_NOTIFY, V1_WRITE, V2_CMD, V2_NOTIFY};
use junk_core::{
    Battery, Bytes, Channel, ChannelSet, Driver, Input, Instant, Output, Outputs, Percent,
    ProtoError, ReqId, TimerId, Timestamp,
};

type Out = Output<Resp, Ev>;

// Lines of `qring-sync-2026-07-02.trace`, as logged.
const PHONE_NAME_TX: &str = "04011200000000000000000000000017";
const PHONE_NAME_ACK: &str = "04000000000000000000000000000004";
const SET_TIME_TX: &str = "0126070214163001000000000000008b";
const PACKET_SIZE: &str = "2ff40000000000000000000000000023";
const CAPS_ACK: &str = "01010000020000000001002000003055";
const RAW_3C_TX: &str = "3c00000000000000000000000000003c";
const RAW_3C_REPLY: &str = "3c00ac2700000000000000000000000f";
const READ_PREFS_TX: &str = "0a01000000000000000000000000000b";
const PREFS_REPLY: &str = "0a010000001eaf46000000000000001e";
const WRITE_PREFS_TX: &str = "0a020000001eaf46785096000000007d";
const WRITE_PREFS_ACK: &str = "0a02000000000000000000000000000c";
const VERSION_TX: &str = "1901010100000000000000000000001c";
const VERSION_ACK: &str = "1901000100000000000000000000001b";
const FIRMWARE: &str = "RT03CR_1.00.02_260319";
const HARDWARE: &str = "RT03CR_V1.0";
const BATTERY_TX: &str = "03010000000000000000000000000004";
const BATTERY_REPLY: &str = "03640000000000000000000000000067";
const FILE_LIST_TX: &str = "bc300000ffff";
const FILE_LIST_REPLY: &str = "bc300100bf4000";
const READ_AUTO_HR_TX: &str = "16010200000000000000000000000019";
const HR_LOG_TX: &str = "1500aa456a000000000000000000006e";
const HR_LOG_FIRST: &str = "150100aa456a3f504a473d3c3a3f75f6";
// The big-data replies the peer hands out: the `QRing` trace's (reconstructed) sleep,
// `SpO2` and temperature, and the thering trace's workout summary, descriptor and series.
const SLEEP_REPLY: &str = "bc27310039ec01002ebb00c50202180320021804100225030b02240413021d0312020b041b02160309021e0412022b031b0219040f0231";
const SPO2_REPLY: &str = "bc2a31003e9800626260606161636360606060616161616060616160606060616161610000000000000000000000000000000000000000";
const TEMPERATURE_REPLY: &str = "bc2532004e81001ea7a8a8a8a8a8a1a8a7a8a8a7a6a7a7a7a7a7a7a6a6a7a8a7a5a0a6a6a700000000000000000000000000000000000000";
const SUMMARY_REPLY: &str = "bc423100e62e013007060180291d6904023d0004030000060400000000040500000406000003073c03083903093e030d00061300000000";
const DESCRIPTOR_REPLY: &str = "bc440600ad87000100010111";
const SERIES_REPLY: &str = "bc453800326501003e3e3e3e3e3e3e3d3d3d3d3d3d3d3d3c3c3c3c3d3d3d3d3b3b3b3b3b3b3b3b3b3b3b3d3d3c3c3c3c3c3c3c3c3c3c3c3c3c39393a3a3a";

/// 2026-07-02 14:16 in UTC-4: when the fixture set the ring's clock.
const FIXTURE_TIME: Timestamp = Timestamp {
    local_minute: 29_716_696,
    utc_offset_min: -240,
};

/// 2026-07-02 00:00 in UTC-4: the fixture day, epoch 1782950400 = 29715840 minutes.
const FIXTURE_DAY: Timestamp = Timestamp {
    local_minute: 29_715_840,
    utc_offset_min: -240,
};

/// Every channel the R10 has.
const ALL: [Channel; 6] = [V1_WRITE, V1_NOTIFY, V2_CMD, V2_NOTIFY, DIS_FW, DIS_HW];

/// The value of a Device Information characteristic as the driver reads it.
fn dis_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim_end_matches('\0').into()
}

fn hex(text: &str) -> Bytes {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

/// A 16-byte frame built by the wire layer, for replies the trace does not carry verbatim.
fn ring(cmd: u8, payload: &[u8]) -> Bytes {
    Frame::new(Cmd::from_byte(cmd), payload)
        .unwrap()
        .to_bytes()
        .to_vec()
}

fn step(driver: &mut ColmiDriver, input: Input<Req>) -> Vec<Out> {
    let mut out = Outputs::new();
    driver.handle(input, &mut out);
    out.into_vec()
}

fn channels(chans: &[Channel]) -> ChannelSet {
    chans.iter().copied().collect()
}

fn connected(chans: &[Channel]) -> Input<Req> {
    Input::Connected {
        resolved: channels(chans),
        mtu: 247,
    }
}

fn connect(driver: &mut ColmiDriver) -> Vec<Out> {
    step(driver, connected(&ALL))
}

fn request(id: u32, req: Req) -> Input<Req> {
    Input::Request {
        id: ReqId(id),
        req,
        now: Instant::ZERO,
    }
}

fn rx(chan: Channel, bytes: &[u8]) -> Input<Req> {
    Input::Rx {
        chan,
        bytes: bytes.to_vec(),
    }
}

fn tx(chan: Channel, bytes: &[u8]) -> Out {
    Output::Tx {
        chan,
        bytes: bytes.to_vec(),
    }
}

fn set_timer() -> Out {
    Output::SetTimer {
        id: TIMEOUT_TIMER,
        after: TIMEOUT,
    }
}

fn cancel_timer() -> Out {
    Output::CancelTimer(TIMEOUT_TIMER)
}

fn done(id: u32, result: Result<Resp, ProtoError>) -> Out {
    Output::Done {
        id: ReqId(id),
        result,
    }
}

fn event(ev: Ev) -> Out {
    Output::Event(ev)
}

fn unparsed(chan: Channel, bytes: &[u8]) -> Out {
    event(Ev::Unparsed {
        chan,
        bytes: bytes.to_vec(),
    })
}

fn caps() -> Capabilities {
    match RingFrame::decode(&Frame::parse(&hex(CAPS_ACK)).unwrap()) {
        Ok(RingFrame::SetTimeAck(caps)) => caps,
        other => panic!("{other:?}"),
    }
}

fn full_battery() -> Battery {
    Battery {
        percent: Percent::FULL,
        charging: false,
    }
}

fn fixture_prefs() -> Prefs {
    Prefs {
        hour12: false,
        imperial: false,
        sex: 0,
        age: 30,
        height_cm: 175,
        weight_kg: 70,
        sbp: 120,
        dbp: 80,
        hr_warn: 150,
    }
}

fn phone_name() -> Req {
    Req::PhoneName {
        platform: Platform::Ios,
        os_version: 18,
        name: vec![],
    }
}

fn set_time() -> Req {
    Req::SetTime {
        at: FIXTURE_TIME,
        second: 30,
        lang: 1,
    }
}

fn raw_3c() -> Req {
    Req::Raw {
        cmd: Cmd::Other(0x3c),
        body: [0; 14],
    }
}

fn file_list() -> Req {
    Req::RawBigData {
        kind: BigDataKind::FileList,
        body: vec![],
    }
}

fn version() -> Resp {
    Resp::Version {
        firmware: FIRMWARE.into(),
        hardware: HARDWARE.into(),
    }
}

/// A stand-in for the ring: answers each request the way the R10 does in the fixtures.
struct Peer {
    mute: bool,
}

impl Peer {
    fn new() -> Peer {
        Peer { mute: false }
    }

    fn on_write(&self, chan: Channel, bytes: &[u8]) -> Vec<(Channel, Bytes)> {
        if self.mute {
            return vec![];
        }
        if chan == V1_WRITE {
            Self::on_v1(bytes)
        } else if chan == V2_CMD {
            Self::on_v2(bytes)
        } else {
            vec![]
        }
    }

    /// The value of a Device Information characteristic, as the R10 has them.
    fn on_read(&self, chan: Channel) -> Option<(Channel, Bytes)> {
        if self.mute {
            return None;
        }
        let value = if chan == DIS_FW {
            FIRMWARE
        } else if chan == DIS_HW {
            HARDWARE
        } else {
            return None;
        };
        Some((chan, value.into()))
    }

    fn on_v1(bytes: &[u8]) -> Vec<(Channel, Bytes)> {
        let Ok(frame) = Frame::parse(bytes) else {
            return vec![];
        };
        let Ok(host) = HostFrame::decode(&frame) else {
            return vec![];
        };
        let replies = match host {
            HostFrame::SetTime { .. } => {
                vec![RingFrame::PacketSize(244), RingFrame::SetTimeAck(caps())]
            }
            HostFrame::Battery => vec![RingFrame::Battery {
                percent: Percent::FULL,
                charging: false,
            }],
            HostFrame::PhoneName { .. } => vec![RingFrame::PhoneNameAck],
            HostFrame::ReadPrefs => vec![RingFrame::Prefs(fixture_prefs())],
            HostFrame::WritePrefs(_) => vec![RingFrame::PrefsWriteAck],
            HostFrame::Version => return vec![(V1_NOTIFY, hex(VERSION_ACK))],
            HostFrame::ReadGoals => vec![RingFrame::Goals {
                steps: 5000,
                calories: 300_000,
                distance: 3000,
                sport_min: 0,
                sleep_min: 0,
            }],
            HostFrame::ReadAutoHrPref => vec![RingFrame::AutoHrPref {
                enabled: true,
                interval_min: 5,
                unknown: 5,
            }],
            HostFrame::ReadAutoSpo2Pref => vec![RingFrame::AutoSpo2Pref {
                enabled: true,
                interval_min: 30,
            }],
            HostFrame::ReadAutoStressPref => vec![RingFrame::AutoStressPref { enabled: true }],
            HostFrame::ReadAutoHrvPref => vec![RingFrame::AutoHrvPref { enabled: true }],
            HostFrame::TodayTotals => vec![RingFrame::TodayTotals {
                steps: 1401,
                running_steps: 0,
                cal: 63407,
                distance_m: 1076,
                active_min: 41,
            }],
            HostFrame::WorkoutCtl { action, .. } => vec![RingFrame::WorkoutCtlAck {
                action,
                start: (action == WorkoutAction::Start).then_some(0x691d_2980),
            }],
            HostFrame::ManualHrStart { .. }
            | HostFrame::ManualHrStop { .. }
            | HostFrame::Raw { .. } => {
                vec![RingFrame::Raw {
                    cmd: frame.cmd,
                    body: frame.body,
                }]
            }
            // The multi-packet requests: a short log each, shaped like the fixtures'.
            HostFrame::HrLog { day_start } => vec![
                RingFrame::HrLog(HrLogPacket::Header {
                    count: 3,
                    interval_min: 5,
                }),
                RingFrame::HrLog(HrLogPacket::First {
                    day_start,
                    samples: [60; 9],
                }),
                RingFrame::HrLog(HrLogPacket::More {
                    index: 2,
                    samples: [61; 13],
                }),
            ],
            HostFrame::StressLog { days_ago } => Self::series(SeriesCmd::Stress, days_ago),
            HostFrame::HrvLog { days_ago } => Self::series(SeriesCmd::Hrv, days_ago),
            HostFrame::Activity { .. } => vec![
                RingFrame::Activity(ActivityPacket::Header {
                    count: 1,
                    new_protocol: true,
                }),
                RingFrame::Activity(ActivityPacket::Row {
                    year: 2026,
                    month: 7,
                    day: 2,
                    bucket: 4,
                    index: 0,
                    total: 1,
                    cal_raw: 112,
                    steps: 28,
                    distance_m: 19,
                }),
            ],
            HostFrame::FindDevice => vec![],
        };
        replies
            .into_iter()
            .map(|reply| (V1_NOTIFY, reply.encode().unwrap().to_bytes().to_vec()))
            .collect()
    }

    /// A two-packet series of `cmd` for `days_ago`: 12 + 13 half-hour slots.
    fn series(cmd: SeriesCmd, days_ago: u8) -> Vec<RingFrame> {
        vec![
            RingFrame::Series {
                cmd,
                packet: SeriesPacket::Header {
                    count: 3,
                    interval_min: 30,
                },
            },
            RingFrame::Series {
                cmd,
                packet: SeriesPacket::First {
                    days_ago,
                    samples: [40; 12],
                },
            },
            RingFrame::Series {
                cmd,
                packet: SeriesPacket::More {
                    index: 2,
                    samples: [41; 13],
                },
            },
        ]
    }

    fn on_v2(bytes: &[u8]) -> Vec<(Channel, Bytes)> {
        let Ok(request) = BigData::parse(bytes) else {
            return vec![];
        };
        let frames: Vec<Bytes> = match RequestBody::decode(&request) {
            Ok(RequestBody::Sleep(_)) => vec![hex(SLEEP_REPLY)],
            Ok(RequestBody::Spo2(_)) => vec![hex(SPO2_REPLY)],
            Ok(RequestBody::Temperature(_)) => vec![hex(TEMPERATURE_REPLY)],
            Ok(RequestBody::FileList) => vec![hex(FILE_LIST_REPLY)],
            Ok(RequestBody::WorkoutList { .. }) => vec![hex(SUMMARY_REPLY)],
            Ok(RequestBody::WorkoutDetail { .. }) => {
                vec![hex(DESCRIPTOR_REPLY), hex(SERIES_REPLY)]
            }
            Ok(RequestBody::Raw { .. }) | Err(_) => {
                vec![BigData::new(request.kind, vec![0x00]).unwrap().to_bytes()]
            }
        };
        // In pieces, as the ring delivers its larger replies: a short first one, then
        // MTU-sized ones.
        frames
            .iter()
            .flat_map(|frame| {
                let (head, tail) = frame.split_at(4.min(frame.len()));
                std::iter::once(head).chain(tail.chunks(20))
            })
            .map(|piece| (V2_NOTIFY, piece.to_vec()))
            .collect()
    }
}

/// Hands every `Tx` and `Read` in `outs` to the peer and returns its notifications and
/// values as driver inputs.
fn forward(peer: &Peer, outs: &[Out]) -> Vec<Input<Req>> {
    outs.iter()
        .filter_map(|out| match out {
            Output::Tx { chan, bytes } => Some(peer.on_write(*chan, bytes)),
            Output::Read(chan) => peer.on_read(*chan).map(|value| vec![value]),
            Output::Subscribe(_)
            | Output::SetTimer { .. }
            | Output::CancelTimer(_)
            | Output::Event(_)
            | Output::Done { .. }
            | Output::Disconnect => None,
        })
        .flatten()
        .map(|(chan, bytes)| Input::Rx { chan, bytes })
        .collect()
}

/// A tiny xorshift64, so the randomised tests need no dev-dependencies.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        let n = u64::try_from(n).unwrap();
        usize::try_from(self.next_u64() % n).unwrap()
    }

    fn byte(&mut self) -> u8 {
        self.next_u64().to_le_bytes()[0]
    }

    fn bytes(&mut self, len: usize) -> Bytes {
        (0..len).map(|_| self.byte()).collect()
    }
}

#[test]
fn gatt_is_the_drivers_map() {
    assert_eq!(<ColmiDriver as Driver>::GATT, &GATT);
    assert_eq!(GATT.required(), channels(&[V1_WRITE, V1_NOTIFY]));
}

#[test]
fn connected_subscribes_what_resolved_or_refuses() {
    let mut driver = ColmiDriver::new();
    assert_eq!(driver.mtu(), None);
    assert_eq!(
        connect(&mut driver),
        vec![Output::Subscribe(V1_NOTIFY), Output::Subscribe(V2_NOTIFY)]
    );
    assert_eq!(driver.mtu(), Some(247));
    assert_eq!(driver.dialect().transport, Transport::Both);
    assert!(driver.dialect().big_data);

    let mut driver = ColmiDriver::new();
    assert_eq!(
        step(&mut driver, connected(&[V1_WRITE, V1_NOTIFY])),
        vec![Output::Subscribe(V1_NOTIFY)]
    );
    assert_eq!(driver.dialect().transport, Transport::V1);
    assert!(!driver.dialect().big_data);
    // A ring without the V2 service cannot be asked for big data.
    assert_eq!(
        step(&mut driver, request(1, file_list())),
        vec![done(
            1,
            Err(ProtoError::Unsupported(
                "big data: the ring has no V2 service"
            ))
        )]
    );
    assert_eq!(
        step(&mut driver, request(2, Req::Battery)),
        vec![tx(V1_WRITE, &hex(BATTERY_TX)), set_timer()]
    );

    let mut driver = ColmiDriver::new();
    assert_eq!(
        step(&mut driver, connected(&[V1_WRITE, V2_CMD, V2_NOTIFY])),
        vec![
            event(Ev::MissingChannels(channels(&[V1_NOTIFY]))),
            Output::Disconnect,
        ]
    );
    assert_eq!(driver.mtu(), None);
    assert_eq!(
        step(&mut driver, request(1, Req::Battery)),
        vec![done(1, Err(ProtoError::Disconnected))]
    );
    assert_eq!(
        step(&mut driver, request(2, file_list())),
        vec![done(2, Err(ProtoError::Disconnected))]
    );
}

#[test]
fn request_before_connected_is_refused() {
    let mut driver = ColmiDriver::new();
    assert_eq!(
        step(&mut driver, request(1, Req::Battery)),
        vec![done(1, Err(ProtoError::Disconnected))]
    );
}

#[test]
fn the_qring_opening() {
    let mut driver = ColmiDriver::new();
    connect(&mut driver);

    // Phone name, then set time queued behind it.
    assert_eq!(
        step(&mut driver, request(1, phone_name())),
        vec![tx(V1_WRITE, &hex(PHONE_NAME_TX)), set_timer()]
    );
    assert_eq!(step(&mut driver, request(2, set_time())), vec![]);
    assert_eq!(
        step(&mut driver, rx(V1_NOTIFY, &hex(PHONE_NAME_ACK))),
        vec![
            cancel_timer(),
            done(1, Ok(Resp::Ack)),
            tx(V1_WRITE, &hex(SET_TIME_TX)),
            set_timer(),
        ]
    );
    // The ring volunteers its packet size, then acks with its capabilities.
    assert_eq!(
        step(&mut driver, rx(V1_NOTIFY, &hex(PACKET_SIZE))),
        vec![event(Ev::PacketSize(244))]
    );
    assert!(!driver.dialect().temperature);
    assert_eq!(driver.dialect().sleep, SleepSource::Legacy44);
    assert_eq!(
        step(&mut driver, rx(V1_NOTIFY, &hex(CAPS_ACK))),
        vec![
            event(Ev::Capabilities(caps())),
            cancel_timer(),
            done(2, Ok(Resp::Capabilities(caps()))),
        ]
    );
    assert!(driver.dialect().temperature);
    assert_eq!(driver.dialect().sleep, SleepSource::BigData27);
    // QRing retried the set-time; the ring answered both. With nothing in flight the second
    // pair is news only.
    assert_eq!(
        step(&mut driver, rx(V1_NOTIFY, &hex(PACKET_SIZE))),
        vec![event(Ev::PacketSize(244))]
    );
    assert_eq!(
        step(&mut driver, rx(V1_NOTIFY, &hex(CAPS_ACK))),
        vec![event(Ev::Capabilities(caps()))]
    );

    // `3c 00`, prefs read, prefs write.
    assert_eq!(
        step(&mut driver, request(3, raw_3c())),
        vec![tx(V1_WRITE, &hex(RAW_3C_TX)), set_timer()]
    );
    assert_eq!(
        step(&mut driver, rx(V1_NOTIFY, &hex(RAW_3C_REPLY))),
        vec![
            cancel_timer(),
            done(3, Ok(Resp::Raw(Frame::parse(&hex(RAW_3C_REPLY)).unwrap()))),
        ]
    );
    assert_eq!(
        step(&mut driver, request(4, Req::ReadPrefs)),
        vec![tx(V1_WRITE, &hex(READ_PREFS_TX)), set_timer()]
    );
    let stored = Prefs {
        sbp: 0,
        dbp: 0,
        hr_warn: 0,
        ..fixture_prefs()
    };
    assert_eq!(
        step(&mut driver, rx(V1_NOTIFY, &hex(PREFS_REPLY))),
        vec![cancel_timer(), done(4, Ok(Resp::Prefs(stored)))]
    );
    assert_eq!(
        step(&mut driver, request(5, Req::WritePrefs(fixture_prefs()))),
        vec![tx(V1_WRITE, &hex(WRITE_PREFS_TX)), set_timer()]
    );
    assert_eq!(
        step(&mut driver, rx(V1_NOTIFY, &hex(WRITE_PREFS_ACK))),
        vec![cancel_timer(), done(5, Ok(Resp::Ack))]
    );
    assert_eq!(
        step(&mut driver, request(6, Req::ReadAutoHrPref)),
        vec![tx(V1_WRITE, &hex(READ_AUTO_HR_TX)), set_timer()]
    );
    assert_eq!(
        step(
            &mut driver,
            rx(V1_NOTIFY, &ring(0x16, &[0x01, 0x01, 0x05, 0x05]))
        ),
        vec![
            cancel_timer(),
            done(
                6,
                Ok(Resp::AutoPref {
                    enabled: true,
                    interval_min: Some(5)
                })
            ),
        ]
    );
}

#[test]
fn version_picks_the_dialect_row() {
    let mut driver = ColmiDriver::new();
    connect(&mut driver);
    assert_eq!(driver.dialect().live_hr, LiveHr::Manual69);
    assert_eq!(
        step(&mut driver, request(1, Req::Version)),
        vec![tx(V1_WRITE, &hex(VERSION_TX)), set_timer()]
    );
    // The ack starts the reads, one after the other; the timer runs on meanwhile.
    assert_eq!(
        step(&mut driver, rx(V1_NOTIFY, &hex(VERSION_ACK))),
        vec![Output::Read(DIS_FW)]
    );
    assert_eq!(
        step(&mut driver, rx(DIS_FW, FIRMWARE.as_bytes())),
        vec![Output::Read(DIS_HW)]
    );
    assert_eq!(
        step(&mut driver, rx(DIS_HW, HARDWARE.as_bytes())),
        vec![cancel_timer(), done(1, Ok(version()))]
    );
    assert_eq!(driver.dialect().live_hr, LiveHr::Workout77);
    assert_eq!(driver.dialect().fw_prefix, ["RT03CR"]);

    // A firmware no row knows keeps the conservative dialect, and says so. Trailing NULs
    // are padding, not part of the string.
    let mut driver = ColmiDriver::new();
    connect(&mut driver);
    step(&mut driver, request(1, Req::Version));
    step(&mut driver, rx(V1_NOTIFY, &hex(VERSION_ACK)));
    step(&mut driver, rx(DIS_FW, b"XX99_1.0\0\0"));
    assert_eq!(
        step(&mut driver, rx(DIS_HW, b"XX99_V1")),
        vec![
            cancel_timer(),
            event(Ev::UnknownFirmware("XX99_1.0".into())),
            done(
                1,
                Ok(Resp::Version {
                    firmware: "XX99_1.0".into(),
                    hardware: "XX99_V1".into(),
                })
            ),
        ]
    );
    assert_eq!(driver.dialect().live_hr, LiveHr::Manual69);
    assert_eq!(driver.dialect().fw_prefix, [] as [&str; 0]);

    // Strings with no version request in flight are news, whatever else is in flight,
    // whether read from the Device Information service or notified on V1.
    assert_eq!(
        step(&mut driver, rx(DIS_FW, FIRMWARE.as_bytes())),
        vec![event(Ev::Text(FIRMWARE.into()))]
    );
    assert_eq!(
        step(&mut driver, rx(V1_NOTIFY, FIRMWARE.as_bytes())),
        vec![event(Ev::Text(FIRMWARE.into()))]
    );
    step(&mut driver, request(2, Req::Battery));
    assert_eq!(
        step(&mut driver, rx(DIS_HW, HARDWARE.as_bytes())),
        vec![event(Ev::Text(HARDWARE.into()))]
    );
    assert_eq!(
        step(&mut driver, rx(V1_NOTIFY, HARDWARE.as_bytes())),
        vec![event(Ev::Text(HARDWARE.into()))]
    );
    assert_eq!(
        step(&mut driver, rx(V1_NOTIFY, &hex(BATTERY_REPLY))),
        vec![cancel_timer(), done(2, Ok(Resp::Battery(full_battery())))]
    );

    // During the request, a string on V1 notify is news too (the strings are read, never
    // notified), and so is the value of the channel not asked for; the transaction waits
    // for its own reads.
    let mut driver = ColmiDriver::new();
    connect(&mut driver);
    step(&mut driver, request(3, Req::Version));
    assert_eq!(
        step(&mut driver, rx(V1_NOTIFY, FIRMWARE.as_bytes())),
        vec![event(Ev::Text(FIRMWARE.into()))]
    );
    assert_eq!(
        step(&mut driver, rx(DIS_FW, FIRMWARE.as_bytes())),
        vec![event(Ev::Text(FIRMWARE.into()))]
    );
    assert_eq!(
        step(&mut driver, rx(V1_NOTIFY, &hex(VERSION_ACK))),
        vec![Output::Read(DIS_FW)]
    );
    assert_eq!(
        step(&mut driver, rx(V1_NOTIFY, FIRMWARE.as_bytes())),
        vec![event(Ev::Text(FIRMWARE.into()))]
    );
    assert_eq!(
        step(&mut driver, rx(DIS_HW, HARDWARE.as_bytes())),
        vec![event(Ev::Text(HARDWARE.into()))]
    );
    assert_eq!(
        step(&mut driver, rx(DIS_FW, FIRMWARE.as_bytes())),
        vec![Output::Read(DIS_HW)]
    );
    assert_eq!(
        step(&mut driver, rx(DIS_HW, HARDWARE.as_bytes())),
        vec![cancel_timer(), done(3, Ok(version()))]
    );
}

#[test]
fn version_needs_the_device_information_service() {
    let refused = |id| {
        done(
            id,
            Err(ProtoError::Unsupported(
                "version: no device information service",
            )),
        )
    };
    // A ring without the service, or with half of it, cannot be asked: nothing is
    // written, even while something else is in flight, and the rest goes on.
    for resolved in [
        &[V1_WRITE, V1_NOTIFY, V2_CMD, V2_NOTIFY][..],
        &[V1_WRITE, V1_NOTIFY, DIS_FW],
        &[V1_WRITE, V1_NOTIFY, DIS_HW],
    ] {
        let mut driver = ColmiDriver::new();
        step(&mut driver, connected(resolved));
        assert_eq!(
            step(&mut driver, request(1, Req::Version)),
            vec![refused(1)],
            "{resolved:?}"
        );
        assert_eq!(
            step(&mut driver, request(2, Req::Battery)),
            vec![tx(V1_WRITE, &hex(BATTERY_TX)), set_timer()]
        );
        assert_eq!(
            step(&mut driver, request(3, Req::Version)),
            vec![refused(3)],
            "{resolved:?}"
        );
        assert_eq!(
            step(&mut driver, rx(V1_NOTIFY, &hex(BATTERY_REPLY))),
            vec![cancel_timer(), done(2, Ok(Resp::Battery(full_battery())))]
        );
    }

    // With it, the V2 service is not needed.
    let mut driver = ColmiDriver::new();
    step(
        &mut driver,
        connected(&[V1_WRITE, V1_NOTIFY, DIS_FW, DIS_HW]),
    );
    assert_eq!(
        step(&mut driver, request(1, Req::Version)),
        vec![tx(V1_WRITE, &hex(VERSION_TX)), set_timer()]
    );
}

#[test]
fn battery_round_trip_and_unsolicited() {
    let mut driver = ColmiDriver::new();
    connect(&mut driver);
    assert_eq!(
        step(&mut driver, request(1, Req::Battery)),
        vec![tx(V1_WRITE, &hex(BATTERY_TX)), set_timer()]
    );
    assert_eq!(
        step(&mut driver, rx(V1_NOTIFY, &hex(BATTERY_REPLY))),
        vec![cancel_timer(), done(1, Ok(Resp::Battery(full_battery())))]
    );
    // A battery reply nobody asked for is still news, idle or not.
    assert_eq!(
        step(&mut driver, rx(V1_NOTIFY, &hex(BATTERY_REPLY))),
        vec![event(Ev::Battery(full_battery()))]
    );
    step(&mut driver, request(2, Req::Version));
    assert_eq!(
        step(&mut driver, rx(V1_NOTIFY, &ring(0x03, &[0x5e, 0x01]))),
        vec![event(Ev::Battery(Battery {
            percent: Percent::new(94).unwrap(),
            charging: true,
        }))]
    );
}

#[test]
fn raw_big_data_reassembles_and_checks() {
    let mut driver = ColmiDriver::new();
    connect(&mut driver);
    let reply = BigData::parse(&hex(FILE_LIST_REPLY)).unwrap();

    assert_eq!(
        step(&mut driver, request(1, file_list())),
        vec![tx(V2_CMD, &hex(FILE_LIST_TX)), set_timer()]
    );
    assert_eq!(
        step(&mut driver, rx(V2_NOTIFY, &hex(FILE_LIST_REPLY))),
        vec![cancel_timer(), done(1, Ok(Resp::RawBigData(reply.clone())))]
    );

    // Split across two notifications.
    step(&mut driver, request(2, file_list()));
    assert_eq!(step(&mut driver, rx(V2_NOTIFY, &hex("bc300100"))), vec![]);
    assert_eq!(
        step(&mut driver, rx(V2_NOTIFY, &hex("bf4000"))),
        vec![cancel_timer(), done(2, Ok(Resp::RawBigData(reply.clone())))]
    );

    // A bad CRC fails the request.
    step(&mut driver, request(3, file_list()));
    assert_eq!(
        step(&mut driver, rx(V2_NOTIFY, &hex("bc300100be4000"))),
        vec![cancel_timer(), done(3, Err(ProtoError::Checksum))]
    );

    // Another kind is reported, and the transaction stays.
    step(&mut driver, request(4, file_list()));
    let spo2 = BigData::new(BigDataKind::Spo2, vec![0x00]).unwrap();
    assert_eq!(
        step(&mut driver, rx(V2_NOTIFY, &spo2.to_bytes())),
        vec![event(Ev::UnexpectedBigData(spo2))]
    );
    assert_eq!(
        step(&mut driver, rx(V2_NOTIFY, &hex(FILE_LIST_REPLY))),
        vec![cancel_timer(), done(4, Ok(Resp::RawBigData(reply)))]
    );

    // V2 bytes with nothing collecting are unparsed, and a straggler that cannot start a
    // frame does not poison a collection.
    assert_eq!(
        step(&mut driver, rx(V2_NOTIFY, &hex(FILE_LIST_REPLY))),
        vec![unparsed(V2_NOTIFY, &hex(FILE_LIST_REPLY))]
    );
    step(&mut driver, request(5, Req::Battery));
    assert_eq!(
        step(&mut driver, rx(V2_NOTIFY, &hex(FILE_LIST_REPLY))),
        vec![unparsed(V2_NOTIFY, &hex(FILE_LIST_REPLY))]
    );
    step(&mut driver, request(6, file_list()));
    step(&mut driver, rx(V1_NOTIFY, &hex(BATTERY_REPLY)));
    assert_eq!(
        step(&mut driver, rx(V2_NOTIFY, &hex("bf4000"))),
        vec![unparsed(V2_NOTIFY, &hex("bf4000"))]
    );
    assert_eq!(
        step(&mut driver, rx(V2_NOTIFY, &hex(FILE_LIST_REPLY))),
        vec![
            cancel_timer(),
            done(
                6,
                Ok(Resp::RawBigData(
                    BigData::parse(&hex(FILE_LIST_REPLY)).unwrap()
                ))
            ),
        ]
    );
}

#[test]
fn the_dialect_refuses_what_the_ring_has_not_announced() {
    let today = FIXTURE_TIME;
    let big_data = || {
        [
            Req::Sleep {
                today,
                selector: vec![0x06, 0x01],
            },
            Req::Spo2 {
                today,
                selector: 0xff,
            },
            Req::Temperature { today, selector: 0 },
            Req::WorkoutList { since: 0 },
            Req::WorkoutDetail {
                sport_type: 7,
                start: 0x691d_2980,
            },
            file_list(),
        ]
    };

    // A ring without the V2 service cannot be asked for big data, whatever else it says.
    let mut driver = ColmiDriver::new();
    step(&mut driver, connected(&[V1_WRITE, V1_NOTIFY]));
    step(&mut driver, rx(V1_NOTIFY, &hex(CAPS_ACK)));
    for (n, req) in (1..).zip(big_data()) {
        let outs = step(&mut driver, request(n, req.clone()));
        assert!(
            matches!(
                outs.as_slice(),
                [Output::Done {
                    id,
                    result: Err(ProtoError::Unsupported(_)),
                }] if *id == ReqId(n)
            ),
            "{req:?}: {outs:?}"
        );
    }
    // The V1 logs still go out.
    assert_eq!(
        step(
            &mut driver,
            request(
                10,
                Req::HrLog {
                    day_start: FIXTURE_DAY
                }
            )
        ),
        vec![tx(V1_WRITE, &hex(HR_LOG_TX)), set_timer()]
    );
}

#[test]
fn temperature_and_sleep_need_the_capability_ack() {
    let today = FIXTURE_TIME;
    // Both services, but no capability ack yet: temperature and sleep are refused at
    // once, even while something else is in flight, and SpO2 goes out.
    let mut driver = ColmiDriver::new();
    connect(&mut driver);
    assert_eq!(
        step(
            &mut driver,
            request(1, Req::Temperature { today, selector: 0 })
        ),
        vec![done(
            1,
            Err(ProtoError::Unsupported(
                "temperature: the ring does not measure it"
            ))
        )]
    );
    assert_eq!(
        step(
            &mut driver,
            request(
                2,
                Req::Sleep {
                    today,
                    selector: vec![0x01, 0x01],
                }
            )
        ),
        vec![done(
            2,
            Err(ProtoError::Unsupported(
                "sleep: the ring does not send it as big data"
            ))
        )]
    );
    assert_eq!(
        step(
            &mut driver,
            request(
                3,
                Req::Spo2 {
                    today,
                    selector: 0xff,
                }
            )
        ),
        vec![tx(V2_CMD, &hex("bc2a0100ff00ff")), set_timer()]
    );
    assert_eq!(
        step(
            &mut driver,
            request(4, Req::Temperature { today, selector: 0 })
        ),
        vec![done(
            4,
            Err(ProtoError::Unsupported(
                "temperature: the ring does not measure it"
            ))
        )]
    );

    // The ack announces both; from then on they are sent.
    step(&mut driver, rx(V1_NOTIFY, &hex(CAPS_ACK)));
    assert_eq!(
        step(
            &mut driver,
            request(5, Req::Temperature { today, selector: 0 })
        ),
        vec![]
    );
    assert_eq!(
        step(
            &mut driver,
            request(
                6,
                Req::Sleep {
                    today,
                    selector: vec![0x01, 0x01],
                }
            )
        ),
        vec![]
    );
    // The SpO2 reply answers 3 and starts the queued temperature request.
    let outs = step(&mut driver, rx(V2_NOTIFY, &hex(SPO2_REPLY)));
    let [
        cancel,
        Output::Done {
            id,
            result: Ok(Resp::Spo2(samples)),
        },
        write,
        timer,
    ] = outs.as_slice()
    else {
        panic!("{outs:?}");
    };
    assert_eq!(*cancel, cancel_timer());
    assert_eq!(*id, ReqId(3));
    assert_eq!(samples.len(), 14);
    assert_eq!(*write, tx(V2_CMD, &hex("bc250100bf4000")));
    assert_eq!(*timer, set_timer());
}

#[test]
fn requests_queue_and_start_in_order() {
    let mut driver = ColmiDriver::new();
    let peer = Peer::new();
    connect(&mut driver);

    let first = step(&mut driver, request(1, Req::Battery));
    assert_eq!(first, vec![tx(V1_WRITE, &hex(BATTERY_TX)), set_timer()]);
    assert_eq!(step(&mut driver, request(2, Req::ReadAutoHrPref)), vec![]);
    assert_eq!(step(&mut driver, request(3, file_list())), vec![]);
    assert_eq!(step(&mut driver, request(4, raw_3c())), vec![]);

    let mut pending: VecDeque<Input<Req>> = forward(&peer, &first).into();
    let mut answered = Vec::new();
    while let Some(reply) = pending.pop_front() {
        let outs = step(&mut driver, reply);
        pending.extend(forward(&peer, &outs));
        answered.push(outs);
    }
    assert_eq!(
        answered,
        vec![
            vec![
                cancel_timer(),
                done(1, Ok(Resp::Battery(full_battery()))),
                tx(V1_WRITE, &hex(READ_AUTO_HR_TX)),
                set_timer(),
            ],
            vec![
                cancel_timer(),
                done(
                    2,
                    Ok(Resp::AutoPref {
                        enabled: true,
                        interval_min: Some(5)
                    })
                ),
                tx(V2_CMD, &hex(FILE_LIST_TX)),
                set_timer(),
            ],
            vec![],
            vec![
                cancel_timer(),
                done(
                    3,
                    Ok(Resp::RawBigData(
                        BigData::parse(&hex(FILE_LIST_REPLY)).unwrap()
                    ))
                ),
                tx(V1_WRITE, &hex(RAW_3C_TX)),
                set_timer(),
            ],
            vec![
                cancel_timer(),
                done(4, Ok(Resp::Raw(Frame::parse(&hex(RAW_3C_TX)).unwrap()))),
            ],
        ]
    );
}

#[test]
fn timeout_fails_the_request_and_late_replies_are_noise() {
    let mut driver = ColmiDriver::new();
    let peer = Peer { mute: true };
    connect(&mut driver);

    let outs = step(&mut driver, request(1, raw_3c()));
    assert_eq!(forward(&peer, &outs), vec![]);
    assert_eq!(step(&mut driver, Input::Timer(TimerId(5))), vec![]);
    assert_eq!(
        step(&mut driver, Input::Timer(TIMEOUT_TIMER)),
        vec![done(1, Err(ProtoError::Timeout))]
    );
    // The late reply is well-formed, so it is reported as what it is, not as garbage.
    let late = RingFrame::decode(&Frame::parse(&hex(RAW_3C_REPLY)).unwrap()).unwrap();
    assert_eq!(
        step(&mut driver, rx(V1_NOTIFY, &hex(RAW_3C_REPLY))),
        vec![event(Ev::UnexpectedReply(late))]
    );

    // A timeout with something queued starts the next request in the same step.
    step(&mut driver, request(2, Req::Battery));
    step(&mut driver, request(3, file_list()));
    assert_eq!(
        step(&mut driver, Input::Timer(TIMEOUT_TIMER)),
        vec![
            done(2, Err(ProtoError::Timeout)),
            tx(V2_CMD, &hex(FILE_LIST_TX)),
            set_timer(),
        ]
    );
    // Half a big-data reply, then the timer: the partial frame dies with the request.
    assert_eq!(step(&mut driver, rx(V2_NOTIFY, &hex("bc300100"))), vec![]);
    assert_eq!(
        step(&mut driver, Input::Timer(TIMEOUT_TIMER)),
        vec![done(3, Err(ProtoError::Timeout))]
    );
    assert_eq!(
        step(&mut driver, rx(V2_NOTIFY, &hex("bf4000"))),
        vec![unparsed(V2_NOTIFY, &hex("bf4000"))]
    );
    assert_eq!(step(&mut driver, Input::Timer(TIMEOUT_TIMER)), vec![]);
}

#[test]
fn disconnect_fails_everything_and_the_driver_is_reusable() {
    let mut driver = ColmiDriver::new();
    let peer = Peer::new();
    connect(&mut driver);
    step(&mut driver, rx(V1_NOTIFY, &hex(CAPS_ACK)));
    assert!(driver.dialect().temperature);
    step(&mut driver, request(1, Req::Version));
    step(&mut driver, request(2, Req::Battery));
    step(&mut driver, request(3, file_list()));

    assert_eq!(
        step(&mut driver, Input::Disconnected),
        vec![
            cancel_timer(),
            done(1, Err(ProtoError::Disconnected)),
            done(2, Err(ProtoError::Disconnected)),
            done(3, Err(ProtoError::Disconnected)),
        ]
    );
    // Nothing survives, the dialect included (invariant 6).
    assert_eq!(driver, ColmiDriver::new());
    assert!(!driver.dialect().temperature);
    assert_eq!(step(&mut driver, Input::Disconnected), vec![]);
    assert_eq!(
        step(&mut driver, request(4, Req::Battery)),
        vec![done(4, Err(ProtoError::Disconnected))]
    );

    assert_eq!(
        connect(&mut driver),
        vec![Output::Subscribe(V1_NOTIFY), Output::Subscribe(V2_NOTIFY)]
    );
    assert_eq!(step(&mut driver, Input::Timer(TIMEOUT_TIMER)), vec![]);
    let outs = step(&mut driver, request(5, Req::Battery));
    assert_eq!(outs, vec![tx(V1_WRITE, &hex(BATTERY_TX)), set_timer()]);
    for reply in forward(&peer, &outs) {
        assert_eq!(
            step(&mut driver, reply),
            vec![cancel_timer(), done(5, Ok(Resp::Battery(full_battery())))]
        );
    }
}

#[test]
fn connected_on_top_of_a_live_link_is_a_new_link() {
    let mut driver = ColmiDriver::new();
    connect(&mut driver);
    step(&mut driver, rx(V1_NOTIFY, &hex(CAPS_ACK)));
    step(&mut driver, request(1, Req::Battery));
    step(&mut driver, request(2, Req::Battery));
    assert_eq!(
        step(&mut driver, connected(&[V1_WRITE, V1_NOTIFY])),
        vec![
            cancel_timer(),
            done(1, Err(ProtoError::Disconnected)),
            done(2, Err(ProtoError::Disconnected)),
            Output::Subscribe(V1_NOTIFY),
        ]
    );
    assert_eq!(driver.dialect().transport, Transport::V1);
    assert!(!driver.dialect().temperature);
    assert_eq!(
        step(&mut driver, request(3, Req::Battery)),
        vec![tx(V1_WRITE, &hex(BATTERY_TX)), set_timer()]
    );
}

#[test]
fn unsolicited_frames_are_events_and_leave_the_transaction_alone() {
    let mut driver = ColmiDriver::new();
    connect(&mut driver);
    let unsolicited = [
        (hex(PACKET_SIZE), Ev::PacketSize(244)),
        (
            ring(0x73, &[0x0c, 0x5e]),
            Ev::Notification(Notification::Battery(Percent::new(94).unwrap())),
        ),
        (ring(0x73, &[0x01]), Ev::Notification(Notification::NewHr)),
        (
            ring(0x78, &[0x07, 0x01, 0x00, 0x0a, 0x3e]),
            Ev::Workout {
                sport_type: 7,
                flag: 1,
                seq: 10,
                bpm: Bpm::Valid(62),
            },
        ),
        (hex(BATTERY_REPLY), Ev::Battery(full_battery())),
    ];
    // A live tick before the sensor locks on reads zero, which is no reading.
    assert_eq!(
        step(
            &mut driver,
            rx(V1_NOTIFY, &ring(0x78, &[0x07, 0x01, 0x00, 0x00, 0x00]))
        ),
        vec![event(Ev::Workout {
            sport_type: 7,
            flag: 1,
            seq: 0,
            bpm: Bpm::Invalid,
        })]
    );
    for (bytes, ev) in &unsolicited {
        assert_eq!(
            step(&mut driver, rx(V1_NOTIFY, bytes)),
            vec![event(ev.clone())]
        );
    }
    step(&mut driver, request(1, raw_3c()));
    for (bytes, ev) in &unsolicited {
        assert_eq!(
            step(&mut driver, rx(V1_NOTIFY, bytes)),
            vec![event(ev.clone())]
        );
    }
    assert_eq!(
        step(&mut driver, rx(V1_NOTIFY, &hex(RAW_3C_REPLY))),
        vec![
            cancel_timer(),
            done(1, Ok(Resp::Raw(Frame::parse(&hex(RAW_3C_REPLY)).unwrap()))),
        ]
    );
}

#[test]
fn out_of_context_frames_are_reported_and_change_nothing() {
    let mut driver = ColmiDriver::new();
    connect(&mut driver);

    // Well-formed replies with no transaction to answer.
    assert_eq!(
        step(&mut driver, rx(V1_NOTIFY, &hex(PHONE_NAME_ACK))),
        vec![event(Ev::UnexpectedReply(RingFrame::PhoneNameAck))]
    );
    let hr_log = ring(0x15, &[0x00, 0x18, 0x05]);
    let decoded = RingFrame::decode(&Frame::parse(&hr_log).unwrap()).unwrap();
    assert_eq!(
        step(&mut driver, rx(V1_NOTIFY, &hr_log)),
        vec![event(Ev::UnexpectedReply(decoded))]
    );

    // Bytes the wire layer rejects, on every channel.
    let mut bad_checksum = hex(BATTERY_REPLY);
    bad_checksum[15] ^= 0xff;
    let cases: Vec<(Channel, Bytes)> = vec![
        (V1_WRITE, hex(PHONE_NAME_TX)),
        (V2_CMD, hex(FILE_LIST_TX)),
        (Channel(9), hex(BATTERY_REPLY)),
        (V1_NOTIFY, bad_checksum),
        (V1_NOTIFY, vec![0x00, 0x80]),
        (V1_NOTIFY, vec![]),
        (V1_NOTIFY, ring(0x03, &[101, 0x00])),
        (V2_NOTIFY, hex(FILE_LIST_REPLY)),
        (V2_NOTIFY, vec![0xbc]),
    ];
    for (chan, bytes) in &cases {
        assert_eq!(
            step(&mut driver, rx(*chan, bytes)),
            vec![unparsed(*chan, bytes)],
            "{chan:?} {bytes:02x?}"
        );
    }

    // The same during a transaction, plus a reply of the wrong kind.
    step(&mut driver, request(1, Req::Battery));
    for (chan, bytes) in &cases {
        assert_eq!(
            step(&mut driver, rx(*chan, bytes)),
            vec![unparsed(*chan, bytes)],
            "{chan:?} {bytes:02x?}"
        );
    }
    assert_eq!(
        step(&mut driver, rx(V1_NOTIFY, &hex(PHONE_NAME_ACK))),
        vec![event(Ev::UnexpectedReply(RingFrame::PhoneNameAck))]
    );
    assert_eq!(
        step(&mut driver, rx(V1_NOTIFY, &hex(BATTERY_REPLY))),
        vec![cancel_timer(), done(1, Ok(Resp::Battery(full_battery())))]
    );
}

/// A fixed script that exercises every input kind, for the determinism test.
fn script() -> Vec<Input<Req>> {
    let script = vec![
        request(1, Req::Battery),
        connected(&ALL),
        request(2, phone_name()),
        request(3, set_time()),
        Input::Tick { now: Instant(5) },
        rx(V1_NOTIFY, &hex(PACKET_SIZE)),
        rx(V1_NOTIFY, &hex(PHONE_NAME_ACK)),
        rx(V1_NOTIFY, &hex(CAPS_ACK)),
        request(4, Req::Version),
        rx(V1_NOTIFY, &hex(VERSION_ACK)),
        rx(V1_WRITE, &hex(VERSION_TX)),
        rx(V1_NOTIFY, &[0xff, 0x00]),
        rx(DIS_FW, FIRMWARE.as_bytes()),
        rx(DIS_HW, HARDWARE.as_bytes()),
        request(5, file_list()),
        request(6, Req::Battery),
        Input::Timer(TimerId(9)),
        rx(V2_NOTIFY, &[0xbc]),
        Input::Timer(TIMEOUT_TIMER),
        rx(V2_NOTIFY, &hex(FILE_LIST_REPLY)),
        rx(V1_NOTIFY, &hex(BATTERY_REPLY)),
        request(
            7,
            Req::PhoneName {
                platform: Platform::Ios,
                os_version: 18,
                name: vec![b'x'; 12],
            },
        ),
        request(8, raw_3c()),
        request(
            9,
            Req::HrLog {
                day_start: FIXTURE_TIME,
            },
        ),
        Input::Disconnected,
        Input::Disconnected,
        request(10, Req::Battery),
        connected(&[V1_WRITE, V2_CMD]),
        request(11, Req::Battery),
        Input::Timer(TIMEOUT_TIMER),
        connected(&[V1_WRITE, V1_NOTIFY]),
        rx(V1_NOTIFY, &ring(0x73, &[0x0c, 0x5e])),
        rx(V1_NOTIFY, &ring(0x03, &[101, 0x00])),
        request(12, file_list()),
        request(13, Req::ReadAutoHrPref),
        rx(V1_NOTIFY, &ring(0x16, &[0x01, 0x01, 0x05, 0x05])),
        request(14, Req::TodayTotals),
        connected(&ALL),
        request(15, Req::Battery),
        Input::Disconnected,
    ];
    assert_eq!(script.len(), 40);
    script
}

#[test]
fn handle_is_deterministic() {
    let run = || {
        let mut driver = ColmiDriver::new();
        script()
            .into_iter()
            .map(|input| step(&mut driver, input))
            .collect::<Vec<_>>()
    };
    let (first, second) = (run(), run());
    assert_eq!(first, second);
    // The script actually answered everything it asked.
    let answered = first
        .iter()
        .flatten()
        .filter(|out| matches!(out, Output::Done { .. }))
        .count();
    assert_eq!(answered, 15);
}

/// One of the requests a session may make, most of them the fixtures' own.
fn random_request(rng: &mut Rng) -> Req {
    let today = FIXTURE_TIME;
    match rng.below(20) {
        0 => Req::Battery,
        1 => phone_name(),
        2 => set_time(),
        3 => Req::Version,
        4 => raw_3c(),
        5 => file_list(),
        6 => Req::ReadPrefs,
        7 => Req::ReadAutoHrPref,
        8 => Req::TodayTotals,
        9 => Req::WorkoutCtl {
            action: WorkoutAction::Start,
            sport_type: 7,
        },
        10 => Req::HrLog {
            day_start: FIXTURE_DAY,
        },
        11 => Req::Stress { days_ago: 0, today },
        12 => Req::Hrv { days_ago: 1, today },
        13 => Req::activity_day(0, today),
        14 => Req::Sleep {
            today,
            selector: vec![0x06, 0x01],
        },
        15 => Req::Spo2 {
            today,
            selector: 0xff,
        },
        16 => Req::Temperature { today, selector: 0 },
        17 => Req::WorkoutList { since: 0 },
        18 => Req::WorkoutDetail {
            sport_type: 7,
            start: 0x691d_2980,
        },
        _ => Req::PhoneName {
            platform: Platform::Ios,
            os_version: 18,
            name: vec![b'x'; 12],
        },
    }
}

/// One random session: any input in any order, every `Tx` answered by the peer eventually.
fn random_session(seed: u64) {
    let mut rng = Rng(seed);
    let mut driver = ColmiDriver::new();
    let peer = Peer::new();
    let mut pending: VecDeque<(Channel, Bytes)> = VecDeque::new();
    let mut requested = Vec::new();
    let mut answered = Vec::new();

    let mut absorb = |outs: Vec<Out>, pending: &mut VecDeque<(Channel, Bytes)>| {
        for out in outs {
            match out {
                Output::Tx { chan, bytes } => pending.extend(peer.on_write(chan, &bytes)),
                Output::Read(chan) => pending.extend(peer.on_read(chan)),
                Output::Done { id, .. } => answered.push(id),
                Output::Subscribe(_)
                | Output::SetTimer { .. }
                | Output::CancelTimer(_)
                | Output::Event(_)
                | Output::Disconnect => {}
            }
        }
    };

    for n in 0..300 {
        let input = match rng.below(6) {
            0 => {
                let resolved = ALL.into_iter().filter(|_| rng.below(4) != 0).collect();
                let mtu = u16::from(rng.byte());
                Input::Connected { resolved, mtu }
            }
            1 => Input::Disconnected,
            2 => match pending.pop_front() {
                Some((chan, bytes)) if rng.below(2) == 0 => Input::Rx { chan, bytes },
                Some(_) | None => {
                    let len = rng.below(41);
                    Input::Rx {
                        chan: Channel(rng.byte() % 7),
                        bytes: rng.bytes(len),
                    }
                }
            },
            3 => Input::Timer(TimerId(rng.byte() % 3)),
            4 => {
                let req = random_request(&mut rng);
                requested.push(ReqId(n));
                request(n, req)
            }
            _ => Input::Tick {
                now: Instant(u64::from(n)),
            },
        };
        absorb(step(&mut driver, input), &mut pending);
    }
    absorb(step(&mut driver, Input::Disconnected), &mut pending);

    let mut expected = requested;
    expected.sort_unstable();
    let mut got = answered.clone();
    got.sort_unstable();
    assert_eq!(got, expected, "seed {seed:#x}");
    got.dedup();
    assert_eq!(
        got.len(),
        answered.len(),
        "seed {seed:#x}: an id was answered twice"
    );
}

#[test]
fn every_request_gets_exactly_one_done() {
    for run in 0..200 {
        random_session(0x9e37_79b9_7f4a_7c15 ^ run);
    }
}

/// What is in flight while the garbage arrives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Busy {
    Nothing,
    Version,
    HrLog,
}

#[test]
fn garbage_never_panics_and_never_touches_the_transaction() {
    for busy in [Busy::Nothing, Busy::Version, Busy::HrLog] {
        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        let mut driver = ColmiDriver::new();
        connect(&mut driver);
        match busy {
            Busy::Nothing => {}
            Busy::Version => assert_eq!(
                step(&mut driver, request(1, Req::Version)),
                vec![tx(V1_WRITE, &hex(VERSION_TX)), set_timer()]
            ),
            Busy::HrLog => {
                assert_eq!(
                    step(
                        &mut driver,
                        request(
                            1,
                            Req::HrLog {
                                day_start: FIXTURE_DAY
                            }
                        )
                    ),
                    vec![tx(V1_WRITE, &hex(HR_LOG_TX)), set_timer()]
                );
                // Half way through: the header has come.
                assert_eq!(
                    step(&mut driver, rx(V1_NOTIFY, &ring(0x15, &[0x00, 0x02, 0x05]))),
                    vec![]
                );
            }
        }
        for len in 0..=40 {
            for chan in 0..=6 {
                let chan = Channel(chan);
                let mut bytes = rng.bytes(len);
                if chan == V1_NOTIFY {
                    // Random bytes that happen to be a frame, or a printable string, are
                    // not garbage; break them.
                    match V1Rx::classify(&bytes) {
                        V1Rx::Frame(_) => bytes[15] ^= 0xff,
                        V1Rx::Text(_) => bytes[0] = 0x80,
                        V1Rx::Invalid(_) => {}
                    }
                }
                // A value read from the Device Information service is text whatever it
                // holds; with nothing waiting for it (the version request is still waiting
                // for its ack), it is news.
                let expected = if chan == DIS_FW || chan == DIS_HW {
                    event(Ev::Text(dis_text(&bytes)))
                } else {
                    unparsed(chan, &bytes)
                };
                assert_eq!(
                    step(&mut driver, rx(chan, &bytes)),
                    vec![expected],
                    "busy {busy:?}, len {len}, {chan:?}"
                );
            }
        }
        match busy {
            Busy::Nothing => assert_eq!(
                step(&mut driver, request(1, Req::Battery)),
                vec![tx(V1_WRITE, &hex(BATTERY_TX)), set_timer()]
            ),
            Busy::Version => {
                assert_eq!(
                    step(&mut driver, rx(V1_NOTIFY, &hex(VERSION_ACK))),
                    vec![Output::Read(DIS_FW)]
                );
                assert_eq!(
                    step(&mut driver, rx(DIS_FW, FIRMWARE.as_bytes())),
                    vec![Output::Read(DIS_HW)]
                );
                assert_eq!(
                    step(&mut driver, rx(DIS_HW, HARDWARE.as_bytes())),
                    vec![cancel_timer(), done(1, Ok(version()))]
                );
            }
            Busy::HrLog => {
                // The log picks up where it was: the first packet completes a count of 2.
                let outs = step(&mut driver, rx(V1_NOTIFY, &hex(HR_LOG_FIRST)));
                let [
                    cancel,
                    Output::Done {
                        id,
                        result: Ok(Resp::HrLog(samples)),
                    },
                ] = outs.as_slice()
                else {
                    panic!("{outs:?}");
                };
                assert_eq!(*cancel, cancel_timer());
                assert_eq!(*id, ReqId(1));
                assert_eq!(samples.len(), 9);
                assert_eq!(samples[0].at, FIXTURE_DAY);
                assert_eq!(samples[0].bpm, Bpm::Valid(63));
                assert_eq!(samples[0].source, Source::Periodic);
                assert_eq!(samples[8].bpm, Bpm::Valid(117));
                assert_eq!(samples[8].at.local_minute, FIXTURE_DAY.local_minute + 40);
            }
        }
    }
}
