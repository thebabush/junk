//! `junk-core`: the device-agnostic contract of the junk protocol stack.
//!
//! Everything a device family needs to plug into the stack lives here and nothing that is
//! specific to any one device does. A family implements [`Driver`] (a pure state machine
//! fed [`Input`]s, emitting [`Output`]s), describes its characteristics as a `const`
//! [`GattMap`], and decodes into its own measurement types (the generic primitives
//! [`Timestamp`], [`Percent`] and [`Battery`] are here). A transport
//! implements [`Link`]. The pump crate owns one of each and runs the only loop.
//!
//! Invariants, from SPEC §3.1 (the device, pump and fuzz tests enforce them):
//!
//! 1. **Driver is pure.** [`Driver::handle`] is deterministic in its inputs: no clock, no
//!    sleeping, no I/O, no allocation outside `alloc`; `no_std` is the compiler-enforced
//!    version of this rule.
//! 2. **Link is dumb.** A [`Link`] moves bytes and channels only and never interprets a
//!    payload.
//! 3. **One loop.** The pump is the only caller of [`Driver::handle`] and the only owner of
//!    the [`Link`], and applies every [`Output`] in order before feeding the next [`Input`].
//! 4. **Every `Request` produces exactly one `Done`.** Timeouts are the driver's job, via
//!    [`Output::SetTimer`] and [`Input::Timer`]; the pump never times anything out.
//! 5. **Malformed bytes never panic.** Garbage in [`Input::Rx`] yields an event or a failed
//!    [`Output::Done`], never a panic.
//! 6. **No hidden state across connections.** [`Input::Disconnected`] resets every
//!    transaction; a driver is reusable for a reconnect without being rebuilt.
#![no_std]
#![warn(missing_docs)]

extern crate alloc;

mod channel;
mod driver;
mod error;
mod framing;
mod gatt;
mod link;
mod model;
mod time;

pub use channel::{Channel, ChannelSet, ChannelSetIter};
pub use driver::{Bytes, Driver, Input, Output, Outputs, ReqId, TimerId};
pub use error::ProtoError;
pub use framing::{FrameLen, Framing};
pub use gatt::{CharDecl, Dir, GattMap, GattMapError, ServiceDecl, Uuid};
pub use link::{Link, LinkError, LinkEvent};
pub use model::{Battery, Percent, Timestamp};
pub use time::{Duration, Instant};
