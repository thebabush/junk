//! The two captured fixtures under `fixtures/colmi-r10`, read and written back exactly.
//!
//! The line counts are what `grep -c '^#'`, `grep -c '^! '` and the rest give on the files;
//! a change here means the fixture changed.

use junk_trace::{Direction, Line, Stamp, Trace};

/// One full `QRing` sync, converted from the app's own log; stamps carry no offset.
const QRING: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/colmi-r10/qring-sync-2026-07-02.trace"
));

/// One live-HR session, written by `tools/pklg2trace.py`; stamps carry `-05:00`.
const THERING: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/colmi-r10/thering-realtime-2025-11-19.trace"
));

fn parse(text: &str) -> Trace {
    Trace::parse(text).unwrap_or_else(|err| panic!("{err}"))
}

fn stamp(text: &str) -> Stamp {
    Stamp::parse(text).unwrap_or_else(|err| panic!("{text}: {err}"))
}

fn counts(trace: &Trace) -> (usize, usize, usize) {
    let mut comments = 0;
    let mut events = 0;
    let mut data = 0;
    for line in &trace.lines {
        match line {
            Line::Comment(_) => comments += 1,
            Line::Event(_) => events += 1,
            Line::Data(_) => data += 1,
        }
    }
    (comments, events, data)
}

#[test]
fn qring_parses_with_the_expected_counts() {
    let trace = parse(QRING);
    assert_eq!(trace.lines.len(), 577);
    assert_eq!(counts(&trace), (5, 45, 527));
    assert_eq!(trace.header().count(), 5);
    assert_eq!(trace.events().count(), 45);
    assert_eq!(trace.data().count(), 527);
}

#[test]
fn thering_parses_with_the_expected_counts() {
    let trace = parse(THERING);
    assert_eq!(trace.lines.len(), 90);
    assert_eq!(counts(&trace), (5, 2, 83));
    assert_eq!(trace.header().count(), 5);
    assert_eq!(trace.events().count(), 2);
    assert_eq!(trace.data().count(), 83);
}

#[test]
fn both_fixtures_round_trip_byte_for_byte() {
    assert_eq!(parse(QRING).render(), QRING);
    assert_eq!(parse(THERING).render(), THERING);
}

#[test]
fn thering_header_is_its_five_leading_comments() {
    let trace = parse(THERING);
    let header: Vec<&str> = trace.header().collect();
    assert_eq!(header.len(), 5);
    assert!(header[0].starts_with(" junk trace v1 — Colmi R10_F300 live HR session"));
    assert_eq!(
        header[1],
        " source: thering-realtime-2025-11-19.pklg (the source capture was not published), converted by tools/pklg2trace.py"
    );
    assert_eq!(header[2], " columns: <iso-ts> <tx|rx> <channel> <hex>");
    assert!(header[3].starts_with(" handle map: 0x0010=v1.write"));
    assert!(header[4].starts_with(" lines starting with # are comments"));
    // The header stops at the first data line; nothing after it is a comment here.
    assert!(matches!(trace.lines.get(5), Some(Line::Data(_))));
}

#[test]
fn qring_header_stops_at_the_first_event() {
    let trace = parse(QRING);
    assert_eq!(trace.header().count(), 5);
    assert!(matches!(trace.lines.get(5), Some(Line::Event(_))));
    assert!(
        trace
            .lines
            .iter()
            .skip(5)
            .all(|line| !matches!(line, Line::Comment(_)))
    );
}

#[test]
fn qring_stamps_have_no_offset_and_delta_as_expected() {
    let trace = parse(QRING);
    let mut data = trace.data();
    let first = data.next().expect("first data line");
    let second = data.next().expect("second data line");
    let last = trace.data().last().expect("last data line");

    assert_eq!(first.at, stamp("2026-07-02T14:16:30.474"));
    assert_eq!(first.at.utc_offset_min(), None);
    assert_eq!(first.dir, Direction::Tx);
    assert_eq!(first.chan, "v1.write");
    assert_eq!(
        first.bytes,
        [0x04, 0x01, 0x12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x17]
    );

    // 14:16:30.474 -> 14:16:30.475
    assert_eq!(second.at.delta_millis(&first.at), 1);
    // 14:16:30.474 -> 14:41:29.103: 24 min 58.629 s
    assert_eq!(last.at, stamp("2026-07-02T14:41:29.103"));
    assert_eq!(last.at.delta_millis(&first.at), 1_498_629);

    assert!(trace.data().all(|d| d.at.utc_offset_min().is_none()));
    assert!(trace.events().all(|e| e.at.utc_offset_min().is_none()));
}

#[test]
fn thering_stamps_carry_minus_five_hours() {
    let trace = parse(THERING);
    let first = trace.data().next().expect("first data line");
    let last = trace.data().last().expect("last data line");
    let last_event = trace.events().last().expect("last event");

    assert_eq!(first.at, stamp("2025-11-19T10:20:45.341-05:00"));
    assert_eq!(first.at.utc_offset_min(), Some(-300));
    assert_eq!(first.at.year(), 2025);
    assert_eq!(first.at.month(), 11);
    assert_eq!(first.at.day(), 19);
    assert_eq!(first.at.hour(), 10);
    assert_eq!(first.at.minute(), 20);
    assert_eq!(first.at.second(), 45);
    assert_eq!(first.at.millis(), 341);
    assert_eq!(first.at.to_string(), "2025-11-19T10:20:45.341-05:00");

    // 10:20:45.341 -> 10:21:49.308
    assert_eq!(last.at.delta_millis(&first.at), 63_967);
    assert_eq!(last.chan, "v2.notify");
    assert_eq!(last.dir, Direction::Rx);
    assert_eq!(last.bytes.first(), Some(&0xbc));
    // bc 45, len 0x0038, crc, then 56 bytes of body.
    assert_eq!(last.bytes.len(), 62);

    assert_eq!(last_event.at, stamp("2025-11-19T10:21:50.657-05:00"));
    assert_eq!(last_event.text, "disconnected reason=0x16");

    assert!(trace.data().all(|d| d.at.utc_offset_min() == Some(-300)));
}

#[test]
fn fixtures_use_only_the_known_channels_and_stamps_never_go_backwards() {
    for text in [QRING, THERING] {
        let trace = parse(text);
        for data in trace.data() {
            assert!(
                matches!(
                    data.chan.as_str(),
                    "v1.write" | "v1.notify" | "v2.cmd" | "v2.notify" | "dis.fw" | "dis.hw"
                ),
                "unexpected channel {}",
                data.chan
            );
            assert_ne!(data.bytes, [] as [u8; 0]);
        }
        let stamps: Vec<Stamp> = trace.data().map(|d| d.at).collect();
        assert!(stamps.windows(2).all(|pair| pair[0] <= pair[1]));
    }
}

#[test]
fn fixture_stamps_rebuild_from_their_fields_and_their_epoch() {
    for text in [QRING, THERING] {
        let trace = parse(text);
        let stamps = trace
            .data()
            .map(|d| d.at)
            .chain(trace.events().map(|e| e.at));
        for at in stamps {
            assert_eq!(
                Stamp::new(
                    at.year(),
                    at.month(),
                    at.day(),
                    at.hour(),
                    at.minute(),
                    at.second(),
                    at.millis(),
                    at.utc_offset_min(),
                ),
                Ok(at)
            );
            assert_eq!(
                Stamp::from_naive_epoch_millis(at.naive_epoch_millis(), at.utc_offset_min()),
                Ok(at)
            );
        }
    }
}
