//! `junk-colmi`: the Colmi R0x ring family (R10 first) for the junk protocol stack.
//!
//! Bottom up, as SPEC §3.2–§3.4 lay it out:
//!
//! - [`GATT`] and the six channels the rings expose: the V1 service carries 16-byte
//!   command frames both ways, the V2 service the length-prefixed `0xbc` big-data frames,
//!   and the standard Device Information service the firmware and hardware revision
//!   strings, which the host reads.
//! - [`measure`]: the measurement model the ring emits ([`HrSample`] and friends), kept here
//!   until a second device emits it.
//! - [`wire`]: bytes ↔ frames. [`wire::Frame`] and [`wire::Cmd`] for the V1 channels,
//!   [`wire::BigData`] and [`wire::BigDataKind`] for the V2 ones, the checksum and CRC they
//!   carry, and [`wire::V1Rx`] for telling a frame from a raw ASCII string on V1 notify.
//!   On top of the 16-byte frame, [`wire::HostFrame`] and
//!   [`wire::RingFrame`] are its typed payloads, one variant per command shape in each
//!   direction; on top of the big-data frame, [`wire::RequestBody`] and
//!   [`wire::ReplyBody`] are its typed bodies, one variant per kind.
//! - [`proto`]: the protocol layer on top. [`ColmiDriver`] is this family's
//!   [`Driver`](junk_core::Driver): it serialises [`proto::Req`]s into transactions, times
//!   them out, sorts the ring's replies from what it sends on its own ([`proto::Ev`]), and
//!   builds the ring's [`proto::Dialect`] as the session reveals it, collecting the
//!   multi-packet logs and the big-data replies into model samples along the way.
//! - [`replay`]: the [`junk_trace::Adapter`] that lets the generic replay harness drive a
//!   [`ColmiDriver`] from a captured trace, mapping each write back to the request that
//!   makes it; the Stage A tests run both fixtures through it.
//!
//! The wire layer knows nothing about sequencing: reassembling a big-data frame from
//! MTU-sized notifications, multi-packet logs and request/reply pairing are the protocol
//! layer's job.
//!
//! The protocol facts every byte here rests on are in `docs/colmi-protocol.md`; the
//! captured sessions under `fixtures/colmi-r10` pin them in this crate's tests.
#![no_std]
#![warn(missing_docs)]

extern crate alloc;

mod gatt;
pub mod measure;
pub mod proto;
pub mod replay;
pub mod wire;

pub use gatt::{
    DIS_FW, DIS_HW, GATT, SERVICE_DIS, SERVICE_V1, SERVICE_V2, V1_NOTIFY, V1_WRITE, V2_CMD,
    V2_NOTIFY, channel_by_name, channel_name,
};
pub use measure::{
    Bpm, HrSample, HrvSample, SleepKind, SleepSession, SleepStage, Source, Spo2Sample, StepBucket,
    StressSample, TempSample,
};
pub use proto::ColmiDriver;
pub use replay::ColmiAdapter;
