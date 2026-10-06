//! The characteristics fakering uses, as `const` data.

use junk_core::{Channel, CharDecl, Dir, GattMap, ServiceDecl, Uuid};

/// The one service.
pub const SERVICE: Uuid = Uuid::from_u128(0x0000_f00d_0000_1000_8000_0080_5f9b_34fb);

/// Host writes commands here. Required.
pub const CMD: Channel = Channel(0);
/// Device notifies replies and unsolicited data here. Required.
pub const EVT: Channel = Channel(1);
/// Declared but never used; optional, so a connection without it is still fine.
pub const EXTRA: Channel = Channel(2);

/// Everything fakering needs from GATT.
pub const GATT: GattMap = GattMap {
    services: &[ServiceDecl {
        uuid: SERVICE,
        required: true,
        chars: &[
            CharDecl {
                id: CMD,
                uuid: Uuid::from_u128(0x0000_f00e_0000_1000_8000_0080_5f9b_34fb),
                dir: Dir::WriteNoResponse,
                required: true,
            },
            CharDecl {
                id: EVT,
                uuid: Uuid::from_u128(0x0000_f00f_0000_1000_8000_0080_5f9b_34fb),
                dir: Dir::Notify,
                required: true,
            },
            CharDecl {
                id: EXTRA,
                uuid: Uuid::from_u128(0x0000_f010_0000_1000_8000_0080_5f9b_34fb),
                dir: Dir::Indicate,
                required: false,
            },
        ],
    }],
};

const _: () = assert!(GATT.check().is_ok(), "GATT has a duplicate channel id");

#[cfg(test)]
mod tests {
    use super::*;
    use junk_core::ChannelSet;

    #[test]
    fn required_is_cmd_and_evt() {
        let required: ChannelSet = [CMD, EVT].into_iter().collect();
        assert_eq!(GATT.required(), required);
        assert_eq!(GATT.chars().count(), 3);
        assert_eq!(GATT.find(EXTRA).map(|c| c.required), Some(false));
    }
}
