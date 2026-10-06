//! Declarative description of the characteristics a device family uses.

use core::fmt;

use crate::{Channel, ChannelSet};

pub use uuid::Uuid;

/// What the host does with a characteristic.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Dir {
    /// Host writes, device acknowledges.
    Write,
    /// Host writes without acknowledgement.
    WriteNoResponse,
    /// Device pushes notifications; the host subscribes.
    Notify,
    /// Device pushes indications; the host subscribes.
    Indicate,
    /// Host reads the value; the device does not push.
    Read,
    /// Host writes and device pushes on the same endpoint: no separate subscription, and
    /// no value to read.
    ///
    /// What a serial-style transport declares: an RFCOMM/SPP connection is one
    /// bidirectional byte stream, so a family that speaks over one declares a single
    /// `Stream` channel and no service structure worth the name. A GATT transport never
    /// resolves one: a characteristic is not a stream endpoint.
    ///
    /// A stream [`Link`](crate::Link) reads the three operations as:
    /// [`write`](crate::Link::write) sends on the stream;
    /// [`subscribe`](crate::Link::subscribe) is a no-op returning `Ok(())`, since the
    /// device pushes without being asked; [`read`](crate::Link::read) is
    /// [`LinkError::UnknownChannel`](crate::LinkError::UnknownChannel), since there is no
    /// attribute to read.
    ///
    /// The bytes arrive in whatever chunks the transport hands over, not one frame per
    /// [`Input::Rx`](crate::Input::Rx) as a notification is. Putting the frames back
    /// together is `junk-pump`'s `Framed`, above the link and given the family's framing
    /// rule, so the link itself still knows nothing about a packet (invariant 2).
    Stream,
}

/// One characteristic a device family declares.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct CharDecl {
    /// The channel the protocol layer uses to refer to it. Unique across the whole map.
    pub id: Channel,
    /// The characteristic UUID the transport looks for.
    pub uuid: Uuid,
    /// How it is used.
    pub dir: Dir,
    /// Whether a connection without it is useless.
    pub required: bool,
}

/// One service a device family declares, with the characteristics it uses from it.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct ServiceDecl {
    /// The service UUID.
    pub uuid: Uuid,
    /// The characteristics of interest inside it.
    pub chars: &'static [CharDecl],
    /// Whether a connection without it is useless.
    pub required: bool,
}

/// Everything a device family needs from GATT, as `const` data.
///
/// Device crates write one of these as a constant and expose it through
/// [`Driver::GATT`](crate::Driver::GATT); a [`Link`](crate::Link) resolves it against the
/// real device and reports which channels it found.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct GattMap {
    /// The services, in no particular order.
    pub services: &'static [ServiceDecl],
}

impl GattMap {
    /// Every declared characteristic, across all services.
    ///
    /// The map is `'static` data, so the declarations outlive any borrow of the map and a
    /// caller may keep them.
    pub fn chars(&self) -> impl Iterator<Item = &'static CharDecl> + use<> {
        self.services.iter().flat_map(|s| s.chars.iter())
    }

    /// The declaration behind `chan`, if there is one.
    #[must_use]
    pub fn find(&self, chan: Channel) -> Option<&'static CharDecl> {
        self.chars().find(|c| c.id == chan)
    }

    /// The channels whose declaration is marked required, regardless of service.
    #[must_use]
    pub fn required(&self) -> ChannelSet {
        self.chars().filter(|c| c.required).map(|c| c.id).collect()
    }

    /// Whether `resolved` contains every required channel.
    #[must_use]
    pub fn satisfied_by(&self, resolved: &ChannelSet) -> bool {
        resolved.is_superset(&self.required())
    }

    /// Validates the map itself.
    ///
    /// This is `const` so a device crate can reject a bad map at compile time:
    ///
    /// ```ignore
    /// const _: () = assert!(MAP.check().is_ok(), "duplicate channel id in MAP");
    /// ```
    ///
    /// # Errors
    ///
    /// [`GattMapError::DuplicateChannel`] if two declarations share a channel id.
    pub const fn check(&self) -> Result<(), GattMapError> {
        let mut seen = ChannelSet::EMPTY;
        let mut s = 0;
        while s < self.services.len() {
            let chars = self.services[s].chars;
            let mut c = 0;
            while c < chars.len() {
                if !seen.insert(chars[c].id) {
                    return Err(GattMapError::DuplicateChannel(chars[c].id));
                }
                c += 1;
            }
            s += 1;
        }
        Ok(())
    }
}

/// A [`GattMap`] that cannot be used as written.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum GattMapError {
    /// Two characteristic declarations use the same channel id.
    DuplicateChannel(Channel),
}

impl fmt::Display for GattMapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GattMapError::DuplicateChannel(Channel(id)) => {
                write!(f, "channel {id} is declared more than once")
            }
        }
    }
}

impl core::error::Error for GattMapError {}

#[cfg(test)]
mod tests {
    use super::*;

    const SVC_MAIN: Uuid = Uuid::from_u128(0x0000_fff0_0000_1000_8000_0080_5f9b_34fb);
    const SVC_EXTRA: Uuid = Uuid::from_u128(0x0000_aaa0_0000_1000_8000_0080_5f9b_34fb);
    const CH_CMD: Channel = Channel(0);
    const CH_REPLY: Channel = Channel(1);
    const CH_EXTRA: Channel = Channel(9);
    const CH_VALUE: Channel = Channel(10);

    const MAP: GattMap = GattMap {
        services: &[
            ServiceDecl {
                uuid: SVC_MAIN,
                required: true,
                chars: &[
                    CharDecl {
                        id: CH_CMD,
                        uuid: Uuid::from_u128(0x0000_fff2_0000_1000_8000_0080_5f9b_34fb),
                        dir: Dir::WriteNoResponse,
                        required: true,
                    },
                    CharDecl {
                        id: CH_REPLY,
                        uuid: Uuid::from_u128(0x0000_fff1_0000_1000_8000_0080_5f9b_34fb),
                        dir: Dir::Notify,
                        required: true,
                    },
                ],
            },
            ServiceDecl {
                uuid: SVC_EXTRA,
                required: false,
                chars: &[
                    CharDecl {
                        id: CH_EXTRA,
                        uuid: Uuid::from_u128(0x0000_aaa1_0000_1000_8000_0080_5f9b_34fb),
                        dir: Dir::Indicate,
                        required: false,
                    },
                    CharDecl {
                        id: CH_VALUE,
                        uuid: Uuid::from_u128(0x0000_aaa2_0000_1000_8000_0080_5f9b_34fb),
                        dir: Dir::Read,
                        required: false,
                    },
                ],
            },
        ],
    };

    #[test]
    fn find_walks_every_service() {
        assert_eq!(MAP.chars().count(), 4);
        // Declarations are 'static: they may be kept after the map borrow ends.
        let kept: &'static CharDecl = { MAP.find(CH_CMD).unwrap() };
        assert_eq!(kept.id, CH_CMD);
        assert_eq!(MAP.find(CH_CMD).map(|c| c.dir), Some(Dir::WriteNoResponse));
        assert_eq!(MAP.find(CH_REPLY).map(|c| c.dir), Some(Dir::Notify));
        assert_eq!(MAP.find(CH_EXTRA).map(|c| c.dir), Some(Dir::Indicate));
        assert_eq!(MAP.find(CH_VALUE).map(|c| c.dir), Some(Dir::Read));
        assert_eq!(MAP.find(Channel(2)), None);
    }

    #[test]
    fn required_ignores_service_flag() {
        let required = MAP.required();
        assert_eq!(required.len(), 2);
        assert!(required.contains(CH_CMD));
        assert!(required.contains(CH_REPLY));
        assert!(!required.contains(CH_EXTRA));
        assert!(!required.contains(CH_VALUE));
    }

    #[test]
    fn satisfied_by_needs_every_required_channel() {
        let all: ChannelSet = [CH_CMD, CH_REPLY, CH_EXTRA, CH_VALUE].into_iter().collect();
        let just_required: ChannelSet = [CH_CMD, CH_REPLY].into_iter().collect();
        let missing_reply: ChannelSet = [CH_CMD, CH_EXTRA].into_iter().collect();
        assert!(MAP.satisfied_by(&all));
        assert!(MAP.satisfied_by(&just_required));
        assert!(!MAP.satisfied_by(&missing_reply));
        assert!(!MAP.satisfied_by(&ChannelSet::EMPTY));
    }

    // The check runs at compile time for const maps.
    const _: () = assert!(MAP.check().is_ok(), "MAP has a duplicate channel id");

    #[test]
    fn check_accepts_unique_ids() {
        assert_eq!(MAP.check(), Ok(()));
    }

    #[test]
    fn check_rejects_duplicate_ids() {
        const DUP: GattMap = GattMap {
            services: &[
                ServiceDecl {
                    uuid: SVC_MAIN,
                    required: true,
                    chars: &[CharDecl {
                        id: Channel(4),
                        uuid: Uuid::from_u128(1),
                        dir: Dir::Write,
                        required: true,
                    }],
                },
                ServiceDecl {
                    uuid: SVC_EXTRA,
                    required: true,
                    chars: &[CharDecl {
                        id: Channel(4),
                        uuid: Uuid::from_u128(2),
                        dir: Dir::Notify,
                        required: true,
                    }],
                },
            ],
        };
        assert_eq!(DUP.check(), Err(GattMapError::DuplicateChannel(Channel(4))));
        assert_ne!(
            alloc::string::ToString::to_string(&DUP.check().unwrap_err()),
            ""
        );
    }
}
