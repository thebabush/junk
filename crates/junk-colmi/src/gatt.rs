//! The characteristics Colmi rings use, as `const` data, and their names in traces.

use junk_core::{Channel, CharDecl, Dir, GattMap, ServiceDecl, Uuid};

/// The V1 service: NUS-like, 16-byte command frames both ways. Every ring has it.
pub const SERVICE_V1: Uuid = Uuid::from_u128(0x6e40_fff0_b5a3_f393_e0a9_e50e_24dc_ca9e);
/// The V2 service: `0xbc` big-data frames. Not every ring has it; a dialect without it
/// still syncs what the V1 commands cover.
pub const SERVICE_V2: Uuid = Uuid::from_u128(0xde5b_f728_d711_4e47_af26_65e3_012a_5dc7);
/// The standard Device Information service (`0x180a`): the firmware and hardware revision
/// strings, read rather than notified. A ring without it cannot be asked its version.
pub const SERVICE_DIS: Uuid = Uuid::from_u128(0x0000_180a_0000_1000_8000_0080_5f9b_34fb);

/// Phone → ring, 16-byte frames. Required.
pub const V1_WRITE: Channel = Channel(0);
/// Ring → phone, 16-byte frames. Required.
pub const V1_NOTIFY: Channel = Channel(1);
/// Phone → ring, `0xbc` requests. Optional.
pub const V2_CMD: Channel = Channel(2);
/// Ring → phone, `0xbc` responses in MTU-sized pieces. Optional.
pub const V2_NOTIFY: Channel = Channel(3);
/// Firmware Revision (`0x2a26`), read: `RT03CR_1.00.02_260319` on the R10. Optional.
pub const DIS_FW: Channel = Channel(4);
/// Hardware Revision (`0x2a27`), read: `RT03CR_V1.0` on the R10. Optional.
pub const DIS_HW: Channel = Channel(5);

/// Everything a Colmi ring needs from GATT.
pub const GATT: GattMap = GattMap {
    services: &[
        ServiceDecl {
            uuid: SERVICE_V1,
            required: true,
            chars: &[
                CharDecl {
                    id: V1_WRITE,
                    uuid: Uuid::from_u128(0x6e40_0002_b5a3_f393_e0a9_e50e_24dc_ca9e),
                    dir: Dir::Write,
                    required: true,
                },
                CharDecl {
                    id: V1_NOTIFY,
                    uuid: Uuid::from_u128(0x6e40_0003_b5a3_f393_e0a9_e50e_24dc_ca9e),
                    dir: Dir::Notify,
                    required: true,
                },
            ],
        },
        ServiceDecl {
            uuid: SERVICE_V2,
            required: false,
            chars: &[
                CharDecl {
                    id: V2_CMD,
                    uuid: Uuid::from_u128(0xde5b_f72a_d711_4e47_af26_65e3_012a_5dc7),
                    dir: Dir::WriteNoResponse,
                    required: false,
                },
                CharDecl {
                    id: V2_NOTIFY,
                    uuid: Uuid::from_u128(0xde5b_f729_d711_4e47_af26_65e3_012a_5dc7),
                    dir: Dir::Notify,
                    required: false,
                },
            ],
        },
        ServiceDecl {
            uuid: SERVICE_DIS,
            required: false,
            chars: &[
                CharDecl {
                    id: DIS_FW,
                    uuid: Uuid::from_u128(0x0000_2a26_0000_1000_8000_0080_5f9b_34fb),
                    dir: Dir::Read,
                    required: false,
                },
                CharDecl {
                    id: DIS_HW,
                    uuid: Uuid::from_u128(0x0000_2a27_0000_1000_8000_0080_5f9b_34fb),
                    dir: Dir::Read,
                    required: false,
                },
            ],
        },
    ],
};

const _: () = assert!(GATT.check().is_ok(), "GATT has a duplicate channel id");

/// The name a trace under `fixtures/` uses for `chan`: `v1.write`, `v1.notify`, `v2.cmd`,
/// `v2.notify`, `dis.fw` or `dis.hw`. `None` for a channel this family does not declare.
#[must_use]
pub fn channel_name(chan: Channel) -> Option<&'static str> {
    match chan {
        V1_WRITE => Some("v1.write"),
        V1_NOTIFY => Some("v1.notify"),
        V2_CMD => Some("v2.cmd"),
        V2_NOTIFY => Some("v2.notify"),
        DIS_FW => Some("dis.fw"),
        DIS_HW => Some("dis.hw"),
        Channel(_) => None,
    }
}

/// The channel a trace calls `name`; the inverse of [`channel_name`].
#[must_use]
pub fn channel_by_name(name: &str) -> Option<Channel> {
    match name {
        "v1.write" => Some(V1_WRITE),
        "v1.notify" => Some(V1_NOTIFY),
        "v2.cmd" => Some(V2_CMD),
        "v2.notify" => Some(V2_NOTIFY),
        "dis.fw" => Some(DIS_FW),
        "dis.hw" => Some(DIS_HW),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use junk_core::ChannelSet;

    #[test]
    fn required_is_the_v1_pair() {
        let required: ChannelSet = [V1_WRITE, V1_NOTIFY].into_iter().collect();
        assert_eq!(GATT.required(), required);
        assert_eq!(GATT.chars().count(), 6);
        assert_eq!(GATT.find(V2_CMD).map(|c| c.required), Some(false));
        assert_eq!(GATT.find(V2_NOTIFY).map(|c| c.required), Some(false));
        assert_eq!(GATT.find(DIS_FW).map(|c| c.required), Some(false));
        assert_eq!(GATT.find(DIS_HW).map(|c| c.required), Some(false));
        assert!(
            GATT.services
                .iter()
                .all(|s| s.required == (s.uuid == SERVICE_V1))
        );
    }

    #[test]
    fn directions_match_the_docs_table() {
        assert_eq!(GATT.find(V1_WRITE).map(|c| c.dir), Some(Dir::Write));
        assert_eq!(GATT.find(V1_NOTIFY).map(|c| c.dir), Some(Dir::Notify));
        assert_eq!(GATT.find(V2_CMD).map(|c| c.dir), Some(Dir::WriteNoResponse));
        assert_eq!(GATT.find(V2_NOTIFY).map(|c| c.dir), Some(Dir::Notify));
        assert_eq!(GATT.find(DIS_FW).map(|c| c.dir), Some(Dir::Read));
        assert_eq!(GATT.find(DIS_HW).map(|c| c.dir), Some(Dir::Read));
    }

    #[test]
    fn names_round_trip_and_reject_strangers() {
        for chan in GATT.chars().map(|c| c.id) {
            let name = channel_name(chan).unwrap_or_else(|| panic!("{chan:?} has no name"));
            assert_eq!(channel_by_name(name), Some(chan));
        }
        assert_eq!(channel_name(DIS_FW), Some("dis.fw"));
        assert_eq!(channel_name(DIS_HW), Some("dis.hw"));
        assert_eq!(channel_name(Channel(6)), None);
        assert_eq!(channel_name(Channel(255)), None);
        assert_eq!(channel_by_name("v1.WRITE"), None);
        assert_eq!(channel_by_name(""), None);
    }
}
