//! How finding an adapter, scanning and opening a link fail.

use std::fmt;
use std::fmt::Write as _;

use btleplug::platform::PeripheralId;

use crate::scan::{Found, Radio, describe};

/// Why [`adapter`](crate::adapter), [`scan`](crate::scan) or
/// [`BleLink::open`](crate::BleLink::open) failed. Once a link is open it fails as a
/// [`Link`](junk_core::Link) does, with [`LinkError`](junk_core::LinkError).
#[derive(Debug)]
pub enum BleError {
    /// The machine has no Bluetooth adapter.
    NoAdapter,
    /// The adapter is there but its radio cannot be used: switched off, or not available
    /// at all (an iOS simulator has an adapter and no radio behind it).
    RadioOff {
        /// What the adapter reported.
        radio: Radio,
    },
    /// The adapter knows no peripheral with this id: it was not seen by a scan.
    NotFound(PeripheralId),
    /// [`choose`](crate::choose) found no ring: none of those scanned matched `wanted`,
    /// or, with nothing asked for, the scan saw none at all.
    NoRing {
        /// The name substring or id that was asked for, if one was.
        wanted: Option<String>,
    },
    /// [`choose`](crate::choose) found more than one, so which is meant has to be said.
    SeveralRings {
        /// The ones that matched, in the order the scan saw them.
        candidates: Vec<Found>,
    },
    /// btleplug failed, for a reason of its own.
    Btleplug(btleplug::Error),
}

impl fmt::Display for BleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BleError::NoAdapter => f.write_str("no Bluetooth adapter"),
            BleError::RadioOff { radio: Radio::Off } => {
                f.write_str("Bluetooth is switched off; turn it on to reach the ring")
            }
            BleError::RadioOff {
                radio: Radio::On | Radio::Unknown,
            } => f.write_str("Bluetooth is not available on this device"),
            BleError::NotFound(id) => write!(f, "peripheral {id} not found; scan first"),
            BleError::NoRing { wanted: Some(name) } => write!(f, "no ring matches {name:?}"),
            BleError::NoRing { wanted: None } => {
                f.write_str("no ring found; is it awake and nearby?")
            }
            BleError::SeveralRings { candidates } => {
                let mut list = String::new();
                for peripheral in candidates {
                    let _ = write!(list, "\n  {}", describe(peripheral));
                }
                write!(f, "several rings found:{list}")
            }
            BleError::Btleplug(err) => write!(f, "bluetooth error: {err}"),
        }
    }
}

impl std::error::Error for BleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BleError::NoAdapter
            | BleError::NotFound(_)
            | BleError::RadioOff { .. }
            | BleError::NoRing { .. }
            | BleError::SeveralRings { .. } => None,
            BleError::Btleplug(err) => Some(err),
        }
    }
}

impl From<btleplug::Error> for BleError {
    fn from(err: btleplug::Error) -> Self {
        BleError::Btleplug(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_source() {
        let err = BleError::from(btleplug::Error::NotConnected);
        assert_eq!(err.to_string(), "bluetooth error: Not connected");
        assert!(std::error::Error::source(&err).is_some());
        assert_eq!(BleError::NoAdapter.to_string(), "no Bluetooth adapter");
        assert!(std::error::Error::source(&BleError::NoAdapter).is_none());
    }

    /// The words both shells show when [`choose`](crate::choose) cannot pick one ring.
    /// The list of candidates is not here: a [`Found`] needs a `PeripheralId`, which only
    /// a platform's own backend can make.
    #[test]
    fn a_ring_that_cannot_be_picked_says_which_way_it_failed() {
        assert_eq!(
            BleError::NoRing {
                wanted: Some("F300".to_owned())
            }
            .to_string(),
            "no ring matches \"F300\""
        );
        assert_eq!(
            BleError::NoRing { wanted: None }.to_string(),
            "no ring found; is it awake and nearby?"
        );
        assert_eq!(
            BleError::SeveralRings {
                candidates: Vec::new()
            }
            .to_string(),
            "several rings found:"
        );
        assert!(
            std::error::Error::source(&BleError::NoRing { wanted: None }).is_none(),
            "the message is the whole story"
        );
    }
}
