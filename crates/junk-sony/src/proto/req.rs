//! What a user asks a Sony headset, what it answers, and what it says on its own.

use alloc::boxed::Box;

use junk_core::{Bytes, ChannelSet};

use crate::payload::Report;
use crate::proto::Status;
use crate::wire::{DataType, FrameError};

/// What a user can ask a Sony headset. Each is answered by exactly one
/// [`Output::Done`](junk_core::Output::Done) carrying the [`Resp`] its doc names.
///
/// # Nothing here changes the headset
///
/// [`Req::Status`] reads. The headset is the owner's, and there is no convenience request
/// for any setter: the only way to write at all is [`Req::Raw`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Req {
    /// Read everything the headset lists: a GET for each readable function in its function
    /// list, in a fixed order, one at a time. Answered by [`Resp::Status`] once the last
    /// one has been answered or has timed out. The connection's init runs first, whether or
    /// not this has been asked.
    Status,
    /// One command, undecoded. Answered by [`Resp::Raw`] with the payload of the next frame
    /// of the same data type that arrives, once the command has been acknowledged.
    ///
    /// # This is the one way to write to the headset, and nothing is checked
    ///
    /// The payload goes out as it is. Anything the headset accepts, this does: its SET
    /// commands (`48`, `58`, `68`, `E8`, `F8`, `D8`, `22 00 01` power off, ...) change what
    /// the owner chose, and the headset keeps it. A shell must not offer this the way it
    /// offers a read: the caller is asserting it knows which command it is sending and that
    /// the owner agreed to it.
    ///
    /// The reply is whatever frame of that data type comes next, so a notification that
    /// arrives first answers it; a raw command does not know what to expect. A data type
    /// that is not acknowledged (`ACK` and the `SHOT_*` types) is refused with
    /// [`ProtoError::Unsupported`](junk_core::ProtoError::Unsupported): this driver's
    /// link layer waits for the ACK of what it sends, and such a frame never gets one.
    Raw {
        /// The data type to send it with: `0C` for table one, `0E` for table two.
        data_type: DataType,
        /// The payload, command id first.
        payload: Bytes,
    },
}

/// The answer to a [`Req`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resp {
    /// Answer to [`Req::Status`]. Boxed: a status is large and a raw reply is small.
    Status(Box<Status>),
    /// Answer to [`Req::Raw`]: the reply's payload, command id first, without its frame.
    Raw(Bytes),
}

/// Things a Sony headset reports on its own, and things the driver could not place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ev {
    /// The headset told the driver something nobody asked: a notification (battery,
    /// noise cancelling, ...), or a reply that arrived after its step had given up.
    Report(Report),
    /// The headset's protocol version, as soon as the driver has it. When `supported` is
    /// `false` the driver does not go on: init failed and every request is answered with
    /// [`ProtoError::Unsupported`](junk_core::ProtoError::Unsupported). The link is left
    /// open; closing it is the shell's call.
    ProtocolVersion {
        /// The 16-bit version.
        version: u16,
        /// Whether it is one Sony's app accepts.
        supported: bool,
    },
    /// A frame that did not decode: a bad checksum, a bad length, a bad escape. It was not
    /// acknowledged, so the headset retransmits it (the link layer of Sony's app).
    Dropped {
        /// Why it was dropped.
        error: FrameError,
        /// The bytes as they came.
        bytes: Bytes,
    },
    /// A well-formed frame the driver could not read: a data type or command it does not
    /// know, or a payload too short for its layout. It was acknowledged if its data type
    /// asks for that. Nothing else happened (invariant 5).
    Unparsed {
        /// The frame's bytes as they came.
        bytes: Bytes,
    },
    /// The link came up without the required byte stream; the driver asked to disconnect.
    MissingChannels(ChannelSet),
}
