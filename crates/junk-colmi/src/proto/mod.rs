//! The protocol layer: requests, transactions and the [`Driver`](junk_core::Driver) of the
//! Colmi rings (SPEC §3.3, §3.4).
//!
//! [`ColmiDriver`] serialises [`Req`]s into internal transactions, one in flight at a
//! time and the rest queued in order. A transaction writes one frame, arms
//! [`TIMEOUT_TIMER`] for [`TIMEOUT`], and turns the ring's replies into a [`Resp`]; frames
//! nobody asked for become [`Ev`]s. The ring's [`Dialect`] is built as the session reveals
//! it: which channels resolved at connect, the capability bitmap in the `0x01` ack, and the
//! firmware string read from the Device Information service after the `0x19` ack.
//!
//! The single-reply transactions answer on the first frame of their command. The
//! multi-packet ones collect: the HR log and the stress and HRV series take a header and
//! then packets in index order, the activity rows a header and then rows in index order,
//! and the big-data collections reassemble one `0xbc` frame at a time and read it as the
//! typed body its kind has (a workout detail is a descriptor and then its packages). The
//! wire values become model samples in `model`, pure functions of the packet and the
//! request's [`Timestamp`]s.
//!
//! # The clock
//!
//! The ring is written the *local* wall clock ([`Req::SetTime`]). It has no notion of a
//! zone, and every timestamp it reports afterwards is that same clock expressed as if it
//! were UTC (`docs/colmi-protocol.md`, "Timestamp policy"). A [`Timestamp`] already is a
//! local minute, so all that is needed here is a calendar to turn its day count into a
//! date and back; `junk-core` has none, so `civil_from_days` and `days_from_civil` live
//! here, with the day arithmetic the ring's day offsets need. The driver never computes
//! "today": every request that places a day carries it, and its UTC offset is the one
//! every sample of that request gets.

mod dialect;
mod driver;
mod model;
mod req;
mod txn;

use junk_core::Timestamp;

pub use dialect::{Dialect, LiveHr, SleepSource, Transport};
pub use driver::{ColmiDriver, TIMEOUT, TIMEOUT_TIMER};
pub use req::{Ev, Req, Resp};
#[cfg(test)]
pub(crate) use txn::Txn;

/// Minutes in a day.
pub(crate) const MINUTES_PER_DAY: i64 = 24 * 60;

/// The proleptic Gregorian date `days` after 1970-01-01, as `(year, month, day)`; negative
/// days are before it.
///
/// Howard Hinnant's `civil_from_days`, exact over the whole `i64` range that keeps the
/// year in an `i64`. Month and day are `1..=12` and `1..=31` by construction.
fn civil_from_days(days: i64) -> (i64, u8, u8) {
    let z = days.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    // Day of era, `0..=146096`.
    let doe = z.rem_euclid(146_097);
    // Year of era, `0..=399`.
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    // Day of year, `0..=365`, counted from March.
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    // Month, `0..=11`, counted from March.
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, small(month), small(day))
}

/// Days from 1970-01-01 to `year-month-day`: Hinnant's `days_from_civil`, the inverse of
/// [`civil_from_days`].
///
/// Exact for any date a ring can send (its years are two BCD digits); a year near the
/// ends of `i64` saturates rather than overflowing.
pub(crate) fn days_from_civil(year: i64, month: u8, day: u8) -> i64 {
    let month = i64::from(month);
    let year = year.saturating_sub(i64::from(month <= 2));
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era.saturating_mul(146_097)
        .saturating_add(doe)
        .saturating_sub(719_468)
}

/// The midnight that starts `at`'s day, in `at`'s zone.
///
/// Saturates in the last day of the range, where flooring would leave it.
fn midnight(at: Timestamp) -> Timestamp {
    Timestamp {
        local_minute: at
            .local_minute
            .div_euclid(MINUTES_PER_DAY)
            .saturating_mul(MINUTES_PER_DAY),
        ..at
    }
}

/// Midnight of the day `days_ago` days before `today`'s: where a ring's day offset
/// (`0` today, `1` yesterday, …) puts slot 0, in `today`'s zone.
fn day_minute(today: Timestamp, days_ago: u8) -> Timestamp {
    minutes_after(midnight(today), -(i64::from(days_ago) * MINUTES_PER_DAY))
}

/// `minutes` after `at` (before it, when negative), in `at`'s zone.
///
/// Saturates at the ends of the range: a request made under an absurd `Timestamp` gives
/// absurd sample times, never a panic (invariant 5).
fn minutes_after(at: Timestamp, minutes: i64) -> Timestamp {
    Timestamp {
        local_minute: at.local_minute.saturating_add(minutes),
        ..at
    }
}

/// `value` as a byte, for values the arithmetic here keeps below 256.
///
/// The fallback is unreachable; were it reached, the wire layer would refuse to encode
/// `255` as a BCD field and the request would fail rather than write a wrong time.
fn small(value: i64) -> u8 {
    u8::try_from(value).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-07-02 00:00 in UTC-4: the `QRing` fixture day, epoch 1782950400 = 20636 days.
    const FIXTURE_DAY: Timestamp = Timestamp {
        local_minute: 20_636 * MINUTES_PER_DAY,
        utc_offset_min: -240,
    };

    #[test]
    fn known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        // 2026-07-02 00:00 UTC is epoch 1782950400 = 20636 days (the QRing fixture day).
        assert_eq!(civil_from_days(20_636), (2026, 7, 2));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(11_017), (2000, 3, 1));
        // 1900 was not a leap year.
        assert_eq!(civil_from_days(-25_509), (1900, 2, 28));
        assert_eq!(civil_from_days(-25_508), (1900, 3, 1));
        assert_eq!(civil_from_days(47_481), (2099, 12, 31));
        assert_eq!(civil_from_days(47_482), (2100, 1, 1));

        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(days_from_civil(2026, 7, 2), 20_636);
        assert_eq!(days_from_civil(2000, 2, 29), 11_016);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(2099, 12, 31), 47_481);
        assert_eq!(days_from_civil(2100, 1, 1), 47_482);
    }

    #[test]
    fn round_trips_every_day_of_four_centuries() {
        // One full 400-year cycle either side of the epoch, day by day.
        let mut expected = (1800, 1, 1);
        for days in days_from_civil(1800, 1, 1)..=days_from_civil(2200, 12, 31) {
            let got = civil_from_days(days);
            assert_eq!(got, expected, "day {days}");
            assert_eq!(days_from_civil(got.0, got.1, got.2), days);
            expected = next_day(expected);
        }
    }

    /// The day after `(year, month, day)`, by the calendar rules spelled out.
    fn next_day((year, month, day): (i64, u8, u8)) -> (i64, u8, u8) {
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let length = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => panic!("month {month}"),
        };
        if day < length {
            (year, month, day + 1)
        } else if month < 12 {
            (year, month + 1, 1)
        } else {
            (year + 1, 1, 1)
        }
    }

    #[test]
    fn midnight_floors_and_keeps_the_offset() {
        let afternoon = Timestamp {
            local_minute: FIXTURE_DAY.local_minute + 14 * 60 + 16,
            utc_offset_min: -240,
        };
        assert_eq!(midnight(afternoon), FIXTURE_DAY);
        assert_eq!(midnight(FIXTURE_DAY), FIXTURE_DAY);
        let last_minute = Timestamp {
            local_minute: FIXTURE_DAY.local_minute + MINUTES_PER_DAY - 1,
            utc_offset_min: 600,
        };
        assert_eq!(
            midnight(last_minute),
            Timestamp {
                utc_offset_min: 600,
                ..FIXTURE_DAY
            }
        );
        // Before the epoch, floor still goes down.
        let before = Timestamp {
            local_minute: -1,
            utc_offset_min: 0,
        };
        assert_eq!(midnight(before).local_minute, -MINUTES_PER_DAY);
    }

    #[test]
    fn day_minute_counts_days_back_from_todays_midnight() {
        let afternoon = Timestamp {
            local_minute: FIXTURE_DAY.local_minute + 14 * 60 + 16,
            utc_offset_min: -240,
        };
        assert_eq!(day_minute(afternoon, 0), FIXTURE_DAY);
        assert_eq!(
            day_minute(afternoon, 1).local_minute,
            FIXTURE_DAY.local_minute - MINUTES_PER_DAY
        );
        assert_eq!(
            day_minute(afternoon, 29).local_minute,
            FIXTURE_DAY.local_minute - 29 * MINUTES_PER_DAY
        );
        assert_eq!(day_minute(afternoon, 29).utc_offset_min, -240);
        assert_eq!(
            minutes_after(FIXTURE_DAY, 187).local_minute,
            FIXTURE_DAY.local_minute + 187
        );
        assert_eq!(
            minutes_after(FIXTURE_DAY, -1).local_minute,
            FIXTURE_DAY.local_minute - 1
        );
    }

    #[test]
    fn extremes_do_not_panic() {
        let _ = civil_from_days(i64::MAX);
        let _ = civil_from_days(i64::MIN);
        let _ = days_from_civil(i64::MAX, 12, 31);
        let _ = days_from_civil(i64::MIN, 1, 1);
        let _ = days_from_civil(0, 0, 0);
        let _ = days_from_civil(0, u8::MAX, u8::MAX);
        for local_minute in [i64::MAX, i64::MIN, 0] {
            let at = Timestamp {
                local_minute,
                utc_offset_min: 0,
            };
            let _ = midnight(at);
            let _ = day_minute(at, u8::MAX);
            let _ = minutes_after(at, i64::MAX);
            let _ = minutes_after(at, i64::MIN);
        }
        assert_eq!(
            minutes_after(
                Timestamp {
                    local_minute: i64::MAX,
                    utc_offset_min: 0
                },
                1
            )
            .local_minute,
            i64::MAX
        );
        assert_eq!(small(0), 0);
        assert_eq!(small(31), 31);
        assert_eq!(small(256), u8::MAX);
        assert_eq!(small(-1), u8::MAX);
    }
}
