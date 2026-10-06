//! The transport boundary: [`Link`], what it reports and how it fails.

use alloc::string::String;
use core::fmt;

use crate::{Bytes, Channel, ChannelSet, GattMap};

/// Something the transport reports to the pump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEvent {
    /// One notification or indication from the device, unmodified.
    Rx {
        /// The channel it arrived on.
        chan: Channel,
        /// Its payload.
        bytes: Bytes,
    },
    /// The connection is gone, for whatever reason.
    Disconnected,
}

/// Why a [`Link`] operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkError {
    /// The operation needs a connection and there is none.
    NotConnected,
    /// The device is there but lacks some required channels; the payload is the set of
    /// required channels that did not resolve.
    MissingRequired(ChannelSet),
    /// The channel is not in the [`GattMap`] the link was connected with.
    UnknownChannel(Channel),
    /// The transport failed for a reason of its own, described for humans.
    Io(String),
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinkError::NotConnected => f.write_str("link is not connected"),
            LinkError::MissingRequired(missing) => {
                write!(f, "device lacks required channels {missing:?}")
            }
            LinkError::UnknownChannel(Channel(id)) => {
                write!(f, "channel {id} is not declared in the GATT map")
            }
            LinkError::Io(what) => write!(f, "transport error: {what}"),
        }
    }
}

impl core::error::Error for LinkError {}

/// The transport. Deliberately stupid: bytes and channels only (invariant 2).
///
/// A `Link` resolves a [`GattMap`] against the real device, moves bytes in both directions
/// on the channels it resolved, and reports when the connection is gone. It never looks at
/// a payload. Only the pump holds one (invariant 3).
///
/// The pump may drop any operation future on shutdown or an I/O deadline. Cancellation
/// need not undo remote effects, but must leave the link safe to disconnect (including
/// after a partial connect) and subsequently reconnect. Disconnect itself may also be
/// cancelled. Backends must not rely on a future being polled to completion for safety;
/// a later connect must not inherit subscriptions or events from an abandoned session.
// `async fn` in a public trait is fine here: the pump is generic over its `Link`, so the
// missing `Send` bound does not matter and `dyn Link` is never needed.
#[allow(
    async_fn_in_trait,
    reason = "the pump is generic over the Link; dyn Link is not needed"
)]
pub trait Link {
    /// Connects and resolves `gatt`, returning the channels found and the ATT MTU.
    ///
    /// # Errors
    ///
    /// [`LinkError::MissingRequired`] if a required channel is absent, [`LinkError::Io`]
    /// if the transport fails.
    async fn connect(&mut self, gatt: &GattMap) -> Result<(ChannelSet, u16), LinkError>;

    /// Writes `bytes` to `chan`, with or without response as its [`Dir`](crate::Dir) says.
    ///
    /// # Errors
    ///
    /// [`LinkError::NotConnected`] without a connection, [`LinkError::UnknownChannel`] for
    /// a channel that did not resolve, [`LinkError::Io`] if the transport fails.
    async fn write(&mut self, chan: Channel, bytes: &[u8]) -> Result<(), LinkError>;

    /// Enables notifications or indications on `chan`; they arrive through [`Link::next`].
    ///
    /// # Errors
    ///
    /// [`LinkError::NotConnected`] without a connection, [`LinkError::UnknownChannel`] for
    /// a channel that did not resolve, [`LinkError::Io`] if the transport fails.
    async fn subscribe(&mut self, chan: Channel) -> Result<(), LinkError>;

    /// Reads the value of `chan` now, for a [`Dir::Read`](crate::Dir::Read) channel.
    ///
    /// # Errors
    ///
    /// [`LinkError::NotConnected`] without a connection, [`LinkError::UnknownChannel`] for
    /// a channel that did not resolve, [`LinkError::Io`] if the transport fails.
    async fn read(&mut self, chan: Channel) -> Result<Bytes, LinkError>;

    /// Drops the connection. A no-op when there is none.
    async fn disconnect(&mut self);

    /// Waits for the next thing the device or the transport has to say.
    ///
    /// Must be cancel-safe: the pump selects over this future together with timers and
    /// requests and drops it whenever another branch wins, then calls it again. Dropping the
    /// future must not lose an event. A channel receive is; a hand-rolled read loop with a
    /// partial buffer is not.
    async fn next(&mut self) -> LinkEvent;
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeSet;
    use alloc::string::ToString;

    #[test]
    fn display_is_non_empty_and_distinct() {
        let all = [
            LinkError::NotConnected,
            LinkError::MissingRequired([Channel(1)].into_iter().collect()),
            LinkError::UnknownChannel(Channel(9)),
            LinkError::Io("adapter went away".into()),
        ];
        let strings: BTreeSet<String> = all.iter().map(ToString::to_string).collect();
        assert_eq!(strings.len(), all.len());
        assert!(strings.iter().all(|s| !s.is_empty()));
        assert!(all[3].to_string().contains("adapter went away"));
    }
}
