//! The two captured fixtures under `fixtures/colmi-r10`, pushed through the big-data body
//! layer.
//!
//! Every complete `0xbc` frame in both directions must decode without `Malformed`, never
//! come back as `Raw` (both directions name every kind the fixtures carry, the file list
//! included), and re-encode to the frame it came from. The counts are what a script over
//! the files gives; a change here means the fixture changed. The values asserted are the
//! app database rows the `QRing` replies were rebuilt from and the thering record the
//! docs decode.

use std::collections::BTreeMap;

use junk_colmi::SleepKind;
use junk_colmi::wire::{
    BigData, BigDataKind, ReplyBody, RequestBody, SampleTag, SleepDay, Spo2Day, TemperatureDay,
    WorkoutRecord, WorkoutTag,
};
use junk_colmi::{V2_CMD, V2_NOTIFY, channel_by_name};
use junk_core::Channel;
use junk_trace::{Direction, Trace};

/// One full `QRing` sync, from the app's own log; the big-data replies are rebuilt from
/// the app database.
const QRING: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/colmi-r10/qring-sync-2026-07-02.trace"
));

/// One workout session and record fetch, from `PacketLogger`.
const THERING: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/colmi-r10/thering-realtime-2025-11-19.trace"
));

/// The workout-list cursor `QRing` sends: `4e 74 21 69`.
const QRING_SINCE: u32 = 0x6921_744e;

/// The start of the thering workout, as the `77 01` ack and the summary record give it.
const THERING_START: u32 = 0x691d_2980;

fn parse(text: &str) -> Trace {
    Trace::parse(text).unwrap_or_else(|err| panic!("{err}"))
}

/// The complete big-data frames going `dir` on `chan`, in order. An incomplete or invalid
/// frame is a panic, the wire fixture tests having already pinned there are none.
fn bigs_on(trace: &Trace, dir: Direction, chan: Channel) -> Vec<BigData> {
    trace
        .data()
        .filter(|line| {
            line.dir == dir
                && channel_by_name(&line.chan)
                    .unwrap_or_else(|| panic!("unknown channel {}", line.chan))
                    == chan
        })
        .map(|line| BigData::parse(&line.bytes).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect()
}

/// Every reply in `trace`, decoded and checked to re-encode to its frame, with the frame.
fn replies(trace: &Trace) -> Vec<(BigData, ReplyBody)> {
    bigs_on(trace, Direction::Rx, V2_NOTIFY)
        .into_iter()
        .map(|big| {
            let value = ReplyBody::decode(&big).unwrap_or_else(|err| panic!("{big:?}: {err}"));
            assert!(
                !matches!(value, ReplyBody::Raw { .. }),
                "{big:?} decoded as Raw"
            );
            let encoded = value
                .encode()
                .unwrap_or_else(|err| panic!("{value:?}: {err}"));
            assert_eq!(encoded, big, "{value:?}");
            assert_eq!(encoded.to_bytes(), big.to_bytes());
            (big, value)
        })
        .collect()
}

/// Every request in `trace`, decoded and checked to re-encode to its frame, with the frame.
fn requests(trace: &Trace) -> Vec<(BigData, RequestBody)> {
    bigs_on(trace, Direction::Tx, V2_CMD)
        .into_iter()
        .map(|big| {
            let value = RequestBody::decode(&big).unwrap_or_else(|err| panic!("{big:?}: {err}"));
            assert!(
                !matches!(value, RequestBody::Raw { .. }),
                "{big:?} decoded as Raw"
            );
            let encoded = value
                .encode()
                .unwrap_or_else(|err| panic!("{value:?}: {err}"));
            assert_eq!(encoded, big, "{value:?}");
            assert_eq!(encoded.to_bytes(), big.to_bytes());
            (big, value)
        })
        .collect()
}

/// How many frames of each kind byte.
fn by_kind(frames: &[BigData]) -> Vec<(u8, usize)> {
    let mut tally = BTreeMap::new();
    for big in frames {
        *tally.entry(big.kind.byte()).or_insert(0) += 1;
    }
    tally.into_iter().collect()
}

fn sleep_days(replies: &[(BigData, ReplyBody)]) -> Vec<&SleepDay> {
    replies
        .iter()
        .filter_map(|(_, value)| match value {
            ReplyBody::Sleep(sleep) => Some(&sleep.days),
            _ => None,
        })
        .flatten()
        .collect()
}

fn spo2_days(replies: &[(BigData, ReplyBody)]) -> Vec<&Spo2Day> {
    replies
        .iter()
        .filter_map(|(_, value)| match value {
            ReplyBody::Spo2(spo2) => Some(&spo2.days),
            _ => None,
        })
        .flatten()
        .collect()
}

fn temperature_days(replies: &[(BigData, ReplyBody)]) -> Vec<&TemperatureDay> {
    replies
        .iter()
        .filter_map(|(_, value)| match value {
            ReplyBody::Temperature(temperature) => Some(&temperature.days),
            _ => None,
        })
        .flatten()
        .collect()
}

fn workout_records(replies: &[(BigData, ReplyBody)]) -> Vec<Vec<&WorkoutRecord>> {
    replies
        .iter()
        .filter_map(|(_, value)| match value {
            ReplyBody::WorkoutSummary(summary) => Some(summary.records.iter().collect()),
            _ => None,
        })
        .collect()
}

#[test]
fn qring_replies_decode_and_re_encode_losslessly() {
    let trace = parse(QRING);
    let replies = replies(&trace);
    assert_eq!(replies.len(), 23);
    let frames: Vec<BigData> = replies.iter().map(|(big, _)| big.clone()).collect();
    assert_eq!(
        by_kind(&frames),
        [(0x25, 5), (0x27, 5), (0x2a, 6), (0x30, 2), (0x42, 5)]
    );
    let lists: Vec<&ReplyBody> = replies
        .iter()
        .filter(|(big, _)| big.kind == BigDataKind::FileList)
        .map(|(_, value)| value)
        .collect();
    assert_eq!(lists, [&ReplyBody::FileList(vec![0x00]); 2]);
}

#[test]
fn qring_requests_decode_and_re_encode_losslessly() {
    let trace = parse(QRING);
    let requests = requests(&trace);
    assert_eq!(requests.len(), 23);
    let frames: Vec<BigData> = requests.iter().map(|(big, _)| big.clone()).collect();
    assert_eq!(
        by_kind(&frames),
        [(0x25, 5), (0x27, 5), (0x2a, 6), (0x30, 2), (0x41, 5)]
    );

    // `ff` on the first SpO2 round of the session, `02` afterwards; `06 01` then `01 01`
    // for sleep; `00` for temperature; nothing for the file list.
    let spo2: Vec<u8> = requests
        .iter()
        .filter_map(|(_, value)| match value {
            RequestBody::Spo2(selector) => Some(*selector),
            _ => None,
        })
        .collect();
    assert_eq!(spo2, [0xff, 0x02, 0x02, 0x02, 0x02, 0x02]);
    let sleep: Vec<&[u8]> = requests
        .iter()
        .filter_map(|(_, value)| match value {
            RequestBody::Sleep(selector) => Some(selector.as_slice()),
            _ => None,
        })
        .collect();
    assert_eq!(
        sleep,
        [
            &[0x06, 0x01][..],
            &[0x01, 0x01],
            &[0x01, 0x01],
            &[0x01, 0x01],
            &[0x01, 0x01]
        ]
    );
    let temperature = requests
        .iter()
        .filter(|(_, value)| matches!(value, RequestBody::Temperature(0x00)))
        .count();
    assert_eq!(temperature, 5);
    let lists = requests
        .iter()
        .filter(|(_, value)| matches!(value, RequestBody::FileList))
        .count();
    assert_eq!(lists, 2);
}

#[test]
fn qring_sleep_reply_is_the_app_db_sleep_v3_row() {
    let trace = parse(QRING);
    let replies = replies(&trace);
    let days = sleep_days(&replies);
    assert_eq!(days.len(), 5, "one day per reply");
    assert!(
        days.iter().all(|day| *day == days[0]),
        "the same day every time"
    );
    let day = days[0];
    assert_eq!(day.days_ago, 0);
    assert_eq!(day.start_min, 187);
    assert_eq!(day.end_min, 709);
    assert_eq!(day.stages.len(), 21);
    assert_eq!(day.minutes(), 522);
    assert_eq!(day.minutes(), u32::from(day.end_min - day.start_min));
    let first = day.stages[0];
    assert_eq!((first.kind, first.minutes), (SleepKind::Light, 24));
    let last = day.stages[20];
    assert_eq!((last.kind, last.minutes), (SleepKind::Light, 49));
    // The trace's pairs `04 10`, `04 13`, `04 1b`, `04 12`, `04 0f`: five REM stages, at
    // every fourth position.
    let rem: Vec<usize> = day
        .stages
        .iter()
        .enumerate()
        .filter(|(_, stage)| stage.kind == SleepKind::Rem)
        .map(|(index, _)| index)
        .collect();
    assert_eq!(rem, [3, 7, 11, 15, 19]);
    assert!(
        day.stages
            .iter()
            .all(|stage| !matches!(stage.kind, SleepKind::Unknown(_) | SleepKind::Awake)),
        "{:?}",
        day.stages
    );
}

#[test]
fn qring_spo2_replies_are_the_blood_oxygen_rows_in_two_variants() {
    let trace = parse(QRING);
    let replies = replies(&trace);
    let days = spo2_days(&replies);
    assert_eq!(days.len(), 6, "one day per reply");
    for day in &days {
        assert_eq!(day.days_ago, 0);
        let morning: Vec<u8> = day.hours[..5].iter().map(|hour| hour.min).collect();
        assert_eq!(morning, [98, 96, 97, 99, 96]);
        assert!(day.hours.iter().all(|hour| hour.min == hour.max));
        assert!(day.hours[..14].iter().all(|hour| !hour.is_empty()));
        assert!(day.hours[15..].iter().all(|hour| hour.is_empty()));
        assert!(
            day.hours
                .iter()
                .filter(|hour| !hour.is_empty())
                .all(|hour| hour.min_percent().is_some() && hour.max_percent().is_some())
        );
    }
    // Hour 14 is empty on the three replies before the 14:30 sample and 99 % after.
    let (filled, empty): (Vec<&Spo2Day>, Vec<&Spo2Day>) =
        days.iter().partition(|day| !day.hours[14].is_empty());
    assert_eq!(empty.len(), 3);
    assert_eq!(filled.len(), 3);
    assert!(filled.iter().all(|day| day.hours[14].min == 99));
}

#[test]
fn qring_temperature_replies_are_the_temperatures_rows_in_two_variants() {
    let trace = parse(QRING);
    let replies = replies(&trace);
    let days = temperature_days(&replies);
    assert_eq!(days.len(), 5, "one day per reply");
    for day in &days {
        assert_eq!(day.days_ago, 0);
        assert_eq!(day.interval_min, 30);
        let first: Vec<Option<i16>> = (0..8).map(|slot| day.deci_celsius(slot)).collect();
        assert_eq!(
            first,
            [
                Some(367),
                Some(368),
                Some(368),
                Some(368),
                Some(368),
                Some(368),
                Some(361),
                Some(368)
            ]
        );
        assert!(day.deci_celsius(28).is_some());
        assert!((30..48).all(|slot| day.deci_celsius(slot).is_none()));
    }
    // Slot 29 (14:30) is empty on the three replies before that sample and filled after.
    let filled: Vec<usize> = days
        .iter()
        .map(|day| {
            (0..48)
                .filter(|&slot| day.deci_celsius(slot).is_some())
                .count()
        })
        .collect();
    assert_eq!(filled, [29, 29, 29, 30, 30]);
}

#[test]
fn qring_workout_summaries_are_empty_and_the_list_cursor_is_the_documented_one() {
    let trace = parse(QRING);
    let replies = replies(&trace);
    let records = workout_records(&replies);
    assert_eq!(records.len(), 5);
    assert!(records.iter().all(Vec::is_empty));
    let since: Vec<u32> = requests(&trace)
        .iter()
        .filter_map(|(_, value)| match value {
            RequestBody::WorkoutList { since } => Some(*since),
            _ => None,
        })
        .collect();
    assert_eq!(since, [QRING_SINCE; 5]);
}

#[test]
fn thering_frames_decode_and_re_encode_losslessly() {
    let trace = parse(THERING);
    let replies = replies(&trace);
    let frames: Vec<BigData> = replies.iter().map(|(big, _)| big.clone()).collect();
    assert_eq!(by_kind(&frames), [(0x42, 1), (0x44, 1), (0x45, 1)]);
    let requests = requests(&trace);
    let values: Vec<&RequestBody> = requests.iter().map(|(_, value)| value).collect();
    assert_eq!(
        values,
        [
            &RequestBody::WorkoutList { since: 0 },
            &RequestBody::WorkoutDetail {
                sport_type: 7,
                start: THERING_START,
            },
        ]
    );
}

#[test]
fn thering_workout_record_is_the_documented_summary() {
    let trace = parse(THERING);
    let replies = replies(&trace);
    let records = workout_records(&replies);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].len(), 1);
    let record = records[0][0];
    assert_eq!(record.sport_type, 7);
    assert_eq!(record.fields.len(), 11);
    let tags: Vec<u8> = record.fields.iter().map(|field| field.tag.byte()).collect();
    assert_eq!(tags, [1, 2, 3, 4, 5, 6, 7, 8, 9, 13, 19]);
    let widths: Vec<usize> = record
        .fields
        .iter()
        .map(|field| field.value.len())
        .collect();
    assert_eq!(widths, [4, 2, 2, 4, 2, 2, 1, 1, 1, 1, 4]);
    let get = |tag| record.get(tag);
    assert_eq!(get(WorkoutTag::StartTime), Some(u64::from(THERING_START)));
    assert_eq!(get(WorkoutTag::Duration), Some(61));
    assert_eq!(get(WorkoutTag::Distance), Some(0));
    assert_eq!(get(WorkoutTag::Calories), Some(0));
    assert_eq!(get(WorkoutTag::SpeedAvg), Some(0));
    assert_eq!(get(WorkoutTag::SpeedMax), Some(0));
    assert_eq!(get(WorkoutTag::RateAvg), Some(60));
    assert_eq!(get(WorkoutTag::RateMin), Some(57));
    assert_eq!(get(WorkoutTag::RateMax), Some(62));
    assert_eq!(get(WorkoutTag::StepRate), Some(0));
    assert_eq!(get(WorkoutTag::Steps), Some(0));
    assert_eq!(get(WorkoutTag::Elevation), None);
    assert_eq!(get(WorkoutTag::SportCount), None);
}

#[test]
fn thering_descriptor_and_series_give_fifty_four_heart_rates() {
    let trace = parse(THERING);
    let replies = replies(&trace);
    let descriptor = replies
        .iter()
        .find_map(|(_, value)| match value {
            ReplyBody::WorkoutDescriptor(descriptor) => Some(descriptor),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no descriptor"));
    assert_eq!(
        (
            descriptor.status,
            descriptor.package_count,
            descriptor.byte2,
            descriptor.sample_second
        ),
        (0, 1, 0, 1)
    );
    assert_eq!(descriptor.fields.len(), 1);
    assert_eq!(
        (descriptor.fields[0].len, descriptor.fields[0].tag),
        (1, SampleTag::HeartRate)
    );
    assert_eq!(descriptor.record_len(), 1);

    let series = replies
        .iter()
        .find_map(|(_, value)| match value {
            ReplyBody::WorkoutSeries(series) => Some(series),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no series"));
    assert_eq!((series.package, series.byte1), (1, 0));
    assert_eq!(series.records(descriptor).count(), 54);
    let rates = series
        .heart_rates(descriptor)
        .unwrap_or_else(|| panic!("no heart rate field"));
    assert_eq!(rates.len(), 54);
    assert!(rates.iter().all(|bpm| (57..=62).contains(bpm)), "{rates:?}");
    // The summary's rate min and max are the series' extremes.
    assert_eq!(rates.iter().min(), Some(&57));
    assert_eq!(rates.iter().max(), Some(&62));
    assert_eq!(&rates[..7], [62; 7]);
    assert_eq!(&rates[51..], [58, 58, 58]);
}
