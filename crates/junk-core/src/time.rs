//! Time as the driver sees it: a millisecond clock that only ever arrives as an input.

use core::ops::{Add, Sub};

pub use core::time::Duration;

/// A point in time, in whole milliseconds since an origin the pump chooses.
///
/// The origin is arbitrary (pump start, device boot, anything monotonic); only differences
/// between `Instant`s mean anything, and only within one pump. The clock reaches the driver
/// solely through [`Input::Tick`](crate::Input::Tick) and
/// [`Input::Request`](crate::Input::Request), never by being read.
///
/// A [`Duration`] applied to an `Instant` is truncated to whole milliseconds first: the
/// sub-millisecond part is dropped, so `Instant(0) + 1999µs == Instant(1)`.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Default, PartialOrd, Ord)]
pub struct Instant(pub u64);

impl Instant {
    /// The origin itself.
    pub const ZERO: Instant = Instant(0);

    /// Whole milliseconds in `d`, or `None` if they do not fit a `u64`.
    fn millis(d: Duration) -> Option<u64> {
        u64::try_from(d.as_millis()).ok()
    }

    /// `self + d`, or `None` if the result does not fit.
    #[must_use]
    pub fn checked_add(self, d: Duration) -> Option<Instant> {
        self.0.checked_add(Self::millis(d)?).map(Instant)
    }

    /// `self + d`, clamped to the largest representable `Instant`.
    #[must_use]
    pub fn saturating_add(self, d: Duration) -> Instant {
        Instant(self.0.saturating_add(Self::millis(d).unwrap_or(u64::MAX)))
    }

    /// Time elapsed from `earlier` to `self`, or zero if `earlier` is later.
    #[must_use]
    pub fn duration_since(self, earlier: Instant) -> Duration {
        Duration::from_millis(self.0.saturating_sub(earlier.0))
    }
}

/// Same as [`Instant::saturating_add`]: never panics on overflow.
impl Add<Duration> for Instant {
    type Output = Instant;

    fn add(self, d: Duration) -> Instant {
        self.saturating_add(d)
    }
}

/// Same as [`Instant::duration_since`]: zero rather than negative.
impl Sub<Instant> for Instant {
    type Output = Duration;

    fn sub(self, earlier: Instant) -> Duration {
        self.duration_since(earlier)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_duration() {
        assert_eq!(Instant(5) + Duration::from_millis(10), Instant(15));
        assert_eq!(Instant::ZERO + Duration::from_secs(2), Instant(2000));
        assert_eq!(
            Instant(5).checked_add(Duration::from_millis(10)),
            Some(Instant(15))
        );
        assert_eq!(
            Instant(5).saturating_add(Duration::from_millis(10)),
            Instant(15)
        );
    }

    #[test]
    fn add_saturates() {
        assert_eq!(
            Instant(u64::MAX).checked_add(Duration::from_millis(1)),
            None
        );
        assert_eq!(Instant(1).checked_add(Duration::MAX), None);
        assert_eq!(
            Instant(u64::MAX).saturating_add(Duration::from_millis(1)),
            Instant(u64::MAX)
        );
        assert_eq!(Instant(1).saturating_add(Duration::MAX), Instant(u64::MAX));
        assert_eq!(
            Instant(u64::MAX) + Duration::from_millis(1),
            Instant(u64::MAX)
        );
    }

    #[test]
    fn sub_saturates_at_zero() {
        assert_eq!(Instant(10) - Instant(3), Duration::from_millis(7));
        assert_eq!(Instant(3) - Instant(10), Duration::ZERO);
        assert_eq!(
            Instant(10).duration_since(Instant(3)),
            Duration::from_millis(7)
        );
        assert_eq!(Instant(3).duration_since(Instant(10)), Duration::ZERO);
        assert_eq!(Instant(3).duration_since(Instant(3)), Duration::ZERO);
    }

    #[test]
    fn sub_millisecond_parts_are_truncated() {
        assert_eq!(Instant(5) + Duration::from_micros(1999), Instant(6));
        assert_eq!(Instant(5) + Duration::from_nanos(999_999), Instant(5));
        assert_eq!(
            Instant(5).checked_add(Duration::from_micros(2001)),
            Some(Instant(7))
        );
        assert_eq!(
            Instant(5).saturating_add(Duration::from_micros(999)),
            Instant(5)
        );
    }

    #[test]
    fn ordering_and_default() {
        assert_eq!(Instant::default(), Instant::ZERO);
        assert!(Instant(1) < Instant(2));
        assert!(Instant::ZERO <= Instant(0));
    }
}
