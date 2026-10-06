//! What a user asks a Soundcore speaker, what it answers, and what it says on its own.

use junk_core::{Bytes, ChannelSet, Percent};

use crate::proto::DeviceInfo;
use crate::wire::Command;

/// What a user can ask a Soundcore speaker. Each is answered by exactly one
/// [`Output::Done`](junk_core::Output::Done) carrying the [`Resp`] its doc names.
///
/// # Nothing here changes the speaker
///
/// Every request in this enum is a read. The speaker is the owner's, not the driver's:
/// the commands that power it off or rewrite its settings have no convenience request and
/// are not going to get one. The only way to send one at all is [`Req::Raw`], whose own
/// docs list what a shell must not offer casually.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Req {
    /// `0x0101`: ask the speaker who it is. Answered by [`Resp::DeviceInfo`].
    DeviceInfo,
    /// Any command, undecoded. Answered by [`Resp::Raw`] with the first reply of the same
    /// command, as it came.
    ///
    /// # This is the one way to write to the speaker
    ///
    /// Everything else here reads. A raw command does whatever its number does, and some
    /// of these numbers are destructive to an owner's speaker:
    ///
    /// - `0x8901` powers the speaker off. It does not answer; the transaction times out.
    /// - `0x8b02` (equalizer preset), `0x8d02` (custom equalizer), `0x9001` (voice
    ///   prompts), `0x8601` (auto power-off) and `0xff01` (LDAC) each overwrite a setting
    ///   the owner chose, and the speaker keeps it.
    ///
    /// A shell must not offer this the way it offers a read: the caller is asserting it
    /// knows which command it is sending and that the speaker's owner agreed to it.
    Raw {
        /// The command to send.
        command: Command,
        /// Its payload, as is.
        payload: Bytes,
    },
}

/// The answer to a [`Req`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resp {
    /// Answer to [`Req::DeviceInfo`].
    DeviceInfo(DeviceInfo),
    /// Answer to [`Req::Raw`]: the whole reply packet, header and checksum included, as it
    /// came off the stream.
    Raw(Bytes),
}

/// Things a Soundcore speaker reports on its own, and things the driver could not place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ev {
    /// `0x0301`: the battery level, whenever the speaker feels like saying it. One byte in
    /// fifths.
    Battery(Percent),
    /// `0x0401`: whether it is charging.
    Charging(bool),
    /// `0x0901`: the volume, as the speaker numbers it.
    Volume(u8),
    /// `0x2101`: playback state. The payload's layout is Gadgetbridge-only and not settled,
    /// so it is reported as it came.
    Playback(Bytes),
    /// Bytes the wire layer rejected, or a notification whose payload was not the shape its
    /// command takes. Nothing else happened (invariant 5).
    Unparsed {
        /// What they were.
        bytes: Bytes,
    },
    /// A well-formed packet no transaction wanted and no notification explains.
    Unexpected(Bytes),
    /// The link came up without these required channels — for this family that can only
    /// be the one byte stream; the driver asked to disconnect.
    MissingChannels(ChannelSet),
}
