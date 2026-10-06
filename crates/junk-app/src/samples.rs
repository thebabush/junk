//! Everything a session collected, by kind, as the stack's own types.

use junk_colmi::proto::Resp;
use junk_colmi::wire::WorkoutRecord;
use junk_colmi::{
    HrSample, HrvSample, SleepSession, Spo2Sample, StepBucket, StressSample, TempSample,
};

/// The samples a session collected, in the order the ring gave them.
///
/// This is what a shell turns into rows, a chart or a database; nothing here knows about
/// any of those.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Samples {
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
    /// Stored workout records, as the ring lists them.
    pub workouts: Vec<WorkoutRecord>,
}

impl Samples {
    /// Adds whatever samples `resp` carries.
    ///
    /// The answers that carry none (acks, preferences, the version, the battery, a
    /// workout's detail) add nothing.
    pub fn absorb(&mut self, resp: &Resp) {
        match resp {
            Resp::HrLog(log) => self.hr.extend_from_slice(log),
            Resp::Activity(buckets) => self.steps.extend_from_slice(buckets),
            Resp::Spo2(series) => self.spo2.extend_from_slice(series),
            Resp::Hrv(series) => self.hrv.extend_from_slice(series),
            Resp::Stress(series) => self.stress.extend_from_slice(series),
            Resp::Temperature(series) => self.temperature.extend_from_slice(series),
            Resp::Sleep(sessions) => self.sleep.extend_from_slice(sessions),
            Resp::Workouts(summary) => self.workouts.extend_from_slice(&summary.records),
            Resp::Ack
            | Resp::Capabilities(_)
            | Resp::Battery(_)
            | Resp::Prefs(_)
            | Resp::Version { .. }
            | Resp::Goals { .. }
            | Resp::AutoPref { .. }
            | Resp::TodayTotals { .. }
            | Resp::WorkoutCtl { .. }
            | Resp::Raw(_)
            | Resp::RawBigData(_)
            | Resp::WorkoutDetail { .. } => {}
        }
    }

    /// Whether no kind has a sample.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hr.is_empty()
            && self.steps.is_empty()
            && self.spo2.is_empty()
            && self.hrv.is_empty()
            && self.stress.is_empty()
            && self.temperature.is_empty()
            && self.sleep.is_empty()
            && self.workouts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use junk_colmi::{Bpm, Source};
    use junk_core::{Battery, Percent, Timestamp};

    const AT: Timestamp = Timestamp {
        local_minute: 1_782_950_400 / 60,
        utc_offset_min: -240,
    };

    #[test]
    fn every_kind_lands_in_its_own_vector_and_the_rest_adds_nothing() {
        let mut samples = Samples::default();
        assert!(samples.is_empty());

        samples.absorb(&Resp::HrLog(vec![HrSample {
            at: AT,
            bpm: Bpm::Valid(63),
            source: Source::Periodic,
        }]));
        samples.absorb(&Resp::Stress(vec![StressSample { at: AT, value: 43 }]));
        samples.absorb(&Resp::Hrv(vec![HrvSample { at: AT, value: 30 }]));
        assert!(!samples.is_empty());
        assert_eq!(samples.hr.len(), 1);
        assert_eq!(samples.stress.len(), 1);
        assert_eq!(samples.hrv.len(), 1);
        assert_eq!(samples.spo2.len(), 0);

        // Absorbing again appends: the ring is free to send a day twice.
        samples.absorb(&Resp::Stress(vec![StressSample { at: AT, value: 39 }]));
        assert_eq!(
            samples.stress,
            [
                StressSample { at: AT, value: 43 },
                StressSample { at: AT, value: 39 }
            ]
        );

        let before = samples.clone();
        for resp in [
            Resp::Ack,
            Resp::Battery(Battery {
                percent: Percent::FULL,
                charging: false,
            }),
            Resp::Version {
                firmware: "RT03CR_1.00.02_260319".to_owned(),
                hardware: "RT03CR_V1.0".to_owned(),
            },
            Resp::WorkoutCtl { start: Some(1) },
        ] {
            samples.absorb(&resp);
        }
        assert_eq!(samples, before);
    }
}
