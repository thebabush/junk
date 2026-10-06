//! What the link and the scan are told.

use std::time::Duration;

/// The default [`BleConfig::assumed_mtu`]: the BLE minimum, which every device supports.
pub const DEFAULT_ASSUMED_MTU: u16 = 23;

/// The default [`BleConfig::scan_timeout`].
pub const DEFAULT_SCAN_TIMEOUT: Duration = Duration::from_secs(10);

/// How [`BleLink`](crate::BleLink) and [`scan`](crate::scan) behave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BleConfig {
    /// The MTU `connect` reports when btleplug has not learned the negotiated one and still
    /// reports the BLE default of 23. The default is that same 23; a CLI on macOS may say
    /// 247, which the Macs and rings at hand negotiate.
    pub assumed_mtu: u16,
    /// How long [`scan`](crate::scan) listens before reporting what it saw, and how long a
    /// reconnecting link waits for its peripheral to be seen again. Default 10 s.
    pub scan_timeout: Duration,
}

impl Default for BleConfig {
    fn default() -> Self {
        BleConfig {
            assumed_mtu: DEFAULT_ASSUMED_MTU,
            scan_timeout: DEFAULT_SCAN_TIMEOUT,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_documented_ones() {
        let config = BleConfig::default();
        assert_eq!(config.assumed_mtu, 23);
        assert_eq!(config.scan_timeout, Duration::from_secs(10));
    }
}
