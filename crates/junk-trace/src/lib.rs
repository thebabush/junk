//! `junk-trace`: reader and writer of the junk trace v1 text format, and the replay
//! harness that drives a driver from one.
//!
//! A trace is a captured session between an app and a device, one line per thing that
//! happened, in order. The files under `fixtures/` are traces; `tools/pklg2trace.py` writes
//! them from `PacketLogger` captures, and [`replay()`] feeds them to a
//! [`Driver`](junk_core::Driver). This crate knows nothing about any device: channels stay
//! the strings the writer used and payloads stay bytes. A device crate maps them to its own
//! types, and gives the harness an [`Adapter`] that does so.
//!
//! The crate is `no_std + alloc`, like the drivers it replays, so a device crate can depend
//! on it without leaving the bare-metal build.
//!
//! # The format
//!
//! Line-oriented UTF-8, `\n`-terminated. Three kinds of line:
//!
//! - `#<text>`: a comment. The first run of them is the header (description, source,
//!   columns, handle map, notes); comments may also appear later.
//! - `! <stamp> <free text>`: an event around the link (connected, disconnected, MTU, a
//!   value on an unmapped handle, an app log line), kept for orientation.
//! - `<stamp> <tx|rx> <channel> <hex>`: bytes that crossed the link, single spaces, hex
//!   without separators, at least one byte.
//!
//! A stamp is `YYYY-MM-DDTHH:MM:SS.mmm`, with or without a `+HH:MM` / `-HH:MM` offset; the
//! whole trace uses one form. Blank lines are an error.
//!
//! Reading and writing are exact inverses of each other for what the writer produces (see
//! [`Trace::render`]); the fixtures pin this byte for byte.
//!
//! ```
//! use junk_trace::{Direction, Trace};
//!
//! let text = "# junk trace v1 — example\n\
//!             2026-07-02T14:16:30.474 tx v1.write 04011200000000000000000000000017\n\
//!             ! 2026-07-02T14:16:31.000 disconnected\n";
//! let trace = Trace::parse(text)?;
//! assert_eq!(trace.header().collect::<Vec<_>>(), [" junk trace v1 — example"]);
//! let first = trace.data().next().ok_or("no data")?;
//! assert_eq!(first.dir, Direction::Tx);
//! assert_eq!(first.chan, "v1.write");
//! assert_eq!(first.bytes.first(), Some(&0x04));
//! assert_eq!(trace.events().count(), 1);
//! assert_eq!(trace.render(), text);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
#![no_std]
#![warn(missing_docs)]

extern crate alloc;

mod line;
pub mod replay;
mod stamp;
mod trace;

pub use line::{DataLine, Direction, EventLine, Line, LineError};
pub use replay::{Adapter, Answer, Mismatch, Replay, replay};
pub use stamp::{Stamp, StampError};
pub use trace::{ParseError, Trace};
