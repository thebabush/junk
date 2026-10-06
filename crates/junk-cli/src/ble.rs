//! Finding the ring: scanning for peripherals with the Colmi services.
//!
//! Which of them `--device` names is [`junk_ble::choose`], and how one reads is
//! [`junk_ble::describe`]: both shells pick and name a ring the same way.

use junk_ble::{Adapter, BleConfig, Found};

/// The ring `wanted` names, with the hint that names this shell's own flag when several
/// rings answer to it. Picking is [`junk_ble::choose`]; only the hint is the CLI's.
///
/// # Errors
///
/// If no ring matches, or more than one does.
pub fn choose(found: Vec<Found>, wanted: Option<&str>) -> anyhow::Result<Found> {
    junk_ble::choose(found, wanted).map_err(|err| match err {
        junk_ble::BleError::SeveralRings { .. } => {
            anyhow::anyhow!("{err}\npick one with --device")
        }
        other => crate::flat(other),
    })
}

/// The peripherals advertising a Colmi service, as [`junk_ble::scan`] finds them.
///
/// # Errors
///
/// [`junk_ble::BleError`] if the scan fails.
pub async fn scan(adapter: &Adapter, config: &BleConfig) -> anyhow::Result<Vec<Found>> {
    junk_ble::scan(adapter, &junk_colmi::GATT, config)
        .await
        .map_err(crate::flat)
}
