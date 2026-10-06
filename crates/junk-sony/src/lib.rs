//! `junk-sony`: Sony headphone protocol primitives for the junk stack.
//!
//! The target is the Sony WH-1000XM4, a Bluetooth Classic RFCOMM/SPP device that speaks
//! the protocol Sony's own app calls "Tandem" (the headphone tables are "MDR"). Bottom up,
//! as SPEC §3.2–§3.4 lay it out:
//!
//! - [`GATT`] and the one channel it declares: RFCOMM is a single bidirectional byte
//!   stream, so the map is one [`Dir::Stream`](junk_core::Dir::Stream) channel on the
//!   Sony service and nothing else.
//! - [`SonyFraming`], the rule that cuts that stream back into frames, for `junk-pump`'s
//!   `Framed` to apply above the link. It implements [`junk_core::Framing`].
//! - [`wire`]: bytes ↔ frames. [`Frame`] and [`DataType`], the escaping and the checksum
//!   they carry, and the ACK frame.
//!
//! - [`payload`]: what a frame's payload says. The byte enums with Sony's names, the typed
//!   decoders ([`payload::decode_report`]) and the [`payload::Report`] they produce.
//! - [`proto`]: the protocol layer on top. [`SonyDriver`] is this family's
//!   [`Driver`](junk_core::Driver): it does the app's link layer (sequence numbers, ACKs,
//!   resends) and init, then answers [`proto::Req`]s with [`proto::Resp`]s, and reports what
//!   the headset says on its own as [`proto::Ev`]s.
//!
//! # What is done and what is not
//!
//! The driver reads: init, the function list that is the capability model, and a typed
//! [`proto::Status`] of everything the headset lists. It sends no setter; [`proto::Req::Raw`]
//! is the one way to write and says so. `junk sony status` in `junk-cli` is the shell: it sends
//! [`proto::Req::Status`] and nothing else.
//!
//! Everything here is read out of an analysis of Sony's own app (the Sound Connect
//! Android app, studied for interoperability) and the community write-ups. It has since been run against one real
//! WH-1000XM4 (firmware 2.7.1), which confirmed the link layer, init and the status read.
//! `docs/sony-wh1000xm4.md` says what that confirmed and what is still unknown.
#![no_std]
#![warn(missing_docs)]

extern crate alloc;

mod framing;
mod gatt;
pub mod payload;
pub mod proto;
pub mod wire;

pub use gatt::{GATT, RFCOMM, SERVICE, SERVICE_V2, channel_by_name, channel_name};
pub use wire::{DataType, Frame, FrameError};

pub use framing::SonyFraming;
pub use proto::SonyDriver;
