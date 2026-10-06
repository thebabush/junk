//! The protocol layer: init, requests and the [`Driver`](junk_core::Driver) of the Sony
//! headphones (SPEC §3.3, §3.4).
//!
//! [`SonyDriver`] does what Sony's own app does, in the order it does it, and then answers
//! [`Req`]s. Three layers, one inside the other:
//!
//! - **The link** (Sony's app's link layer): sequence numbers that toggle `0`/`1`, an ACK for
//!   every frame that asks for one, a resend after [`ACK_TIMEOUT`] with the identical
//!   frame, and a give-up after [`MAX_RESENDS`] resends that closes the link.
//! - **Steps**: one command, the frame that answers it, and a reply timeout of
//!   [`REPLY_TIMEOUT`]. One step is in flight at a time. A step that times out is recorded
//!   as no reply and the session goes on: reads are best-effort.
//! - **Jobs**: init (Sony's app's init sequence), run on its own as soon as the link is up, and then
//!   each [`Req`] in the order it came.
//!
//! # Timers
//!
//! Two, because the two waits are different and can both be outstanding: [`ACK_TIMER`]
//! (id `0`) guards the ACK of the command on the wire, and [`REPLY_TIMER`] (id `1`) guards
//! the reply once the command has been acknowledged. The pump re-arms an id by sending
//! `SetTimer` with it, and applies every output of a step before it picks the next input,
//! so a timer cancelled in the same step as the frame that made it moot never fires.
//!
//! # What the shell can rely on
//!
//! - Every frame that needs an ACK gets one, immediately, including frames the driver does
//!   not understand and retransmissions of frames it has already seen.
//! - A frame that does not decode is dropped without an ACK and reported
//!   ([`Ev::Dropped`]); one that decodes but does not read is acknowledged and reported
//!   ([`Ev::Unparsed`]).
//! - [`Input::Disconnected`](junk_core::Input::Disconnected) resets everything and fails
//!   every outstanding request with
//!   [`ProtoError::Disconnected`](junk_core::ProtoError::Disconnected), so one driver value
//!   serves any number of connections.
//!
//! # Nothing here changes the headset
//!
//! The driver sends reads only. [`Req::Raw`] is the single way to write, and says what it
//! allows.

mod driver;
mod plan;
mod req;
mod slot;
mod status;
mod txn;

pub use driver::{
    ACK_TIMEOUT, ACK_TIMER, DEFAULT_LANGUAGE, MAX_RESENDS, REPLY_TIMEOUT, REPLY_TIMER,
    SUPPORTED_PROTOCOL_VERSIONS, SonyDriver,
};
pub use req::{Ev, Req, Resp};
pub use status::{Capabilities, DeviceInfo, GeneralSetting, RawReply, Reading, Status};
