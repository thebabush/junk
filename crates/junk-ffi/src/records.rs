//! What crosses the boundary: plain records, and how a [`Session`] becomes them.
//!
//! Every type here is a `uniffi::Record` or `uniffi::Enum` of numbers, strings and other
//! records — never a frame, a channel or a byte (SPEC §4). The stack's own types stay on
//! this side of the wall, so a change to the wire is not a change to the app.
//!
//! Times cross twice over, because an app wants both and neither is the other:
//!
//! - `at` is the ring's own wall clock as ISO 8601 with its offset, the same text the CSVs
//!   carry, for showing;
//! - `at_utc_ms` is the true instant, milliseconds since the epoch (the local minute less
//!   the offset), for plotting and for ordering against anything else the phone knows.

use junk_app::{Device, Samples, Session, stamp_of};
use junk_colmi::Source;
use junk_colmi::wire::WorkoutTag;
use junk_core::Timestamp;

/// Milliseconds in a minute.
const MS_PER_MINUTE: i64 = 60_000;

/// A ring a [`scan`](crate::scan) saw.
#[derive(uniffi::Record)]
pub struct FoundRing {
    /// What to pass back as `device` to talk to this one: the platform's own identifier.
    pub id: String,
    /// The advertised name, if it had one; `R10_F300` on the ring at hand.
    pub name: Option<String>,
    /// Signal strength in dBm, if the platform reported one.
    pub rssi: Option<i16>,
}

impl From<&junk_ble::Found> for FoundRing {
    fn from(found: &junk_ble::Found) -> FoundRing {
        FoundRing {
            id: found.id.to_string(),
            name: found.name.clone(),
            rssi: found.rssi,
        }
    }
}

/// How a heart-rate reading came to be.
#[derive(uniffi::Enum)]
pub enum HrSource {
    /// The ring's own scheduled measurement.
    Periodic,
    /// A one-off measurement the wearer asked for.
    Manual,
    /// A streamed measurement during a live session.
    Live,
}

impl From<Source> for HrSource {
    fn from(source: Source) -> HrSource {
        match source {
            Source::Periodic => HrSource::Periodic,
            Source::Manual => HrSource::Manual,
            Source::Live => HrSource::Live,
        }
    }
}

/// A heart rate, or the lack of one: the ring sends `0` bpm while its sensor has no reading.
#[derive(Copy, Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Bpm {
    /// Beats per minute, never `0`.
    Valid {
        /// The rate.
        bpm: u8,
    },
    /// The sensor had no reading: it had not locked on yet, or the slot is empty.
    Invalid,
}

impl From<junk_colmi::Bpm> for Bpm {
    fn from(bpm: junk_colmi::Bpm) -> Bpm {
        match bpm {
            junk_colmi::Bpm::Valid(bpm) => Bpm::Valid { bpm },
            junk_colmi::Bpm::Invalid => Bpm::Invalid,
        }
    }
}

/// One heart-rate reading.
#[derive(uniffi::Record)]
pub struct HrSample {
    /// When it was taken, ISO 8601 with the ring's offset.
    pub at: String,
    /// The same moment, milliseconds since the epoch.
    pub at_utc_ms: i64,
    /// Beats per minute, or [`Bpm::Invalid`] where the sensor had no reading.
    pub bpm: Bpm,
    /// How it was taken.
    pub source: HrSource,
}

impl From<&junk_colmi::HrSample> for HrSample {
    fn from(sample: &junk_colmi::HrSample) -> HrSample {
        HrSample {
            at: iso(sample.at),
            at_utc_ms: utc_ms(sample.at),
            bpm: sample.bpm.into(),
            source: sample.source.into(),
        }
    }
}

/// Activity totals over one interval, an hour on the ring at hand.
#[derive(uniffi::Record)]
pub struct StepBucket {
    /// When the interval began, ISO 8601 with the ring's offset.
    pub start: String,
    /// The same moment, milliseconds since the epoch.
    pub start_utc_ms: i64,
    /// How long the interval lasted, in seconds.
    pub span_s: u32,
    /// Steps taken in it.
    pub steps: u16,
    /// Energy burned, in calories (thousandths of a kilocalorie).
    pub cal: u32,
    /// Distance covered, in metres.
    pub distance_m: u16,
}

impl From<&junk_colmi::StepBucket> for StepBucket {
    fn from(bucket: &junk_colmi::StepBucket) -> StepBucket {
        StepBucket {
            start: iso(bucket.start),
            start_utc_ms: utc_ms(bucket.start),
            // A bucket is an hour; the clamp is for a span no ring would ever report.
            span_s: u32::try_from(bucket.span.as_secs()).unwrap_or(u32::MAX),
            steps: bucket.steps,
            cal: bucket.cal,
            distance_m: bucket.distance_m,
        }
    }
}

/// One blood-oxygen reading.
#[derive(uniffi::Record)]
pub struct Spo2Sample {
    /// When it was taken, ISO 8601 with the ring's offset.
    pub at: String,
    /// The same moment, milliseconds since the epoch.
    pub at_utc_ms: i64,
    /// Saturation, `0..=100`.
    pub percent: u8,
}

impl From<&junk_colmi::Spo2Sample> for Spo2Sample {
    fn from(sample: &junk_colmi::Spo2Sample) -> Spo2Sample {
        Spo2Sample {
            at: iso(sample.at),
            at_utc_ms: utc_ms(sample.at),
            percent: sample.percent.get(),
        }
    }
}

/// One heart-rate-variability reading.
#[derive(uniffi::Record)]
pub struct HrvSample {
    /// When it was taken, ISO 8601 with the ring's offset.
    pub at: String,
    /// The same moment, milliseconds since the epoch.
    pub at_utc_ms: i64,
    /// The ring's HRV figure, in whatever unit it uses.
    pub value: u8,
}

impl From<&junk_colmi::HrvSample> for HrvSample {
    fn from(sample: &junk_colmi::HrvSample) -> HrvSample {
        HrvSample {
            at: iso(sample.at),
            at_utc_ms: utc_ms(sample.at),
            value: sample.value,
        }
    }
}

/// One stress reading.
#[derive(uniffi::Record)]
pub struct StressSample {
    /// When it was taken, ISO 8601 with the ring's offset.
    pub at: String,
    /// The same moment, milliseconds since the epoch.
    pub at_utc_ms: i64,
    /// The ring's stress figure, in whatever scale it uses.
    pub value: u8,
}

impl From<&junk_colmi::StressSample> for StressSample {
    fn from(sample: &junk_colmi::StressSample) -> StressSample {
        StressSample {
            at: iso(sample.at),
            at_utc_ms: utc_ms(sample.at),
            value: sample.value,
        }
    }
}

/// One skin-temperature reading.
#[derive(uniffi::Record)]
pub struct TempSample {
    /// When it was taken, ISO 8601 with the ring's offset.
    pub at: String,
    /// The same moment, milliseconds since the epoch.
    pub at_utc_ms: i64,
    /// Tenths of a degree Celsius: `364` is 36.4 °C.
    pub deci_celsius: i16,
}

impl From<&junk_colmi::TempSample> for TempSample {
    fn from(sample: &junk_colmi::TempSample) -> TempSample {
        TempSample {
            at: iso(sample.at),
            at_utc_ms: utc_ms(sample.at),
            deci_celsius: sample.deci_celsius,
        }
    }
}

/// A sleep stage as the ring classifies it.
#[derive(uniffi::Enum)]
pub enum SleepKind {
    /// Light sleep.
    Light,
    /// Deep sleep.
    Deep,
    /// REM sleep.
    Rem,
    /// Awake during the session.
    Awake,
    /// A code the driver does not know, passed through rather than dropped.
    Unknown {
        /// The byte the ring sent.
        code: u8,
    },
}

impl From<junk_colmi::SleepKind> for SleepKind {
    fn from(kind: junk_colmi::SleepKind) -> SleepKind {
        match kind {
            junk_colmi::SleepKind::Light => SleepKind::Light,
            junk_colmi::SleepKind::Deep => SleepKind::Deep,
            junk_colmi::SleepKind::Rem => SleepKind::Rem,
            junk_colmi::SleepKind::Awake => SleepKind::Awake,
            junk_colmi::SleepKind::Unknown(code) => SleepKind::Unknown { code },
        }
    }
}

/// One stretch of a sleep session.
#[derive(uniffi::Record)]
pub struct SleepStage {
    /// What kind of sleep it was.
    pub kind: SleepKind,
    /// How long it lasted, in minutes.
    pub minutes: u8,
}

impl From<&junk_colmi::SleepStage> for SleepStage {
    fn from(stage: &junk_colmi::SleepStage) -> SleepStage {
        SleepStage {
            kind: stage.kind.into(),
            minutes: stage.minutes,
        }
    }
}

/// One night's sleep as a sequence of stages, in order from `start`.
#[derive(uniffi::Record)]
pub struct SleepSession {
    /// When it began, ISO 8601 with the ring's offset.
    pub start: String,
    /// When it ended, ISO 8601 with the ring's offset.
    pub end: String,
    /// The start as milliseconds since the epoch.
    pub start_utc_ms: i64,
    /// The end as milliseconds since the epoch.
    pub end_utc_ms: i64,
    /// How long it lasted: `end` less `start`, in minutes, awake stretches included.
    pub minutes: u32,
    /// The stages, in order from `start`.
    pub stages: Vec<SleepStage>,
}

impl From<&junk_colmi::SleepSession> for SleepSession {
    fn from(session: &junk_colmi::SleepSession) -> SleepSession {
        let span = session
            .end
            .local_minute
            .saturating_sub(session.start.local_minute);
        SleepSession {
            start: iso(session.start),
            end: iso(session.end),
            start_utc_ms: utc_ms(session.start),
            end_utc_ms: utc_ms(session.end),
            // A session that ends before it starts is not one; it reads as lasting nothing.
            minutes: u32::try_from(span).unwrap_or(0),
            stages: session.stages.iter().map(SleepStage::from).collect(),
        }
    }
}

/// One workout the ring stored, as it lists them.
///
/// The ring fills in what its sport knows about, so every figure but the start and the
/// sport is optional: a record's TLV either carries the tag or it does not.
#[derive(uniffi::Record)]
pub struct WorkoutRecord {
    /// When it started, on the ring's own clock (seconds, its epoch); `0` if the record
    /// carried no start at all.
    pub start_ring: u32,
    /// The sport, as the app numbers them; `7` is the one `junk live` starts.
    pub sport_type: u8,
    /// How long it lasted, in seconds.
    pub duration_s: Option<u64>,
    /// Distance covered, in the ring's own unit.
    pub distance: Option<u64>,
    /// Energy burned, in the ring's own unit.
    pub calories: Option<u64>,
    /// Average heart rate, bpm.
    pub rate_avg: Option<u8>,
    /// Lowest heart rate, bpm.
    pub rate_min: Option<u8>,
    /// Highest heart rate, bpm.
    pub rate_max: Option<u8>,
    /// Steps taken.
    pub steps: Option<u64>,
}

impl From<&junk_colmi::wire::WorkoutRecord> for WorkoutRecord {
    fn from(record: &junk_colmi::wire::WorkoutRecord) -> WorkoutRecord {
        // A TLV is bytes wide enough for anything; a rate is one byte and a value that does
        // not fit one is not a rate, so it reads as missing rather than as a wrong number.
        let rate = |tag| record.get(tag).and_then(|value| u8::try_from(value).ok());
        WorkoutRecord {
            start_ring: record
                .get(WorkoutTag::StartTime)
                .and_then(|start| u32::try_from(start).ok())
                .unwrap_or(0),
            sport_type: record.sport_type,
            duration_s: record.get(WorkoutTag::Duration),
            distance: record.get(WorkoutTag::Distance),
            calories: record.get(WorkoutTag::Calories),
            rate_avg: rate(WorkoutTag::RateAvg),
            rate_min: rate(WorkoutTag::RateMin),
            rate_max: rate(WorkoutTag::RateMax),
            steps: record.get(WorkoutTag::Steps),
        }
    }
}

/// The workout a [`live`](crate::live) session ran, fetched back after stopping it.
#[derive(uniffi::Record)]
pub struct Workout {
    /// The record the ring stored: its start, duration and totals.
    pub record: WorkoutRecord,
    /// Seconds between the heart rates below.
    pub sample_seconds: u8,
    /// The heart rates, one per sampling interval from the record's start.
    pub heart_rates: Vec<Bpm>,
}

impl From<&junk_app::Workout> for Workout {
    fn from(workout: &junk_app::Workout) -> Workout {
        Workout {
            record: WorkoutRecord::from(&workout.record),
            sample_seconds: workout.descriptor.sample_second,
            heart_rates: workout.heart_rates.iter().copied().map(Bpm::from).collect(),
        }
    }
}

/// What the session learned about the ring itself.
#[derive(uniffi::Record)]
pub struct DeviceInfo {
    /// The Firmware Revision string, `RT03CR_1.00.02_260319` on the R10.
    pub firmware: Option<String>,
    /// The Hardware Revision string, `RT03CR_V1.0` on the R10.
    pub hardware: Option<String>,
    /// The charge, `0..=100`, as the ring last reported it.
    pub battery_percent: Option<u8>,
    /// Whether it was charging then.
    pub charging: Option<bool>,
    /// The ATT MTU the link came up with, in bytes.
    pub mtu: Option<u16>,
    /// Whether the ring measures skin temperature. `false` if it never sent its bitmap.
    pub temperature: bool,
    /// Whether the ring measures blood oxygen. `false` if it never sent its bitmap.
    pub spo2: bool,
    /// Whether sleep comes through big data rather than an old-style frame. `false` if the
    /// ring never sent its bitmap.
    pub new_sleep_protocol: bool,
}

impl From<&Device> for DeviceInfo {
    fn from(device: &Device) -> DeviceInfo {
        DeviceInfo {
            firmware: device.firmware.clone(),
            hardware: device.hardware.clone(),
            battery_percent: device.battery.map(|battery| battery.percent.get()),
            charging: device.battery.map(|battery| battery.charging),
            mtu: device.mtu,
            // A ring that never sent the bitmap has said nothing, which reads as no.
            temperature: device.capabilities.is_some_and(|caps| caps.temperature),
            spo2: device.capabilities.is_some_and(|caps| caps.spo2()),
            new_sleep_protocol: device
                .capabilities
                .is_some_and(|caps| caps.new_sleep_protocol),
        }
    }
}

/// One request the ring refused. The session went on past it.
#[derive(uniffi::Record)]
pub struct Failure {
    /// What was asked, in a few words: `spo2`, `hr log 2026-09-06`.
    pub what: String,
    /// Why not, as the driver put it.
    pub why: String,
}

/// Everything a [`sync`](crate::sync) collected.
///
/// A ring that refuses a request is not an error: a dialect without a feature refuses the
/// request for it and the session carries on, so a session that ran to the end can still
/// have [`failures`](SyncResult::failures).
#[derive(uniffi::Record)]
pub struct SyncResult {
    /// What the ring is.
    pub device: DeviceInfo,
    /// The periodic heart-rate log.
    pub hr: Vec<HrSample>,
    /// Activity buckets.
    pub steps: Vec<StepBucket>,
    /// Blood oxygen.
    pub spo2: Vec<Spo2Sample>,
    /// Heart-rate variability.
    pub hrv: Vec<HrvSample>,
    /// Stress.
    pub stress: Vec<StressSample>,
    /// Skin temperature.
    pub temperature: Vec<TempSample>,
    /// Sleep sessions, each with its stages.
    pub sleep: Vec<SleepSession>,
    /// The workout records the ring lists.
    pub workouts: Vec<WorkoutRecord>,
    /// Every request the ring refused.
    pub failures: Vec<Failure>,
}

impl From<&Session> for SyncResult {
    fn from(session: &Session) -> SyncResult {
        let Samples {
            hr,
            steps,
            spo2,
            hrv,
            stress,
            temperature,
            sleep,
            workouts,
        } = &session.samples;
        SyncResult {
            device: DeviceInfo::from(&session.device),
            hr: hr.iter().map(HrSample::from).collect(),
            steps: steps.iter().map(StepBucket::from).collect(),
            spo2: spo2.iter().map(Spo2Sample::from).collect(),
            hrv: hrv.iter().map(HrvSample::from).collect(),
            stress: stress.iter().map(StressSample::from).collect(),
            temperature: temperature.iter().map(TempSample::from).collect(),
            sleep: sleep.iter().map(SleepSession::from).collect(),
            workouts: workouts.iter().map(WorkoutRecord::from).collect(),
            failures: session
                .failures
                .iter()
                .map(|(what, err)| Failure {
                    what: what.clone(),
                    why: err.to_string(),
                })
                .collect(),
        }
    }
}

/// Everything a [`live`](crate::live) collected: the session, and the workout it ran.
#[derive(uniffi::Record)]
pub struct LiveResult {
    /// What the session collected, the same shape a sync returns.
    pub session: SyncResult,
    /// The workout, once it has been fetched. `None` when the ring stored none: one under
    /// about a minute is not kept.
    pub workout: Option<Workout>,
}

impl From<&Session> for LiveResult {
    fn from(session: &Session) -> LiveResult {
        LiveResult {
            session: SyncResult::from(session),
            workout: session.workout.as_ref().map(Workout::from),
        }
    }
}

/// `at` as the text the CSVs carry: ISO 8601 with the ring's offset.
///
/// A time the format cannot hold (a year past 9999) is `?`, as [`junk_app::date_of`]
/// writes it: a reading is still worth showing when its clock is nonsense.
fn iso(at: Timestamp) -> String {
    stamp_of(at).map_or_else(|_| "?".to_owned(), |stamp| stamp.to_string())
}

/// `at` as true milliseconds since the epoch: the local minute less its offset.
fn utc_ms(at: Timestamp) -> i64 {
    at.to_utc_minute().saturating_mul(MS_PER_MINUTE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use junk_colmi::wire::{WorkoutField, WorkoutTag};

    /// 2026-09-06 00:00 in UTC-4, the first heart rate of the sync fixture.
    const AT: Timestamp = Timestamp {
        local_minute: 29_810_880,
        utc_offset_min: -240,
    };

    #[test]
    fn a_time_crosses_as_its_own_wall_clock_and_as_the_instant_it_was() {
        assert_eq!(iso(AT), "2026-09-06T00:00:00.000-04:00");
        assert_eq!(utc_ms(AT), 1_788_667_200_000);
        // Midnight UTC-4 is four hours later than midnight UTC.
        assert_eq!(
            utc_ms(Timestamp {
                utc_offset_min: 0,
                ..AT
            }) + 4 * 3_600_000,
            utc_ms(AT)
        );
        // A year the stamp format cannot write still leaves a showable sample.
        assert_eq!(
            iso(Timestamp {
                local_minute: i64::MAX,
                utc_offset_min: 0,
            }),
            "?"
        );
    }

    #[test]
    fn a_record_narrows_its_rates_and_passes_the_rest_through() {
        let field = |tag, value: Vec<u8>| WorkoutField { tag, value };
        let record = junk_colmi::wire::WorkoutRecord {
            sport_type: 7,
            fields: vec![
                field(
                    WorkoutTag::StartTime,
                    1_788_632_166u32.to_le_bytes().to_vec(),
                ),
                field(WorkoutTag::Duration, 75u16.to_le_bytes().to_vec()),
                field(WorkoutTag::RateAvg, vec![58]),
                // Four bytes for a rate: not one, so it is not read as one.
                field(WorkoutTag::RateMax, 300u32.to_le_bytes().to_vec()),
            ],
        };
        let record = WorkoutRecord::from(&record);
        assert_eq!(record.start_ring, 1_788_632_166);
        assert_eq!(record.sport_type, 7);
        assert_eq!(record.duration_s, Some(75));
        assert_eq!(record.rate_avg, Some(58));
        assert_eq!(record.rate_max, None);
        assert_eq!(record.rate_min, None);
        assert_eq!(record.steps, None);

        // A record with no start at all still crosses; it simply has none to show.
        let empty = junk_colmi::wire::WorkoutRecord {
            sport_type: 1,
            fields: Vec::new(),
        };
        assert_eq!(WorkoutRecord::from(&empty).start_ring, 0);
    }

    #[test]
    fn a_ring_that_never_sent_its_bitmap_can_do_nothing_it_did_not_say() {
        let info = DeviceInfo::from(&Device::default());
        assert!(!info.temperature && !info.spo2 && !info.new_sleep_protocol);
        assert_eq!(info.battery_percent, None);
        assert_eq!(info.charging, None);
        assert_eq!(info.mtu, None);
    }

    #[test]
    fn a_sleep_session_lasts_from_its_start_to_its_end() {
        let session = junk_colmi::SleepSession {
            start: AT,
            end: Timestamp {
                local_minute: AT.local_minute + 397,
                ..AT
            },
            stages: vec![junk_colmi::SleepStage {
                kind: junk_colmi::SleepKind::Unknown(9),
                minutes: 12,
            }],
        };
        let session = SleepSession::from(&session);
        assert_eq!(session.minutes, 397);
        assert_eq!(session.start, "2026-09-06T00:00:00.000-04:00");
        assert_eq!(session.end, "2026-09-06T06:37:00.000-04:00");
        assert!(matches!(
            session.stages[0].kind,
            SleepKind::Unknown { code: 9 }
        ));
    }
}
