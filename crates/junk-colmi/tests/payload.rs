//! The two captured fixtures under `fixtures/colmi-r10`, pushed through the payload layer.
//!
//! Every 16-byte frame in both directions must decode without `Malformed`, come back as
//! `Raw` only for a `Cmd::Other`, and re-encode to the bytes it came from. The counts are
//! what a script over the files gives; a change here means the fixture changed.

use std::collections::BTreeSet;

use junk_colmi::wire::{
    ActivityPacket, Cmd, Frame, HostFrame, HrLogPacket, RingFrame, SeriesCmd, SeriesPacket, V1Rx,
};
use junk_colmi::{V1_NOTIFY, V1_WRITE, channel_by_name};
use junk_core::Channel;
use junk_trace::{Direction, Trace};

/// One full `QRing` sync, from the app's own log.
const QRING: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/colmi-r10/qring-sync-2026-07-02.trace"
));

/// One workout session and record fetch, from `PacketLogger`.
const THERING: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/colmi-r10/thering-realtime-2025-11-19.trace"
));

/// The day the `QRing` fixture asks the HR log for: 2026-07-02 00:00 as if UTC.
const QRING_DAY_START: u32 = 1_782_950_400;

fn parse(text: &str) -> Trace {
    Trace::parse(text).unwrap_or_else(|err| panic!("{err}"))
}

/// The 16-byte frames going `dir` on `chan`, in order. Version strings are skipped; a bad
/// checksum is a panic, the wire fixture tests having already pinned there are none.
fn frames_on(trace: &Trace, dir: Direction, chan: Channel) -> Vec<Frame> {
    trace
        .data()
        .filter(|line| {
            line.dir == dir
                && channel_by_name(&line.chan)
                    .unwrap_or_else(|| panic!("unknown channel {}", line.chan))
                    == chan
        })
        .filter_map(|line| match V1Rx::classify(&line.bytes) {
            V1Rx::Frame(frame) => Some(frame),
            V1Rx::Text(_) => None,
            V1Rx::Invalid(err) => panic!("{line}: {err}"),
        })
        .collect()
}

/// What decoding one direction of a trace gave.
#[derive(Default)]
struct Tally {
    frames: usize,
    /// The command bytes that decoded as `Raw`, with how many frames each.
    raw: Vec<u8>,
}

fn tally_ring(trace: &Trace) -> Tally {
    let mut tally = Tally::default();
    for frame in frames_on(trace, Direction::Rx, V1_NOTIFY) {
        let value = RingFrame::decode(&frame).unwrap_or_else(|err| panic!("{frame:?}: {err}"));
        if let RingFrame::Raw { cmd, .. } = value {
            assert!(matches!(cmd, Cmd::Other(_)), "{frame:?} decoded as Raw");
            tally.raw.push(cmd.byte());
        }
        let encoded = value
            .encode()
            .unwrap_or_else(|err| panic!("{value:?}: {err}"));
        assert_eq!(encoded, frame, "{value:?}");
        assert_eq!(encoded.to_bytes(), frame.to_bytes());
        tally.frames += 1;
    }
    tally
}

fn tally_host(trace: &Trace) -> Tally {
    let mut tally = Tally::default();
    for frame in frames_on(trace, Direction::Tx, V1_WRITE) {
        let value = HostFrame::decode(&frame).unwrap_or_else(|err| panic!("{frame:?}: {err}"));
        if let HostFrame::Raw { cmd, .. } = value {
            assert!(matches!(cmd, Cmd::Other(_)), "{frame:?} decoded as Raw");
            tally.raw.push(cmd.byte());
        }
        let encoded = value
            .encode()
            .unwrap_or_else(|err| panic!("{value:?}: {err}"));
        assert_eq!(encoded, frame, "{value:?}");
        assert_eq!(encoded.to_bytes(), frame.to_bytes());
        tally.frames += 1;
    }
    tally
}

fn distinct(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .copied()
        .collect::<BTreeSet<u8>>()
        .into_iter()
        .collect()
}

#[test]
fn qring_ring_frames_decode_and_re_encode_losslessly() {
    let tally = tally_ring(&parse(QRING));
    assert_eq!(tally.frames, 382);
    assert_eq!(tally.raw.len(), 6);
    assert_eq!(distinct(&tally.raw), [0x3a, 0x3b, 0x3c]);
}

#[test]
fn qring_host_frames_decode_and_re_encode_losslessly() {
    let tally = tally_host(&parse(QRING));
    assert_eq!(tally.frames, 95);
    assert_eq!(tally.raw.len(), 6);
    assert_eq!(distinct(&tally.raw), [0x3a, 0x3b, 0x3c]);
}

#[test]
fn thering_frames_decode_and_re_encode_losslessly() {
    let trace = parse(THERING);
    let ring = tally_ring(&trace);
    assert_eq!(ring.frames, 75);
    assert_eq!(ring.raw, [] as [u8; 0]);
    let host = tally_host(&trace);
    assert_eq!(host.frames, 3);
    assert_eq!(host.raw, [] as [u8; 0]);
}

#[test]
fn qring_first_activity_row_is_the_app_db_one_oclock_row() {
    let trace = parse(QRING);
    let row = frames_on(&trace, Direction::Rx, V1_NOTIFY)
        .iter()
        .filter(|frame| frame.cmd == Cmd::Activity)
        .find_map(|frame| match RingFrame::decode(frame) {
            Ok(RingFrame::Activity(row @ ActivityPacket::Row { .. })) => Some(row),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no activity row"));
    // The app DB stores this hour as 28 steps, 19 m and 1120 cal: raw × 10.
    assert_eq!(
        row,
        ActivityPacket::Row {
            year: 2026,
            month: 7,
            day: 2,
            bucket: 4,
            index: 0,
            total: 5,
            cal_raw: 112,
            steps: 28,
            distance_m: 19,
        }
    );
}

#[test]
fn qring_today_totals_match_the_documented_numbers() {
    let trace = parse(QRING);
    let frames = frames_on(&trace, Direction::Rx, V1_NOTIFY);
    let reply = frames
        .iter()
        .find(|frame| frame.cmd == Cmd::TodayTotals)
        .unwrap_or_else(|| panic!("no 0x48 reply"));
    assert_eq!(
        RingFrame::decode(reply),
        Ok(RingFrame::TodayTotals {
            steps: 1401,
            running_steps: 0,
            cal: 63407,
            distance_m: 1076,
            active_min: 41,
        })
    );
}

#[test]
fn qring_set_time_ack_is_the_documented_capability_bitmap() {
    let trace = parse(QRING);
    let frames = frames_on(&trace, Direction::Rx, V1_NOTIFY);
    let ack = frames
        .iter()
        .find(|frame| frame.cmd == Cmd::SetTime)
        .unwrap_or_else(|| panic!("no 0x01 ack"));
    let Ok(RingFrame::SetTimeAck(caps)) = RingFrame::decode(ack) else {
        panic!("{ack:?}");
    };
    assert!(caps.temperature);
    assert!(caps.spo2());
    assert!(caps.new_sleep_protocol);
    assert!(!caps.watch_faces);
    assert!(!caps.menstruation);
    assert_eq!(caps.features, 0x02);
    assert_eq!((caps.screen_w, caps.screen_h), (0, 0));
    assert_eq!((caps.raw[10], caps.raw[13]), (0x20, 0x30));
}

#[test]
fn qring_hrv_packet_two_is_the_app_db_slots_twelve_onward() {
    let trace = parse(QRING);
    let packet = frames_on(&trace, Direction::Rx, V1_NOTIFY)
        .iter()
        .filter(|frame| frame.cmd == Cmd::HrvLog)
        .find_map(|frame| match RingFrame::decode(frame) {
            Ok(RingFrame::Series {
                cmd: SeriesCmd::Hrv,
                packet: packet @ SeriesPacket::More { index: 2, .. },
            }) => Some(packet),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no HRV packet 2"));
    assert_eq!(
        packet,
        SeriesPacket::More {
            index: 2,
            samples: [50, 0, 32, 0, 36, 0, 44, 0, 36, 0, 37, 0, 38],
        }
    );
}

// The docs write the request as `15 00 <ts>`; the bytes only give the documented day
// start when the u32 begins at the first body byte, and the reply's first packet carries
// the same number.
#[test]
fn qring_hr_log_request_and_reply_carry_the_documented_day_start() {
    let trace = parse(QRING);
    let requests: Vec<HostFrame> = frames_on(&trace, Direction::Tx, V1_WRITE)
        .iter()
        .filter(|frame| frame.cmd == Cmd::HrLog)
        .map(|frame| HostFrame::decode(frame).unwrap_or_else(|err| panic!("{err}")))
        .collect();
    assert_eq!(requests.len(), 9);
    assert!(requests.iter().all(|request| {
        *request
            == HostFrame::HrLog {
                day_start: QRING_DAY_START,
            }
    }));

    let firsts: Vec<u32> = frames_on(&trace, Direction::Rx, V1_NOTIFY)
        .iter()
        .filter(|frame| frame.cmd == Cmd::HrLog)
        .filter_map(|frame| match RingFrame::decode(frame) {
            Ok(RingFrame::HrLog(HrLogPacket::First { day_start, .. })) => Some(day_start),
            _ => None,
        })
        .collect();
    assert_eq!(firsts, [QRING_DAY_START; 9]);
}
