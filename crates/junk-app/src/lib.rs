//! `junk-app`: what a shell asks of a ring, and what comes back.
//!
//! Between the pump and a shell there is a layer that is neither: the order the requests
//! go in, the wall-clock rule, what a workout's cursor is, which refusals are survivable.
//! That is this crate. It has no I/O of its own and knows nothing about CSV, printing or
//! FFI, so the CLI and an app run the *same* session (SPEC §4: `junk-cli` and `junk-ffi`
//! are two shells over one stack).
//!
//! - [`sync`] is the app's session: the preamble, a week of logs, the big-data collections
//!   and the workout list.
//! - [`live`] is Stage C: start a workout, stream it, stop it, fetch the record.
//! - Both take an `observe` closure and hand it a [`Progress`] as each step happens; both
//!   hand the link back afterwards, so a
//!   [`RecordingLink`](junk_pump::record::RecordingLink)'s lines can be taken.
//! - Both return a [`Session`]: the [`Samples`] collected, the [`Device`] they came from,
//!   and every request the ring refused.
//! - [`describe_req`], [`summary`] and [`describe_event`] are the words *both* shells show
//!   for a request, its answer and what the ring said on its own; where a line goes is the
//!   shell's own business.
//!
//! [`Clock`] is the wall-clock rule both shells need: a ring is written the local wall
//! clock and reports everything as that clock expressed as if it were UTC, so a reading is
//! a local minute and an offset, never an instant.
//!
//! Driving [`sync`] over a [`TraceLink`](junk_pump::trace_link::TraceLink) replays a
//! recorded session with no hardware, which is how this crate is tested.
#![warn(missing_docs)]

mod clock;
mod samples;
mod session;
mod words;

pub use clock::{Clock, date_of, stamp_of, wall_clock};
pub use samples::Samples;
pub use session::{Device, Progress, Run, Session, Workout, describe_req, drive, live, sync};
pub use words::{
    battery_summary, capabilities, channel_label, channel_names, describe_event, hex, summary,
    workout_summary,
};
