//! The multi-packet transactions against the frames of `fixtures/colmi-r10`: each request
//! written byte for byte as the app wrote it, the trace's replies to that request fed
//! back, and the samples compared with the app database rows the docs verified
//! (`docs/colmi-protocol.md`, "Ground truth for tests").
//!
//! The `QRing` trace covers the HR log, the activity rows, the stress and HRV series, and
//! the (reconstructed) sleep, `SpO2` and temperature replies for 2026-07-02; the thering
//! trace covers the workout record flow.

use junk_colmi::proto::{Ev, Req, Resp, TIMEOUT, TIMEOUT_TIMER};
use junk_colmi::wire::{
    BigData, DescriptorField, ReplyBody, SampleTag, WorkoutDescriptor, WorkoutSeries, WorkoutTag,
};
use junk_colmi::{Bpm, SleepKind, Source};
use junk_colmi::{ColmiDriver, V1_NOTIFY, V1_WRITE, V2_CMD, V2_NOTIFY, channel_by_name};
use junk_core::{
    Channel, Driver, Duration, Input, Instant, Output, Outputs, ProtoError, ReqId, Timestamp,
};
use junk_trace::{Direction, Trace};

type Out = Output<Resp, Ev>;

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

/// The `QRing` fixture's `0x01` ack: temperature and the new sleep protocol.
const CAPS_ACK: &str = "01010000020000000001002000003055";

/// 2026-07-02 00:00 in UTC-4: the `QRing` fixture day, epoch 1782950400.
const FIXTURE_DAY: Timestamp = Timestamp {
    local_minute: 1_782_950_400 / 60,
    utc_offset_min: -240,
};

/// 2026-07-02 14:16 in UTC-4: when the fixture set the ring's clock, and "today" for
/// the requests that place days.
const FIXTURE_TIME: Timestamp = Timestamp {
    local_minute: FIXTURE_DAY.local_minute + 14 * 60 + 16,
    utc_offset_min: -240,
};

/// The start of the thering workout, as the `77 01` ack and the summary record give it.
const THERING_START: u32 = 0x691d_2980;

/// Five-minute slots in a day, as the app stores the HR log.
const HR_SLOTS: i64 = 288;

fn parse(text: &str) -> Trace {
    Trace::parse(text).unwrap_or_else(|err| panic!("{err}"))
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

/// The data lines of `trace` as `(direction, channel, bytes)`.
fn lines(trace: &Trace) -> Vec<(Direction, Channel, Vec<u8>)> {
    trace
        .data()
        .map(|line| {
            let chan = channel_by_name(&line.chan)
                .unwrap_or_else(|| panic!("unknown channel {}", line.chan));
            (line.dir, chan, line.bytes.clone())
        })
        .collect()
}

/// The replies to the first `tx` of exactly `request` in `trace`: every `rx` line after
/// it, up to the next `tx`.
fn replies_to(trace: &Trace, request: &[u8]) -> Vec<(Channel, Vec<u8>)> {
    let lines = lines(trace);
    let at = lines
        .iter()
        .position(|(dir, _, bytes)| *dir == Direction::Tx && bytes == request)
        .unwrap_or_else(|| panic!("no tx {request:02x?}"));
    lines[at + 1..]
        .iter()
        .take_while(|(dir, ..)| *dir == Direction::Rx)
        .map(|(_, chan, bytes)| (*chan, bytes.clone()))
        .collect()
}

/// The first `rx` big-data frame of `kind` in `trace` whose CRC field is `crc`, for the
/// reply variants the docs name by their CRC.
fn reply_with_crc(trace: &Trace, kind: u8, crc: [u8; 2]) -> Vec<u8> {
    lines(trace)
        .into_iter()
        .find(|(dir, chan, bytes)| {
            *dir == Direction::Rx
                && *chan == V2_NOTIFY
                && bytes.get(1) == Some(&kind)
                && bytes.get(4..6) == Some(&crc[..])
        })
        .map_or_else(
            || panic!("no {kind:#04x} reply with crc {crc:02x?}"),
            |(_, _, bytes)| bytes,
        )
}

fn step(driver: &mut ColmiDriver, input: Input<Req>) -> Vec<Out> {
    let mut out = Outputs::new();
    driver.handle(input, &mut out);
    out.into_vec()
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

/// A driver on a ring with both services that has acked the set-time with the fixture's
/// capabilities, so every request is allowed.
fn connected() -> ColmiDriver {
    let mut driver = ColmiDriver::new();
    step(
        &mut driver,
        Input::Connected {
            resolved: [V1_WRITE, V1_NOTIFY, V2_CMD, V2_NOTIFY]
                .into_iter()
                .collect(),
            mtu: 247,
        },
    );
    step(&mut driver, rx(V1_NOTIFY, &hex(CAPS_ACK)));
    driver
}

/// Sends `req` as request `id`, checks it writes `write` on `chan`, feeds `replies` and
/// returns the answer: every reply but the last must produce nothing, and the last must
/// cancel the timer and answer `id`.
fn exchange(
    driver: &mut ColmiDriver,
    id: u32,
    req: Req,
    chan: Channel,
    write: &[u8],
    replies: &[(Channel, Vec<u8>)],
) -> Result<Resp, ProtoError> {
    assert_eq!(
        step(driver, request(id, req)),
        vec![tx(chan, write), set_timer()]
    );
    let (last, rest) = replies.split_last().unwrap_or_else(|| panic!("no replies"));
    for (n, (chan, bytes)) in rest.iter().enumerate() {
        assert_eq!(step(driver, rx(*chan, bytes)), vec![], "reply {n}");
    }
    let outs = step(driver, rx(last.0, &last.1));
    match outs.as_slice() {
        [
            Output::CancelTimer(TIMEOUT_TIMER),
            Output::Done { id: got, result },
        ] if *got == ReqId(id) => result.clone(),
        other => panic!("last reply gave {other:?}"),
    }
}

/// Minutes after the fixture day's midnight.
fn at(minute_of_day: i64) -> Timestamp {
    Timestamp {
        local_minute: FIXTURE_DAY.local_minute + minute_of_day,
        ..FIXTURE_DAY
    }
}

#[test]
fn hr_log_is_the_schedual_heart_rate_row() {
    let trace = parse(QRING);
    let mut driver = connected();
    let write = hex("1500aa456a000000000000000000006e");
    let replies = replies_to(&trace, &write);
    assert_eq!(replies.len(), 24, "the header and packets 1–23");
    assert_eq!(replies[0].1, hex("15001805000000000000000000000032"));
    let result = exchange(
        &mut driver,
        1,
        Req::HrLog {
            day_start: FIXTURE_DAY,
        },
        V1_WRITE,
        &write,
        &replies,
    );
    let Ok(Resp::HrLog(samples)) = result else {
        panic!("{result:?}");
    };

    // Into the app's 288 five-minute slots.
    let mut slots = [0u8; 288];
    let mut seen = [false; 288];
    for sample in &samples {
        assert_eq!(sample.source, Source::Periodic);
        assert_eq!(sample.at.utc_offset_min, -240);
        let minute = sample.at.local_minute - FIXTURE_DAY.local_minute;
        assert_eq!(minute % 5, 0, "{sample:?}");
        let slot = minute / 5;
        assert!((0..HR_SLOTS).contains(&slot), "{sample:?}");
        let slot = usize::try_from(slot).unwrap();
        assert!(!seen[slot], "slot {slot} twice");
        seen[slot] = true;
        slots[slot] = match sample.bpm {
            Bpm::Valid(bpm) => bpm,
            Bpm::Invalid => 0,
        };
    }
    assert_eq!(
        slots[..30],
        [
            63, 80, 74, 71, 61, 60, 58, 63, 117, 61, 59, 60, 59, 59, 56, 80, 82, 78, 79, 73, 88,
            60, 60, 71, 57, 81, 62, 58, 114, 83
        ]
    );
    // Packet 14 ends the morning's readings at slot 171; nothing after 14:16 yet.
    assert_eq!(slots[171], 0x47);
    assert!(slots[172..].iter().all(|&bpm| bpm == 0));
    assert_eq!(samples.len(), 172);
    assert_eq!(samples[171].at, at(171 * 5));
}

#[test]
fn activity_is_the_step_rows() {
    let trace = parse(QRING);
    let mut driver = connected();
    let write = hex("43000f005f01000000000000000000b2");
    let replies = replies_to(&trace, &write);
    assert_eq!(replies.len(), 6, "the header and five rows");
    assert_eq!(replies[0].1, hex("43f00501000000000000000000000039"));
    let result = exchange(
        &mut driver,
        1,
        Req::activity_day(0, FIXTURE_TIME),
        V1_WRITE,
        &write,
        &replies,
    );
    let Ok(Resp::Activity(buckets)) = result else {
        panic!("{result:?}");
    };
    assert_eq!(buckets.len(), 5);
    let first = buckets[0];
    assert_eq!(first.start, at(60));
    assert_eq!(first.span, Duration::from_secs(3600));
    assert_eq!(first.steps, 28);
    assert_eq!(first.cal, 1120);
    assert_eq!(first.distance_m, 19);
    for bucket in &buckets {
        let minute = bucket.start.local_minute - FIXTURE_DAY.local_minute;
        assert!((0..24 * 60).contains(&minute), "{bucket:?}");
        assert_eq!(minute % 60, 0, "{bucket:?}");
        assert_eq!(bucket.start.utc_offset_min, -240);
        assert_eq!(bucket.span, Duration::from_secs(3600));
    }
    let hours: Vec<i64> = buckets
        .iter()
        .map(|b| (b.start.local_minute - FIXTURE_DAY.local_minute) / 60)
        .collect();
    assert_eq!(hours, [1, 11, 12, 13, 14]);
    let steps: Vec<u16> = buckets.iter().map(|b| b.steps).collect();
    assert_eq!(steps, [28, 3, 732, 371, 267]);

    // The oldest day QRing keeps has nothing; QRing asks for it from bucket 1
    // (`43 1d 0f 01 5f 01`), which the bucket range reproduces exactly.
    let replies = replies_to(&trace, &hex("431d0f015f01000000000000000000d0"));
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].1, hex("43ff0000000000000000000000000042"));
    let result = exchange(
        &mut driver,
        2,
        Req::Activity {
            days_ago: 29,
            today: FIXTURE_TIME,
            first_bucket: 1,
            last_bucket: Req::LAST_BUCKET,
        },
        V1_WRITE,
        &hex("431d0f015f01000000000000000000d0"),
        &replies,
    );
    assert_eq!(result, Ok(Resp::Activity(vec![])));
}

#[test]
fn stress_and_hrv_are_the_series_rows() {
    let trace = parse(QRING);
    let mut driver = connected();
    let write = hex("37000000000000000000000000000037");
    let replies = replies_to(&trace, &write);
    assert_eq!(replies.len(), 5, "the header and packets 1–4");
    assert_eq!(replies[0].1, hex("3700051e00000000000000000000005a"));
    let result = exchange(
        &mut driver,
        1,
        Req::Stress {
            days_ago: 0,
            today: FIXTURE_TIME,
        },
        V1_WRITE,
        &write,
        &replies,
    );
    let Ok(Resp::Stress(samples)) = result else {
        panic!("{result:?}");
    };
    let slots: Vec<(i64, u8)> = samples
        .iter()
        .map(|s| ((s.at.local_minute - FIXTURE_DAY.local_minute) / 30, s.value))
        .collect();
    assert_eq!(
        slots[..12],
        [
            (0, 43),
            (1, 39),
            (2, 36),
            (3, 34),
            (4, 47),
            (5, 46),
            (6, 42),
            (7, 35),
            (8, 34),
            (9, 39),
            (10, 36),
            (11, 33)
        ]
    );
    for sample in &samples {
        assert_eq!((sample.at.local_minute - FIXTURE_DAY.local_minute) % 30, 0);
        assert_eq!(sample.at.utc_offset_min, -240);
    }
    // Half-hourly through 14:00: slots 0–28.
    assert_eq!(samples.len(), 29);
    assert_eq!(samples[28].at, at(14 * 60));

    let write = hex("39000000000000000000000000000039");
    let replies = replies_to(&trace, &write);
    assert_eq!(replies.len(), 5);
    let result = exchange(
        &mut driver,
        2,
        Req::Hrv {
            days_ago: 0,
            today: FIXTURE_TIME,
        },
        V1_WRITE,
        &write,
        &replies,
    );
    let Ok(Resp::Hrv(samples)) = result else {
        panic!("{result:?}");
    };
    assert_eq!((samples[0].at, samples[0].value), (at(0), 30));
    assert_eq!((samples[1].at, samples[1].value), (at(60), 43));
    assert_eq!((samples[2].at, samples[2].value), (at(120), 49));
    // Hourly: the half-hour slots are zero on the wire and absent here.
    assert!(
        samples
            .iter()
            .all(|s| (s.at.local_minute - FIXTURE_DAY.local_minute) % 60 == 0)
    );
    assert!(!samples.iter().any(|s| s.at == at(30)));
    assert_eq!(samples.len(), 15, "hours 0–14");
    assert_eq!((samples[14].at, samples[14].value), (at(14 * 60), 39));
}

#[test]
fn sleep_is_the_sleep_v3_row() {
    let trace = parse(QRING);
    let mut driver = connected();
    let write = hex("bc270200c3d00601");
    let replies = replies_to(&trace, &write);
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].0, V2_NOTIFY);
    assert_eq!(replies[0].1.len(), 55);
    let result = exchange(
        &mut driver,
        1,
        Req::Sleep {
            today: FIXTURE_TIME,
            selector: vec![0x06, 0x01],
        },
        V2_CMD,
        &write,
        &replies,
    );
    let Ok(Resp::Sleep(sessions)) = result else {
        panic!("{result:?}");
    };
    assert_eq!(sessions.len(), 1);
    let session = &sessions[0];
    assert_eq!(session.start, at(3 * 60 + 7));
    assert_eq!(session.end, at(11 * 60 + 49));
    assert_eq!(session.stages.len(), 21);
    let minutes: u32 = session
        .stages
        .iter()
        .map(|stage| u32::from(stage.minutes))
        .sum();
    assert_eq!(minutes, 522);
    assert_eq!(
        session
            .stages
            .iter()
            .filter(|stage| stage.kind == SleepKind::Rem)
            .count(),
        5
    );
    let first = session.stages[0];
    assert_eq!((first.kind, first.minutes), (SleepKind::Light, 24));
    let last = session.stages[20];
    assert_eq!((last.kind, last.minutes), (SleepKind::Light, 49));

    // The later rounds ask with `01 01` and get the same night.
    let write = hex("bc270200c1e00101");
    let replies = replies_to(&trace, &write);
    let result = exchange(
        &mut driver,
        2,
        Req::Sleep {
            today: FIXTURE_TIME,
            selector: vec![0x01, 0x01],
        },
        V2_CMD,
        &write,
        &replies,
    );
    assert_eq!(result, Ok(Resp::Sleep(sessions)));
}

#[test]
fn spo2_is_the_blood_oxygen_rows() {
    let trace = parse(QRING);
    let mut driver = connected();
    let write = hex("bc2a0100ff00ff");
    let replies = replies_to(&trace, &write);
    assert_eq!(replies.len(), 1);
    let result = exchange(
        &mut driver,
        1,
        Req::Spo2 {
            today: FIXTURE_TIME,
            selector: 0xff,
        },
        V2_CMD,
        &write,
        &replies,
    );
    let Ok(Resp::Spo2(samples)) = result else {
        panic!("{result:?}");
    };
    let hours: Vec<(Timestamp, u8)> = samples.iter().map(|s| (s.at, s.percent.get())).collect();
    assert_eq!(
        hours[..5],
        [
            (at(0), 98),
            (at(60), 96),
            (at(120), 97),
            (at(180), 99),
            (at(240), 96)
        ]
    );
    assert_eq!(samples.len(), 14, "hours 0–13");
    for (hour, sample) in samples.iter().enumerate() {
        assert_eq!(sample.at, at(i64::try_from(hour).unwrap() * 60));
    }

    // The 14:32 reply has the 14:00 reading too.
    let later = reply_with_crc(&trace, 0x2a, [0x29, 0x4f]);
    let result = exchange(
        &mut driver,
        2,
        Req::Spo2 {
            today: FIXTURE_TIME,
            selector: 0x02,
        },
        V2_CMD,
        &hex("bc2a01003e8102"),
        &[(V2_NOTIFY, later)],
    );
    let Ok(Resp::Spo2(samples)) = result else {
        panic!("{result:?}");
    };
    assert_eq!(samples.len(), 15);
    assert_eq!(
        (samples[14].at, samples[14].percent.get()),
        (at(14 * 60), 99)
    );
}

#[test]
fn temperature_is_the_temperatures_rows() {
    let trace = parse(QRING);
    let mut driver = connected();
    let write = hex("bc250100bf4000");
    let replies = replies_to(&trace, &write);
    assert_eq!(replies.len(), 1);
    let result = exchange(
        &mut driver,
        1,
        Req::Temperature {
            today: FIXTURE_TIME,
            selector: 0,
        },
        V2_CMD,
        &write,
        &replies,
    );
    let Ok(Resp::Temperature(samples)) = result else {
        panic!("{result:?}");
    };
    assert_eq!(samples.len(), 29, "slots 0–28");
    let first: Vec<i16> = samples[..8].iter().map(|s| s.deci_celsius).collect();
    assert_eq!(first, [367, 368, 368, 368, 368, 368, 361, 368]);
    for (slot, sample) in samples.iter().enumerate() {
        assert_eq!(sample.at, at(i64::try_from(slot).unwrap() * 30));
        assert_eq!(sample.at.utc_offset_min, -240);
    }

    // The reply after the 14:30 sample has one more.
    let later = reply_with_crc(&trace, 0x25, [0x64, 0x10]);
    let result = exchange(
        &mut driver,
        2,
        Req::Temperature {
            today: FIXTURE_TIME,
            selector: 0,
        },
        V2_CMD,
        &write,
        &[(V2_NOTIFY, later)],
    );
    let Ok(Resp::Temperature(samples)) = result else {
        panic!("{result:?}");
    };
    assert_eq!(samples.len(), 30);
    assert_eq!(
        (samples[29].at, samples[29].deci_celsius),
        (at(14 * 60 + 30), 366)
    );
}

#[test]
fn workouts_are_the_thering_record() {
    let trace = parse(THERING);
    let mut driver = connected();
    let write = hex("bc410400002400000000");
    let replies = replies_to(&trace, &write);
    assert_eq!(replies.len(), 1);
    let result = exchange(
        &mut driver,
        1,
        Req::WorkoutList { since: 0 },
        V2_CMD,
        &write,
        &replies,
    );
    let Ok(Resp::Workouts(summary)) = result else {
        panic!("{result:?}");
    };
    assert_eq!(summary.records.len(), 1);
    let record = &summary.records[0];
    assert_eq!(record.sport_type, 7);
    assert_eq!(
        record.get(WorkoutTag::StartTime),
        Some(u64::from(THERING_START))
    );
    assert_eq!(record.get(WorkoutTag::Duration), Some(61));
    assert_eq!(record.get(WorkoutTag::RateAvg), Some(60));
    assert_eq!(record.get(WorkoutTag::RateMin), Some(57));
    assert_eq!(record.get(WorkoutTag::RateMax), Some(62));

    let write = hex("bc430500a0b60780291d69");
    let replies = replies_to(&trace, &write);
    assert_eq!(replies.len(), 2, "the descriptor and one package");
    let result = exchange(
        &mut driver,
        2,
        Req::WorkoutDetail {
            sport_type: 7,
            start: THERING_START,
        },
        V2_CMD,
        &write,
        &replies,
    );
    let Ok(Resp::WorkoutDetail {
        descriptor,
        heart_rates,
    }) = result
    else {
        panic!("{result:?}");
    };
    assert_eq!(descriptor.package_count, 1);
    assert_eq!(descriptor.sample_second, 1);
    assert_eq!(heart_rates.len(), 54);
    let valid: Vec<u8> = heart_rates
        .iter()
        .map(|bpm| match bpm {
            Bpm::Valid(value) => *value,
            Bpm::Invalid => panic!("{heart_rates:?}"),
        })
        .collect();
    assert!(valid.iter().all(|bpm| (57..=62).contains(bpm)));
    assert_eq!(valid.iter().min(), Some(&57));
    assert_eq!(valid.iter().max(), Some(&62));
}

#[test]
fn workout_packages_must_follow_the_descriptor_in_order() {
    let trace = parse(THERING);
    let write = hex("bc430500a0b60780291d69");
    let replies = replies_to(&trace, &write);
    let (descriptor, series) = (&replies[0].1, &replies[1].1);
    let detail = || Req::WorkoutDetail {
        sport_type: 7,
        start: THERING_START,
    };

    // The series before the descriptor is a complete frame nobody wanted; the
    // transaction is still waiting, and the descriptor then the series answer it.
    let mut driver = connected();
    assert_eq!(
        step(&mut driver, request(1, detail())),
        vec![tx(V2_CMD, &write), set_timer()]
    );
    assert_eq!(
        step(&mut driver, rx(V2_NOTIFY, series)),
        vec![Output::Event(Ev::UnexpectedBigData(
            BigData::parse(series).unwrap()
        ))]
    );
    assert_eq!(step(&mut driver, rx(V2_NOTIFY, descriptor)), vec![]);
    let outs = step(&mut driver, rx(V2_NOTIFY, series));
    assert!(
        matches!(
            outs.as_slice(),
            [Output::CancelTimer(TIMEOUT_TIMER), Output::Done { id: ReqId(1), result: Ok(Resp::WorkoutDetail { heart_rates, .. }) }]
                if heart_rates.len() == 54
        ),
        "{outs:?}"
    );

    // A descriptor announcing three packages, then package 1 and package 3.
    let three = ReplyBody::WorkoutDescriptor(WorkoutDescriptor {
        status: 0,
        package_count: 3,
        byte2: 0,
        sample_second: 1,
        fields: vec![DescriptorField {
            len: 1,
            tag: SampleTag::HeartRate,
        }],
    })
    .encode()
    .unwrap()
    .to_bytes();
    let package = |number: u8| {
        ReplyBody::WorkoutSeries(WorkoutSeries {
            package: number,
            byte1: 0,
            data: vec![60, 61],
        })
        .encode()
        .unwrap()
        .to_bytes()
    };
    let mut driver = connected();
    step(&mut driver, request(2, detail()));
    assert_eq!(step(&mut driver, rx(V2_NOTIFY, &three)), vec![]);
    assert_eq!(step(&mut driver, rx(V2_NOTIFY, &package(1))), vec![]);
    assert_eq!(
        step(&mut driver, rx(V2_NOTIFY, &package(3))),
        vec![
            cancel_timer(),
            Output::Done {
                id: ReqId(2),
                result: Err(ProtoError::Malformed("workout: package order")),
            }
        ]
    );
}
