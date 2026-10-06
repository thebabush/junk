//! The measurement model the Colmi ring emits: heart rate, `SpO2`, HRV, stress, steps,
//! temperature and sleep, in integer units, timestamped in local time.
//!
//! These live here, not in `junk-core`, because the Colmi ring is the only device that has
//! produced them; they move to core once a second device emits them.

use alloc::vec::Vec;

use junk_core::{Duration, Percent, Timestamp};

/// Reads a heart-rate byte as the ring sends it: `0` is [`Bpm::Invalid`], the rest [`Bpm::Valid`].
pub(crate) const fn bpm_from_raw(raw: u8) -> Bpm {
    if raw == 0 {
        Bpm::Invalid
    } else {
        Bpm::Valid(raw)
    }
}

/// How a heart-rate sample came to be.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum Source {
    /// The device's own scheduled measurement.
    Periodic,
    /// A one-off measurement the user asked for.
    Manual,
    /// A streamed measurement during a live session.
    Live,
}

/// A heart-rate reading, or the lack of one: [`Bpm::Valid`] is never `0`. The Colmi ring
/// signals "no reading" with `0` on the wire; that is a convention it was observed to
/// follow, not a spec.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum Bpm {
    /// Beats per minute, `1..=255`.
    Valid(u8),
    /// The sensor had no reading: it had not locked on yet, or the slot is empty.
    Invalid,
}

/// One heart-rate reading.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct HrSample {
    /// When it was taken.
    pub at: Timestamp,
    /// Beats per minute, or [`Bpm::Invalid`] where the sensor had no reading.
    pub bpm: Bpm,
    /// How it was taken.
    pub source: Source,
}

/// One blood-oxygen reading.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct Spo2Sample {
    /// When it was taken.
    pub at: Timestamp,
    /// Saturation.
    pub percent: Percent,
}

/// One heart-rate-variability reading.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct HrvSample {
    /// When it was taken.
    pub at: Timestamp,
    /// The device's HRV figure, in whatever unit it uses.
    pub value: u8,
}

/// One stress reading.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct StressSample {
    /// When it was taken.
    pub at: Timestamp,
    /// The device's stress figure, in whatever scale it uses.
    pub value: u8,
}

/// One temperature reading.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct TempSample {
    /// When it was taken.
    pub at: Timestamp,
    /// Tenths of a degree Celsius.
    pub deci_celsius: i16,
}

/// Activity totals over one interval.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct StepBucket {
    /// When the interval began.
    pub start: Timestamp,
    /// How long it lasted.
    pub span: Duration,
    /// Steps taken.
    pub steps: u16,
    /// Energy burned, in calories (thousandths of a kilocalorie). Hourly buckets are around
    /// a thousand; a `u16` of kilocalories would round them to one.
    pub cal: u32,
    /// Distance covered, in metres.
    pub distance_m: u16,
}

/// A sleep stage as the device classifies it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum SleepKind {
    /// Light sleep.
    Light,
    /// Deep sleep.
    Deep,
    /// REM sleep.
    Rem,
    /// Awake during the session.
    Awake,
    /// A code the driver does not know, passed through.
    Unknown(u8),
}

/// One stretch of a sleep session.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct SleepStage {
    /// What kind of sleep.
    pub kind: SleepKind,
    /// How long it lasted.
    pub minutes: u8,
}

/// One night's sleep as a sequence of stages.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SleepSession {
    /// When the session began.
    pub start: Timestamp,
    /// When it ended.
    pub end: Timestamp,
    /// The stages, in order from `start`.
    pub stages: Vec<SleepStage>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bpm_from_raw_reads_zero_as_no_reading() {
        assert_eq!(bpm_from_raw(0), Bpm::Invalid);
        assert_eq!(bpm_from_raw(1), Bpm::Valid(1));
        assert_eq!(bpm_from_raw(255), Bpm::Valid(255));
    }
}
