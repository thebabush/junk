//! `junk-soundcore`: Anker/Soundcore protocol primitives for the junk stack.
//!
//! The first target is the Soundcore Motion 300 (`A3135`), a Bluetooth Classic
//! RFCOMM/SPP device rather than a BLE GATT one. Bottom up, as SPEC §3.2–§3.4 lay it out:
//!
//! - [`GATT`] and the one channel it declares: RFCOMM is a single bidirectional byte
//!   stream, so the map is one [`Dir::Stream`](junk_core::Dir::Stream) channel on the
//!   vendor control service and nothing else.
//! - [`wire`]: bytes ↔ packets. [`Packet`] and [`Command`], the checksum they carry, and
//!   the Motion 300 payloads Gadgetbridge documents.
//! - [`proto`]: the protocol layer on top. [`SoundcoreDriver`] is this family's
//!   [`Driver`](junk_core::Driver): it serialises [`proto::Req`]s into transactions, times
//!   them out, and sorts the speaker's replies from what it says on its own
//!   ([`proto::Ev`]).
//!
//! # A stream needs a framing
//!
//! The bytes arrive in whatever chunks the radio hands over, not one packet per
//! notification as a GATT device's would. [`SoundcoreFraming`] is the rule that puts them
//! back together, for `junk-pump`'s `Framed` to apply above the link. It implements
//! [`junk_core::Framing`], which is where that contract lives precisely so a `no_std`
//! family can supply one without taking on the `std` crate that does the buffering.
//!
//! # What is verified and what is not
//!
//! The packet header, the `0x0101` device-info reply and the RFCOMM transport were
//! captured from the speaker on 2026-09-15 and are replayed in this crate's tests from
//! `fixtures/soundcore-motion-300/probe-2026-09-15.trace`. Everything else — the rest of
//! the command catalogue, the equalizer payloads — is read out of Gadgetbridge and has
//! never been on a wire here. `docs/soundcore-motion-300.md` says which is which.
//!
//! # Nothing here changes the speaker
//!
//! The driver has exactly one request that reads and one that sends what it is given.
//! There is no convenience request for powering the speaker off or for rewriting its
//! settings, and there is not going to be one: see [`proto::Req::Raw`].
#![no_std]
#![warn(missing_docs)]

extern crate alloc;

#[cfg(test)]
mod captured;
mod framing;
mod gatt;
pub mod proto;
pub mod wire;

pub use gatt::{GATT, RFCOMM, SERVICE, channel_by_name, channel_name};
pub use proto::SoundcoreDriver;
pub use wire::{
    Command, CustomEqualizerSet, EqualizerBand, EqualizerProfile, Motion300DeviceInfo,
    Motion300Equalizer, Packet, PacketError, PayloadError, SERVICE_MOTION_300,
};

pub use framing::SoundcoreFraming;
