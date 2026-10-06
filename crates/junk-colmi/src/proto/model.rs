//! From wire values to model samples: where a packet's slots get their timestamps.
//!
//! Every function here is pure: what the wire layer decoded, plus the
//! [`Timestamp`]s the request was made under, in; core samples out. The clock rules are
//! the ones `docs/colmi-protocol.md` states and the app database confirms, and the tests
//! pin them on the fixture bytes:
//!
//! - A slot's time is its day's midnight plus `slot × interval`, in the request's zone.
//! - A zero slot is no sample and yields nothing, except in the HR log, where it is an
//!   [`Bpm::Invalid`](crate::Bpm::Invalid) sample that keeps its slot (unless it is padding
//!   after the last reading).
//! - Activity rows carry their own date; only the zone comes from the request.
//! - A sleep session that starts at or after 18:00 of its day began the evening before.

use alloc::vec::Vec;

use crate::measure::{
    HrSample, HrvSample, SleepSession, Source, Spo2Sample, StepBucket, StressSample, TempSample,
    bpm_from_raw,
};
use junk_core::{Duration, Timestamp};

use crate::proto::{MINUTES_PER_DAY, day_minute, days_from_civil, minutes_after};
use crate::wire::{ActivityPacket, SleepBody, Spo2Body, TemperatureBody};

/// Minutes per activity bucket index: the `0f` of the request asks for 15-minute units,
/// and a row's `bucket` counts in them (R09 decompilation; `04` → 01:00, `2c` → 11:00 in
/// the fixture).
const BUCKET_MIN: i64 = 15;
/// How long an activity row covers. This ring sends whole hours only (five rows on the
/// fixture day, every one on the hour, and the app's `step` table has one row per hour).
/// A ring that sent finer rows would need the span derived from consecutive rows'
/// buckets instead of a constant.
const BUCKET_SPAN: Duration = Duration::from_secs(60 * 60);
/// Under the new activity protocol the calorie field is in units of ten calories: raw
/// `112` is the app's `1120.0`.
const NEW_PROTOCOL_CAL_UNIT: u32 = 10;
/// A sleep session whose start is at or after this minute of its day (18:00) began the
/// evening before, which is how the app places it.
const EVENING_MIN: u16 = 18 * 60;
/// Minutes per `SpO2` slot: the ring reports one value per hour.
const SPO2_SLOT_MIN: u8 = 60;

/// The non-zero entries of `slots` with their times: slot `i` is `i × interval_min`
/// minutes after `day`.
fn samples(
    day: Timestamp,
    interval_min: u8,
    slots: &[u8],
) -> impl Iterator<Item = (Timestamp, u8)> {
    slots
        .iter()
        .enumerate()
        .filter(|&(_, &value)| value != 0)
        .map(move |(slot, &value)| (slot_at(day, interval_min, slot), value))
}

/// The time of `slot` in a day whose slots are `interval_min` apart, counted from `day`.
fn slot_at(day: Timestamp, interval_min: u8, slot: usize) -> Timestamp {
    let minutes = i64::try_from(slot)
        .unwrap_or(i64::MAX)
        .saturating_mul(i64::from(interval_min));
    minutes_after(day, minutes)
}

/// The samples of a day's HR log: `slots` are the packets' samples end to end (nine from
/// the first packet, thirteen from each after it), `interval_min` the header's, and slot
/// `i` was taken `i × interval_min` minutes after `day_start`. Every sample is
/// [`Source::Periodic`]; a zero slot between readings keeps its place in the timeline as
/// [`Bpm::Invalid`](crate::Bpm::Invalid). The log is padded with zeros after its last reading (to the end of the
/// day and past it), and that padding is not a sample: it is cut.
pub(super) fn hr_samples(day_start: Timestamp, interval_min: u8, slots: &[u8]) -> Vec<HrSample> {
    let end = slots
        .iter()
        .rposition(|&raw| raw != 0)
        .map_or(0, |last| last + 1);
    slots[..end]
        .iter()
        .enumerate()
        .map(|(slot, &raw)| HrSample {
            at: slot_at(day_start, interval_min, slot),
            bpm: bpm_from_raw(raw),
            source: Source::Periodic,
        })
        .collect()
}

/// The samples of a day's stress series: `slots` are the packets' samples end to end
/// (twelve from the first packet, thirteen from each after it), slot `i` at
/// `i × interval_min` minutes after `day`'s midnight. Zero slots are skipped.
pub(super) fn stress_samples(day: Timestamp, interval_min: u8, slots: &[u8]) -> Vec<StressSample> {
    samples(day, interval_min, slots)
        .map(|(at, value)| StressSample { at, value })
        .collect()
}

/// The samples of a day's HRV series, laid out like [`stress_samples`]. The ring reports
/// HRV hourly with the half-hour slot between zero, so every other slot is skipped.
pub(super) fn hrv_samples(day: Timestamp, interval_min: u8, slots: &[u8]) -> Vec<HrvSample> {
    samples(day, interval_min, slots)
        .map(|(at, value)| HrvSample { at, value })
        .collect()
}

/// The bucket an activity row describes; `None` for the header and the empty marker,
/// which describe nothing.
///
/// The row's BCD date places it: `start` is that date's midnight in the zone of
/// `utc_offset_min` plus `bucket × 15` minutes, `span` is [`BUCKET_SPAN`], and `cal` is
/// the raw field times ten when the header said `new_protocol`, else as sent.
pub(super) fn step_bucket(
    packet: &ActivityPacket,
    new_protocol: bool,
    utc_offset_min: i16,
) -> Option<StepBucket> {
    match *packet {
        ActivityPacket::NoData { .. } | ActivityPacket::Header { .. } => None,
        ActivityPacket::Row {
            year,
            month,
            day,
            bucket,
            cal_raw,
            steps,
            distance_m,
            ..
        } => {
            let date = Timestamp {
                local_minute: days_from_civil(i64::from(year), month, day)
                    .saturating_mul(MINUTES_PER_DAY),
                utc_offset_min,
            };
            let cal = if new_protocol {
                u32::from(cal_raw) * NEW_PROTOCOL_CAL_UNIT
            } else {
                u32::from(cal_raw)
            };
            Some(StepBucket {
                start: minutes_after(date, i64::from(bucket) * BUCKET_MIN),
                span: BUCKET_SPAN,
                steps,
                cal,
                distance_m,
            })
        }
    }
}

/// The sessions of a sleep reply, one per day block.
///
/// A block's `days_ago` counts back from `today`'s midnight to a date; `end` is that
/// date plus the block's end minute, and so is `start` unless the start minute is
/// [`EVENING_MIN`] or later, in which case the session began the evening before and
/// `start` is the day before plus that minute. The stages are the block's, in order.
pub(super) fn sleep_sessions(today: Timestamp, sleep: SleepBody) -> Vec<SleepSession> {
    sleep
        .days
        .into_iter()
        .map(|day| {
            let date = day_minute(today, day.days_ago);
            let start_day = if day.start_min >= EVENING_MIN {
                minutes_after(date, -MINUTES_PER_DAY)
            } else {
                date
            };
            SleepSession {
                start: minutes_after(start_day, i64::from(day.start_min)),
                end: minutes_after(date, i64::from(day.end_min)),
                stages: day.stages,
            }
        })
        .collect()
}

/// The samples of an `SpO2` reply: per day block, one per hour that has data, at that
/// hour on the block's date. The value is the hour's maximum: the ring sends one
/// reading per hour, so its minimum and maximum are the same byte, and the maximum is
/// the one the app's `soa2` column carries. An hour whose maximum is not a percentage is
/// skipped.
pub(super) fn spo2_samples(today: Timestamp, spo2: &Spo2Body) -> Vec<Spo2Sample> {
    spo2.days
        .iter()
        .flat_map(|day| {
            let date = day_minute(today, day.days_ago);
            day.hours
                .iter()
                .enumerate()
                .filter_map(move |(hour, reading)| {
                    reading.max_percent().map(|percent| Spo2Sample {
                        at: slot_at(date, SPO2_SLOT_MIN, hour),
                        percent,
                    })
                })
        })
        .collect()
}

/// The samples of a temperature reply: per day block, one per slot that has a sample,
/// slot `i` at `i × interval_min` minutes after the block's date.
pub(super) fn temperature_samples(
    today: Timestamp,
    temperature: &TemperatureBody,
) -> Vec<TempSample> {
    temperature
        .days
        .iter()
        .flat_map(|day| {
            let date = day_minute(today, day.days_ago);
            (0..day.samples.len()).filter_map(move |slot| {
                day.deci_celsius(slot).map(|deci_celsius| TempSample {
                    at: slot_at(date, day.interval_min, slot),
                    deci_celsius,
                })
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    use crate::measure::{Bpm, SleepKind};
    use junk_core::Percent;

    use crate::wire::{BigData, Cmd, Frame, ReplyBody, RingFrame};

    /// 2026-07-02 00:00 in UTC-4: the `QRing` fixture day.
    const FIXTURE_DAY: Timestamp = Timestamp {
        local_minute: 20_636 * MINUTES_PER_DAY,
        utc_offset_min: -240,
    };

    /// The fixture's `bc 27` sleep reply, as logged (reconstructed from the app database).
    const SLEEP_REPLY: &str = "bc27310039ec01002ebb00c50202180320021804100225030b02240413021d0312020b041b02160309021e0412022b031b0219040f0231";
    /// The fixture's first `bc 2a` `SpO2` reply: hours 0–13.
    const SPO2_REPLY: &str = "bc2a31003e9800626260606161636360606060616161616060616160606060616161610000000000000000000000000000000000000000";
    /// The fixture's first `bc 25` temperature reply: slots 0–28.
    const TEMPERATURE_REPLY: &str = "bc2532004e81001ea7a8a8a8a8a8a1a8a7a8a8a7a6a7a7a7a7a7a7a6a6a7a8a7a5a0a6a6a700000000000000000000000000000000000000";

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap_or_else(|err| panic!("{err}")))
            .collect()
    }

    fn reply(text: &str) -> ReplyBody {
        let big = BigData::parse(&hex(text)).unwrap_or_else(|err| panic!("{err}"));
        ReplyBody::decode(&big).unwrap_or_else(|err| panic!("{err}"))
    }

    /// The `0x43` row with `payload`, decoded.
    fn row(payload: &[u8]) -> ActivityPacket {
        let frame = Frame::new(Cmd::Activity, payload).unwrap_or_else(|err| panic!("{err}"));
        match RingFrame::decode(&frame) {
            Ok(RingFrame::Activity(packet)) => packet,
            other => panic!("{other:?}"),
        }
    }

    fn at(minute_of_day: i64) -> Timestamp {
        minutes_after(FIXTURE_DAY, minute_of_day)
    }

    #[test]
    fn hr_samples_place_the_fixture_packets_five_minutes_apart() {
        // Packets 1–3 of the fixture's HR log: 9 + 13 + 13 samples.
        let mut slots = vec![0x3f, 0x50, 0x4a, 0x47, 0x3d, 0x3c, 0x3a, 0x3f, 0x75];
        slots.extend_from_slice(&[
            0x3d, 0x3b, 0x3c, 0x3b, 0x3b, 0x38, 0x50, 0x52, 0x4e, 0x4f, 0x49, 0x58, 0x3c,
        ]);
        slots.extend_from_slice(&[
            0x3c, 0x47, 0x39, 0x51, 0x3e, 0x3a, 0x72, 0x53, 0x39, 0x40, 0x52, 0x4b, 0x3d,
        ]);
        let samples = hr_samples(FIXTURE_DAY, 5, &slots);
        assert_eq!(samples.len(), 35);
        let bpm: Vec<Bpm> = samples.iter().map(|sample| sample.bpm).collect();
        assert_eq!(
            &bpm[..30],
            [
                63u8, 80, 74, 71, 61, 60, 58, 63, 117, 61, 59, 60, 59, 59, 56, 80, 82, 78, 79, 73,
                88, 60, 60, 71, 57, 81, 62, 58, 114, 83
            ]
            .map(Bpm::Valid)
        );
        for (slot, sample) in samples.iter().enumerate() {
            let slot = i64::try_from(slot).unwrap_or_else(|err| panic!("{err}"));
            assert_eq!(sample.at, at(slot * 5));
            assert_eq!(sample.source, Source::Periodic);
        }
        assert_eq!(samples[8].at, at(40));
        assert_eq!(samples[8].bpm, Bpm::Valid(117));
    }

    #[test]
    fn hr_zero_slots_stay_in_the_timeline_as_invalid() {
        let samples = hr_samples(FIXTURE_DAY, 5, &[0, 60, 0, 0, 61, 0, 0]);
        let sample = |minute, bpm| HrSample {
            at: at(minute),
            bpm,
            source: Source::Periodic,
        };
        assert_eq!(
            samples,
            [
                sample(0, Bpm::Invalid),
                sample(5, Bpm::Valid(60)),
                sample(10, Bpm::Invalid),
                sample(15, Bpm::Invalid),
                sample(20, Bpm::Valid(61)),
            ]
        );
        assert!(hr_samples(FIXTURE_DAY, 5, &[]).is_empty());
        // The ring's zero padding after the last reading is not a sample, and a log with no
        // reading at all is empty.
        assert_eq!(hr_samples(FIXTURE_DAY, 5, &[0, 60, 0, 0, 0, 0, 0]).len(), 2);
        assert!(hr_samples(FIXTURE_DAY, 5, &[0; 300]).is_empty());
        // A zero interval puts every sample at the day's start rather than failing.
        let same = hr_samples(FIXTURE_DAY, 0, &[1, 2, 3]);
        assert!(same.iter().all(|sample| sample.at == FIXTURE_DAY));
    }

    #[test]
    fn other_zero_slots_are_skipped_and_an_empty_log_is_empty() {
        assert!(stress_samples(FIXTURE_DAY, 30, &[0; 64]).is_empty());
        assert!(hrv_samples(FIXTURE_DAY, 30, &[]).is_empty());
    }

    #[test]
    fn stress_and_hrv_place_the_fixture_packets_half_an_hour_apart() {
        // Stress packet 1 and HRV packet 1 of the fixture.
        let stress = stress_samples(
            FIXTURE_DAY,
            30,
            &[
                0x2b, 0x27, 0x24, 0x22, 0x2f, 0x2e, 0x2a, 0x23, 0x22, 0x27, 0x24, 0x21,
            ],
        );
        let values: Vec<u8> = stress.iter().map(|sample| sample.value).collect();
        assert_eq!(values, [43, 39, 36, 34, 47, 46, 42, 35, 34, 39, 36, 33]);
        for (slot, sample) in stress.iter().enumerate() {
            let slot = i64::try_from(slot).unwrap_or_else(|err| panic!("{err}"));
            assert_eq!(sample.at, at(slot * 30));
        }

        let hrv = hrv_samples(
            FIXTURE_DAY,
            30,
            &[
                0x1e, 0x00, 0x2b, 0x00, 0x31, 0x00, 0x2d, 0x00, 0x2a, 0x00, 0x1f, 0x00,
            ],
        );
        assert_eq!(
            hrv,
            [
                HrvSample {
                    at: at(0),
                    value: 30
                },
                HrvSample {
                    at: at(60),
                    value: 43
                },
                HrvSample {
                    at: at(120),
                    value: 49
                },
                HrvSample {
                    at: at(180),
                    value: 45
                },
                HrvSample {
                    at: at(240),
                    value: 42
                },
                HrvSample {
                    at: at(300),
                    value: 31
                },
            ]
        );
        assert!(hrv.iter().all(|sample| sample.at.utc_offset_min == -240));
    }

    #[test]
    fn step_bucket_places_the_fixture_rows_on_the_hour() {
        let first = row(&[
            0x26, 0x07, 0x02, 0x04, 0x00, 0x05, 0x70, 0x00, 0x1c, 0x00, 0x13, 0x00,
        ]);
        assert_eq!(
            step_bucket(&first, true, -240),
            Some(StepBucket {
                start: at(60),
                span: Duration::from_secs(3600),
                steps: 28,
                cal: 1120,
                distance_m: 19,
            })
        );
        // Without the new protocol the calories are as sent.
        assert_eq!(step_bucket(&first, false, -240).map(|b| b.cal), Some(112));
        // The zone is the request's, the date the row's.
        assert_eq!(
            step_bucket(&first, true, 600).map(|b| b.start),
            Some(Timestamp {
                local_minute: FIXTURE_DAY.local_minute + 60,
                utc_offset_min: 600,
            })
        );
        let noon = row(&[
            0x26, 0x07, 0x02, 0x30, 0x02, 0x05, 0x50, 0x0c, 0xdc, 0x02, 0x18, 0x02,
        ]);
        assert_eq!(
            step_bucket(&noon, true, -240),
            Some(StepBucket {
                start: at(12 * 60),
                span: Duration::from_secs(3600),
                steps: 732,
                cal: 31_520,
                distance_m: 536,
            })
        );
        // Another date, and a bucket that is not a whole hour.
        let other = row(&[
            0x00, 0x01, 0x01, 0x05, 0x00, 0x01, 0xff, 0xff, 0x01, 0x00, 0x02, 0x00,
        ]);
        assert_eq!(
            step_bucket(&other, true, 0),
            Some(StepBucket {
                start: Timestamp {
                    local_minute: 10_957 * MINUTES_PER_DAY + 75,
                    utc_offset_min: 0,
                },
                span: Duration::from_secs(3600),
                steps: 1,
                cal: 655_350,
                distance_m: 2,
            })
        );

        assert_eq!(step_bucket(&row(&[0xff, 0x00, 0x01]), true, -240), None);
        assert_eq!(step_bucket(&row(&[0xf0, 0x05, 0x01]), true, -240), None);
    }

    #[test]
    fn sleep_sessions_are_the_fixture_night() {
        let ReplyBody::Sleep(sleep) = reply(SLEEP_REPLY) else {
            panic!("not sleep");
        };
        let sessions = sleep_sessions(FIXTURE_DAY, sleep);
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
        assert_eq!(
            (session.stages[0].kind, session.stages[0].minutes),
            (SleepKind::Light, 24)
        );
        assert_eq!(
            (session.stages[20].kind, session.stages[20].minutes),
            (SleepKind::Light, 49)
        );
        // Asked in the afternoon, the day is still the request's day.
        let ReplyBody::Sleep(sleep) = reply(SLEEP_REPLY) else {
            panic!("not sleep");
        };
        assert_eq!(sleep_sessions(at(14 * 60 + 16), sleep), sessions);
    }

    #[test]
    fn a_sleep_that_starts_in_the_evening_began_the_day_before() {
        let day = |days_ago, start_min, end_min| crate::wire::SleepDay {
            days_ago,
            start_min,
            end_min,
            stages: Vec::new(),
        };
        let sleep = SleepBody {
            days: vec![
                day(0, 1080, 400),
                day(0, 1079, 400),
                day(1, 1439, 0),
                day(2, 0, 0),
            ],
        };
        let sessions = sleep_sessions(FIXTURE_DAY, sleep);
        assert_eq!(sessions.len(), 4);
        assert_eq!(sessions[0].start, at(-MINUTES_PER_DAY + 1080));
        assert_eq!(sessions[0].end, at(400));
        assert_eq!(sessions[1].start, at(1079));
        assert_eq!(sessions[1].end, at(400));
        assert_eq!(sessions[2].start, at(-2 * MINUTES_PER_DAY + 1439));
        assert_eq!(sessions[2].end, at(-MINUTES_PER_DAY));
        assert_eq!(sessions[3].start, at(-2 * MINUTES_PER_DAY));
        assert_eq!(sessions[3].end, at(-2 * MINUTES_PER_DAY));
        assert!(sessions.iter().all(|s| s.stages.is_empty()));
        assert!(sleep_sessions(FIXTURE_DAY, SleepBody { days: Vec::new() }).is_empty());
    }

    #[test]
    fn spo2_samples_are_the_fixture_hours() {
        let ReplyBody::Spo2(spo2) = reply(SPO2_REPLY) else {
            panic!("not spo2");
        };
        let samples = spo2_samples(FIXTURE_DAY, &spo2);
        assert_eq!(samples.len(), 14);
        let morning: Vec<(Timestamp, u8)> = samples[..5]
            .iter()
            .map(|sample| (sample.at, sample.percent.get()))
            .collect();
        assert_eq!(
            morning,
            [
                (at(0), 98),
                (at(60), 96),
                (at(120), 97),
                (at(180), 99),
                (at(240), 96)
            ]
        );
        assert_eq!(samples[13].at, at(13 * 60));
        assert!(
            samples
                .iter()
                .all(|sample| sample.at.utc_offset_min == -240)
        );

        // Two days, the maximum of an hour, and bytes that are not percentages.
        let mut two = spo2.clone();
        let mut yesterday = spo2.days[0].clone();
        yesterday.days_ago = 1;
        yesterday.hours = [crate::wire::Spo2Hour::EMPTY; 24];
        yesterday.hours[23] = crate::wire::Spo2Hour { min: 90, max: 95 };
        yesterday.hours[0] = crate::wire::Spo2Hour { min: 101, max: 101 };
        two.days.push(yesterday);
        let samples = spo2_samples(FIXTURE_DAY, &two);
        assert_eq!(samples.len(), 15);
        assert_eq!(
            samples[14],
            Spo2Sample {
                at: at(-MINUTES_PER_DAY + 23 * 60),
                percent: Percent::new(95).unwrap_or(Percent::ZERO),
            }
        );
        assert!(spo2_samples(FIXTURE_DAY, &Spo2Body { days: Vec::new() }).is_empty());
    }

    #[test]
    fn temperature_samples_are_the_fixture_slots() {
        let ReplyBody::Temperature(temperature) = reply(TEMPERATURE_REPLY) else {
            panic!("not temperature");
        };
        let samples = temperature_samples(FIXTURE_DAY, &temperature);
        assert_eq!(samples.len(), 29);
        let first: Vec<i16> = samples[..8].iter().map(|s| s.deci_celsius).collect();
        assert_eq!(first, [367, 368, 368, 368, 368, 368, 361, 368]);
        for (slot, sample) in samples.iter().enumerate() {
            let slot = i64::try_from(slot).unwrap_or_else(|err| panic!("{err}"));
            assert_eq!(sample.at, at(slot * 30));
        }
        assert_eq!(samples[28].at, at(14 * 60));

        // A second day at another interval.
        let mut two = temperature.clone();
        let mut day = temperature.days[0].clone();
        day.days_ago = 3;
        day.interval_min = 15;
        day.samples = [0; 48];
        day.samples[47] = 0xa0;
        two.days.push(day);
        let samples = temperature_samples(FIXTURE_DAY, &two);
        assert_eq!(samples.len(), 30);
        assert_eq!(
            samples[29],
            TempSample {
                at: at(-3 * MINUTES_PER_DAY + 47 * 15),
                deci_celsius: 360,
            }
        );
        assert!(temperature_samples(FIXTURE_DAY, &TemperatureBody { days: Vec::new() }).is_empty());
    }

    #[test]
    fn absurd_timestamps_saturate_rather_than_panic() {
        for local_minute in [i64::MAX, i64::MIN] {
            let today = Timestamp {
                local_minute,
                utc_offset_min: 0,
            };
            let _ = hr_samples(today, u8::MAX, &[1; 300]);
            let _ = stress_samples(today, u8::MAX, &[1; 64]);
            let _ = hrv_samples(today, u8::MAX, &[1; 64]);
            let ReplyBody::Sleep(sleep) = reply(SLEEP_REPLY) else {
                panic!("not sleep");
            };
            let _ = sleep_sessions(today, sleep);
            let ReplyBody::Spo2(spo2) = reply(SPO2_REPLY) else {
                panic!("not spo2");
            };
            let _ = spo2_samples(today, &spo2);
            let ReplyBody::Temperature(temperature) = reply(TEMPERATURE_REPLY) else {
                panic!("not temperature");
            };
            let _ = temperature_samples(today, &temperature);
        }
        let _ = slot_at(FIXTURE_DAY, u8::MAX, usize::MAX);
        let far = row(&[
            0x99, 0x12, 0x31, 0xff, 0x00, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        ]);
        let _ = step_bucket(&far, true, i16::MIN);
    }
}
