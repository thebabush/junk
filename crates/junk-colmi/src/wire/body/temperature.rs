//! The `0x25` temperature reply: 50-byte day blocks of half-hourly samples.

use alloc::vec::Vec;

use crate::wire::BigDataKind;
use crate::wire::body::{BodyError, arrays};

/// Bytes in a day block: the day byte, the interval byte and 48 samples.
const BLOCK_LEN: usize = 50;
/// Samples in a day at the 30-minute interval the ring uses.
const SLOTS: usize = 48;
/// The sample byte that means no sample.
const NO_SAMPLE: u8 = 0;
/// What a sample byte is offset by: °C = (v + 200) / 10.
const OFFSET_DECI_CELSIUS: i16 = 200;

/// The body of a `0x25` reply: zero or more [`TemperatureDay`] blocks, nothing else.
///
/// The fixture replies are one day each, today.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct TemperatureBody {
    /// One block per day, in the order the ring sent them.
    pub days: Vec<TemperatureDay>,
}

impl TemperatureBody {
    /// The body's days, or why they do not decode.
    pub(super) fn from_body(body: &[u8]) -> Result<TemperatureBody, BodyError> {
        if !body.len().is_multiple_of(BLOCK_LEN) {
            return Err(BodyError::Malformed {
                kind: BigDataKind::Temperature,
                what: "day blocks",
            });
        }
        Ok(TemperatureBody {
            days: arrays(body).map(TemperatureDay::from_block).collect(),
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

/// serde only implements `Serialize` for arrays up to 32 elements.
fn serialize_samples<S: serde::Serializer>(
    samples: &[u8; SLOTS],
    ser: S,
) -> Result<S::Ok, S::Error> {
    ser.collect_seq(samples)
}

/// One day of skin temperature: `<days_ago:u8> <interval_min:u8> <48 × u8>`, slot `i`
/// taken `i × interval_min` minutes after midnight, °C = (v + 200) / 10, `0` = no sample.
///
/// The fixture block starts `00 1e a7 a8 a8 a8 a8 a8 a1 a8`: today, 30-minute slots, 36.7,
/// 36.8, 36.8, 36.8, 36.8, 36.8, 36.1, 36.8, which are the app's `Temperatures` rows at
/// 00:00–03:30. Slots 0–28 are filled on the replies before 14:30 and 0–29 after.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct TemperatureDay {
    /// Which day: `0` today, `1` yesterday, …
    pub days_ago: u8,
    /// Minutes between samples; `30` (`1e`) on this ring.
    pub interval_min: u8,
    /// The samples as sent: 48 slots, `0` for none.
    #[serde(serialize_with = "serialize_samples")]
    pub samples: [u8; SLOTS],
}

impl TemperatureDay {
    /// The sample in `slot` as tenths of a degree Celsius (`a7` → 367); `None` for a slot
    /// with no sample or past the 48.
    #[must_use]
    pub fn deci_celsius(&self, slot: usize) -> Option<i16> {
        self.samples
            .get(slot)
            .copied()
            .filter(|&sample| sample != NO_SAMPLE)
            .map(|sample| i16::from(sample) + OFFSET_DECI_CELSIUS)
    }

    /// The day a block carries; total.
    fn from_block([days_ago, interval_min, samples @ ..]: [u8; BLOCK_LEN]) -> TemperatureDay {
        TemperatureDay {
            days_ago,
            interval_min,
            samples,
        }
    }

    /// Appends the day's block to `body`.
    fn extend_body(&self, body: &mut Vec<u8>) {
        body.extend_from_slice(&[self.days_ago, self.interval_min]);
        body.extend_from_slice(&self.samples);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// Slots 0–28 of the `QRing` fixture day, as the 14:16 reply carries them.
    const MORNING: [u8; 29] = [
        0xa7, 0xa8, 0xa8, 0xa8, 0xa8, 0xa8, 0xa1, 0xa8, 0xa7, 0xa8, 0xa8, 0xa7, 0xa6, 0xa7, 0xa7,
        0xa7, 0xa7, 0xa7, 0xa7, 0xa6, 0xa6, 0xa7, 0xa8, 0xa7, 0xa5, 0xa0, 0xa6, 0xa6, 0xa7,
    ];

    /// A day block for today at 30 minutes with `values` as the first slots and the rest
    /// empty.
    fn block(values: &[u8]) -> Vec<u8> {
        let mut block = vec![0x00, 0x1e];
        block.extend_from_slice(values);
        block.resize(BLOCK_LEN, 0);
        block
    }

    #[test]
    fn fixture_day_is_the_temperatures_rows() {
        let body = block(&MORNING);
        let temperature = TemperatureBody::from_body(&body).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(temperature.days.len(), 1);
        let day = &temperature.days[0];
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
        assert_eq!(day.deci_celsius(28), Some(367));
        assert_eq!(day.deci_celsius(29), None);
        assert_eq!(day.deci_celsius(47), None);
        assert_eq!(day.deci_celsius(48), None);
        assert_eq!(day.deci_celsius(usize::MAX), None);
        assert_eq!(temperature.to_body(), body);

        let mut afternoon = MORNING.to_vec();
        afternoon.push(0xa6);
        let body = block(&afternoon);
        let later = TemperatureBody::from_body(&body).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(later.days[0].deci_celsius(29), Some(366));
        assert_eq!(later.days[0].deci_celsius(30), None);
        assert_eq!(later.to_body(), body);
    }

    #[test]
    fn a_body_that_is_not_whole_blocks_is_malformed() {
        let malformed = Err(BodyError::Malformed {
            kind: BigDataKind::Temperature,
            what: "day blocks",
        });
        assert_eq!(TemperatureBody::from_body(&[0; 49]), malformed);
        assert_eq!(TemperatureBody::from_body(&[0; 51]), malformed);
        assert_eq!(TemperatureBody::from_body(&[0; 2]), malformed);
        assert_eq!(
            TemperatureBody::from_body(&[]),
            Ok(TemperatureBody { days: Vec::new() })
        );
    }

    #[test]
    fn several_days_and_the_whole_byte_range_round_trip() {
        let mut body = block(&[0xa7]);
        body.extend_from_slice(&[0x01, 0x0f]);
        body.extend_from_slice(&[0xff; SLOTS]);
        let temperature = TemperatureBody::from_body(&body).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(temperature.days.len(), 2);
        assert_eq!(temperature.days[1].days_ago, 1);
        assert_eq!(temperature.days[1].interval_min, 15);
        assert_eq!(temperature.days[1].samples, [0xff; SLOTS]);
        assert_eq!(temperature.days[1].deci_celsius(0), Some(455));
        assert_eq!(temperature.days[0].deci_celsius(1), None);
        assert_eq!(temperature.to_body(), body);
    }
}
