//! How opening and connecting an RFCOMM link fail.

use std::fmt;

use junk_core::{LinkError, Uuid};

/// Why [`RfcommLink::open`](crate::RfcommLink::open) or a connection failed.
///
/// [`RfcommLink::open`](crate::RfcommLink::open) only ever reports
/// [`BadAddress`](RfcommError::BadAddress), or [`Unsupported`](RfcommError::Unsupported)
/// off macOS: it parses the address and nothing else. Everything that needs the radio —
/// the device lookup, the SDP query, opening the channel — happens inside
/// [`Link::connect`](junk_core::Link::connect), which reports these as
/// [`LinkError::Io`] through the [`From`] impl below, since that is the only way a
/// [`Link`](junk_core::Link) has of describing a transport's own troubles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RfcommError {
    /// The address is not six separated hex octets, as in `F4:2B:7D:A1:B2:C3`.
    BadAddress {
        /// What was given.
        got: String,
    },
    /// The Mac is not paired with a device at this address. Bluetooth Classic has no
    /// scanning-then-connecting the way BLE does: the device has to be paired first, in
    /// System Settings.
    NotPaired {
        /// The address, normalised.
        address: String,
    },
    /// The device is there but its SDP records offer no service with this UUID, so there
    /// is no RFCOMM channel number to open.
    NoService {
        /// The service UUID that was looked for: the enclosing
        /// [`ServiceDecl`](junk_core::ServiceDecl)'s.
        uuid: Uuid,
    },
    /// `IOBluetooth` failed, for a reason of its own, described for humans.
    Io {
        /// What went wrong.
        why: String,
    },
    /// This is not macOS, and this crate speaks RFCOMM through Apple's `IOBluetooth` only.
    Unsupported,
}

impl fmt::Display for RfcommError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RfcommError::BadAddress { got } => {
                write!(
                    f,
                    "{got:?} is not a Bluetooth address like F4:2B:7D:A1:B2:C3"
                )
            }
            RfcommError::NotPaired { address } => {
                write!(
                    f,
                    "no device paired at {address}; pair it in System Settings"
                )
            }
            RfcommError::NoService { uuid } => {
                write!(f, "the device offers no service {uuid}")
            }
            RfcommError::Io { why } => write!(f, "bluetooth error: {why}"),
            RfcommError::Unsupported => {
                f.write_str("Bluetooth Classic RFCOMM is only supported on macOS here")
            }
        }
    }
}

impl std::error::Error for RfcommError {}

impl From<RfcommError> for LinkError {
    fn from(err: RfcommError) -> LinkError {
        LinkError::Io(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const SERVICE: Uuid = Uuid::from_u128(0x0cf1_2d31_fac3_4553_bd80_d683_2e7b_3135);

    fn all() -> [RfcommError; 5] {
        [
            RfcommError::BadAddress {
                got: "nonsense".to_owned(),
            },
            RfcommError::NotPaired {
                address: "F4:2B:7D:A1:B2:C3".to_owned(),
            },
            RfcommError::NoService { uuid: SERVICE },
            RfcommError::Io {
                why: "kIOReturnNotOpen".to_owned(),
            },
            RfcommError::Unsupported,
        ]
    }

    #[test]
    fn display_is_non_empty_and_distinct() {
        let strings: BTreeSet<String> = all().iter().map(ToString::to_string).collect();
        assert_eq!(strings.len(), all().len());
        assert!(strings.iter().all(|s| !s.is_empty()));
    }

    #[test]
    fn each_message_says_what_went_wrong() {
        let [bad_address, not_paired, no_service, io, unsupported] = all();
        assert_eq!(
            bad_address.to_string(),
            "\"nonsense\" is not a Bluetooth address like F4:2B:7D:A1:B2:C3"
        );
        assert_eq!(
            not_paired.to_string(),
            "no device paired at F4:2B:7D:A1:B2:C3; pair it in System Settings"
        );
        assert_eq!(
            no_service.to_string(),
            "the device offers no service 0cf12d31-fac3-4553-bd80-d6832e7b3135"
        );
        assert_eq!(io.to_string(), "bluetooth error: kIOReturnNotOpen");
        assert_eq!(
            unsupported.to_string(),
            "Bluetooth Classic RFCOMM is only supported on macOS here"
        );
    }

    /// The one thing a [`Link`](junk_core::Link) can say about a transport's own trouble.
    #[test]
    fn every_error_reaches_the_pump_as_link_io() {
        for err in all() {
            assert_eq!(LinkError::from(err.clone()), LinkError::Io(err.to_string()));
        }
        assert!(std::error::Error::source(&RfcommError::Unsupported).is_none());
    }
}
