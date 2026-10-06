//! The one byte stream a Soundcore speaker exposes, as `const` data, and its name in
//! traces.
//!
//! Bluetooth Classic RFCOMM is not GATT: there is one bidirectional byte stream and no
//! service structure worth the name. A family that speaks over one declares a single
//! [`Dir::Stream`] channel, and its [`CharDecl`] repeats the service's UUID because a
//! stream has no characteristic of its own — `junk-rfcomm`'s `resolve` opens the enclosing
//! [`ServiceDecl`]'s UUID and ignores the characteristic's. See [`Dir::Stream`]'s own docs.

use junk_core::{Channel, CharDecl, Dir, GattMap, ServiceDecl, Uuid};

/// The vendor control service, from Gadgetbridge's `SoundcoreMotion300DeviceSupport` and
/// confirmed against the speaker on 2026-09-15: RFCOMM channel 11, MTU 668.
pub const SERVICE: Uuid = Uuid::from_u128(0x0cf1_2d31_fac3_4553_bd80_d683_2e7b_3135);

/// The RFCOMM byte stream, both ways. Required: it is the whole protocol.
pub const RFCOMM: Channel = Channel(0);

/// Everything a Soundcore speaker needs from its transport.
pub const GATT: GattMap = GattMap {
    services: &[ServiceDecl {
        uuid: SERVICE,
        required: true,
        chars: &[CharDecl {
            id: RFCOMM,
            // The service's own UUID: a stream has no characteristic, and RFCOMM opens the
            // service.
            uuid: SERVICE,
            dir: Dir::Stream,
            required: true,
        }],
    }],
};

const _: () = assert!(GATT.check().is_ok(), "GATT has a duplicate channel id");

/// The name a trace under `fixtures/` uses for `chan`: `rfcomm`. `None` for a channel this
/// family does not declare.
#[must_use]
pub fn channel_name(chan: Channel) -> Option<&'static str> {
    match chan {
        RFCOMM => Some("rfcomm"),
        Channel(_) => None,
    }
}

/// The channel a trace calls `name`; the inverse of [`channel_name`].
#[must_use]
pub fn channel_by_name(name: &str) -> Option<Channel> {
    match name {
        "rfcomm" => Some(RFCOMM),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::SERVICE_MOTION_300;
    use junk_core::ChannelSet;

    #[test]
    fn the_one_stream_is_required() {
        let required: ChannelSet = [RFCOMM].into_iter().collect();
        assert_eq!(GATT.required(), required);
        assert_eq!(GATT.chars().count(), 1);
        assert_eq!(GATT.find(RFCOMM).map(|c| c.dir), Some(Dir::Stream));
        assert!(GATT.services.iter().all(|s| s.required));
    }

    /// A stream has no characteristic of its own, so the two UUIDs are deliberately equal;
    /// `junk-rfcomm` opens the service's.
    #[test]
    fn the_stream_repeats_its_services_uuid() {
        assert_eq!(GATT.find(RFCOMM).map(|c| c.uuid), Some(SERVICE));
        assert_eq!(GATT.services[0].uuid, SERVICE);
    }

    /// The text form the SDP notes and the probe use, and the `const` the map uses, are the
    /// same UUID.
    #[test]
    fn the_uuid_matches_the_text_form() {
        assert_eq!(Uuid::parse_str(SERVICE_MOTION_300).ok(), Some(SERVICE));
    }

    #[test]
    fn names_round_trip_and_reject_strangers() {
        for chan in GATT.chars().map(|c| c.id) {
            let name = channel_name(chan).unwrap_or_else(|| panic!("{chan:?} has no name"));
            assert_eq!(channel_by_name(name), Some(chan));
        }
        assert_eq!(channel_name(RFCOMM), Some("rfcomm"));
        assert_eq!(channel_name(Channel(1)), None);
        assert_eq!(channel_name(Channel(255)), None);
        assert_eq!(channel_by_name("RFCOMM"), None);
        assert_eq!(channel_by_name(""), None);
    }
}
