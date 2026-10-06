//! Timestamps as the trace writes them: wall-clock fields, kept exactly as read.

use core::fmt;
use core::str::FromStr;

/// Width of the time part, `HH:MM:SS.mmm`.
const TIME_LEN: usize = "HH:MM:SS.mmm".len();

/// Milliseconds in a day.
const MS_PER_DAY: i64 = 86_400_000;

/// The largest offset magnitude, in minutes: `23:59`.
const MAX_OFFSET_MIN: u16 = 23 * 60 + 59;

/// A trace timestamp: `YYYY-MM-DDTHH:MM:SS.mmm`, optionally followed by `+HH:MM` or `-HH:MM`.
///
/// The fields are stored as read, not folded into an epoch, so that [`Display`](fmt::Display)
/// writes back exactly what [`Stamp::parse`] took: a stamp without an offset (the `QRing`
/// app log) stays without one, a stamp with an offset (`PacketLogger` captures converted by
/// `tools/pklg2trace.py`) keeps it. The one normalisation is `-00:00`, which is read as an
/// offset of zero and written back as `+00:00`.
///
/// Ordering and equality are field by field, wall clock first, offset last; the offset is
/// never applied. Every stamp of one trace carries the same offset (or none), so within a
/// trace that is chronological order. A `Z` suffix is not accepted: the fixtures are local
/// wall-clock captures and never carry one.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Stamp {
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    millis: u16,
    /// Minutes east of UTC, or `None` if the text carried no offset.
    offset_min: Option<i16>,
}

impl Stamp {
    /// Reads `YYYY-MM-DDTHH:MM:SS.mmm` with an optional `+HH:MM` or `-HH:MM`.
    ///
    /// Every field must have exactly its width; the millisecond part is mandatory. Ranges
    /// are checked field by field (month `1..=12`, day `1..=31`, hour `< 24`, minute and
    /// second `< 60`, offset hours `< 24` and offset minutes `< 60`) but not against each
    /// other: `2026-02-31` is accepted.
    ///
    /// # Errors
    ///
    /// [`StampError::Malformed`] if the text does not have that shape (a `Z` suffix, a
    /// missing millisecond part, any other layout), [`StampError::OutOfRange`] if a field
    /// is outside its range.
    pub fn parse(text: &str) -> Result<Stamp, StampError> {
        let shape = StampError::Malformed("expected YYYY-MM-DDTHH:MM:SS.mmm");
        let (date, rest) = text.split_once('T').ok_or(shape)?;
        let (time, offset) = rest.split_at_checked(TIME_LEN).ok_or(shape)?;

        let mut date = date.split('-');
        let year = digits(date.next(), 4)?;
        let month = ranged(digits(date.next(), 2)?, 1, 12, "month")?;
        let day = ranged(digits(date.next(), 2)?, 1, 31, "day")?;
        if date.next().is_some() {
            return Err(shape);
        }

        let mut time = time.split(':');
        let hour = ranged(digits(time.next(), 2)?, 0, 23, "hour")?;
        let minute = ranged(digits(time.next(), 2)?, 0, 59, "minute")?;
        let (second, millis) = time.next().and_then(|s| s.split_once('.')).ok_or(shape)?;
        let second = ranged(digits(Some(second), 2)?, 0, 59, "second")?;
        let millis = ranged(digits(Some(millis), 3)?, 0, 999, "millisecond")?;

        Ok(Stamp {
            year,
            month,
            day,
            hour,
            minute,
            second,
            millis,
            offset_min: parse_offset(offset)?,
        })
    }

    /// A stamp from its fields, checked as [`Stamp::parse`] checks them.
    ///
    /// Year `0..=9999` (it is written with four digits), month `1..=12`, day `1..=31`,
    /// hour `< 24`, minute and second `< 60`, millisecond `< 1000`, offset magnitude below
    /// 24 hours. Fields are not checked against each other, as in `parse`. An offset of
    /// zero, either sign, is written `+00:00`.
    ///
    /// # Errors
    ///
    /// [`StampError::OutOfRange`] naming the first field outside its range.
    #[allow(
        clippy::too_many_arguments,
        reason = "one parameter per field of the stamp, in the order they are written"
    )]
    pub fn new(
        year: i32,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
        millis: u16,
        utc_offset_min: Option<i16>,
    ) -> Result<Stamp, StampError> {
        let year = ranged(year, 0, 9999, "year")?;
        let month = ranged(month, 1, 12, "month")?;
        let day = ranged(day, 1, 31, "day")?;
        let hour = ranged(hour, 0, 23, "hour")?;
        let minute = ranged(minute, 0, 59, "minute")?;
        let second = ranged(second, 0, 59, "second")?;
        let millis = ranged(millis, 0, 999, "millisecond")?;
        if let Some(offset) = utc_offset_min {
            // A magnitude of 24 hours or more is an hours field `parse` would reject.
            ranged(offset.unsigned_abs(), 0, MAX_OFFSET_MIN, "offset hours")?;
        }
        Ok(Stamp {
            year,
            month,
            day,
            hour,
            minute,
            second,
            millis,
            offset_min: utc_offset_min,
        })
    }

    /// The inverse of [`Stamp::naive_epoch_millis`]: the wall-clock fields `millis`
    /// milliseconds after `1970-01-01T00:00:00.000`, read as if UTC, with `utc_offset_min`
    /// attached as given, not applied. No leap seconds.
    ///
    /// # Errors
    ///
    /// [`StampError::OutOfRange`] if the year falls outside `0..=9999` or the offset
    /// magnitude is 24 hours or more.
    pub fn from_naive_epoch_millis(
        millis: i64,
        utc_offset_min: Option<i16>,
    ) -> Result<Stamp, StampError> {
        let days = millis.div_euclid(MS_PER_DAY);
        let of_day = millis.rem_euclid(MS_PER_DAY);
        let (year, month, day) = civil_from_days(days);
        Stamp::new(
            narrow(year, "year")?,
            narrow(month, "month")?,
            narrow(day, "day")?,
            narrow(of_day / 3_600_000, "hour")?,
            narrow(of_day / 60_000 % 60, "minute")?,
            narrow(of_day / 1000 % 60, "second")?,
            narrow(of_day % 1000, "millisecond")?,
            utc_offset_min,
        )
    }

    /// The year, four digits as written.
    #[must_use]
    pub fn year(&self) -> i32 {
        self.year
    }

    /// The month, `1..=12`.
    #[must_use]
    pub fn month(&self) -> u8 {
        self.month
    }

    /// The day of the month, `1..=31`.
    #[must_use]
    pub fn day(&self) -> u8 {
        self.day
    }

    /// The hour, `0..=23`.
    #[must_use]
    pub fn hour(&self) -> u8 {
        self.hour
    }

    /// The minute, `0..=59`.
    #[must_use]
    pub fn minute(&self) -> u8 {
        self.minute
    }

    /// The second, `0..=59`.
    #[must_use]
    pub fn second(&self) -> u8 {
        self.second
    }

    /// The millisecond, `0..=999`.
    #[must_use]
    pub fn millis(&self) -> u16 {
        self.millis
    }

    /// The offset from UTC in minutes, east positive, or `None` if the stamp carried none.
    #[must_use]
    pub fn utc_offset_min(&self) -> Option<i16> {
        self.offset_min
    }

    /// The wall-clock fields as milliseconds since `1970-01-01T00:00:00.000`, read as if
    /// they were UTC: the offset, if any, is ignored. No leap seconds.
    ///
    /// Meant for differences between stamps of one trace, which share an offset; it is not
    /// the Unix time of a stamp that carries one.
    #[must_use]
    pub fn naive_epoch_millis(&self) -> i64 {
        let days = days_from_civil(
            i64::from(self.year),
            i64::from(self.month),
            i64::from(self.day),
        );
        let hours = days * 24 + i64::from(self.hour);
        let minutes = hours * 60 + i64::from(self.minute);
        let seconds = minutes * 60 + i64::from(self.second);
        seconds * 1000 + i64::from(self.millis)
    }

    /// `self - earlier` in milliseconds, by [`Stamp::naive_epoch_millis`]: offsets are
    /// ignored, so this only means something between stamps of one trace. Negative if
    /// `earlier` is in fact the later one.
    #[must_use]
    pub fn delta_millis(&self, earlier: &Stamp) -> i64 {
        self.naive_epoch_millis() - earlier.naive_epoch_millis()
    }
}

/// Same as [`Stamp::parse`].
impl FromStr for Stamp {
    type Err = StampError;

    fn from_str(text: &str) -> Result<Stamp, StampError> {
        Stamp::parse(text)
    }
}

impl fmt::Display for Stamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Stamp {
            year,
            month,
            day,
            hour,
            minute,
            second,
            millis,
            offset_min,
        } = *self;
        write!(
            f,
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}"
        )?;
        if let Some(offset) = offset_min {
            let sign = if offset < 0 { '-' } else { '+' };
            let magnitude = offset.unsigned_abs();
            write!(f, "{sign}{:02}:{:02}", magnitude / 60, magnitude % 60)?;
        }
        Ok(())
    }
}

/// Reads a field of exactly `width` ASCII digits.
fn digits<T: FromStr>(text: Option<&str>, width: usize) -> Result<T, StampError> {
    let malformed = StampError::Malformed("expected a fixed-width decimal field");
    let text = text.ok_or(malformed)?;
    if text.len() != width || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(malformed);
    }
    text.parse().map_err(|_| malformed)
}

/// Checks that `value` is in `low..=high`, naming `what` if it is not.
fn ranged<T: PartialOrd>(value: T, low: T, high: T, what: &'static str) -> Result<T, StampError> {
    if (low..=high).contains(&value) {
        Ok(value)
    } else {
        Err(StampError::OutOfRange(what))
    }
}

/// Reads what follows the time: nothing, or `+HH:MM` / `-HH:MM`, as minutes east of UTC.
fn parse_offset(text: &str) -> Result<Option<i16>, StampError> {
    if text.is_empty() {
        return Ok(None);
    }
    let shape = StampError::Malformed("expected +HH:MM or -HH:MM after the time");
    let (sign, rest) = text.split_at_checked(1).ok_or(shape)?;
    let east = match sign {
        "+" => true,
        "-" => false,
        _ => return Err(shape),
    };
    let (hours, minutes) = rest.split_once(':').ok_or(shape)?;
    let hours: i16 = ranged(digits(Some(hours), 2)?, 0, 23, "offset hours")?;
    let minutes: i16 = ranged(digits(Some(minutes), 2)?, 0, 59, "offset minutes")?;
    let total = hours * 60 + minutes;
    Ok(Some(if east { total } else { -total }))
}

/// Days from 1970-01-01 to the given proleptic Gregorian date, negative before it.
///
/// Howard Hinnant's `days_from_civil`: years are counted from March so that the leap day
/// falls at the end of the year, and in 400-year eras of exactly 146 097 days.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The proleptic Gregorian date `days` after 1970-01-01, as `(year, month, day)`: the
/// inverse of [`days_from_civil`], from the same source.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Narrows a field computed in `i64` to the type it is stored as; one that does not fit is
/// out of range, and [`Stamp::new`] then checks the range proper.
fn narrow<T: TryFrom<i64>>(value: i64, what: &'static str) -> Result<T, StampError> {
    T::try_from(value).map_err(|_| StampError::OutOfRange(what))
}

/// Why a timestamp did not read.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum StampError {
    /// The text is not `YYYY-MM-DDTHH:MM:SS.mmm` with an optional `+HH:MM` or `-HH:MM`;
    /// the text says what was expected.
    Malformed(&'static str),
    /// A field is outside its range; the text says which field.
    OutOfRange(&'static str),
}

impl fmt::Display for StampError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StampError::Malformed(what) => write!(f, "malformed timestamp: {what}"),
            StampError::OutOfRange(what) => write!(f, "timestamp {what} out of range"),
        }
    }
}

impl core::error::Error for StampError {}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    fn stamp(text: &str) -> Stamp {
        Stamp::parse(text).unwrap_or_else(|err| panic!("{text}: {err}"))
    }

    /// `at` rebuilt from its own fields, through [`Stamp::new`].
    fn rebuilt(at: Stamp) -> Result<Stamp, StampError> {
        Stamp::new(
            at.year(),
            at.month(),
            at.day(),
            at.hour(),
            at.minute(),
            at.second(),
            at.millis(),
            at.utc_offset_min(),
        )
    }

    /// A small deterministic generator (xorshift64*), so the tests need no `rand`.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        /// A value in `low..=high`.
        fn range(&mut self, low: i64, high: i64) -> i64 {
            let span = u64::try_from(high - low + 1).expect("non-empty range");
            low + i64::try_from(self.next() % span).expect("below span")
        }
    }

    #[test]
    fn round_trips_without_offset() {
        let text = "2026-07-02T14:16:30.474";
        let at = stamp(text);
        assert_eq!(at.to_string(), text);
        assert_eq!(at.year(), 2026);
        assert_eq!(at.month(), 7);
        assert_eq!(at.day(), 2);
        assert_eq!(at.hour(), 14);
        assert_eq!(at.minute(), 16);
        assert_eq!(at.second(), 30);
        assert_eq!(at.millis(), 474);
        assert_eq!(at.utc_offset_min(), None);
        assert_eq!(text.parse::<Stamp>(), Ok(at));
    }

    #[test]
    fn round_trips_with_offset() {
        let text = "2025-11-19T10:20:45.341-05:00";
        let at = stamp(text);
        assert_eq!(at.to_string(), text);
        assert_eq!(at.year(), 2025);
        assert_eq!(at.month(), 11);
        assert_eq!(at.day(), 19);
        assert_eq!(at.hour(), 10);
        assert_eq!(at.minute(), 20);
        assert_eq!(at.second(), 45);
        assert_eq!(at.millis(), 341);
        assert_eq!(at.utc_offset_min(), Some(-300));

        for text in [
            "2025-11-19T10:20:45.341+05:30",
            "2025-11-19T10:20:45.341-00:30",
            "2025-11-19T10:20:45.341+00:00",
            "0000-01-01T00:00:00.000+23:59",
            "9999-12-31T23:59:59.999-23:59",
        ] {
            assert_eq!(stamp(text).to_string(), text);
        }
        assert_eq!(
            stamp("2025-11-19T10:20:45.341+05:30").utc_offset_min(),
            Some(330)
        );
        assert_eq!(
            stamp("2025-11-19T10:20:45.341-00:30").utc_offset_min(),
            Some(-30)
        );
        assert_eq!(
            stamp("2025-11-19T10:20:45.341-23:59").utc_offset_min(),
            Some(-1439)
        );
    }

    #[test]
    fn negative_zero_offset_is_normalised() {
        let at = stamp("2025-11-19T10:20:45.341-00:00");
        assert_eq!(at.utc_offset_min(), Some(0));
        assert_eq!(at.to_string(), "2025-11-19T10:20:45.341+00:00");
    }

    #[test]
    fn rejects_malformed() {
        for text in [
            "",
            "garbage",
            "2026-07-02",
            "2026-07-02T14:16:30",
            "2026-07-02T14:16:30.47",
            "2026-07-02T14:16:30.4745",
            "2026-07-02T14:16:30.474Z",
            "2026-07-02T14:16:30.474-05",
            "2026-07-02T14:16:30.474-0500",
            "2026-07-02T14:16:30.474 -05:00",
            "2026-07-02T14:16:30.474-05:00x",
            "2026-07-02t14:16:30.474",
            "2026-7-02T14:16:30.474",
            "26-07-02T14:16:30.474",
            "2026-07-02T14:16:30,474",
            "2026-07-02-1T14:16:30.474",
            "+026-07-02T14:16:30.474",
            "2026-07-02T14:16:+0.474",
            "2026-07-02T14:16:30.474+5:00",
            "2026-07-02T14:16:30.474+05:0",
            "2026-07-02T14:16:30.474±05:00",
        ] {
            assert!(
                matches!(Stamp::parse(text), Err(StampError::Malformed(_))),
                "{text:?} should be malformed, got {:?}",
                Stamp::parse(text)
            );
        }
    }

    #[test]
    fn rejects_out_of_range() {
        let cases = [
            ("2026-13-01T00:00:00.000", "month"),
            ("2026-00-01T00:00:00.000", "month"),
            ("2026-01-32T00:00:00.000", "day"),
            ("2026-01-00T00:00:00.000", "day"),
            ("2026-01-01T24:00:00.000", "hour"),
            ("2026-01-01T00:60:00.000", "minute"),
            ("2026-01-01T00:00:60.000", "second"),
            ("2026-01-01T00:00:00.000+24:00", "offset hours"),
            ("2026-01-01T00:00:00.000-05:60", "offset minutes"),
        ];
        for (text, what) in cases {
            assert_eq!(
                Stamp::parse(text),
                Err(StampError::OutOfRange(what)),
                "{text}"
            );
        }
    }

    #[test]
    fn new_agrees_with_parse() {
        for text in [
            "2026-07-02T14:16:30.474",
            "2025-11-19T10:20:45.341-05:00",
            "2025-11-19T10:20:45.341+05:30",
            "0000-01-01T00:00:00.000+23:59",
            "9999-12-31T23:59:59.999-23:59",
        ] {
            let at = stamp(text);
            assert_eq!(rebuilt(at), Ok(at), "{text}");
            assert_eq!(rebuilt(at).map(|s| s.to_string()), Ok(text.to_string()));
        }
        assert_eq!(
            Stamp::new(2026, 7, 2, 14, 16, 30, 474, None),
            Ok(stamp("2026-07-02T14:16:30.474"))
        );
        assert_eq!(
            Stamp::new(2025, 11, 19, 10, 20, 45, 341, Some(-300)),
            Ok(stamp("2025-11-19T10:20:45.341-05:00"))
        );
        // Like `parse`, day and month are not checked against each other.
        assert!(Stamp::new(2026, 2, 31, 0, 0, 0, 0, None).is_ok());
        // A zero offset is written `+00:00` whichever way it was made.
        assert_eq!(
            Stamp::new(2026, 1, 1, 0, 0, 0, 0, Some(0)),
            Ok(stamp("2026-01-01T00:00:00.000-00:00"))
        );
    }

    #[test]
    fn new_rejects_out_of_range() {
        let ok = (2026, 7, 2, 14, 16, 30, 474, Some(-300));
        let (y, mo, d, h, mi, s, ms, off) = ok;
        assert!(Stamp::new(y, mo, d, h, mi, s, ms, off).is_ok());
        let cases = [
            (Stamp::new(-1, mo, d, h, mi, s, ms, off), "year"),
            (Stamp::new(10_000, mo, d, h, mi, s, ms, off), "year"),
            (Stamp::new(y, 0, d, h, mi, s, ms, off), "month"),
            (Stamp::new(y, 13, d, h, mi, s, ms, off), "month"),
            (Stamp::new(y, mo, 0, h, mi, s, ms, off), "day"),
            (Stamp::new(y, mo, 32, h, mi, s, ms, off), "day"),
            (Stamp::new(y, mo, d, 24, mi, s, ms, off), "hour"),
            (Stamp::new(y, mo, d, h, 60, s, ms, off), "minute"),
            (Stamp::new(y, mo, d, h, mi, 60, ms, off), "second"),
            (Stamp::new(y, mo, d, h, mi, s, 1000, off), "millisecond"),
            (
                Stamp::new(y, mo, d, h, mi, s, ms, Some(1440)),
                "offset hours",
            ),
            (
                Stamp::new(y, mo, d, h, mi, s, ms, Some(-1440)),
                "offset hours",
            ),
            (
                Stamp::new(y, mo, d, h, mi, s, ms, Some(i16::MIN)),
                "offset hours",
            ),
        ];
        for (result, what) in cases {
            assert_eq!(result, Err(StampError::OutOfRange(what)));
        }
        assert!(Stamp::new(y, mo, d, h, mi, s, ms, Some(1439)).is_ok());
        assert!(Stamp::new(y, mo, d, h, mi, s, ms, Some(-1439)).is_ok());
        assert!(Stamp::new(0, 1, 1, 0, 0, 0, 0, None).is_ok());
        assert!(Stamp::new(9999, 12, 31, 23, 59, 59, 999, None).is_ok());
    }

    #[test]
    fn epoch_inverse_on_known_dates() {
        let cases = [
            (0, "1970-01-01T00:00:00.000"),
            (1, "1970-01-01T00:00:00.001"),
            (-1, "1969-12-31T23:59:59.999"),
            (MS_PER_DAY, "1970-01-02T00:00:00.000"),
            (951_868_800_000, "2000-03-01T00:00:00.000"),
            (951_868_800_000 - 1, "2000-02-29T23:59:59.999"),
            (1_709_164_800_000, "2024-02-29T00:00:00.000"),
            (1_735_689_599_999, "2024-12-31T23:59:59.999"),
            (1_735_689_600_000, "2025-01-01T00:00:00.000"),
        ];
        for (millis, text) in cases {
            assert_eq!(
                Stamp::from_naive_epoch_millis(millis, None),
                Ok(stamp(text))
            );
            assert_eq!(stamp(text).naive_epoch_millis(), millis, "{text}");
        }
        // The offset is attached, not applied.
        assert_eq!(
            Stamp::from_naive_epoch_millis(951_868_800_000, Some(-300)),
            Ok(stamp("2000-03-01T00:00:00.000-05:00"))
        );
        assert_eq!(
            Stamp::from_naive_epoch_millis(0, Some(1440)),
            Err(StampError::OutOfRange("offset hours"))
        );

        // The four-digit years, exactly.
        let first = days_from_civil(0, 1, 1) * MS_PER_DAY;
        let last = days_from_civil(9999, 12, 31) * MS_PER_DAY + MS_PER_DAY - 1;
        assert_eq!(
            Stamp::from_naive_epoch_millis(first, None),
            Ok(stamp("0000-01-01T00:00:00.000"))
        );
        assert_eq!(
            Stamp::from_naive_epoch_millis(last, None),
            Ok(stamp("9999-12-31T23:59:59.999"))
        );
        assert_eq!(
            Stamp::from_naive_epoch_millis(first - 1, None),
            Err(StampError::OutOfRange("year"))
        );
        assert_eq!(
            Stamp::from_naive_epoch_millis(last + 1, None),
            Err(StampError::OutOfRange("year"))
        );
    }

    #[test]
    fn epoch_round_trips_random_values() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let first = days_from_civil(0, 1, 1) * MS_PER_DAY;
        let last = days_from_civil(9999, 12, 31) * MS_PER_DAY + MS_PER_DAY - 1;
        for _ in 0..5000 {
            // Milliseconds -> fields -> milliseconds, and through the text form.
            let millis = rng.range(first, last);
            let offset = match rng.range(0, 2) {
                0 => None,
                _ => Some(i16::try_from(rng.range(-1439, 1439)).expect("fits")),
            };
            let at = Stamp::from_naive_epoch_millis(millis, offset)
                .unwrap_or_else(|err| panic!("{millis}: {err}"));
            assert_eq!(at.naive_epoch_millis(), millis);
            assert_eq!(at.utc_offset_min(), offset);
            assert_eq!(stamp(&at.to_string()), at);
            assert_eq!(rebuilt(at), Ok(at));

            // Fields -> milliseconds -> fields, on dates every month has.
            let fields = Stamp::new(
                i32::try_from(rng.range(0, 9999)).expect("fits"),
                u8::try_from(rng.range(1, 12)).expect("fits"),
                u8::try_from(rng.range(1, 28)).expect("fits"),
                u8::try_from(rng.range(0, 23)).expect("fits"),
                u8::try_from(rng.range(0, 59)).expect("fits"),
                u8::try_from(rng.range(0, 59)).expect("fits"),
                u16::try_from(rng.range(0, 999)).expect("fits"),
                offset,
            )
            .unwrap_or_else(|err| panic!("{err}"));
            assert_eq!(
                Stamp::from_naive_epoch_millis(fields.naive_epoch_millis(), offset),
                Ok(fields)
            );
        }
    }

    #[test]
    fn civil_conversions_are_inverses_day_by_day() {
        // Every day of a 400-year era, which covers every leap rule once.
        let start = days_from_civil(2000, 1, 1);
        for days in start..start + 146_097 {
            let (year, month, day) = civil_from_days(days);
            assert!(
                (1..=12).contains(&month) && (1..=31).contains(&day),
                "{days}"
            );
            assert_eq!(days_from_civil(year, month, day), days);
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(-719_468), (0, 3, 1));
    }

    #[test]
    fn naive_epoch_matches_known_dates() {
        assert_eq!(stamp("1970-01-01T00:00:00.000").naive_epoch_millis(), 0);
        assert_eq!(stamp("1970-01-01T00:00:00.001").naive_epoch_millis(), 1);
        assert_eq!(stamp("1969-12-31T23:59:59.999").naive_epoch_millis(), -1);
        assert_eq!(
            stamp("1970-01-02T00:00:00.000").naive_epoch_millis(),
            MS_PER_DAY
        );
        // 2000-03-01T00:00:00Z is 951 868 800 s, a value every Unix clock agrees on.
        assert_eq!(
            stamp("2000-03-01T00:00:00.000").naive_epoch_millis(),
            951_868_800_000
        );
        // The offset is deliberately ignored.
        assert_eq!(
            stamp("2000-03-01T00:00:00.000-05:00").naive_epoch_millis(),
            951_868_800_000
        );
    }

    #[test]
    fn deltas_cross_boundaries() {
        let first = stamp("2026-07-02T14:16:30.474");
        let second = stamp("2026-07-02T14:16:30.475");
        assert_eq!(second.delta_millis(&first), 1);
        assert_eq!(first.delta_millis(&second), -1);
        assert_eq!(first.delta_millis(&first), 0);

        let before_midnight = stamp("2026-07-02T23:59:59.999");
        let after_midnight = stamp("2026-07-03T00:00:00.000");
        assert_eq!(after_midnight.delta_millis(&before_midnight), 1);

        let old_year = stamp("2025-12-31T23:59:59.000");
        let new_year = stamp("2026-01-01T00:00:01.000");
        assert_eq!(new_year.delta_millis(&old_year), 2000);

        let leap = stamp("2024-02-28T12:00:00.000");
        let after_leap = stamp("2024-03-01T12:00:00.000");
        assert_eq!(after_leap.delta_millis(&leap), 2 * MS_PER_DAY);
        let common = stamp("2023-02-28T12:00:00.000");
        let after_common = stamp("2023-03-01T12:00:00.000");
        assert_eq!(after_common.delta_millis(&common), MS_PER_DAY);
    }

    #[test]
    fn orders_chronologically_within_one_offset() {
        let first = stamp("2026-07-02T14:16:30.474");
        let next_ms = stamp("2026-07-02T14:16:30.475");
        let next_day = stamp("2026-07-03T00:00:00.000");
        assert!(first < next_ms && next_ms < next_day);
        let with_offset = stamp("2025-11-19T10:20:45.341-05:00");
        let later_with_offset = stamp("2025-11-19T10:20:45.721-05:00");
        assert!(with_offset < later_with_offset);
    }
}
