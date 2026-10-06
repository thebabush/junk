//! `junk-pump`: the one loop of the junk protocol stack, on tokio.
//!
//! A [`Pump`] owns one [`Driver`](junk_core::Driver) and one [`Link`](junk_core::Link) and
//! runs the only loop that connects them (SPEC §3.1, invariant 3). It waits on the link,
//! on the timers the driver armed, on the requests coming through its [`Handle`]s and on an
//! optional clock tick; feeds the driver one [`Input`](junk_core::Input) at a time; and
//! applies [`Output`](junk_core::Output)s of that step in order, until the step completes
//! or the session ends. Nothing else calls [`Driver::handle`](junk_core::Driver::handle) and nothing else
//! touches the link.
//!
//! Protocol timeouts belong to the driver: [`SetTimer`](junk_core::Output::SetTimer)
//! comes back as [`Timer`](junk_core::Input::Timer). Separately, transport operations are
//! bounded by [`PumpConfig::io_timeout`] and any already armed driver deadline. If an
//! operation stalls until that deadline, the pump ends the session rather than retrying
//! an operation whose device-side effects are unknown. I/O remains sequential, so ticks
//! and inputs can be delayed while an operation is running; they are not processed by a
//! concurrent transport worker. Disconnect cleanup has its own bounded wait.
//!
//! # Priority
//!
//! Explicit shutdown interrupts the session, including in-progress I/O. Otherwise, when
//! several things are ready at once in the input loop they are taken in this order:
//!
//! 1. the link (a notification or a disconnect),
//! 2. the earliest armed timer,
//! 3. a request or a shutdown from a [`Handle`],
//! 4. the tick, if [`PumpConfig::tick_every`] is set.
//!
//! Before any of them, the value of a read: a [`Read`](junk_core::Output::Read) is
//! applied in its turn among the step's outputs, and what [`Link::read`](junk_core::Link::read)
//! returned is fed as [`Rx`](junk_core::Input::Rx) as soon as the step is over, before the
//! loop waits again. A read that fails is reported as [`PumpEvent::LinkError`] and feeds
//! nothing.
//!
//! Because the link comes before the timers, a reply and the deadline that guarded it,
//! both ready in the same cycle, are handled reply first; the driver cancels the timer
//! while handling the reply and the pump drops the deadline before it could fire. A timer
//! cancelled in the same step as the reply that made it moot therefore never reaches the
//! driver. The pump's tests pin this.
//!
//! # What a `Link` must provide
//!
//! The pump creates a fresh [`Link::next`](junk_core::Link::next) future every time it
//! waits and drops it, possibly unfinished, as soon as something else is ready. `Link::next`
//! must therefore be **cancel-safe**: dropping its future before it completes must lose no
//! event. A future that resolves from a channel receiver (as the [`channel_link`] one does)
//! is; one that pulls a notification out of a transport and then awaits something else
//! before returning it is not.
//!
//! Other operation futures may be dropped on shutdown or an I/O deadline. The link must
//! remain safe to disconnect and reconnect after cancellation; see [`Link`](junk_core::Link).
//!
//! # Testing a driver
//!
//! [`channel_link::ChannelLink`] is a `Link` over tokio channels whose other end,
//! [`channel_link::PeerSide`], is held by the test. It is the standard way to drive a
//! device family end to end without hardware.
//!
//! # Framing a byte stream
//!
//! [`framed::Framed`] wraps a `Link` whose transport is a byte stream — a
//! [`Dir::Stream`](junk_core::Dir::Stream) endpoint, RFCOMM and its like — and hands the
//! pump whole frames instead of the chunks the radio delivered, with the packet knowledge
//! supplied by the device family as a [`framed::Framing`] rather than learnt by the link.
//!
//! # Recording a session
//!
//! [`record::RecordingLink`] wraps any `Link` and writes down everything that crosses it
//! as trace lines, so a live session can be kept as a fixture (SPEC §5, Stage B4).
//!
//! # Replaying a session
//!
//! [`trace_link::TraceLink`] is its mirror: a `Link` that serves a captured trace back, so
//! the whole session a real device answered — not just the driver — runs again offline.
#![warn(missing_docs)]

pub mod channel_link;
pub mod framed;
mod pump;
pub mod record;
pub mod trace_link;

pub use pump::{Events, Handle, Pump, PumpConfig, PumpError, PumpEvent, RequestError, Stop};
