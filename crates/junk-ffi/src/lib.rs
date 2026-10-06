//! `junk-ffi`: the phone's shell over the stack, as uniffi bindings (SPEC §4).
//!
//! **Rust keeps the Bluetooth.** btleplug's `CoreBluetooth` backend is the same code on macOS
//! and iOS, so a phone runs the identical driver, pump and [`Link`](junk_core::Link) the
//! CLI runs, and this crate exposes what an app actually asks for — [`scan`], [`sync`],
//! [`live`] — rather than the driver. Swift never sees a frame, a channel or a byte: what
//! crosses is [records](crate::SyncResult) of numbers and strings, and the words in them
//! are `junk-app`'s, the same ones `junk-cli` prints.
//!
//! ```text
//! scan(timeout)                       -> [FoundRing]
//! sync(device, days, timeout, obs)    -> SyncResult   + Progress as it goes
//! live(device, sport, secs, t, stop, obs) -> LiveResult
//! ```
//!
//! - `device` picks the ring: a substring of its advertised name, case aside, or its exact
//!   id from a [`FoundRing`]. Without one, the single ring in range.
//! - `timeout_secs` is how long to scan before giving up on finding it.
//! - The [`Observer`] is handed a [`Progress`] at every step, from the session's own task;
//!   it must not block.
//! - [`Stop`] ends a [`live`] stream early *without* abandoning the workout on the ring.
//!
//! # What is an error and what is not
//!
//! A ring that refuses a request is not a failure of the call: dialects differ, the session
//! carries on, and the refusal comes back in [`SyncResult::failures`]. A [`JunkError`] is
//! only ever a link that never came up, a ring that could not be found, or a session that
//! stopped with a request in flight.
//!
//! # Generating the Swift
//!
//! Proc-macro bindings only, no UDL and no `build.rs`; the bindgen is a binary of this
//! crate, so:
//!
//! ```sh
//! cargo build -p junk-ffi --target aarch64-apple-ios-sim
//! cargo run -q -p junk-ffi --bin uniffi-bindgen -- generate \
//!     --library target/aarch64-apple-ios-sim/debug/libjunk_ffi.a \
//!     --language swift --out-dir bindings
//! ```
#![warn(missing_docs)]

mod progress;
mod records;
mod stop;

use std::sync::Arc;
use std::time::Duration;

use junk_app::Clock;
use junk_ble::{BleConfig, BleError, BleLink, Radio};

pub use progress::{Observer, Progress};
pub use records::{
    Bpm, DeviceInfo, Failure, FoundRing, HrSample, HrSource, HrvSample, LiveResult, SleepKind,
    SleepSession, SleepStage, Spo2Sample, StepBucket, StressSample, SyncResult, TempSample,
    Workout, WorkoutRecord,
};
pub use stop::Stop;

uniffi::setup_scaffolding!();

/// Why a call never got as far as a finished session.
///
/// What the *ring* refused is not here: that is a [`Failure`] in [`SyncResult::failures`],
/// and the session went on past it.
#[derive(uniffi::Error, Debug, thiserror::Error)]
pub enum JunkError {
    /// The adapter, the scan or the connection failed.
    #[error("{message}")]
    Bluetooth {
        /// The sentence to show. Every variant has one, so an app can say what went wrong
        /// without knowing which variant it caught: uniffi renders a Swift error by its
        /// `Debug`, not by this crate's words, so the words travel in a field instead.
        message: String,
    },
    /// The Bluetooth radio cannot be used: switched off, or absent (an iOS simulator has
    /// no radio, so only a real phone can reach a ring).
    #[error("{message}")]
    RadioOff {
        /// The sentence to show.
        message: String,
        /// Whether a person could turn it on. False in a simulator.
        switchable: bool,
    },
    /// No ring is the one asked for: none of those scanned matched `device`, or, with none
    /// asked for, the scan saw none at all.
    #[error("{message}")]
    NoRing {
        /// The sentence to show.
        message: String,
        /// The name substring or id that was asked for, if one was.
        wanted: Option<String>,
    },
    /// The session stopped before it was done: the ring went away, or the pump stopped with
    /// a request in flight.
    #[error("{message}")]
    Session {
        /// The sentence to show.
        message: String,
    },
}

impl From<BleError> for JunkError {
    fn from(err: BleError) -> JunkError {
        // Every message is `junk-ble`'s own, so both shells say the same thing.
        let message = err.to_string();
        match err {
            BleError::RadioOff { radio } => JunkError::RadioOff {
                message,
                switchable: radio == Radio::Off,
            },
            BleError::NoRing { wanted } => JunkError::NoRing { message, wanted },
            BleError::NoAdapter
            | BleError::NotFound(_)
            | BleError::SeveralRings { .. }
            | BleError::Btleplug(_) => JunkError::Bluetooth { message },
        }
    }
}

/// The rings in range, after listening for `timeout_secs`.
///
/// Only peripherals advertising a Colmi service are reported, so this is a list of rings
/// and not of everything nearby. A ring the phone is already connected to is listed too,
/// though it advertises nothing.
///
/// # Errors
///
/// [`JunkError::Bluetooth`] if the phone has no adapter or the scan failed.
#[uniffi::export(async_runtime = "tokio")]
pub async fn scan(timeout_secs: u64) -> Result<Vec<FoundRing>, JunkError> {
    let adapter = junk_ble::adapter().await?;
    let found = junk_ble::scan(&adapter, &junk_colmi::GATT, &config(timeout_secs)).await?;
    Ok(found.iter().map(FoundRing::from).collect())
}

/// The app's session: connect, set the ring's clock, read `days` days of logs, then the
/// big-data collections and the workout list.
///
/// `observer` sees every step as it happens. The phone's own wall clock is what the ring is
/// set to and what places "today", so a ring synced from a phone in another zone reports
/// its readings in *that* zone.
///
/// # Errors
///
/// [`JunkError::NoRing`] if `device` matches nothing, [`JunkError::Bluetooth`] if the link
/// never came up, [`JunkError::Session`] if the session stopped with a request in flight. A
/// request the ring merely refused is in [`SyncResult::failures`] instead.
#[uniffi::export(async_runtime = "tokio")]
pub async fn sync(
    device: Option<String>,
    days: u8,
    timeout_secs: u64,
    stop: Arc<Stop>,
    observer: Arc<dyn Observer>,
) -> Result<SyncResult, JunkError> {
    let link = open(device.as_deref(), timeout_secs).await?;
    let run = junk_app::sync(link, Clock::now(), days, stop.pressed(), |progress| {
        progress::report(&*observer, &progress);
    })
    .await;
    finish(run.result)?;
    Ok(SyncResult::from(&run.session))
}

/// Stage C: start a workout of `sport_type` on the ring, stream its heart rate for
/// `seconds`, stop it, and fetch the record the ring stored.
///
/// Each streamed sample reaches `observer` as a [`Progress::WorkoutSample`]. `stop` ends the
/// stream early; that is not an abort, and the workout is still stopped and fetched — see
/// [`Stop`]. A workout under about a minute is not stored, and then
/// [`LiveResult::workout`] is `None`.
///
/// # Errors
///
/// The same three as [`sync`], and [`JunkError::Session`] if the ring answered the start,
/// the list or the record with something that makes no sense.
#[uniffi::export(async_runtime = "tokio")]
pub async fn live(
    device: Option<String>,
    sport_type: u8,
    seconds: u64,
    timeout_secs: u64,
    stop: Arc<Stop>,
    observer: Arc<dyn Observer>,
) -> Result<LiveResult, JunkError> {
    let link = open(device.as_deref(), timeout_secs).await?;
    let run = junk_app::live(link, sport_type, seconds, stop.pressed(), |progress| {
        progress::report(&*observer, &progress);
    })
    .await;
    finish(run.result)?;
    Ok(LiveResult::from(&run.session))
}

/// How the scan and the link are configured: `timeout_secs` to find the ring, and the
/// assumed MTU [`BleConfig`] defaults to for a platform that reports none.
fn config(timeout_secs: u64) -> BleConfig {
    BleConfig {
        scan_timeout: Duration::from_secs(timeout_secs),
        ..BleConfig::default()
    }
}

/// Scans for the ring `device` names and opens a link to it.
async fn open(device: Option<&str>, timeout_secs: u64) -> Result<BleLink, JunkError> {
    let config = config(timeout_secs);
    let adapter = junk_ble::adapter().await?;
    let found = junk_ble::scan(&adapter, &junk_colmi::GATT, &config).await?;
    let ring = junk_ble::choose(found, device)?;
    Ok(BleLink::open(&adapter, &ring.id, config).await?)
}

/// How a script ended, as an error an app can show. `{err:#}` is the whole chain on one
/// line, the way `junk-cli` prints a failure.
fn finish(result: anyhow::Result<()>) -> Result<(), JunkError> {
    result.map_err(|err| JunkError::Session {
        message: format!("{err:#}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sentence a variant carries, whichever variant it is.
    fn message_of(err: &JunkError) -> &str {
        match err {
            JunkError::Bluetooth { message }
            | JunkError::RadioOff { message, .. }
            | JunkError::NoRing { message, .. }
            | JunkError::Session { message } => message,
        }
    }

    #[test]
    fn a_ring_that_cannot_be_found_says_what_junk_ble_says() {
        let wanted = Some("F300".to_owned());
        assert_eq!(
            JunkError::from(BleError::NoRing {
                wanted: wanted.clone()
            })
            .to_string(),
            BleError::NoRing { wanted }.to_string()
        );
        assert!(matches!(
            JunkError::from(BleError::NoRing { wanted: None }),
            JunkError::NoRing { wanted: None, .. }
        ));
        // Everything else Bluetooth failed at keeps its own words.
        let err = JunkError::from(BleError::NoAdapter);
        assert!(matches!(err, JunkError::Bluetooth { .. }));
        assert_eq!(err.to_string(), "no Bluetooth adapter");

        // A radio a person can turn on reads differently from one that is not there.
        let off = JunkError::from(BleError::RadioOff { radio: Radio::Off });
        assert!(matches!(
            off,
            JunkError::RadioOff {
                switchable: true,
                ..
            }
        ));
        assert!(off.to_string().contains("switched off"));
        let none = JunkError::from(BleError::RadioOff {
            radio: Radio::Unknown,
        });
        assert!(matches!(
            none,
            JunkError::RadioOff {
                switchable: false,
                ..
            }
        ));

        // Every variant carries the sentence to show, so an app never rewrites it.
        for err in [off, none, JunkError::from(BleError::NoAdapter)] {
            assert_eq!(message_of(&err), err.to_string());
        }
    }

    #[test]
    fn the_scan_timeout_is_the_only_thing_a_caller_sets() {
        let config = config(3);
        assert_eq!(config.scan_timeout, Duration::from_secs(3));
        assert_eq!(config.assumed_mtu, BleConfig::default().assumed_mtu);
    }

    #[test]
    fn a_session_that_stopped_mid_request_is_an_error_with_its_whole_chain() {
        let err = finish(Err(
            anyhow::anyhow!("the pump no longer runs requests").context("battery")
        ))
        .expect_err("the script failed");
        assert!(matches!(err, JunkError::Session { .. }));
        assert_eq!(err.to_string(), "battery: the pump no longer runs requests");
        assert!(finish(Ok(())).is_ok());
    }
}
