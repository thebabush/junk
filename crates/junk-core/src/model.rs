//! The generic primitives every device family shares: timestamps, percentages and battery state.

/// A wall-clock minute in the device's local zone, with the offset that zone had.
///
/// `local_minute` is minutes since 1970-01-01T00:00 *in that zone*; `utc_offset_min` is what
/// the zone was offset from UTC at that moment. Devices do not know time zones; the app that
/// set their clock does, and hands the offset to the driver in every request that needs it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct Timestamp {
    /// Minutes since 1970-01-01T00:00 in the local zone.
    pub local_minute: i64,
    /// Offset of the local zone from UTC, in minutes, east positive.
    pub utc_offset_min: i16,
}

impl Timestamp {
    /// The same moment as minutes since 1970-01-01T00:00 UTC.
    #[must_use]
    pub fn to_utc_minute(&self) -> i64 {
        self.local_minute - i64::from(self.utc_offset_min)
    }

    /// The moment `utc` minutes after 1970-01-01T00:00 UTC, seen from a zone at `utc_offset_min`.
    #[must_use]
    pub fn from_utc_minute(utc: i64, utc_offset_min: i16) -> Timestamp {
        Timestamp {
            local_minute: utc + i64::from(utc_offset_min),
            utc_offset_min,
        }
    }
}

/// A percentage that is known to be in `0..=100`.
///
/// Decoders build one with [`Percent::new`] and must decide what to do with a byte outside
/// the range instead of passing it through (invariant 5: garbage becomes an error or an
/// event, never a bogus value).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
pub struct Percent(u8);

impl Percent {
    /// Zero percent.
    pub const ZERO: Percent = Percent(0);
    /// One hundred percent.
    pub const FULL: Percent = Percent(100);

    /// `value` as a percentage, or `None` if it is above 100.
    #[must_use]
    pub const fn new(value: u8) -> Option<Percent> {
        if value <= 100 {
            Some(Percent(value))
        } else {
            None
        }
    }

    /// The value, `0..=100`.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl core::fmt::Display for Percent {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}%", self.0)
    }
}

/// Battery state.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct Battery {
    /// Charge.
    pub percent: Percent,
    /// Whether it is charging right now.
    pub charging: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_rejects_values_above_100() {
        assert_eq!(Percent::new(0), Some(Percent::ZERO));
        assert_eq!(Percent::new(100), Some(Percent::FULL));
        assert_eq!(Percent::new(101), None);
        assert_eq!(Percent::new(255), None);
        assert_eq!(Percent::new(64).map(Percent::get), Some(64));
        assert!(Percent::new(3) < Percent::new(4));
        assert_eq!(alloc::format!("{}", Percent::FULL), "100%");
    }

    #[test]
    fn utc_round_trip_with_negative_offset() {
        // 2026-07-02 12:00 in UTC-4 is 16:00 UTC.
        let utc = 29_715_840;
        let ts = Timestamp::from_utc_minute(utc, -240);
        assert_eq!(ts.utc_offset_min, -240);
        assert_eq!(ts.local_minute, utc - 240);
        assert_eq!(ts.to_utc_minute(), utc);

        let local = Timestamp {
            local_minute: 1_000,
            utc_offset_min: -240,
        };
        assert_eq!(local.to_utc_minute(), 1_240);
        assert_eq!(
            Timestamp::from_utc_minute(local.to_utc_minute(), -240),
            local
        );
    }

    #[test]
    fn utc_round_trip_with_positive_and_zero_offset() {
        let east = Timestamp::from_utc_minute(500, 90);
        assert_eq!(east.local_minute, 590);
        assert_eq!(east.to_utc_minute(), 500);

        let zero = Timestamp::from_utc_minute(500, 0);
        assert_eq!(zero.local_minute, 500);
        assert_eq!(zero.to_utc_minute(), 500);
    }
}
