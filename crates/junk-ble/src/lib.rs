//! `junk-ble`: the real transport of the junk protocol stack, a [`Link`](junk_core::Link)
//! over [btleplug] (macOS, Linux, Windows).
//!
//! [`adapter()`] finds the machine's Bluetooth adapter, [`scan()`] lists the peripherals
//! that advertise a [`GattMap`](junk_core::GattMap)'s required services, [`choose()`] picks
//! the one a shell asked for among them, and [`BleLink::open`] wraps it as a `Link` for the
//! pump. Matching a map against
//! what a peripheral actually has is [`resolve()`], which is pure and tested without
//! hardware; the rest is a thin layer over btleplug that only a real device exercises.
//!
//! Nothing here looks at a payload (SPEC §3.1, invariant 2): the link moves bytes between
//! channels and characteristics and reports when the connection is gone.
//!
//! # What btleplug gives, and how the link copes with it
//!
//! - **The ATT MTU.** [`Peripheral::mtu`](btleplug::api::Peripheral::mtu) after service
//!   discovery, inferred on macOS from the maximum write-without-response length. Until a
//!   backend has learned one it reports the BLE default of 23, and `connect` then reports
//!   [`BleConfig::assumed_mtu`] instead.
//! - **Disconnects.** Not on the notification stream but as
//!   [`CentralEvent::DeviceDisconnected`](btleplug::api::CentralEvent::DeviceDisconnected)
//!   on the adapter's event stream, for every peripheral; the link filters by id.
//! - **Notifications.** A broadcast stream per peripheral, subscribed in `connect` before
//!   anything is enabled and drained by a task into a channel, so that `Link::next` is a
//!   channel receive and cancel-safe. btleplug's broadcast buffers hold 16 items and drop
//!   silently when a reader lags; the task does nothing but move items, so it does not.
//! - **Forgetting.** btleplug forgets a peripheral once it disconnects: the adapter no
//!   longer lists it and a `connect` on the old handle fails. A second `connect` on the same
//!   link therefore gets the handle back first, by identifier where the backend supports
//!   it and by a short scan otherwise.
#![warn(missing_docs)]

mod config;
mod error;
mod link;
mod resolve;
mod scan;

pub use btleplug::platform::{Adapter, PeripheralId};
pub use config::{BleConfig, DEFAULT_ASSUMED_MTU, DEFAULT_SCAN_TIMEOUT};
pub use error::BleError;
pub use link::{BleLink, Inspection, inspect};
pub use resolve::{CharInfo, Resolution, resolve};
pub use scan::{Found, Radio, adapter, choose, describe, name_of, radio, scan};
