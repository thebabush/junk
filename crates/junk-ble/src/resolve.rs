//! Matching a [`GattMap`] against the characteristics a peripheral has. Pure.

use btleplug::api::{CharPropFlags, Characteristic};
use junk_core::{Channel, ChannelSet, Dir, GattMap, LinkError, Uuid};

/// What matters, for resolution, about one characteristic a peripheral has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "five independent capabilities, read straight off the characteristic's property bits"
)]
pub struct CharInfo {
    /// The service it belongs to.
    pub service: Uuid,
    /// The characteristic UUID.
    pub uuid: Uuid,
    /// Whether it accepts writes with response.
    pub writable: bool,
    /// Whether it accepts writes without response.
    pub writable_without_response: bool,
    /// Whether it notifies.
    pub notify: bool,
    /// Whether it indicates.
    pub indicate: bool,
    /// Whether it can be read.
    pub readable: bool,
}

impl CharInfo {
    /// Whether the characteristic supports what a channel used as `dir` needs.
    #[must_use]
    pub const fn supports(&self, dir: Dir) -> bool {
        match dir {
            Dir::Write => self.writable,
            Dir::WriteNoResponse => self.writable_without_response,
            Dir::Notify => self.notify,
            Dir::Indicate => self.indicate,
            Dir::Read => self.readable,
            // A characteristic is never a stream endpoint: `Dir::Stream` is what a
            // serial-style transport declares, and nothing over GATT can serve it.
            Dir::Stream => false,
        }
    }
}

impl From<&Characteristic> for CharInfo {
    fn from(characteristic: &Characteristic) -> Self {
        let properties = characteristic.properties;
        CharInfo {
            service: characteristic.service_uuid,
            uuid: characteristic.uuid,
            writable: properties.contains(CharPropFlags::WRITE),
            writable_without_response: properties.contains(CharPropFlags::WRITE_WITHOUT_RESPONSE),
            notify: properties.contains(CharPropFlags::NOTIFY),
            indicate: properties.contains(CharPropFlags::INDICATE),
            readable: properties.contains(CharPropFlags::READ),
        }
    }
}

/// What [`resolve`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// The declared channels the peripheral has, usable as declared.
    pub resolved: ChannelSet,
    /// For each resolved channel, the index into the characteristics given of the one it
    /// resolved to; in map order.
    pub matched: Vec<(Channel, usize)>,
}

/// Resolves `gatt` against `chars`.
///
/// A declaration resolves to the first characteristic with its service and characteristic
/// UUIDs that also supports what its [`Dir`] needs: a characteristic that has the right UUID
/// but, say, does not notify does not resolve a notify channel. Optional channels may be
/// absent; every required one must resolve.
///
/// # Errors
///
/// [`LinkError::MissingRequired`] with the required channels that did not resolve.
pub fn resolve(gatt: &GattMap, chars: &[CharInfo]) -> Result<Resolution, LinkError> {
    let mut resolved = ChannelSet::EMPTY;
    let mut matched = Vec::new();
    for service in gatt.services {
        for decl in service.chars {
            let found = chars.iter().position(|c| {
                c.service == service.uuid && c.uuid == decl.uuid && c.supports(decl.dir)
            });
            if let Some(index) = found {
                resolved.insert(decl.id);
                matched.push((decl.id, index));
            }
        }
    }
    let missing = gatt.required().difference(&resolved);
    if missing.is_empty() {
        Ok(Resolution { resolved, matched })
    } else {
        Err(LinkError::MissingRequired(missing))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use junk_colmi::{DIS_FW, DIS_HW, GATT, SERVICE_V1, V1_NOTIFY, V1_WRITE, V2_CMD, V2_NOTIFY};
    use junk_core::{CharDecl, ServiceDecl};
    use std::collections::BTreeSet;

    /// The characteristic behind `chan`, supporting exactly what its declaration needs.
    fn as_declared(chan: Channel) -> CharInfo {
        let decl = GATT.find(chan).expect("declared");
        let service = GATT
            .services
            .iter()
            .find(|s| s.chars.contains(decl))
            .expect("in a service");
        CharInfo {
            service: service.uuid,
            uuid: decl.uuid,
            writable: decl.dir == Dir::Write,
            writable_without_response: decl.dir == Dir::WriteNoResponse,
            notify: decl.dir == Dir::Notify,
            indicate: decl.dir == Dir::Indicate,
            readable: decl.dir == Dir::Read,
        }
    }

    fn channels(chans: &[Channel]) -> ChannelSet {
        chans.iter().copied().collect()
    }

    #[test]
    fn all_six_present_resolve_all_six() {
        // Listed in the reverse of map order, so the indices mean something.
        let chars = [
            as_declared(DIS_HW),
            as_declared(DIS_FW),
            as_declared(V2_NOTIFY),
            as_declared(V2_CMD),
            as_declared(V1_NOTIFY),
            as_declared(V1_WRITE),
        ];
        let resolution = resolve(&GATT, &chars).expect("all present");
        assert_eq!(
            resolution.resolved,
            channels(&[V1_WRITE, V1_NOTIFY, V2_CMD, V2_NOTIFY, DIS_FW, DIS_HW])
        );
        assert_eq!(
            resolution.matched,
            [
                (V1_WRITE, 5),
                (V1_NOTIFY, 4),
                (V2_CMD, 3),
                (V2_NOTIFY, 2),
                (DIS_FW, 1),
                (DIS_HW, 0)
            ]
        );
    }

    #[test]
    fn v2_and_device_information_are_optional() {
        let chars = [as_declared(V1_WRITE), as_declared(V1_NOTIFY)];
        let resolution = resolve(&GATT, &chars).expect("V1 is enough");
        assert_eq!(resolution.resolved, channels(&[V1_WRITE, V1_NOTIFY]));
        assert_eq!(resolution.matched, [(V1_WRITE, 0), (V1_NOTIFY, 1)]);

        let chars = [
            as_declared(V1_WRITE),
            as_declared(V1_NOTIFY),
            as_declared(DIS_FW),
        ];
        let resolution = resolve(&GATT, &chars).expect("V1 is enough");
        assert_eq!(
            resolution.resolved,
            channels(&[V1_WRITE, V1_NOTIFY, DIS_FW])
        );
    }

    #[test]
    fn a_read_channel_needs_a_readable_characteristic() {
        let mut unreadable = as_declared(DIS_FW);
        unreadable.readable = false;
        unreadable.notify = true;
        let chars = [
            as_declared(V1_WRITE),
            as_declared(V1_NOTIFY),
            unreadable,
            as_declared(DIS_HW),
        ];
        let resolution = resolve(&GATT, &chars).expect("V1 is there");
        assert_eq!(
            resolution.resolved,
            channels(&[V1_WRITE, V1_NOTIFY, DIS_HW])
        );
        assert_eq!(
            resolution.matched,
            [(V1_WRITE, 0), (V1_NOTIFY, 1), (DIS_HW, 3)]
        );
    }

    #[test]
    fn a_missing_required_channel_is_an_error() {
        let chars = [
            as_declared(V1_WRITE),
            as_declared(V2_CMD),
            as_declared(V2_NOTIFY),
        ];
        assert_eq!(
            resolve(&GATT, &chars),
            Err(LinkError::MissingRequired(channels(&[V1_NOTIFY])))
        );
        assert_eq!(
            resolve(&GATT, &[]),
            Err(LinkError::MissingRequired(channels(&[V1_WRITE, V1_NOTIFY])))
        );
    }

    #[test]
    fn the_right_uuid_without_the_property_does_not_resolve() {
        let mut deaf = as_declared(V1_NOTIFY);
        deaf.notify = false;
        deaf.indicate = true;
        let chars = [as_declared(V1_WRITE), deaf];
        assert_eq!(
            resolve(&GATT, &chars),
            Err(LinkError::MissingRequired(channels(&[V1_NOTIFY])))
        );

        let mut read_only = as_declared(V1_WRITE);
        read_only.writable = false;
        read_only.writable_without_response = true;
        let chars = [read_only, as_declared(V1_NOTIFY)];
        assert_eq!(
            resolve(&GATT, &chars),
            Err(LinkError::MissingRequired(channels(&[V1_WRITE])))
        );
    }

    #[test]
    fn the_right_uuid_under_the_wrong_service_does_not_resolve() {
        let mut misplaced = as_declared(V2_CMD);
        misplaced.service = SERVICE_V1;
        let chars = [as_declared(V1_WRITE), as_declared(V1_NOTIFY), misplaced];
        let resolution = resolve(&GATT, &chars).expect("V1 is there");
        assert_eq!(resolution.resolved, channels(&[V1_WRITE, V1_NOTIFY]));
    }

    #[test]
    fn extra_characteristics_are_ignored() {
        let mut stranger = as_declared(V1_WRITE);
        stranger.uuid = Uuid::from_u128(0xdead_beef);
        let chars = [stranger, as_declared(V1_WRITE), as_declared(V1_NOTIFY)];
        let resolution = resolve(&GATT, &chars).expect("V1 is there");
        assert_eq!(resolution.matched, [(V1_WRITE, 1), (V1_NOTIFY, 2)]);
    }

    #[test]
    fn char_info_reads_btleplug_properties() {
        let characteristic = Characteristic {
            uuid: Uuid::from_u128(1),
            service_uuid: Uuid::from_u128(2),
            properties: CharPropFlags::WRITE | CharPropFlags::NOTIFY | CharPropFlags::READ,
            descriptors: BTreeSet::new(),
        };
        let info = CharInfo::from(&characteristic);
        assert_eq!(
            info,
            CharInfo {
                service: Uuid::from_u128(2),
                uuid: Uuid::from_u128(1),
                writable: true,
                writable_without_response: false,
                notify: true,
                indicate: false,
                readable: true,
            }
        );
        assert!(info.supports(Dir::Write));
        assert!(!info.supports(Dir::WriteNoResponse));
        assert!(info.supports(Dir::Notify));
        assert!(!info.supports(Dir::Indicate));
        assert!(info.supports(Dir::Read));

        let read_only = Characteristic {
            uuid: Uuid::from_u128(1),
            service_uuid: Uuid::from_u128(2),
            properties: CharPropFlags::READ,
            descriptors: BTreeSet::new(),
        };
        let info = CharInfo::from(&read_only);
        assert!(info.supports(Dir::Read));
        assert!(!info.supports(Dir::Write));
        assert!(!info.supports(Dir::Notify));
    }

    #[test]
    fn a_stream_channel_never_resolves_over_gatt() {
        /// The one endpoint a serial-style family declares, wrongly asked of GATT here.
        const STREAM: Channel = Channel(0);
        const STREAM_UUID: Uuid = Uuid::from_u128(0x0000_1101_0000_1000_8000_0080_5f9b_34fb);
        const STREAM_MAP: GattMap = GattMap {
            services: &[ServiceDecl {
                uuid: SERVICE_V1,
                required: true,
                chars: &[CharDecl {
                    id: STREAM,
                    uuid: STREAM_UUID,
                    dir: Dir::Stream,
                    required: true,
                }],
            }],
        };

        let everything = CharInfo {
            service: SERVICE_V1,
            uuid: STREAM_UUID,
            writable: true,
            writable_without_response: true,
            notify: true,
            indicate: true,
            readable: true,
        };
        assert!(everything.supports(Dir::Write));
        assert!(everything.supports(Dir::WriteNoResponse));
        assert!(everything.supports(Dir::Notify));
        assert!(everything.supports(Dir::Indicate));
        assert!(everything.supports(Dir::Read));
        // However capable, a characteristic is not a stream endpoint.
        assert!(!everything.supports(Dir::Stream));
        assert_eq!(
            resolve(&STREAM_MAP, &[everything]),
            Err(LinkError::MissingRequired(channels(&[STREAM])))
        );
    }
}
