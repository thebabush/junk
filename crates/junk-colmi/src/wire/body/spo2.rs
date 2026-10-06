//! The `0x2a` `SpO2` reply: 49-byte day blocks of hourly (min, max) pairs.

use alloc::vec::Vec;

use junk_core::Percent;

use crate::wire::BigDataKind;
use crate::wire::body::{BodyError, arrays};

/// Bytes in a day block: the day byte and 24 pairs.
const BLOCK_LEN: usize = 49;
/// Hours in a day, one pair each.
const HOURS: usize = 24;

/// The body of a `0x2a` reply: zero or more [`Spo2Day`] blocks, nothing else.
///
/// The fixture replies are one day each; the app asked with `ff` then `02` and got the
/// same one block, today, either way.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct Spo2Body {
    /// One block per day, in the order the ring sent them.
    pub days: Vec<Spo2Day>,
}

impl Spo2Body {
    /// The body's days, or why they do not decode.
    pub(super) fn from_body(body: &[u8]) -> Result<Spo2Body, BodyError> {
        if !body.len().is_multiple_of(BLOCK_LEN) {
            return Err(BodyError::Malformed {
                kind: BigDataKind::Spo2,
                what: "day blocks",
            });
        }
        Ok(Spo2Body {
            days: arrays(body).map(Spo2Day::from_block).collect(),
        })
    }

    /// The body: each day's block, end to end.
    pub(super) fn to_body(&self) -> Vec<u8> {
        let mut body = Vec::with_capacity(BLOCK_LEN * self.days.len());
        for day in &self.days {
            day.extend_body(&mut body);
        }
        body
    }
}

/// One day of hourly `SpO2`: `<days_ago:u8> <24 × (min:u8, max:u8)>`, one pair per hour
/// from midnight, both zero for an hour with no data.
///
/// The fixture block starts `00 62 62 60 60 61 61 63 63 60 60`: today, hours 0–4 = 98, 96,
/// 97, 99, 96, which are the app's `BloodOxygen` rows at 00:00–04:00. The ring reports one
/// value per hour, so min = max in every pair seen; which of the two is first is from the
/// decompilation and cannot be told from the fixtures.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct Spo2Day {
    /// Which day: `0` today, `1` yesterday, …
    pub days_ago: u8,
    /// Hours 0–23.
    pub hours: [Spo2Hour; HOURS],
}

impl Spo2Day {
    /// The day a block carries; total.
    fn from_block([days_ago, pairs @ ..]: [u8; BLOCK_LEN]) -> Spo2Day {
        let mut hours = [Spo2Hour::EMPTY; HOURS];
        for (hour, [min, max]) in hours.iter_mut().zip(arrays(&pairs)) {
            *hour = Spo2Hour { min, max };
        }
        Spo2Day { days_ago, hours }
    }

    /// Appends the day's block to `body`.
    fn extend_body(&self, body: &mut Vec<u8>) {
        body.push(self.days_ago);
        for hour in &self.hours {
            body.extend_from_slice(&[hour.min, hour.max]);
        }
    }
}

/// One hour's `SpO2` range, as the ring sends it: two bytes, both zero when there is no
/// data.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct Spo2Hour {
    /// The lowest saturation in the hour, percent; `0` for no data.
    pub min: u8,
    /// The highest saturation in the hour, percent; `0` for no data.
    pub max: u8,
}

impl Spo2Hour {
    /// An hour with no data.
    pub const EMPTY: Spo2Hour = Spo2Hour { min: 0, max: 0 };

    /// Whether the hour has no data: both bytes zero.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.min == 0 && self.max == 0
    }

    /// The minimum as a percentage; `None` for no data or a byte above 100.
    #[must_use]
    pub const fn min_percent(self) -> Option<Percent> {
        percent(self.min)
    }

    /// The maximum as a percentage; `None` for no data or a byte above 100.
    #[must_use]
    pub const fn max_percent(self) -> Option<Percent> {
        percent(self.max)
    }
}

/// `value` as a percentage: `None` for the no-data zero, or above 100 (invariant 5: a
/// bogus byte is not passed through as a reading).
const fn percent(value: u8) -> Option<Percent> {
    match value {
        0 => None,
        value => Percent::new(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// Hours 0–13 of the `QRing` fixture day, as the 14:16 reply carries them.
    const MORNING: [u8; 14] = [
        0x62, 0x60, 0x61, 0x63, 0x60, 0x60, 0x61, 0x61, 0x60, 0x61, 0x60, 0x60, 0x61, 0x61,
    ];

    /// A day block for today with `values` as the first hours, min = max, and the rest
    /// empty.
    fn block(values: &[u8]) -> Vec<u8> {
        let mut block = vec![0x00];
        for &value in values {
            block.extend_from_slice(&[value, value]);
        }
        block.resize(BLOCK_LEN, 0);
        block
    }

    fn hour(value: u8) -> Spo2Hour {
        Spo2Hour {
            min: value,
            max: value,
        }
    }

    #[test]
    fn fixture_day_is_the_blood_oxygen_rows() {
        let body = block(&MORNING);
        let spo2 = Spo2Body::from_body(&body).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(spo2.days.len(), 1);
        let day = &spo2.days[0];
        assert_eq!(day.days_ago, 0);
        assert_eq!(
            &day.hours[..5],
            [hour(98), hour(96), hour(97), hour(99), hour(96)]
        );
        assert_eq!(day.hours[13], hour(97));
        assert!(day.hours[14..].iter().all(|hour| hour.is_empty()));
        assert!(day.hours[..14].iter().all(|hour| !hour.is_empty()));
        assert_eq!(day.hours[0].min_percent().map(Percent::get), Some(98));
        assert_eq!(day.hours[0].max_percent(), day.hours[0].min_percent());
        assert_eq!(spo2.to_body(), body);

        let mut afternoon = MORNING.to_vec();
        afternoon.push(0x63);
        let body = block(&afternoon);
        let later = Spo2Body::from_body(&body).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(later.days[0].hours[14], hour(99));
        assert!(later.days[0].hours[15..].iter().all(|hour| hour.is_empty()));
        assert_eq!(later.to_body(), body);
    }

    #[test]
    fn a_body_that_is_not_whole_blocks_is_malformed() {
        let malformed = Err(BodyError::Malformed {
            kind: BigDataKind::Spo2,
            what: "day blocks",
        });
        assert_eq!(Spo2Body::from_body(&[0; 48]), malformed);
        assert_eq!(Spo2Body::from_body(&[0; 50]), malformed);
        assert_eq!(Spo2Body::from_body(&[0; 1]), malformed);
        assert_eq!(Spo2Body::from_body(&[]), Ok(Spo2Body { days: Vec::new() }));
        assert_eq!(Spo2Body { days: Vec::new() }.to_body(), Vec::<u8>::new());
    }

    #[test]
    fn several_days_round_trip() {
        let mut body = block(&[97]);
        body.extend_from_slice(&[0x01]);
        body.extend_from_slice(&[0xff; 48]);
        let spo2 = Spo2Body::from_body(&body).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(spo2.days.len(), 2);
        assert_eq!(spo2.days[1].days_ago, 1);
        assert_eq!(spo2.days[1].hours, [hour(0xff); 24]);
        assert_eq!(spo2.days[1].hours[0].min_percent(), None);
        assert_eq!(spo2.days[1].hours[0].max_percent(), None);
        assert!(!spo2.days[1].hours[0].is_empty());
        assert_eq!(spo2.to_body(), body);
    }

    #[test]
    fn percent_treats_zero_as_no_data() {
        assert_eq!(percent(0), None);
        assert_eq!(percent(1).map(Percent::get), Some(1));
        assert_eq!(percent(100), Some(Percent::FULL));
        assert_eq!(percent(101), None);
        assert!(Spo2Hour::EMPTY.is_empty());
        assert!(!Spo2Hour { min: 0, max: 1 }.is_empty());
        assert_eq!(Spo2Hour { min: 0, max: 1 }.min_percent(), None);
        assert_eq!(
            Spo2Hour { min: 0, max: 1 }.max_percent().map(Percent::get),
            Some(1)
        );
    }
}
