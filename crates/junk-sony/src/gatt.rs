//! The one byte stream a Sony headphone exposes, as `const` data, and its name in traces.
//!
//! Bluetooth Classic RFCOMM is not GATT: there is one bidirectional byte stream and no
//! service structure worth the name. A family that speaks over one declares a single
//! [`Dir::Stream`] channel, and its [`CharDecl`] repeats the service's UUID because a
//! stream has no characteristic of its own — `junk-rfcomm`'s `resolve` opens the enclosing
//! [`ServiceDecl`]'s UUID and ignores the characteristic's. See [`Dir::Stream`]'s own docs.

use junk_core::{Channel, CharDecl, Dir, GattMap, ServiceDecl, Uuid};

/// The v1 SDP service UUID, `96CC203E-5068-46AD-B32D-E316F5E069BA`, from Sony's own app. The channel is found through SDP; none is hard-coded.
///
/// A second UUID, [`SERVICE_V2`], exists. Sony's app lists `[v2, v1]` and connects with the
/// first one the device advertises, and it picks its command table by that UUID rather than
/// by anything in the init reply: v1 selects the v1 tables, anything else the v2 ones. The
/// WH-1000XM4 is expected, not verified, to be a v1-UUID device: the community's four-byte
/// init reply and working v1 command ids point that way, but the app code cannot confirm it.
/// This map declares v1 only, because `junk-rfcomm`'s `resolve` allows exactly one
/// [`Dir::Stream`] channel.
pub const SERVICE: Uuid = Uuid::from_u128(0x96cc_203e_5068_46ad_b32d_e316_f5e0_69ba);

/// The v2 SDP service UUID, `956C7B26-D49A-4BA8-B03F-B17D393CB6E2`. Not in [`GATT`]; see
/// [`SERVICE`] for why it is recorded at all.
pub const SERVICE_V2: Uuid = Uuid::from_u128(0x956c_7b26_d49a_4ba8_b03f_b17d_393c_b6e2);

/// The RFCOMM byte stream, both ways. Required: it is the whole protocol.
pub const RFCOMM: Channel = Channel(0);

/// Everything a Sony headphone needs from its transport.
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

    /// The text forms in Sony's app and the `const`s the map uses are the same UUIDs, and
    /// the map declares v1 only.
    #[test]
    fn the_uuids_match_their_text_forms() {
        assert_eq!(
            Uuid::parse_str("96CC203E-5068-46AD-B32D-E316F5E069BA").ok(),
            Some(SERVICE)
        );
        assert_eq!(
            Uuid::parse_str("956C7B26-D49A-4BA8-B03F-B17D393CB6E2").ok(),
            Some(SERVICE_V2)
        );
        assert_ne!(SERVICE, SERVICE_V2);
        assert!(GATT.services.iter().all(|s| s.uuid != SERVICE_V2));
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
