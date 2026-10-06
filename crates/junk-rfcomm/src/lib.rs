//! `junk-rfcomm`: a [`Link`](junk_core::Link) over Bluetooth Classic RFCOMM on macOS,
//! through Apple's `IOBluetooth` framework.
//!
//! # This is the one crate in the tree that may write `unsafe`
//!
//! The workspace sets `unsafe_code = "forbid"`, and this crate does not take
//! `[lints] workspace = true`: it declares its own `unsafe_code = "allow"`. That is the
//! price of binding an Objective-C framework. There is no `btleplug` for Bluetooth
//! Classic — `junk-ble` needs no such exception only because btleplug swallows the unsafe
//! on its behalf — so the calls into `IOBluetooth` are made here, by hand, and every one of
//! them is an `unsafe` call to a C ABI the compiler cannot check.
//!
//! The exception is paid for with discipline rather than trust:
//!
//! - Every `unsafe` block carries a comment saying why it is sound, and each is one call
//!   or one pointer read, never a stretch of code.
//! - All of it is in [`runloop`](self), on one thread, behind a channel. Nothing outside
//!   that module writes `unsafe`, and nothing outside this crate sees an Objective-C type.
//! - The pure parts — parsing an address, finding the map's stream channel, the error
//!   messages — are separate modules with no framework in them, and are unit-tested off
//!   macOS as well as on it.
//!
//! Nothing here looks at a payload (SPEC §3.1, invariant 2): the link moves bytes on one
//! channel and reports when the connection is gone. Bytes arrive in whatever chunks the
//! radio hands over, not one frame per event — putting the frames back together is
//! `junk-pump`'s `Framed`, above this.
//!
//! # macOS only
//!
//! `IOBluetooth` is a macOS framework, so the dependency is target-gated and everything that
//! touches it is behind `#[cfg(target_os = "macos")]`. Elsewhere the crate is the same API
//! over nothing: [`RfcommLink::open`] parses the address and then reports
//! [`RfcommError::Unsupported`], and `cargo build --workspace` on Linux is none the wiser.
//!
//! # What a device family must declare
//!
//! RFCOMM is one bidirectional byte stream, so this link serves a map with exactly one
//! [`Dir::Stream`](junk_core::Dir::Stream) channel and resolves that channel alone.
//! [`Link::connect`](junk_core::Link::connect) opens **the enclosing
//! [`ServiceDecl`](junk_core::ServiceDecl)'s** UUID as the SDP service, because a stream
//! has no characteristic and the [`CharDecl`](junk_core::CharDecl)'s own UUID means nothing
//! to RFCOMM; a family should set the two equal, which is what a reader will assume.
//! The MTU `connect` reports is the RFCOMM channel's, not an ATT MTU.
//!
//! # How the framework is kept at arm's length
//!
//! `IOBluetooth` delivers through a `CFRunLoop` and several of its calls block, neither of
//! which an async runtime will put up with. So one dedicated thread per connection creates
//! the device, runs the SDP query, opens the channel, installs the delegate and turns the
//! run loop; it takes commands over a [`std::sync::mpsc`] and pushes events into a
//! [`tokio::sync::mpsc`]. [`Link::next`](junk_core::Link::next) is then a channel receive,
//! and cancel-safe for the same reason `junk-pump`'s `ChannelLink` is. No `IOBluetooth` call
//! is ever made from the async side, including in [`RfcommLink::open`], which does nothing
//! but parse the address.
//!
//! # The host must turn its main run loop
//!
//! `IOBluetooth` schedules an RFCOMM channel's `NSStream`s on `[NSRunLoop mainRunLoop]` and
//! sends the delegate its open, data and close messages from blocks dispatched out of those
//! stream events. A process whose main thread is doing something else — a tokio runtime,
//! say — therefore never sees them, however diligently this crate's own thread turns its
//! own run loop.
//!
//! **This is measured, not inferred.** Against a powered-on Soundcore Motion 300 on
//! 2026-09-15: from a plain `#[tokio::main]`, with nobody turning the main run loop,
//! `connect` timed out after **25 s**; with the main thread turning the main run loop and
//! tokio on a second thread, the same `connect` returned in **122 ms**, and the speaker
//! answered the first write 47 ms later. The capture is
//! `fixtures/soundcore-motion-300/probe-2026-09-15.trace`.
//!
//! So a host of this link must leave its main thread turning the main run loop and put the
//! pump on another thread. [`turn_main_loop`] and [`run_main_loop_forever`] are that loop,
//! with the recipe in the [`mainloop`] module docs; `examples/probe.rs` is the smallest
//! whole example of the shape.
//!
//! Because the delegate is reached through a `dispatch_async`, it can be called on a thread
//! other than the link's own, so everything it shares with that thread is atomic.
//!
//! # What only a device can tell you
//!
//! The pure parts are unit-tested. Everything that reaches `IOBluetooth` — the device
//! lookup, the SDP query, the channel, the delegate, the run loop — is verified by
//! `examples/probe.rs` against a real paired speaker, and by nothing else: there is no way
//! to fake `IOBluetooth` from a test, and a test that needed the speaker awake in the room
//! would not be a test. What that probe did see is kept as a fixture and replayed by
//! `junk-soundcore`'s tests, so the bytes it proved do not depend on the speaker being in
//! the room either.

#![warn(missing_docs)]

mod address;
mod error;
pub mod mainloop;
mod resolve;

#[cfg(target_os = "macos")]
mod link;
#[cfg(target_os = "macos")]
mod runloop;
#[cfg(not(target_os = "macos"))]
mod unsupported;

pub use error::RfcommError;
pub use mainloop::{TURN, run_main_loop_forever, turn_main_loop};
pub use resolve::{Endpoint, resolve};

#[cfg(target_os = "macos")]
pub use link::RfcommLink;
#[cfg(not(target_os = "macos"))]
pub use unsupported::RfcommLink;
