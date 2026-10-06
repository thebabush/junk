//! Stage A of the smoke test (SPEC §5): both fixtures under `fixtures/colmi-r10` replayed
//! through the driver by the generic harness.
//!
//! A1: every `rx` line goes to the driver and every `tx` line is turned back into its
//! request, and the driver writes exactly what the app wrote, byte for byte, every request
//! answered once. A2: the answers are the app database rows the docs verified
//! (`docs/colmi-protocol.md`, "Ground truth for tests"); Stage C: the thering trace's
//! workout stream and record fetch.

use std::collections::BTreeMap;

use junk_colmi::proto::{Ev, Resp};
use junk_colmi::wire::{BigDataKind, Notification, WorkoutTag};
use junk_colmi::{Bpm, SleepKind, Source};
use junk_colmi::{ColmiAdapter, ColmiDriver, DIS_FW, DIS_HW, V1_NOTIFY, V2_NOTIFY};
use junk_core::{Channel, Percent, Timestamp};
use junk_trace::{Answer, DataLine, Direction, Replay, Trace, replay};

/// The data line an answer's request came from; every answer here answers a fed request.
fn line_of(answer: &Answer<ColmiDriver>) -> usize {
    answer.line.expect("an answer to a request the harness fed")
}

/// One full `QRing` sync, from the app's own log; the big-data replies are rebuilt from
/// the app database. Its stamps carry no offset: the zone was UTC−4 that day.
const QRING: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/colmi-r10/qring-sync-2026-07-02.trace"
));
const QRING_OFFSET_MIN: i16 = -240;

/// One workout session and record fetch, from `PacketLogger`. Its stamps carry `-05:00`,
/// so the fallback offset is never used.
const THERING: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/colmi-r10/thering-realtime-2025-11-19.trace"
));
const THERING_OFFSET_MIN: i16 = -300;

/// A seven-day `junk sync --record` against the ring on 2026-09-06, and a 75-second
/// `junk live --record`: recorded by the pump itself, so nothing is elided or inferred.
/// Their stamps carry `-04:00`; the fallback offset is never used.
const JUNK_SYNC: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/colmi-r10/junk-sync-2026-09-06.trace"
));
const JUNK_LIVE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/colmi-r10/junk-live-2026-09-06.trace"
));

/// 2026-07-02 00:00 in UTC-4: the `QRing` fixture day, epoch 1782950400.
const FIXTURE_DAY: Timestamp = Timestamp {
    local_minute: 1_782_950_400 / 60,
    utc_offset_min: QRING_OFFSET_MIN,
};

/// The start of the thering workout, as the `77 01` ack and the summary record give it.
const THERING_START: u32 = 0x691d_2980;

/// Five-minute slots in a day, as the app stores the HR log.
const HR_SLOTS: i64 = 288;

/// A replayed fixture: its data lines, for looking requests up, and what the driver did.
struct Session {
    lines: Vec<DataLine>,
    out: Replay<ColmiDriver>,
}

impl Session {
    fn run(text: &str, fallback_offset_min: i16) -> Session {
        let trace = Trace::parse(text).unwrap_or_else(|err| panic!("{err}"));
        let adapter = ColmiAdapter::for_trace(&trace, fallback_offset_min)
            .unwrap_or_else(|| panic!("no data lines"));
        let mut driver = ColmiDriver::new();
        let out = replay(&trace, &adapter, &mut driver);
        assert_eq!(
            driver,
            ColmiDriver::new(),
            "disconnected leaves the driver fresh"
        );
        Session {
            lines: trace.data().cloned().collect(),
            out,
        }
    }

    /// The indices of the `tx` lines.
    fn tx_lines(&self) -> Vec<usize> {
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.dir == Direction::Tx)
            .map(|(index, _)| index)
            .collect()
    }

    /// The answers to the requests whose write starts with `prefix`, in trace order.
    fn answers_to<'a>(
        &'a self,
        prefix: &[u8],
    ) -> impl Iterator<Item = &'a Answer<ColmiDriver>> + 'a {
        let prefix = prefix.to_vec();
        self.out
            .answers
            .iter()
            .filter(move |answer| self.lines[line_of(answer)].bytes.starts_with(&prefix))
    }

    /// The results of the answers whose write starts with `prefix`, all of which must be
    /// `Ok`.
    fn responses(&self, prefix: &[u8]) -> Vec<&Resp> {
        self.answers_to(prefix)
            .map(|answer| {
                answer.result.as_ref().unwrap_or_else(|err| {
                    panic!("{prefix:02x?} at line {}: {err}", line_of(answer))
                })
            })
            .collect()
    }

    /// The first `rx` line after data line `line`.
    fn reply_after(&self, line: usize) -> &[u8] {
        self.lines[line + 1..]
            .iter()
            .find(|next| next.dir == Direction::Rx)
            .map_or_else(|| panic!("no reply after line {line}"), |next| &next.bytes)
    }

    /// The events, tallied by variant name.
    fn events_by_kind(&self) -> BTreeMap<&'static str, usize> {
        let mut tally = BTreeMap::new();
        for event in &self.out.events {
            let kind = match event {
                Ev::Capabilities(_) => "Capabilities",
                Ev::PacketSize(_) => "PacketSize",
                Ev::Notification(_) => "Notification",
                Ev::Battery(_) => "Battery",
                Ev::Workout { .. } => "Workout",
                Ev::Text(_) => "Text",
                Ev::UnexpectedReply(_) => "UnexpectedReply",
                Ev::UnexpectedBigData(_) => "UnexpectedBigData",
                Ev::Unparsed { .. } => "Unparsed",
                Ev::MissingChannels(_) => "MissingChannels",
                Ev::UnknownFirmware(_) => "UnknownFirmware",
            };
            *tally.entry(kind).or_default() += 1;
        }
        tally
    }
}

/// Minutes after the fixture day's midnight.
fn at(minute_of_day: i64) -> Timestamp {
    Timestamp {
        local_minute: FIXTURE_DAY.local_minute + minute_of_day,
        ..FIXTURE_DAY
    }
}

/// Stage A1 on one fixture: the writes match byte for byte, every write was a request,
/// every request was answered exactly once and well, nothing timed out, and the driver
/// asked for exactly the reads whose values the trace carries.
fn stage_a1(session: &Session, tx_lines: usize, reads: &[Channel]) {
    let out = &session.out;
    assert_eq!(out.writes_match(), Ok(()));
    assert_eq!(out.skipped, [] as [usize; 0]);
    assert_eq!(out.unknown_channels, [] as [usize; 0]);
    assert_eq!(out.expected.len(), tx_lines);
    assert_eq!(out.written.len(), tx_lines);
    assert_eq!(out.answers.len(), out.expected.len());

    let mut answered: Vec<usize> = out.answers.iter().map(line_of).collect();
    answered.sort_unstable();
    assert_eq!(answered, session.tx_lines(), "one answer per request");
    for answer in &out.answers {
        assert!(
            answer.result.is_ok(),
            "line {} {:02x?}: {:?}",
            line_of(answer),
            session.lines[line_of(answer)].bytes,
            answer.result
        );
    }

    assert_eq!(out.timers_fired, [] as [(usize, junk_core::TimerId); 0]);
    assert_eq!(out.subscribed, [V1_NOTIFY, V2_NOTIFY]);
    assert_eq!(out.reads, reads);
    assert!(!out.disconnect_requested);
}

#[test]
fn qring_a1_writes_match_and_every_request_is_answered() {
    let session = Session::run(QRING, QRING_OFFSET_MIN);
    // Two version requests, two reads each.
    stage_a1(&session, 118, &[DIS_FW, DIS_HW, DIS_FW, DIS_HW]);
}

#[test]
fn thering_a1_writes_match_and_every_request_is_answered() {
    let session = Session::run(THERING, THERING_OFFSET_MIN);
    stage_a1(&session, 5, &[]);
}

#[test]
fn junk_sync_a1_writes_match_and_every_request_is_answered() {
    let session = Session::run(JUNK_SYNC, QRING_OFFSET_MIN);
    // The app's preamble (nine requests), seven days of four logs, four collections: 41.
    stage_a1(&session, 41, &[DIS_FW, DIS_HW]);
    assert!(session.out.timers_fired.is_empty());
    let versions: Vec<&Resp> = session
        .out
        .answers
        .iter()
        .filter_map(|answer| answer.result.as_ref().ok())
        .filter(|resp| matches!(resp, Resp::Version { .. }))
        .collect();
    assert_eq!(
        versions,
        [&Resp::Version {
            firmware: "RT03CR_1.00.02_260319".to_owned(),
            hardware: "RT03CR_V1.0".to_owned(),
        }]
    );
}

#[test]
fn junk_live_a1_and_stage_c_record_fetch() {
    let session = Session::run(JUNK_LIVE, QRING_OFFSET_MIN);
    // Start, stop, list, detail.
    stage_a1(&session, 4, &[]);
    assert!(session.out.timers_fired.is_empty());
    let workouts: Vec<&Resp> = session
        .out
        .answers
        .iter()
        .filter_map(|answer| answer.result.as_ref().ok())
        .filter(|resp| matches!(resp, Resp::Workouts(_) | Resp::WorkoutDetail { .. }))
        .collect();
    let [
        Resp::Workouts(summary),
        Resp::WorkoutDetail { heart_rates, .. },
    ] = workouts[..]
    else {
        panic!("expected a summary then a detail, got {workouts:?}");
    };
    assert_eq!(summary.records.len(), 1);
    assert_eq!(summary.records[0].get(WorkoutTag::Duration), Some(75));
    assert_eq!(heart_rates.len(), 65);
    assert!(
        heart_rates
            .iter()
            .all(|bpm| matches!(bpm, Bpm::Valid(56..=60)))
    );
}

#[test]
fn qring_a2_battery_and_version() {
    let session = Session::run(QRING, QRING_OFFSET_MIN);

    // The app polls the battery once per connection and then every ten seconds at the
    // end of the log: five polls, 100 % each.
    let batteries = session.responses(&[0x03, 0x01]);
    assert_eq!(batteries.len(), 5);
    for battery in batteries {
        let Resp::Battery(battery) = battery else {
            panic!("{battery:?}");
        };
        assert_eq!(battery.percent, Percent::FULL);
        assert!(!battery.charging);
    }

    // Once per connection, two connections; the strings are read from the Device
    // Information service after each ack, firmware first.
    let versions = session.responses(&[0x19, 0x01]);
    assert_eq!(versions.len(), 2);
    for version in versions {
        assert_eq!(
            version,
            &Resp::Version {
                firmware: "RT03CR_1.00.02_260319".into(),
                hardware: "RT03CR_V1.0".into(),
            }
        );
    }
    assert_eq!(session.out.reads, [DIS_FW, DIS_HW, DIS_FW, DIS_HW]);

    // The set-time acks: the first was sent twice (the app thought it failed) and once on
    // the second connection.
    let caps = session.responses(&[0x01, 0x26]);
    assert_eq!(caps.len(), 3);
    for caps in caps {
        let Resp::Capabilities(caps) = caps else {
            panic!("{caps:?}");
        };
        assert!(caps.temperature);
        assert!(caps.spo2());
        assert!(caps.new_sleep_protocol);
    }
}

#[test]
fn qring_a2_events() {
    let session = Session::run(QRING, QRING_OFFSET_MIN);
    let events = &session.out.events;

    let caps = events
        .iter()
        .filter_map(|event| match event {
            Ev::Capabilities(caps) => Some(caps),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(caps.len(), 3);
    for caps in caps {
        assert!(caps.temperature);
        assert!(caps.spo2());
        assert!(caps.new_sleep_protocol);
    }
    assert!(events.contains(&Ev::PacketSize(244)));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Ev::Notification(_)))
    );

    // Three `2f`, three `01` acks, eight `73` notifications; nothing the driver could not
    // place.
    let tally = session.events_by_kind();
    assert_eq!(
        tally,
        BTreeMap::from([("Capabilities", 3), ("Notification", 8), ("PacketSize", 3)])
    );
    assert_eq!(events.len(), 14);
}

#[test]
fn qring_a2_hr_log_is_the_schedual_heart_rate_row() {
    let session = Session::run(QRING, QRING_OFFSET_MIN);
    let logs = session.responses(&[0x15, 0x00]);
    assert_eq!(logs.len(), 9);
    let Resp::HrLog(samples) = logs[0] else {
        panic!("{:?}", logs[0]);
    };

    // Into the app's 288 five-minute slots.
    let mut slots = [0u8; 288];
    let mut seen = [false; 288];
    for sample in samples {
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
    assert_eq!(samples.len(), 172, "through 14:15");
    assert_eq!(samples[171].at, at(171 * 5));

    // Each later fetch has the readings taken since; the last, at 14:40, reaches 14:35.
    let counts: Vec<usize> = logs
        .iter()
        .map(|log| match log {
            Resp::HrLog(samples) => samples.len(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(counts, [172, 172, 173, 174, 174, 175, 176, 177, 177]);
    let Resp::HrLog(last) = logs[8] else {
        panic!("{:?}", logs[8]);
    };
    assert_eq!(last[176].at, at(176 * 5));
}

#[test]
fn qring_a2_activity_is_the_step_rows() {
    let session = Session::run(QRING, QRING_OFFSET_MIN);
    let days = session.responses(&[0x43]);
    assert_eq!(
        days.len(),
        38,
        "30 days on the first sync, then two per round"
    );

    let first = days
        .iter()
        .find_map(|day| match day {
            Resp::Activity(buckets) if !buckets.is_empty() => Some(buckets),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no activity rows"));
    assert_eq!(first.len(), 5);
    let bucket = first[0];
    assert_eq!(bucket.start, at(60));
    assert_eq!(bucket.steps, 28);
    assert_eq!(bucket.cal, 1120);
    assert_eq!(bucket.distance_m, 19);
    let steps: Vec<u16> = first.iter().map(|b| b.steps).collect();
    assert_eq!(steps, [28, 3, 732, 371, 267]);
    for bucket in first {
        assert_eq!(bucket.start.utc_offset_min, -240);
        assert_eq!(
            (bucket.start.local_minute - FIXTURE_DAY.local_minute) % 60,
            0
        );
    }

    // Every other day is empty; today's later fetches have the same five hours, the 14:00
    // one growing.
    let empty = days
        .iter()
        .filter(|day| matches!(day, Resp::Activity(buckets) if buckets.is_empty()))
        .count();
    assert_eq!(empty, 38 - 5);
}

#[test]
fn qring_a2_stress_and_hrv_are_the_series_rows() {
    let session = Session::run(QRING, QRING_OFFSET_MIN);

    let stress = session.responses(&[0x37]);
    assert_eq!(stress.len(), 6);
    let Resp::Stress(samples) = stress[0] else {
        panic!("{:?}", stress[0]);
    };
    let slots: Vec<(Timestamp, u8)> = samples.iter().map(|s| (s.at, s.value)).collect();
    assert_eq!(
        slots[..12],
        [
            (at(0), 43),
            (at(30), 39),
            (at(60), 36),
            (at(90), 34),
            (at(120), 47),
            (at(150), 46),
            (at(180), 42),
            (at(210), 35),
            (at(240), 34),
            (at(270), 39),
            (at(300), 36),
            (at(330), 33)
        ]
    );
    assert_eq!(samples.len(), 29, "half-hourly through 14:00");
    assert!(samples.iter().all(|s| s.at.utc_offset_min == -240));

    let hrv = session.responses(&[0x39]);
    assert_eq!(hrv.len(), 5);
    let Resp::Hrv(samples) = hrv[0] else {
        panic!("{:?}", hrv[0]);
    };
    assert_eq!((samples[0].at, samples[0].value), (at(0), 30));
    assert_eq!((samples[1].at, samples[1].value), (at(60), 43));
    assert_eq!((samples[2].at, samples[2].value), (at(120), 49));
    assert_eq!(samples.len(), 15, "hourly through 14:00");
}

#[test]
fn qring_a2_sleep_is_the_sleep_v3_row() {
    let session = Session::run(QRING, QRING_OFFSET_MIN);
    let sleeps = session.responses(&[0xbc, 0x27]);
    assert_eq!(sleeps.len(), 5);
    let Resp::Sleep(sessions) = sleeps[0] else {
        panic!("{:?}", sleeps[0]);
    };
    assert_eq!(sessions.len(), 1);
    let session = &sessions[0];
    assert_eq!(session.start, at(3 * 60 + 7));
    assert_eq!(session.end, at(11 * 60 + 49));
    assert_eq!(session.stages.len(), 21);
    let minutes: u32 = session.stages.iter().map(|s| u32::from(s.minutes)).sum();
    assert_eq!(minutes, 522);
    let rem = session
        .stages
        .iter()
        .filter(|s| s.kind == SleepKind::Rem)
        .count();
    assert_eq!(rem, 5);
    // Every later round gets the same night.
    assert!(sleeps.iter().all(|sleep| sleep == &sleeps[0]));
}

#[test]
fn qring_a2_spo2_is_the_blood_oxygen_rows() {
    let session = Session::run(QRING, QRING_OFFSET_MIN);
    let spo2 = session.responses(&[0xbc, 0x2a]);
    assert_eq!(spo2.len(), 6);
    let Resp::Spo2(samples) = spo2[0] else {
        panic!("{:?}", spo2[0]);
    };
    assert_eq!(samples.len(), 14, "hours 0–13");
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
    // From the 14:32 fetch on, the 14:00 reading is there too.
    let counts: Vec<usize> = spo2
        .iter()
        .map(|reply| match reply {
            Resp::Spo2(samples) => samples.len(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(counts, [14, 14, 14, 15, 15, 15]);
    let Resp::Spo2(later) = spo2[3] else {
        panic!("{:?}", spo2[3]);
    };
    assert_eq!((later[14].at, later[14].percent.get()), (at(14 * 60), 99));
}

#[test]
fn qring_a2_temperature_is_the_temperatures_rows() {
    let session = Session::run(QRING, QRING_OFFSET_MIN);
    let temperature = session.responses(&[0xbc, 0x25]);
    assert_eq!(temperature.len(), 5);
    let Resp::Temperature(samples) = temperature[0] else {
        panic!("{:?}", temperature[0]);
    };
    assert_eq!(samples.len(), 29, "slots 0–28");
    let first: Vec<i16> = samples[..8].iter().map(|s| s.deci_celsius).collect();
    assert_eq!(first, [367, 368, 368, 368, 368, 368, 361, 368]);
    for (slot, sample) in samples.iter().enumerate() {
        assert_eq!(sample.at, at(i64::try_from(slot).unwrap() * 30));
    }
    // The second connection's fetches have the 14:30 sample too.
    let counts: Vec<usize> = temperature
        .iter()
        .map(|reply| match reply {
            Resp::Temperature(samples) => samples.len(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(counts, [29, 29, 29, 30, 30]);
    let Resp::Temperature(later) = temperature[3] else {
        panic!("{:?}", temperature[3]);
    };
    assert_eq!(
        (later[29].at, later[29].deci_celsius),
        (at(14 * 60 + 30), 366)
    );
}

#[test]
fn qring_a2_raw_commands_pass_through() {
    let session = Session::run(QRING, QRING_OFFSET_MIN);

    // The three unknown small-channel commands: the answer is the trace's reply frame.
    for cmd in [0x3a, 0x3b, 0x3c] {
        let answers: Vec<_> = session.answers_to(&[cmd]).collect();
        assert_eq!(answers.len(), 2, "{cmd:#04x}");
        for answer in answers {
            let Ok(Resp::Raw(frame)) = &answer.result else {
                panic!("{cmd:#04x}: {:?}", answer.result);
            };
            assert_eq!(
                frame.to_bytes(),
                session.reply_after(line_of(answer)),
                "{cmd:#04x}"
            );
        }
    }

    // The file list, undecoded: kind `0x30`, body `00`.
    let files = session.responses(&[0xbc, 0x30]);
    assert_eq!(files.len(), 2);
    for file in files {
        let Resp::RawBigData(big) = file else {
            panic!("{file:?}");
        };
        assert_eq!(big.kind, BigDataKind::FileList);
        assert_eq!(big.body(), [0x00]);
    }

    // The workout list: no records stored.
    let workouts = session.responses(&[0xbc, 0x41]);
    assert_eq!(workouts.len(), 5);
    for workouts in workouts {
        let Resp::Workouts(summary) = workouts else {
            panic!("{workouts:?}");
        };
        assert!(summary.records.is_empty());
    }

    // What else the app asked, once per connection.
    assert_eq!(session.responses(&[0x04, 0x01]), [&Resp::Ack, &Resp::Ack]);
    assert_eq!(session.responses(&[0x0a, 0x02]), [&Resp::Ack, &Resp::Ack]);
    assert_eq!(session.responses(&[0x0a, 0x01]).len(), 2);
    assert_eq!(session.responses(&[0x21]).len(), 2);
    for prefix in [[0x16, 0x01], [0x2c, 0x01], [0x36, 0x01], [0x38, 0x01]] {
        let prefs = session.responses(&prefix);
        assert_eq!(prefs.len(), 2, "{prefix:02x?}");
        assert!(
            prefs
                .iter()
                .all(|pref| matches!(pref, Resp::AutoPref { enabled: true, .. })),
            "{prefix:02x?}: {prefs:?}"
        );
    }
    let totals = session.responses(&[0x48]);
    assert_eq!(totals.len(), 5);
    assert_eq!(
        totals[0],
        &Resp::TodayTotals {
            steps: 1401,
            running_steps: 0,
            cal: 63_407,
            distance_m: 1076,
            active_min: 41,
        }
    );
}

#[test]
fn thering_stage_c_workout_stream_and_record() {
    let session = Session::run(THERING, THERING_OFFSET_MIN);
    let out = &session.out;

    // Start, pause, stop; then the record list and the record's detail.
    let results: Vec<&Resp> = out
        .answers
        .iter()
        .map(|answer| answer.result.as_ref().unwrap_or_else(|err| panic!("{err}")))
        .collect();
    assert_eq!(results.len(), 5);
    assert_eq!(
        results[0],
        &Resp::WorkoutCtl {
            start: Some(THERING_START)
        }
    );
    assert_eq!(results[1], &Resp::WorkoutCtl { start: None });
    assert_eq!(results[2], &Resp::WorkoutCtl { start: None });
    let Resp::Workouts(summary) = results[3] else {
        panic!("{:?}", results[3]);
    };
    assert_eq!(summary.records.len(), 1);
    let record = &summary.records[0];
    assert_eq!(record.sport_type, 7);
    assert_eq!(
        record.get(WorkoutTag::StartTime),
        Some(u64::from(THERING_START))
    );
    assert_eq!(record.get(WorkoutTag::Duration), Some(61));
    let Resp::WorkoutDetail {
        descriptor,
        heart_rates,
    } = results[4]
    else {
        panic!("{:?}", results[4]);
    };
    assert_eq!(descriptor.package_count, 1);
    assert_eq!(heart_rates.len(), 54);
    assert!(
        heart_rates
            .iter()
            .all(|bpm| matches!(bpm, Bpm::Valid(57..=62)))
    );

    // The live stream: 70 samples of sport 7, sequence numbers 0–61 with eight repeats,
    // flag 1 while running and 2 after the pause.
    let samples: Vec<(u8, u8, u8, Bpm)> = out
        .events
        .iter()
        .filter_map(|event| match event {
            Ev::Workout {
                sport_type,
                flag,
                seq,
                bpm,
            } => Some((*sport_type, *flag, *seq, *bpm)),
            _ => None,
        })
        .collect();
    assert_eq!(samples.len(), 70);
    assert!(samples.iter().all(|&(sport, ..)| sport == 7));
    let mut seqs: Vec<u8> = samples.iter().map(|&(_, _, seq, _)| seq).collect();
    seqs.dedup();
    assert_eq!(seqs, (0..=61).collect::<Vec<u8>>());
    assert_eq!(samples[0], (7, 1, 0, Bpm::Invalid));
    assert_eq!(samples[7], (7, 1, 7, Bpm::Valid(62)));
    assert_eq!(samples[68], (7, 2, 61, Bpm::Valid(58)));
    assert_eq!(samples[69], (7, 2, 61, Bpm::Valid(58)));
    let running = samples.iter().filter(|&&(_, flag, ..)| flag == 1).count();
    let paused = samples.iter().filter(|&&(_, flag, ..)| flag == 2).count();
    assert_eq!((running, paused), (68, 2));

    // The ring's own news: its battery, and that the record was stored.
    assert!(out.events.contains(&Ev::Notification(Notification::Battery(
        Percent::new(94).unwrap()
    ))));
    assert!(
        out.events
            .contains(&Ev::Notification(Notification::WorkoutStored))
    );
    assert_eq!(
        session.events_by_kind(),
        BTreeMap::from([("Notification", 2), ("Workout", 70)])
    );
}
