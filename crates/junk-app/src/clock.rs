//! The wall clock: chrono's local time as the stack's [`Timestamp`] and the trace's
//! [`Stamp`].
//!
//! A ring is written the local wall clock and reports everything as that clock expressed
//! as if it were UTC (`docs/colmi-protocol.md`, "Timestamp policy"), so what a shell needs
//! from chrono is the *local naive* time and the zone's offset, never the instant. A
//! [`Clock`] is one reading of the wall clock reduced to exactly that.

use chrono::{DateTime, Local, Offset, TimeZone};
use junk_core::Timestamp;
use junk_trace::{Stamp, StampError};

/// Milliseconds in a minute.
const MS_PER_MINUTE: i64 = 60_000;
/// Minutes in a day.
const MINUTES_PER_DAY: i64 = 24 * 60;

/// One reading of the wall clock: the local time as milliseconds since
/// `1970-01-01T00:00 local`, and the zone's offset at that moment.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Clock {
    /// The local wall clock as milliseconds since 1970-01-01T00:00 in the local zone, read
    /// as if that were UTC.
    naive_millis: i64,
    /// The zone's offset from UTC, in minutes, east positive.
    utc_offset_min: i16,
}

impl Clock {
    /// The wall clock now, in the machine's local zone.
    #[must_use]
    pub fn now() -> Clock {
        Clock::at(&Local::now())
    }

    /// The wall clock at `at`, in `at`'s zone.
    pub fn at<Tz: TimeZone>(at: &DateTime<Tz>) -> Clock {
        let naive_millis = at.naive_local().and_utc().timestamp_millis();
        // chrono keeps an offset within a day either side of UTC, so the minutes always
        // fit; the fallback is unreachable.
        let utc_offset_min = i16::try_from(at.offset().fix().local_minus_utc() / 60).unwrap_or(0);
        Clock {
            naive_millis,
            utc_offset_min,
        }
    }

    /// The reading as a [`Timestamp`]: its minute, in its zone.
    #[must_use]
    pub fn timestamp(&self) -> Timestamp {
        Timestamp {
            local_minute: self.naive_millis.div_euclid(MS_PER_MINUTE),
            utc_offset_min: self.utc_offset_min,
        }
    }

    /// The second within the minute, `0..=59`.
    #[must_use]
    pub fn second(&self) -> u8 {
        // Always below 60; the fallback is unreachable.
        u8::try_from(self.naive_millis.div_euclid(1000).rem_euclid(60)).unwrap_or(u8::MAX)
    }

    /// Midnight of the reading's local date, in its zone: "today" for the requests that
    /// place days.
    #[must_use]
    pub fn midnight(&self) -> Timestamp {
        let minute = self.naive_millis.div_euclid(MS_PER_MINUTE);
        Timestamp {
            local_minute: minute.div_euclid(MINUTES_PER_DAY) * MINUTES_PER_DAY,
            utc_offset_min: self.utc_offset_min,
        }
    }

    /// The reading as a trace stamp, millisecond precision, with its offset.
    ///
    /// # Errors
    ///
    /// [`StampError::OutOfRange`] if the year is outside `0..=9999`.
    pub fn stamp(&self) -> Result<Stamp, StampError> {
        Stamp::from_naive_epoch_millis(self.naive_millis, Some(self.utc_offset_min))
    }
}

/// `at` as a trace stamp, the form the CSVs and traces use: ISO with the offset attached.
///
/// # Errors
///
/// [`StampError::OutOfRange`] if the year is outside `0..=9999`.
pub fn stamp_of(at: Timestamp) -> Result<Stamp, StampError> {
    Stamp::from_naive_epoch_millis(
        at.local_minute.saturating_mul(MS_PER_MINUTE),
        Some(at.utc_offset_min),
    )
}

/// `at`'s date as `YYYY-MM-DD`, or `?` if it has none the format can carry.
#[must_use]
pub fn date_of(at: Timestamp) -> String {
    stamp_of(at).map_or_else(
        |_| "?".to_owned(),
        |stamp| {
            format!(
                "{:04}-{:02}-{:02}",
                stamp.year(),
                stamp.month(),
                stamp.day()
            )
        },
    )
}

/// A stamp source for a recording link: the wall clock, read at every call.
///
/// A reading the format cannot carry (a year past 9999) repeats the previous stamp rather
/// than failing a write.
///
/// # Errors
///
/// [`StampError::OutOfRange`] if the clock is already outside the format's range.
pub fn wall_clock() -> Result<impl FnMut() -> Stamp, StampError> {
    let mut last = Clock::now().stamp()?;
    Ok(move || {
        if let Ok(at) = Clock::now().stamp() {
            last = at;
        }
        last
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, Timelike};

    /// 2026-07-02 00:00 in UTC-4: the `QRing` fixture day, epoch 1782950400.
    const FIXTURE_DAY: Timestamp = Timestamp {
        local_minute: 1_782_950_400 / 60,
        utc_offset_min: -240,
    };

    fn utc_minus_4(hour: u32, minute: u32, second: u32) -> DateTime<FixedOffset> {
        FixedOffset::west_opt(4 * 3600)
            .expect("in range")
            .with_ymd_and_hms(2026, 7, 2, hour, minute, second)
            .single()
            .expect("unambiguous")
    }

    #[test]
    fn a_local_time_is_its_minute_second_and_midnight() {
        let clock = Clock::at(&utc_minus_4(14, 16, 30));
        assert_eq!(
            clock.timestamp(),
            Timestamp {
                local_minute: FIXTURE_DAY.local_minute + 14 * 60 + 16,
                utc_offset_min: -240,
            }
        );
        assert_eq!(clock.second(), 30);
        assert_eq!(clock.midnight(), FIXTURE_DAY);
        assert_eq!(
            clock.stamp().map(|s| s.to_string()),
            Ok("2026-07-02T14:16:30.000-04:00".to_owned())
        );

        // Midnight itself, and the last minute of the day, stay on that day.
        assert_eq!(Clock::at(&utc_minus_4(0, 0, 0)).timestamp(), FIXTURE_DAY);
        assert_eq!(Clock::at(&utc_minus_4(0, 0, 0)).midnight(), FIXTURE_DAY);
        assert_eq!(Clock::at(&utc_minus_4(23, 59, 59)).midnight(), FIXTURE_DAY);
        assert_eq!(Clock::at(&utc_minus_4(23, 59, 59)).second(), 59);
    }

    #[test]
    fn a_positive_offset_and_milliseconds_are_kept() {
        let at = FixedOffset::east_opt(5 * 3600 + 30 * 60)
            .expect("in range")
            .with_ymd_and_hms(2026, 1, 1, 0, 30, 15)
            .single()
            .expect("unambiguous")
            .with_nanosecond(250_000_000)
            .expect("in range");
        let clock = Clock::at(&at);
        // 2026-01-01 is 20454 days after the epoch.
        assert_eq!(
            clock.timestamp(),
            Timestamp {
                local_minute: 20_454 * MINUTES_PER_DAY + 30,
                utc_offset_min: 330,
            }
        );
        assert_eq!(clock.second(), 15);
        assert_eq!(
            clock.stamp().map(|s| s.to_string()),
            Ok("2026-01-01T00:30:15.250+05:30".to_owned())
        );
    }

    #[test]
    fn a_timestamp_renders_with_its_offset() {
        assert_eq!(
            stamp_of(FIXTURE_DAY).map(|s| s.to_string()),
            Ok("2026-07-02T00:00:00.000-04:00".to_owned())
        );
        let later = Timestamp {
            local_minute: FIXTURE_DAY.local_minute + 3 * 60 + 7,
            utc_offset_min: 0,
        };
        assert_eq!(
            stamp_of(later).map(|s| s.to_string()),
            Ok("2026-07-02T03:07:00.000+00:00".to_owned())
        );
        assert_eq!(date_of(FIXTURE_DAY), "2026-07-02");
        assert_eq!(
            date_of(Timestamp {
                local_minute: i64::MAX,
                utc_offset_min: 0,
            }),
            "?"
        );
    }

    #[test]
    fn now_is_in_range_and_the_wall_clock_ticks_forward() {
        let clock = Clock::now();
        assert!(clock.stamp().is_ok());
        assert!(clock.midnight().local_minute <= clock.timestamp().local_minute);
        let mut wall = wall_clock().expect("in range");
        let first = wall();
        let second = wall();
        assert!(second >= first);
    }
}
