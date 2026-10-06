//! Finding the adapter, the peripherals that advertise a map's services, and the one a
//! shell asked for among them.

use std::collections::HashSet;

use btleplug::api::{
    Central, CentralState, Manager as _, Peripheral as _, RetrievePeripheralsOptions, ScanFilter,
};
use btleplug::platform::{Adapter, Manager, PeripheralId};
use junk_core::{GattMap, Uuid};

use crate::{BleConfig, BleError};

/// The machine's first Bluetooth adapter.
///
/// # Errors
///
/// [`BleError::NoAdapter`] if there is none, [`BleError::Btleplug`] if btleplug fails.
pub async fn adapter() -> Result<Adapter, BleError> {
    let manager = Manager::new().await?;
    manager
        .adapters()
        .await?
        .into_iter()
        .next()
        .ok_or(BleError::NoAdapter)
}

/// What the machine's Bluetooth radio can do right now.
///
/// An adapter can exist with no usable radio behind it: switched off on a phone, or absent
/// altogether in an iOS simulator. A scan then finds nothing, which is worth telling a
/// person apart from "no ring in range".
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Radio {
    /// Powered on: a scan can find something.
    On,
    /// Powered off. On a phone, the person can turn it on.
    Off,
    /// The adapter would not say. An iOS simulator answers this way: there is no radio.
    Unknown,
}

/// What `adapter`'s radio can do right now.
///
/// # Errors
///
/// [`BleError::Btleplug`] if the adapter will not say.
pub async fn radio(adapter: &Adapter) -> Result<Radio, BleError> {
    Ok(match adapter.adapter_state().await? {
        CentralState::PoweredOn => Radio::On,
        CentralState::PoweredOff => Radio::Off,
        CentralState::Unknown => Radio::Unknown,
    })
}

/// A peripheral a [`scan`] saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// What to give [`BleLink::open`](crate::BleLink::open).
    pub id: PeripheralId,
    /// Its advertised name, if it had one.
    pub name: Option<String>,
    /// Signal strength in dBm, if the platform reported one.
    pub rssi: Option<i16>,
    /// Every service it advertised.
    pub services: Vec<Uuid>,
}

/// Lists the peripherals that have at least one of `gatt`'s required services: those
/// advertising one, and those the system already holds a connection to that offer one.
///
/// Scans for [`BleConfig::scan_timeout`] with a filter on those services, then reports
/// every peripheral the adapter knows that advertises one of them, and every peripheral the
/// platform can hand over as already connected with one of them (a connected peripheral
/// does not advertise, so a scan alone would miss a ring the system keeps a link to), each
/// once. A map that marks no service required is scanned for all its services instead.
///
/// The scan is stopped before returning; if this future is dropped while it waits, the
/// adapter keeps scanning until something else stops it.
///
/// # Errors
///
/// [`BleError::Btleplug`] if starting or stopping the scan, or listing what it found, fails.
/// A platform that cannot hand over connected peripherals is not an error: only the scan's
/// results are reported then.
pub async fn scan(
    adapter: &Adapter,
    gatt: &GattMap,
    config: &BleConfig,
) -> Result<Vec<Found>, BleError> {
    // A scan with no radio behind it finds nothing and says nothing; say why instead.
    let radio = radio(adapter).await?;
    if radio != Radio::On {
        return Err(BleError::RadioOff { radio });
    }
    let wanted = wanted_services(gatt);
    adapter
        .start_scan(ScanFilter {
            services: wanted.clone(),
        })
        .await?;
    tokio::time::sleep(config.scan_timeout).await;
    adapter.stop_scan().await?;

    let mut seen = HashSet::new();
    let mut found = Vec::new();
    for peripheral in adapter.peripherals().await? {
        let id = peripheral.id();
        if !seen.insert(id.clone()) {
            continue;
        }
        let Some(properties) = peripheral.properties().await? else {
            continue;
        };
        if !properties.services.iter().any(|s| wanted.contains(s)) {
            continue;
        }
        found.push(Found {
            id,
            name: properties.local_name,
            rssi: properties.rssi,
            services: properties.services,
        });
    }

    // Already connected to the system: retrieved by service, so no advertisement check.
    let connected = RetrievePeripheralsOptions {
        identifiers: None,
        services: Some(wanted.clone()),
    };
    for peripheral in adapter
        .retrieve_peripherals(connected)
        .await
        .unwrap_or_default()
    {
        let id = peripheral.id();
        if !seen.insert(id.clone()) {
            continue;
        }
        let properties = peripheral.properties().await?;
        let (name, rssi, advertised) = match properties {
            Some(properties) => (properties.local_name, properties.rssi, properties.services),
            None => (None, None, Vec::new()),
        };
        found.push(Found {
            id,
            name,
            rssi,
            services: if advertised.is_empty() {
                wanted.clone()
            } else {
                advertised
            },
        });
    }
    Ok(found)
}

/// The peripheral `wanted` names among `found`: the one whose name contains it, case
/// aside, or whose id is exactly it. Without `wanted`, the one peripheral found.
///
/// Both shells pick a ring this way, and say the same thing when they cannot.
///
/// # Errors
///
/// [`BleError::NoRing`] if none matches, [`BleError::SeveralRings`] if more than one does,
/// naming those that did.
pub fn choose(found: Vec<Found>, wanted: Option<&str>) -> Result<Found, BleError> {
    let mut candidates: Vec<Found> = match wanted {
        Some(wanted) => {
            let lower = wanted.to_lowercase();
            found
                .into_iter()
                .filter(|peripheral| {
                    peripheral.id.to_string() == wanted
                        || peripheral
                            .name
                            .as_deref()
                            .is_some_and(|name| name.to_lowercase().contains(&lower))
                })
                .collect()
        }
        None => found,
    };
    match candidates.len() {
        1 => Ok(candidates.remove(0)),
        0 => Err(BleError::NoRing {
            wanted: wanted.map(str::to_owned),
        }),
        _ => Err(BleError::SeveralRings { candidates }),
    }
}

/// `found` on one line: name (or `?`), id, RSSI (or `?`).
#[must_use]
pub fn describe(found: &Found) -> String {
    let rssi = found
        .rssi
        .map_or_else(|| "?".to_owned(), |rssi| rssi.to_string());
    format!("{}  {}  {rssi}", name_of(found), found.id)
}

/// `found`'s advertised name, or `?`.
#[must_use]
pub fn name_of(found: &Found) -> &str {
    found.name.as_deref().unwrap_or("?")
}

/// The services a peripheral must advertise to be one of `gatt`'s: the required ones, or
/// all of them if none is marked required.
pub(crate) fn wanted_services(gatt: &GattMap) -> Vec<Uuid> {
    let required: Vec<Uuid> = gatt
        .services
        .iter()
        .filter(|s| s.required)
        .map(|s| s.uuid)
        .collect();
    if required.is_empty() {
        gatt.services.iter().map(|s| s.uuid).collect()
    } else {
        required
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use junk_core::ServiceDecl;

    #[test]
    fn colmi_is_wanted_by_its_v1_service() {
        assert_eq!(wanted_services(&junk_colmi::GATT), [junk_colmi::SERVICE_V1]);
    }

    #[test]
    fn a_map_without_required_services_wants_all_of_them() {
        const MAP: GattMap = GattMap {
            services: &[
                ServiceDecl {
                    uuid: Uuid::from_u128(1),
                    chars: &[],
                    required: false,
                },
                ServiceDecl {
                    uuid: Uuid::from_u128(2),
                    chars: &[],
                    required: false,
                },
            ],
        };
        assert_eq!(
            wanted_services(&MAP),
            [Uuid::from_u128(1), Uuid::from_u128(2)]
        );
        assert_eq!(
            wanted_services(&GattMap { services: &[] }),
            [] as [junk_core::Uuid; 0]
        );
    }
}
