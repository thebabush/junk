//! The `0x27` sleep reply: one block per day, each a session as a run of stages.

use alloc::vec;
use alloc::vec::Vec;

use crate::measure::{SleepKind, SleepStage};

use crate::wire::body::{BodyError, arrays, byte_of};
use crate::wire::{BigDataKind, WireError};

/// The stage code for light sleep.
const LIGHT: u8 = 2;
/// The stage code for deep sleep.
const DEEP: u8 = 3;
/// The stage code for REM sleep.
const REM: u8 = 4;
/// The stage code for being awake.
const AWAKE: u8 = 5;
/// The bytes of a day block after its length byte that are not stage pairs: start and end.
const BLOCK_FIXED_LEN: usize = 4;
/// Bytes per stage: the code and the minutes.
const PAIR_LEN: usize = 2;

/// The body of a `0x27` reply: `<days:u8>` then one [`SleepDay`] block per day.
///
/// The fixture reply is one day, today: `01 | 00 2e bb 00 c5 02 | 02 18 03 20 … 02 31`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct SleepBody {
    /// One block per day, in the order the ring sent them.
    pub days: Vec<SleepDay>,
}

impl SleepBody {
    /// The body's days, or why they do not decode.
    pub(super) fn from_body(body: &[u8]) -> Result<SleepBody, BodyError> {
        let malformed = |what| BodyError::Malformed {
            kind: BigDataKind::Sleep,
            what,
        };
        let (&count, mut rest) = body.split_first().ok_or(malformed("day count"))?;
        let mut days = Vec::new();
        while !rest.is_empty() {
            let (&[days_ago, len, s0, s1, e0, e1], tail) =
                rest.split_first_chunk().ok_or(malformed("day block"))?;
            let pairs_len = usize::from(len)
                .checked_sub(BLOCK_FIXED_LEN)
                .ok_or(malformed("day block"))?;
            let (pairs, next) = tail
                .split_at_checked(pairs_len)
                .ok_or(malformed("day block"))?;
            if pairs.len() % PAIR_LEN != 0 {
                return Err(malformed("stage pairs"));
            }
            days.push(SleepDay {
                days_ago,
                start_min: u16::from_le_bytes([s0, s1]),
                end_min: u16::from_le_bytes([e0, e1]),
                stages: arrays(pairs)
                    .map(|[code, minutes]| SleepStage {
                        kind: kind_of_code(code),
                        minutes,
                    })
                    .collect(),
            });
            rest = next;
        }
        if days.len() != usize::from(count) {
            return Err(malformed("day count"));
        }
        Ok(SleepBody { days })
    }

    /// The body: the day count then each day's block.
    pub(super) fn to_body(&self) -> Result<Vec<u8>, WireError> {
        let mut body = vec![byte_of(self.days.len(), "sleep days")?];
        for day in &self.days {
            body.push(day.days_ago);
            body.push(byte_of(
                BLOCK_FIXED_LEN + PAIR_LEN * day.stages.len(),
                "sleep stages",
            )?);
            body.extend_from_slice(&day.start_min.to_le_bytes());
            body.extend_from_slice(&day.end_min.to_le_bytes());
            for stage in &day.stages {
                body.extend_from_slice(&[code_of_kind(stage.kind), stage.minutes]);
            }
        }
        Ok(body)
    }
}

/// One day's sleep session.
///
/// Layout: `<days_ago:u8> <len:u8> <start u16> <end u16> <(code:u8, minutes:u8)…>`, where
/// `len` counts the bytes after it (`4 + 2 × stages`, the block's length less its two
/// header bytes) and start and end are minutes after midnight of `days_ago`. Stage codes:
/// `2` light, `3` deep, `4` REM, `5` awake; any other code is kept as
/// [`SleepKind::Unknown`] and written back unchanged.
///
/// The fixture day is `00 2e bb 00 c5 02` then 21 pairs: today, 46 bytes, 187 (03:07) to
/// 709 (11:49), from (light, 24) to (light, 49), the minutes summing to 522 = end − start,
/// which is the app's `sleepV3` row for the day.
///
/// The app treats a start at or after 18:00 (minute 1080) as the evening before; that
/// rule, and which midnight `days_ago` counts from, belong to the protocol layer.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct SleepDay {
    /// Which day: `0` today, `1` yesterday, …
    pub days_ago: u8,
    /// When the session began, minutes after midnight.
    pub start_min: u16,
    /// When it ended, minutes after midnight.
    pub end_min: u16,
    /// The stages, in order from `start_min`.
    pub stages: Vec<SleepStage>,
}

impl SleepDay {
    /// The minutes the stages add up to; `end_min - start_min` on the fixture.
    #[must_use]
    pub fn minutes(&self) -> u32 {
        self.stages
            .iter()
            .map(|stage| u32::from(stage.minutes))
            .sum()
    }
}

/// The stage a code stands for. Total: an unnamed code is [`SleepKind::Unknown`].
fn kind_of_code(code: u8) -> SleepKind {
    match code {
        LIGHT => SleepKind::Light,
        DEEP => SleepKind::Deep,
        REM => SleepKind::Rem,
        AWAKE => SleepKind::Awake,
        other => SleepKind::Unknown(other),
    }
}

/// The code on the wire for a stage.
fn code_of_kind(kind: SleepKind) -> u8 {
    match kind {
        SleepKind::Light => LIGHT,
        SleepKind::Deep => DEEP,
        SleepKind::Rem => REM,
        SleepKind::Awake => AWAKE,
        SleepKind::Unknown(code) => code,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 21 `(code, minutes)` pairs of the `QRing` fixture's sleep day, in order.
    const PAIRS: [(u8, u8); 21] = [
        (2, 0x18),
        (3, 0x20),
        (2, 0x18),
        (4, 0x10),
        (2, 0x25),
        (3, 0x0b),
        (2, 0x24),
        (4, 0x13),
        (2, 0x1d),
        (3, 0x12),
        (2, 0x0b),
        (4, 0x1b),
        (2, 0x16),
        (3, 0x09),
        (2, 0x1e),
        (4, 0x12),
        (2, 0x2b),
        (3, 0x1b),
        (2, 0x19),
        (4, 0x0f),
        (2, 0x31),
    ];

    /// The fixture body: `01 | 00 2e bb 00 c5 02 | pairs`.
    fn fixture_body() -> Vec<u8> {
        let mut body = vec![0x01, 0x00, 0x2e, 0xbb, 0x00, 0xc5, 0x02];
        for (code, minutes) in PAIRS {
            body.extend_from_slice(&[code, minutes]);
        }
        body
    }

    fn stage(code: u8, minutes: u8) -> SleepStage {
        SleepStage {
            kind: kind_of_code(code),
            minutes,
        }
    }

    fn malformed(what: &'static str) -> Result<SleepBody, BodyError> {
        Err(BodyError::Malformed {
            kind: BigDataKind::Sleep,
            what,
        })
    }

    #[test]
    fn stage_codes_round_trip_over_every_value() {
        for code in 0..=u8::MAX {
            assert_eq!(code_of_kind(kind_of_code(code)), code);
        }
        assert_eq!(kind_of_code(2), SleepKind::Light);
        assert_eq!(kind_of_code(3), SleepKind::Deep);
        assert_eq!(kind_of_code(4), SleepKind::Rem);
        assert_eq!(kind_of_code(5), SleepKind::Awake);
        assert_eq!(kind_of_code(0), SleepKind::Unknown(0));
        assert_eq!(kind_of_code(6), SleepKind::Unknown(6));
        assert_eq!(code_of_kind(SleepKind::Unknown(0x99)), 0x99);
    }

    #[test]
    fn fixture_day_is_the_sleep_v3_row() {
        let body = fixture_body();
        assert_eq!(body.len(), 49);
        let sleep = SleepBody::from_body(&body).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(sleep.days.len(), 1);
        let day = &sleep.days[0];
        assert_eq!(day.days_ago, 0);
        assert_eq!(day.start_min, 187);
        assert_eq!(day.end_min, 709);
        assert_eq!(day.stages.len(), 21);
        assert_eq!(day.minutes(), 522);
        assert_eq!(day.minutes(), u32::from(day.end_min - day.start_min));
        assert_eq!(day.stages[0], stage(2, 24));
        assert_eq!(day.stages[1], stage(3, 32));
        assert_eq!(day.stages[3], stage(4, 16));
        assert_eq!(day.stages[20], stage(2, 49));
        let rem = day
            .stages
            .iter()
            .filter(|stage| stage.kind == SleepKind::Rem)
            .count();
        assert_eq!(rem, 5);
        assert!(day.stages.iter().all(|stage| {
            matches!(
                stage.kind,
                SleepKind::Light | SleepKind::Deep | SleepKind::Rem
            )
        }));
        assert_eq!(sleep.to_body(), Ok(body));
    }

    #[test]
    fn several_days_and_unknown_codes_round_trip() {
        let sleep = SleepBody {
            days: vec![
                SleepDay {
                    days_ago: 0,
                    start_min: 0x0102,
                    end_min: 0x0304,
                    stages: vec![stage(5, 7), stage(9, 1)],
                },
                SleepDay {
                    days_ago: 1,
                    start_min: 1100,
                    end_min: 400,
                    stages: Vec::new(),
                },
            ],
        };
        let body = sleep.to_body().unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(
            body,
            [
                0x02, 0x00, 0x08, 0x02, 0x01, 0x04, 0x03, 0x05, 0x07, 0x09, 0x01, 0x01, 0x04, 0x4c,
                0x04, 0x90, 0x01
            ]
        );
        assert_eq!(SleepBody::from_body(&body), Ok(sleep));
        assert_eq!(
            SleepBody::from_body(&[0x00]),
            Ok(SleepBody { days: Vec::new() })
        );
        assert_eq!(SleepBody { days: Vec::new() }.to_body(), Ok(vec![0x00]));
    }

    #[test]
    fn a_block_length_that_disagrees_with_the_bytes_is_malformed() {
        let mut short = fixture_body();
        short.pop();
        short.pop();
        assert_eq!(SleepBody::from_body(&short), malformed("day block"));
        let mut long = fixture_body();
        long.extend_from_slice(&[0x02, 0x05]);
        assert_eq!(SleepBody::from_body(&long), malformed("day block"));
        // A length byte below the four fixed bytes.
        assert_eq!(
            SleepBody::from_body(&[0x01, 0x00, 0x03, 0xbb, 0x00, 0xc5, 0x02]),
            malformed("day block")
        );
        // A block header cut short.
        assert_eq!(
            SleepBody::from_body(&[0x01, 0x00, 0x04, 0xbb]),
            malformed("day block")
        );
    }

    #[test]
    fn an_odd_number_of_stage_bytes_is_malformed() {
        assert_eq!(
            SleepBody::from_body(&[0x01, 0x00, 0x07, 0xbb, 0x00, 0xc5, 0x02, 0x02, 0x18, 0x03]),
            malformed("stage pairs")
        );
    }

    #[test]
    fn a_day_count_that_does_not_match_the_blocks_is_malformed() {
        let mut two = fixture_body();
        two[0] = 0x02;
        assert_eq!(SleepBody::from_body(&two), malformed("day count"));
        let mut zero = fixture_body();
        zero[0] = 0x00;
        assert_eq!(SleepBody::from_body(&zero), malformed("day count"));
        assert_eq!(SleepBody::from_body(&[0x01]), malformed("day count"));
        assert_eq!(SleepBody::from_body(&[]), malformed("day count"));
    }

    #[test]
    fn counts_the_layout_writes_as_a_byte_do_not_encode_past_it() {
        let day = SleepDay {
            days_ago: 0,
            start_min: 0,
            end_min: 0,
            stages: Vec::new(),
        };
        let many_days = SleepBody {
            days: vec![day.clone(); 256],
        };
        assert_eq!(
            many_days.to_body(),
            Err(WireError::Value { what: "sleep days" })
        );
        let most_days = SleepBody {
            days: vec![day.clone(); 255],
        };
        assert!(most_days.to_body().is_ok());
        let long_night = SleepBody {
            days: vec![SleepDay {
                stages: vec![stage(2, 1); 126],
                ..day.clone()
            }],
        };
        assert_eq!(
            long_night.to_body(),
            Err(WireError::Value {
                what: "sleep stages"
            })
        );
        let longest_night = SleepBody {
            days: vec![SleepDay {
                stages: vec![stage(2, 1); 125],
                ..day
            }],
        };
        let body = longest_night
            .to_body()
            .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(body[2], 0xfe);
        assert_eq!(SleepBody::from_body(&body), Ok(longest_night));
    }
}
