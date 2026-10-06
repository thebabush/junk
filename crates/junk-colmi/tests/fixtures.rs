//! The two captured fixtures under `fixtures/colmi-r10`, pushed through the wire layer.
//!
//! The counts are what a script over the files gives (frames with a valid checksum,
//! version strings, complete `0xbc` frames); a change here means the fixture changed. The
//! `QRing` trace's big-data replies are reconstructed from the app database (its header
//! says how), so every `0xbc` line in both fixtures is a complete, CRC-valid frame.

use std::collections::{BTreeMap, BTreeSet};

use junk_colmi::wire::{BigData, Cmd, Frame, V1Rx, WireError};
use junk_colmi::{
    DIS_FW, DIS_HW, V1_NOTIFY, V1_WRITE, V2_CMD, V2_NOTIFY, channel_by_name, channel_name,
};
use junk_core::Channel;
use junk_trace::{DataLine, Direction, Trace};

/// One full `QRing` sync, from the app's own log; `0xbc` replies are header-only.
const QRING: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/colmi-r10/qring-sync-2026-07-02.trace"
));

/// One workout session and record fetch, from `PacketLogger`; `0xbc` frames are complete.
const THERING: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/colmi-r10/thering-realtime-2025-11-19.trace"
));

fn parse(text: &str) -> Trace {
    Trace::parse(text).unwrap_or_else(|err| panic!("{err}"))
}

/// The data lines on any of `chans`, by the trace's channel names.
fn lines_on<'a>(trace: &'a Trace, chans: &[Channel]) -> Vec<&'a DataLine> {
    trace
        .data()
        .filter(|line| {
            let chan = channel_by_name(&line.chan)
                .unwrap_or_else(|| panic!("unknown channel {}", line.chan));
            chans.contains(&chan)
        })
        .collect()
}

/// What the V1 lines of a trace classify as.
#[derive(Default)]
struct V1Tally {
    frames: usize,
    bad_checksum: usize,
    texts: Vec<String>,
    cmds: BTreeSet<u8>,
}

fn tally_v1(trace: &Trace) -> V1Tally {
    let mut tally = V1Tally::default();
    for line in lines_on(trace, &[V1_WRITE, V1_NOTIFY]) {
        match V1Rx::classify(&line.bytes) {
            V1Rx::Frame(frame) => {
                assert_eq!(frame.to_bytes().as_slice(), line.bytes, "{line}");
                assert_eq!(Frame::parse(&line.bytes), Ok(frame), "{line}");
                tally.frames += 1;
                tally.cmds.insert(frame.cmd.byte());
            }
            V1Rx::Text(text) => tally.texts.push(text),
            V1Rx::Invalid(WireError::Checksum { .. }) => tally.bad_checksum += 1,
            V1Rx::Invalid(err) => panic!("{line}: {err}"),
        }
    }
    tally
}

/// What the V2 lines of a trace decode as.
#[derive(Default)]
struct V2Tally {
    /// Complete frames, by kind byte.
    complete: BTreeMap<u8, usize>,
    /// Body length of the last complete frame of each kind.
    body_len: BTreeMap<u8, usize>,
    /// Lines that are only a header, with the length each promises.
    header_only: Vec<usize>,
}

fn tally_v2(trace: &Trace) -> V2Tally {
    let mut tally = V2Tally::default();
    for line in lines_on(trace, &[V2_CMD, V2_NOTIFY]) {
        match BigData::parse(&line.bytes) {
            Ok(big) => {
                assert_eq!(big.to_bytes(), line.bytes, "{line}");
                *tally.complete.entry(big.kind.byte()).or_default() += 1;
                tally.body_len.insert(big.kind.byte(), big.body().len());
            }
            Err(WireError::Incomplete { expected, got }) => {
                assert_eq!(got, line.bytes.len(), "{line}");
                assert_eq!(BigData::expected_len(&line.bytes), Some(expected), "{line}");
                tally.header_only.push(expected);
            }
            Err(err) => panic!("{line}: {err}"),
        }
    }
    tally
}

#[test]
fn qring_v1_lines_are_all_frames() {
    let tally = tally_v1(&parse(QRING));
    assert_eq!(tally.frames, 477);
    assert_eq!(tally.bad_checksum, 0);
    assert!(tally.texts.is_empty(), "{:?}", tally.texts);
}

#[test]
fn qring_reads_the_version_strings_from_device_information() {
    // Two version requests, one per connection; each is acked on V1 notify and then the
    // two Device Information strings are read, firmware first.
    let trace = parse(QRING);
    let lines: Vec<&DataLine> = trace.data().collect();
    let acks: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| {
            line.dir == Direction::Rx && line.bytes.starts_with(&[0x19, 0x01, 0x00, 0x01])
        })
        .map(|(index, _)| index)
        .collect();
    assert_eq!(acks.len(), 2);
    for ack in acks {
        let firmware = lines[ack + 1];
        let hardware = lines[ack + 2];
        assert_eq!(firmware.dir, Direction::Rx);
        assert_eq!(channel_by_name(&firmware.chan), Some(DIS_FW));
        assert_eq!(firmware.bytes, b"RT03CR_1.00.02_260319");
        assert_eq!(hardware.dir, Direction::Rx);
        assert_eq!(channel_by_name(&hardware.chan), Some(DIS_HW));
        assert_eq!(hardware.bytes, b"RT03CR_V1.0");
    }
    assert_eq!(lines_on(&trace, &[DIS_FW]).len(), 2);
    assert_eq!(lines_on(&trace, &[DIS_HW]).len(), 2);
}

#[test]
fn qring_exchanges_the_documented_commands() {
    let tally = tally_v1(&parse(QRING));
    let seen: Vec<u8> = tally.cmds.iter().copied().collect();
    assert_eq!(
        seen,
        [
            0x01, 0x03, 0x04, 0x0a, 0x15, 0x16, 0x19, 0x21, 0x2c, 0x2f, 0x36, 0x37, 0x38, 0x39,
            0x3a, 0x3b, 0x3c, 0x43, 0x48, 0x73,
        ]
    );
    let others: Vec<u8> = seen
        .iter()
        .copied()
        .filter(|&byte| matches!(Cmd::from_byte(byte), Cmd::Other(_)))
        .collect();
    assert_eq!(others, [0x3a, 0x3b, 0x3c]);
}

#[test]
fn qring_v2_lines_are_all_complete_frames() {
    let tally = tally_v2(&parse(QRING));
    let complete: Vec<(u8, usize)> = tally.complete.iter().map(|(&k, &n)| (k, n)).collect();
    // Requests and replies both count: five temperature, five sleep and six SpO2 rounds.
    assert_eq!(
        complete,
        [
            (0x25, 10),
            (0x27, 10),
            (0x2a, 12),
            (0x30, 4),
            (0x41, 5),
            (0x42, 5)
        ]
    );
    assert_eq!(tally.complete.values().sum::<usize>(), 46);
    assert_eq!(tally.header_only, [] as [usize; 0]);
    // The reconstructed reply bodies: one 49-byte sleep day, 49-byte SpO2 day blocks, a
    // 50-byte temperature day.
    assert_eq!(tally.body_len.get(&0x27), Some(&49));
    assert_eq!(tally.body_len.get(&0x2a), Some(&49));
    assert_eq!(tally.body_len.get(&0x25), Some(&50));
}

#[test]
fn thering_v1_lines_are_all_workout_frames() {
    let tally = tally_v1(&parse(THERING));
    assert_eq!(tally.frames, 78);
    assert_eq!(tally.bad_checksum, 0);
    assert_eq!(tally.texts, [] as [std::string::String; 0]);
    let seen: Vec<u8> = tally.cmds.iter().copied().collect();
    assert_eq!(seen, [0x73, 0x77, 0x78]);
    assert_eq!(Cmd::from_byte(0x73), Cmd::Notify);
    assert_eq!(Cmd::from_byte(0x77), Cmd::WorkoutCtl);
    assert_eq!(Cmd::from_byte(0x78), Cmd::WorkoutData);
}

#[test]
fn thering_v2_lines_are_the_complete_workout_record_flow() {
    let tally = tally_v2(&parse(THERING));
    let complete: Vec<(u8, usize)> = tally.complete.iter().map(|(&k, &n)| (k, n)).collect();
    assert_eq!(
        complete,
        [(0x41, 1), (0x42, 1), (0x43, 1), (0x44, 1), (0x45, 1)]
    );
    assert_eq!(tally.header_only, [] as [usize; 0]);
    assert_eq!(tally.body_len.get(&0x42), Some(&49));
    assert_eq!(tally.body_len.get(&0x45), Some(&56));
}

#[test]
fn every_channel_name_in_the_fixtures_round_trips() {
    // The QRing sync uses every channel; the thering session never asks the version.
    for (text, exercised) in [(QRING, 6), (THERING, 4)] {
        let trace = parse(text);
        let mut seen = BTreeSet::new();
        for line in trace.data() {
            let chan = channel_by_name(&line.chan)
                .unwrap_or_else(|| panic!("unknown channel {}", line.chan));
            assert!([V1_WRITE, V1_NOTIFY, V2_CMD, V2_NOTIFY, DIS_FW, DIS_HW].contains(&chan));
            assert_eq!(channel_name(chan), Some(line.chan.as_str()));
            seen.insert(chan);
        }
        assert_eq!(seen.len(), exercised, "every channel the session used");
    }
}
