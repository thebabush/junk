//! The protocol layer: requests, transactions and the [`Driver`](junk_core::Driver) of the
//! Soundcore speakers (SPEC §3.3, §3.4).
//!
//! [`SoundcoreDriver`] serialises [`Req`]s into transactions ([`Txn`]), one in flight at a
//! time and the rest queued in order. A transaction writes one packet, arms
//! [`TIMEOUT_TIMER`] for [`TIMEOUT`], and turns the speaker's reply into a [`Resp`];
//! packets nobody asked for become [`Ev`]s.
//!
//! There is no dialect here. The ring family has one because its firmwares disagree about
//! what they can do; the Motion 300 answers `0x0101` with everything it has to say about
//! itself, and one speaker is all that has been captured (`docs/soundcore-motion-300.md`).
//! A second Soundcore model is where a `Dialect`-shaped type would start to earn its place.
//!
//! # The speaker is the owner's
//!
//! This driver never emits a command that changes the speaker. [`Req::DeviceInfo`] reads;
//! [`Req::Raw`] sends whatever it is given and says in its own docs which numbers power the
//! speaker off or rewrite its settings. No convenience request wraps any of them.

mod driver;
mod info;
mod req;
mod txn;

pub use driver::{SoundcoreDriver, TIMEOUT, TIMEOUT_TIMER};
pub use info::DeviceInfo;
pub use req::{Ev, Req, Resp};
pub use txn::{Started, Step, Txn};
