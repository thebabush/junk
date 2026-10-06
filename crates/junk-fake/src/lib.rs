//! `junk-fake`: a second, invented device family that exists to prove `junk-core` is generic.
//!
//! SPEC §2 says multi-device support is "proven by a second, fake device family in tests,
//! not by a real second device". This crate is that family. It implements
//! [`Driver`](junk_core::Driver) for a protocol nobody has ever put in hardware
//! ("fakering"), ships a pure peer simulator ([`FakePeer`]) that answers the driver's
//! writes, and its tests pin the six invariants from SPEC §3.1 without any I/O.
//!
//! # The fakering protocol
//!
//! One service, three characteristics ([`GATT`]): [`CMD`] (host writes commands), [`EVT`]
//! (device notifies replies and unsolicited data) and [`EXTRA`] (declared, optional, never
//! used; it exists so tests can connect with it unresolved).
//!
//! Every frame on either channel is `[cmd:u8][len:u8][payload; len]`. A frame shorter or
//! longer than `2 + len`, or with an unknown command, is not a frame ([`Frame::parse`]).
//!
//! | cmd    | direction    | payload                          | meaning                         |
//! |--------|--------------|----------------------------------|---------------------------------|
//! | `0x01` | host→device  | none                             | Ping                            |
//! | `0x02` | host→device  | `[key:u8]`                       | Get a value                     |
//! | `0x03` | host→device  | up to [`MAX_ECHO`] bytes         | Echo them back                  |
//! | `0x81` | device→host  | none                             | Pong                            |
//! | `0x82` | device→host  | `[key:u8][value:u32 le]`         | The value                       |
//! | `0x83` | device→host  | `[idx:u8][total:u8][chunk…]`     | One chunk of the echo           |
//! | `0x90` | device→host  | `[counter:u32 le]`               | Heartbeat, unsolicited          |
//! | `0x91` | device→host  | `[percent:u8]`, at most 100      | Battery, unsolicited            |
//!
//! Requests ([`Req`]) are serialised: one transaction ([`Txn`]) in flight, the rest queued
//! in order. Each transaction arms [`TIMEOUT_TIMER`] for [`TIMEOUT`] and fails with
//! [`ProtoError::Timeout`](junk_core::ProtoError::Timeout) when it fires. Anything received
//! that is not a valid frame in context (garbage, an unknown command, a reply with no
//! transaction to answer, a reply of the wrong kind, bytes on a channel other than
//! [`EVT`]) becomes [`Ev::Unparsed`] and touches nothing else.
#![no_std]
#![warn(missing_docs)]

extern crate alloc;

mod driver;
mod gatt;
mod peer;
mod txn;
mod wire;

pub use driver::{Ev, FakeDriver, Req, Resp, TIMEOUT, TIMEOUT_TIMER};
pub use gatt::{CMD, EVT, EXTRA, GATT, SERVICE};
pub use peer::FakePeer;
pub use txn::{EchoTxn, Step, Txn};
pub use wire::{Cmd, Command, Frame, MAX_ECHO, Reply, Unsolicited};
