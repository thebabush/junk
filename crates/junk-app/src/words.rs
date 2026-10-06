//! The words a shell shows: what an answer says, and what the ring said on its own.
//!
//! Two shells need the same lines — `junk-cli` prints them, `junk-ffi` hands them to a
//! phone app as strings — so they live here beside [`describe_req`](crate::describe_req),
//! next to the session that produces them. Nothing here decides where a line *goes*
//! (stdout, stderr, a callback): that is the shell's own business, and so is anything one
//! shell alone shows.

use std::fmt::Write as _;

use junk_colmi::Bpm;
use junk_colmi::channel_name;
use junk_colmi::proto::{Ev, Resp};
use junk_colmi::wire::{Capabilities, Notification, WorkoutRecord, WorkoutTag};
use junk_core::{Battery, Channel, ChannelSet};

/// A heart rate in words: `58 bpm`, or `no reading` while the sensor has none.
fn bpm_words(bpm: Bpm) -> String {
    match bpm {
        Bpm::Valid(value) => format!("{value} bpm"),
        Bpm::Invalid => "no reading".to_owned(),
    }
}

/// One line saying what `resp` is: its kind and, for a collection, its count.
#[must_use]
pub fn summary(resp: &Resp) -> String {
    match resp {
        Resp::Ack => "ack".to_owned(),
        Resp::Capabilities(caps) => capabilities(caps),
        Resp::Battery(battery) => battery_summary(*battery),
        Resp::Prefs(prefs) => format!(
            "{} clock, {} units, sex {}, age {}, {} cm, {} kg, {}/{} mmHg, hr warn {}",
            if prefs.hour12 { "12h" } else { "24h" },
            if prefs.imperial { "imperial" } else { "metric" },
            prefs.sex,
            prefs.age,
            prefs.height_cm,
            prefs.weight_kg,
            prefs.sbp,
            prefs.dbp,
            prefs.hr_warn
        ),
        Resp::Version { firmware, hardware } => format!("{firmware} / {hardware}"),
        Resp::Goals {
            steps,
            calories,
            distance,
            sport_min,
            sleep_min,
        } => format!(
            "{steps} steps, {calories} cal, {distance} m, {sport_min} min sport, {sleep_min} min sleep"
        ),
        Resp::AutoPref {
            enabled: true,
            interval_min: Some(minutes),
        } => format!("on, every {minutes} min"),
        Resp::AutoPref {
            enabled: true,
            interval_min: None,
        } => "on".to_owned(),
        Resp::AutoPref { enabled: false, .. } => "off".to_owned(),
        Resp::TodayTotals {
            steps,
            running_steps,
            cal,
            distance_m,
            active_min,
        } => format!(
            "{steps} steps ({running_steps} running), {cal} cal, {distance_m} m, {active_min} min active"
        ),
        Resp::WorkoutCtl { start: Some(start) } => format!("acked, ring clock {start}"),
        Resp::WorkoutCtl { start: None } => "acked".to_owned(),
        Resp::Raw(frame) => format!("reply {}", hex(&frame.to_bytes())),
        Resp::RawBigData(big) => {
            format!("kind {:#04x}, {} bytes", big.kind.byte(), big.body().len())
        }
        Resp::HrLog(log) => format!("{} samples", log.len()),
        Resp::Stress(series) => format!("{} samples", series.len()),
        Resp::Hrv(series) => format!("{} samples", series.len()),
        Resp::Activity(buckets) => format!("{} buckets", buckets.len()),
        Resp::Sleep(sessions) => format!(
            "{} sessions, {} stages",
            sessions.len(),
            sessions.iter().map(|s| s.stages.len()).sum::<usize>()
        ),
        Resp::Spo2(series) => format!("{} samples", series.len()),
        Resp::Temperature(series) => format!("{} samples", series.len()),
        Resp::Workouts(summary) => format!("{} records", summary.records.len()),
        Resp::WorkoutDetail {
            descriptor,
            heart_rates,
        } => format!(
            "{} heart rates, {} packages, every {} s",
            heart_rates.len(),
            descriptor.package_count,
            descriptor.sample_second
        ),
    }
}

/// The capability bitmap in words.
#[must_use]
pub fn capabilities(caps: &Capabilities) -> String {
    format!(
        "temperature {}, spo2 {}, new sleep protocol {}",
        yes_no(caps.temperature),
        yes_no(caps.spo2()),
        yes_no(caps.new_sleep_protocol)
    )
}

/// The battery in words.
#[must_use]
pub fn battery_summary(battery: Battery) -> String {
    format!(
        "{}, {}",
        battery.percent,
        if battery.charging {
            "charging"
        } else {
            "not charging"
        }
    )
}

/// `yes` or `no`.
fn yes_no(flag: bool) -> &'static str {
    if flag { "yes" } else { "no" }
}

/// `bytes` as lowercase hex, no separators, as the traces write them.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// One summary of a workout record: the fields the ring filled in.
#[must_use]
pub fn workout_summary(record: &WorkoutRecord) -> String {
    let mut text = format!("sport {}", record.sport_type);
    for (tag, name) in [
        (WorkoutTag::StartTime, "start"),
        (WorkoutTag::Duration, "duration s"),
        (WorkoutTag::RateAvg, "rate avg"),
        (WorkoutTag::RateMin, "rate min"),
        (WorkoutTag::RateMax, "rate max"),
        (WorkoutTag::Steps, "steps"),
        (WorkoutTag::Distance, "distance"),
        (WorkoutTag::Calories, "calories"),
    ] {
        if let Some(value) = record.get(tag) {
            let _ = write!(text, ", {name} {value}");
        }
    }
    text
}

/// `ev` in one line.
#[must_use]
pub fn describe_event(ev: &Ev) -> String {
    match ev {
        Ev::Capabilities(caps) => format!("capabilities: {}", capabilities(caps)),
        Ev::PacketSize(size) => format!("packet size: {size}"),
        Ev::Notification(notification) => {
            format!("notification: {}", describe_notification(notification))
        }
        Ev::Battery(battery) => format!("battery: {}", battery_summary(*battery)),
        Ev::Workout {
            sport_type,
            flag,
            seq,
            bpm,
        } => format!(
            "workout: sport {sport_type}, flag {flag}, seq {seq}, {}",
            bpm_words(*bpm)
        ),
        Ev::Text(text) => format!("text: {text}"),
        Ev::UnexpectedReply(frame) => format!("unexpected reply: {frame:?}"),
        Ev::UnexpectedBigData(big) => format!(
            "unexpected big data: kind {:#04x}, {} bytes",
            big.kind.byte(),
            big.body().len()
        ),
        Ev::Unparsed { chan, bytes } => {
            format!("unparsed on {}: {}", channel_label(*chan), hex(bytes))
        }
        Ev::MissingChannels(missing) => {
            format!("missing channels: {}", channel_names(missing))
        }
        Ev::UnknownFirmware(firmware) => format!("unknown firmware: {firmware}"),
    }
}

/// A `0x73` notification in words.
fn describe_notification(notification: &Notification) -> String {
    match notification {
        Notification::NewHr => "new hr".to_owned(),
        Notification::NewSpo2 => "new spo2".to_owned(),
        Notification::NewSteps => "new steps".to_owned(),
        Notification::WorkoutStored => "workout stored".to_owned(),
        Notification::Battery(percent) => format!("battery {percent}"),
        Notification::LiveActivity {
            steps,
            cal,
            distance_m,
        } => format!("live activity {steps} steps, {cal} cal, {distance_m} m"),
        Notification::Other { sub, body } => {
            format!("other {sub:#04x} {}", hex(body))
        }
    }
}

/// The name a trace gives `chan`, or `unknown.<id>`.
#[must_use]
pub fn channel_label(chan: Channel) -> String {
    channel_name(chan).map_or_else(|| format!("unknown.{}", chan.0), str::to_owned)
}

/// The channels' names, comma-separated, in id order.
#[must_use]
pub fn channel_names(set: &ChannelSet) -> String {
    let mut text = String::new();
    for chan in set {
        if !text.is_empty() {
            text.push(',');
        }
        let _ = write!(text, "{}", channel_label(chan));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use junk_colmi::wire::WorkoutField;
    use junk_colmi::{HrSample, Source};
    use junk_core::{Percent, Timestamp};

    const DAY: Timestamp = Timestamp {
        local_minute: 1_782_950_400 / 60,
        utc_offset_min: -240,
    };

    #[test]
    fn a_workout_summary_names_the_fields_the_ring_filled_in() {
        let record = WorkoutRecord {
            sport_type: 7,
            fields: vec![
                WorkoutField {
                    tag: WorkoutTag::StartTime,
                    value: 0x691d_2980u32.to_le_bytes().to_vec(),
                },
                WorkoutField {
                    tag: WorkoutTag::Duration,
                    value: 61u16.to_le_bytes().to_vec(),
                },
                WorkoutField {
                    tag: WorkoutTag::RateAvg,
                    value: vec![60],
                },
            ],
        };
        assert_eq!(
            workout_summary(&record),
            "sport 7, start 1763518848, duration s 61, rate avg 60"
        );
    }

    #[test]
    fn summaries_count() {
        assert_eq!(summary(&Resp::HrLog(vec![])), "0 samples");
        assert_eq!(
            summary(&Resp::HrLog(vec![HrSample {
                at: DAY,
                bpm: Bpm::Valid(63),
                source: Source::Periodic,
            }])),
            "1 samples"
        );
        assert_eq!(
            summary(&Resp::AutoPref {
                enabled: true,
                interval_min: Some(5)
            }),
            "on, every 5 min"
        );
        assert_eq!(
            summary(&Resp::Battery(Battery {
                percent: Percent::FULL,
                charging: true
            })),
            "100%, charging"
        );
        assert_eq!(hex(&[0xbc, 0x30, 0x00]), "bc3000");
        // The date a request names is `crate::date_of`, tested there.
        assert_eq!(crate::date_of(DAY), "2026-07-02");
    }

    #[test]
    fn an_event_is_one_line_and_a_channel_has_a_name_either_way() {
        assert_eq!(
            describe_event(&Ev::Workout {
                sport_type: 7,
                flag: 1,
                seq: 3,
                bpm: Bpm::Valid(58),
            }),
            "workout: sport 7, flag 1, seq 3, 58 bpm"
        );
        assert_eq!(
            describe_event(&Ev::Workout {
                sport_type: 7,
                flag: 1,
                seq: 0,
                bpm: Bpm::Invalid,
            }),
            "workout: sport 7, flag 1, seq 0, no reading"
        );
        assert_eq!(
            describe_event(&Ev::Notification(Notification::WorkoutStored)),
            "notification: workout stored"
        );

        let known = junk_colmi::channel_by_name("v2.cmd").expect("a channel of this family");
        assert_eq!(channel_label(known), "v2.cmd");
        assert_eq!(channel_label(Channel(200)), "unknown.200");

        let mut set = ChannelSet::EMPTY;
        assert_eq!(channel_names(&set), "");
        set.insert(known);
        set.insert(Channel(200));
        assert_eq!(channel_names(&set), "v2.cmd,unknown.200");
    }
}
