//! Finding the one stream endpoint a [`GattMap`] declares. Pure, and tested without a
//! device or a Mac.

use junk_core::{Channel, ChannelSet, Dir, GattMap, LinkError, Uuid};

/// The single [`Dir::Stream`] channel a map declares, and the service UUID to open it on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Endpoint {
    /// The channel the protocol layer will name the stream by.
    pub chan: Channel,
    /// The SDP service UUID, which is the **enclosing**
    /// [`ServiceDecl`](junk_core::ServiceDecl)'s.
    pub service: Uuid,
}

/// The stream endpoint of `gatt`, or why it has none this transport can serve.
///
/// RFCOMM is one bidirectional byte stream, so a family that speaks over it declares
/// exactly one [`Dir::Stream`] channel (see [`Dir::Stream`]'s own docs) and this link
/// resolves that one channel and nothing else. A map declaring several streams, or none,
/// is not one an RFCOMM link can serve, and neither is one whose other channels are
/// required: nothing else here can resolve, so a required [`Dir::Notify`] channel is
/// permanently missing.
///
/// The SDP UUID is the enclosing service's, not the [`CharDecl`](junk_core::CharDecl)'s: a
/// stream has no characteristic, so its own UUID means nothing to RFCOMM. A family should
/// set it equal to its service's, which is what a reader will assume.
///
/// # Errors
///
/// [`LinkError::MissingRequired`] with the required channels that cannot resolve, and
/// [`LinkError::Io`] when the map itself is not one a byte stream can serve: no stream
/// channel at all, or more than one.
pub fn resolve(gatt: &GattMap) -> Result<Endpoint, LinkError> {
    let mut streams = gatt.services.iter().flat_map(|service| {
        service
            .chars
            .iter()
            .filter(|decl| decl.dir == Dir::Stream)
            .map(|decl| Endpoint {
                chan: decl.id,
                service: service.uuid,
            })
    });
    let Some(endpoint) = streams.next() else {
        let missing = gatt.required();
        return Err(if missing.is_empty() {
            LinkError::Io("the map declares no Dir::Stream channel to open".into())
        } else {
            LinkError::MissingRequired(missing)
        });
    };
    let extra = streams.count();
    if extra > 0 {
        return Err(LinkError::Io(format!(
            "the map declares {} Dir::Stream channels; RFCOMM is one byte stream",
            extra + 1
        )));
    }
    let resolved: ChannelSet = [endpoint.chan].into_iter().collect();
    let missing = gatt.required().difference(&resolved);
    if missing.is_empty() {
        Ok(endpoint)
    } else {
        Err(LinkError::MissingRequired(missing))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use junk_core::{CharDecl, ServiceDecl};

    const SERVICE: Uuid = Uuid::from_u128(0x0cf1_2d31_fac3_4553_bd80_d683_2e7b_3135);
    const OTHER: Uuid = Uuid::from_u128(0x0000_fff0_0000_1000_8000_0080_5f9b_34fb);
    const STREAM: Channel = Channel(0);
    const SECOND: Channel = Channel(1);
    const NOTIFY: Channel = Channel(2);

    /// A stream channel, declared the way a family should: its UUID is its service's.
    const fn stream(id: Channel, uuid: Uuid) -> CharDecl {
        CharDecl {
            id,
            uuid,
            dir: Dir::Stream,
            required: true,
        }
    }

    const fn notify(id: Channel, required: bool) -> CharDecl {
        CharDecl {
            id,
            uuid: OTHER,
            dir: Dir::Notify,
            required,
        }
    }

    const fn map(services: &'static [ServiceDecl]) -> GattMap {
        GattMap { services }
    }

    const MOTION_300: GattMap = map(&[ServiceDecl {
        uuid: SERVICE,
        required: true,
        chars: &[stream(STREAM, SERVICE)],
    }]);

    #[test]
    fn one_stream_resolves_to_its_services_uuid() {
        assert_eq!(
            resolve(&MOTION_300),
            Ok(Endpoint {
                chan: STREAM,
                service: SERVICE,
            })
        );
    }

    /// The stream's own UUID is meaningless to RFCOMM: the service's is what is opened.
    #[test]
    fn the_chars_own_uuid_is_ignored() {
        const ODD: GattMap = map(&[ServiceDecl {
            uuid: SERVICE,
            required: true,
            chars: &[stream(STREAM, OTHER)],
        }]);
        assert_eq!(resolve(&ODD).map(|e| e.service), Ok(SERVICE));
    }

    const NONE_REQUIRED: GattMap = map(&[ServiceDecl {
        uuid: SERVICE,
        required: false,
        chars: &[notify(NOTIFY, false)],
    }]);

    const ONLY_NOTIFY: GattMap = map(&[ServiceDecl {
        uuid: SERVICE,
        required: true,
        chars: &[notify(NOTIFY, true)],
    }]);

    #[test]
    fn a_map_with_no_stream_cannot_be_served() {
        assert_eq!(
            resolve(&NONE_REQUIRED),
            Err(LinkError::Io(
                "the map declares no Dir::Stream channel to open".into()
            ))
        );

        assert_eq!(
            resolve(&ONLY_NOTIFY),
            Err(LinkError::MissingRequired(
                [NOTIFY].into_iter().collect::<ChannelSet>()
            ))
        );
    }

    #[test]
    fn two_streams_are_not_one_byte_stream() {
        const TWO_IN_ONE_SERVICE: GattMap = map(&[ServiceDecl {
            uuid: SERVICE,
            required: true,
            chars: &[stream(STREAM, SERVICE), stream(SECOND, SERVICE)],
        }]);
        const TWO_SERVICES: GattMap = map(&[
            ServiceDecl {
                uuid: SERVICE,
                required: true,
                chars: &[stream(STREAM, SERVICE)],
            },
            ServiceDecl {
                uuid: OTHER,
                required: true,
                chars: &[stream(SECOND, OTHER)],
            },
        ]);
        for two in [TWO_IN_ONE_SERVICE, TWO_SERVICES] {
            assert_eq!(
                resolve(&two),
                Err(LinkError::Io(
                    "the map declares 2 Dir::Stream channels; RFCOMM is one byte stream".into()
                ))
            );
        }
    }

    /// Nothing but the stream resolves, so anything else the map requires is missing.
    #[test]
    fn another_required_channel_can_never_resolve() {
        const WITH_NOTIFY: GattMap = map(&[ServiceDecl {
            uuid: SERVICE,
            required: true,
            chars: &[stream(STREAM, SERVICE), notify(NOTIFY, true)],
        }]);
        assert_eq!(
            resolve(&WITH_NOTIFY),
            Err(LinkError::MissingRequired(
                [NOTIFY].into_iter().collect::<ChannelSet>()
            ))
        );
    }

    #[test]
    fn an_optional_extra_channel_is_simply_not_resolved() {
        const WITH_OPTIONAL: GattMap = map(&[ServiceDecl {
            uuid: SERVICE,
            required: true,
            chars: &[stream(STREAM, SERVICE), notify(NOTIFY, false)],
        }]);
        assert_eq!(resolve(&WITH_OPTIONAL).map(|e| e.chan), Ok(STREAM));
    }
}
