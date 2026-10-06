//! [`RfcommLink`] where there is no `IOBluetooth`: the same API, saying so.
//!
//! The crate builds everywhere so that `cargo build --workspace` on Linux is unaffected by
//! a transport only macOS has. What survives is the shape — the type, its methods and its
//! errors — with [`RfcommError::Unsupported`] where the framework would have been.

use junk_core::{Bytes, Channel, ChannelSet, GattMap, Link, LinkError, LinkEvent};

use crate::{RfcommError, address, resolve};

/// A [`Link`] over one Bluetooth Classic RFCOMM channel — on macOS. Here, nothing.
///
/// [`RfcommLink::open`] rejects a malformed address as it would anywhere, and then reports
/// [`RfcommError::Unsupported`]: there is no other Bluetooth Classic backend behind this
/// crate, so a link is never built and the [`Link`] methods below are unreachable.
pub struct RfcommLink {
    address: String,
}

impl RfcommLink {
    /// A link to the device with this address, e.g. `"F4:2B:7D:A1:B2:C3"` — on macOS.
    ///
    /// # Errors
    ///
    /// [`RfcommError::BadAddress`] if `address` is not six separated hex octets, and
    /// [`RfcommError::Unsupported`] otherwise, this not being macOS.
    pub fn open(address: &str) -> Result<RfcommLink, RfcommError> {
        let _ = address::normalise(address)?;
        Err(RfcommError::Unsupported)
    }

    /// The device's address, normalised to `XX:XX:XX:XX:XX:XX`.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// The device's name as the machine knows it: never anything here.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        None
    }
}

impl Link for RfcommLink {
    async fn connect(&mut self, gatt: &GattMap) -> Result<(ChannelSet, u16), LinkError> {
        // The map is still checked, so a family's map is wrong in the same words anywhere.
        resolve(gatt)?;
        Err(RfcommError::Unsupported.into())
    }

    async fn write(&mut self, _chan: Channel, _bytes: &[u8]) -> Result<(), LinkError> {
        Err(LinkError::NotConnected)
    }

    async fn subscribe(&mut self, _chan: Channel) -> Result<(), LinkError> {
        Err(LinkError::NotConnected)
    }

    async fn read(&mut self, chan: Channel) -> Result<Bytes, LinkError> {
        Err(LinkError::UnknownChannel(chan))
    }

    async fn disconnect(&mut self) {}

    async fn next(&mut self) -> LinkEvent {
        LinkEvent::Disconnected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_good_address_is_still_unsupported_and_a_bad_one_is_still_bad() {
        assert_eq!(
            RfcommLink::open("F4:2B:7D:A1:B2:C3").err(),
            Some(RfcommError::Unsupported)
        );
        assert_eq!(
            RfcommLink::open("nonsense").err(),
            Some(RfcommError::BadAddress {
                got: "nonsense".to_owned()
            })
        );
    }
}
