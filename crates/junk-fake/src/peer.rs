//! A fakering device, simulated: pure, `no_std`, and never surprised by its input.

use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;
use core::num::NonZeroUsize;

use junk_core::{Bytes, Channel};

use crate::gatt::{CMD, EVT};
use crate::wire::{Cmd, Command, Frame, Reply, Unsolicited, fixed};

/// A device on the other end of the link, as a value.
///
/// Feed it what the driver writes ([`FakePeer::on_write`]) and it hands back the
/// notifications the device would send. It never does I/O and never panics, whatever it
/// is given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FakePeer {
    /// Values set explicitly; anything else is [`FakePeer::value`]'s default.
    values: BTreeMap<u8, u32>,
    /// Echo chunk size.
    chunk: NonZeroUsize,
    /// Whether replies are being swallowed.
    mute: bool,
    /// The last heartbeat counter sent.
    heartbeat: u32,
}

impl FakePeer {
    /// The echo chunk size a fresh peer uses.
    pub const DEFAULT_CHUNK: NonZeroUsize = NonZeroUsize::new(4).expect("4 is not zero");

    /// A device with default values, [`FakePeer::DEFAULT_CHUNK`]-byte echo chunks, not muted.
    #[must_use]
    pub fn new() -> Self {
        FakePeer {
            values: BTreeMap::new(),
            chunk: Self::DEFAULT_CHUNK,
            mute: false,
            heartbeat: 0,
        }
    }

    /// Stores `value` under `key`, replacing the default.
    pub fn set_value(&mut self, key: u8, value: u32) {
        self.values.insert(key, value);
    }

    /// What a Get of `key` answers: what was set, or `key` repeated in every byte.
    #[must_use]
    pub fn value(&self, key: u8) -> u32 {
        // 255 * 0x0101_0101 == u32::MAX, so the default never overflows.
        self.values
            .get(&key)
            .copied()
            .unwrap_or(u32::from(key) * 0x0101_0101)
    }

    /// How many bytes go in each echo chunk. Zero is taken as one.
    pub fn set_chunk(&mut self, size: usize) {
        self.chunk = NonZeroUsize::new(size).unwrap_or(NonZeroUsize::MIN);
    }

    /// While muted the device accepts writes but sends nothing back, so timeouts can be tested.
    pub fn set_mute(&mut self, mute: bool) {
        self.mute = mute;
    }

    /// Takes one write from the host and returns the notifications it provokes, in order.
    ///
    /// Every element is one notification on [`EVT`]. A write to any other channel, bytes
    /// that are not a command frame, or any write while muted, yields none.
    #[must_use]
    pub fn on_write(&mut self, chan: Channel, bytes: &[u8]) -> Vec<(Channel, Bytes)> {
        if chan != CMD || self.mute {
            return Vec::new();
        }
        let command = match Frame::parse(bytes) {
            Some(Frame::Command(command)) => command,
            Some(Frame::Reply(_) | Frame::Unsolicited(_)) | None => return Vec::new(),
        };
        let replies = match command {
            Command::Ping => vec![Reply::Pong],
            Command::Get { key } => vec![Reply::Value {
                key,
                value: self.value(key),
            }],
            Command::Echo { payload } => self.chunked(&payload),
        };
        // Every chunk is at most as long as the payload it came from, so encoding cannot
        // fail; if it somehow did, the device says nothing rather than something wrong.
        replies
            .iter()
            .map(|reply| reply.encode().map(|bytes| (EVT, bytes)))
            .collect::<Result<Vec<_>, _>>()
            .unwrap_or_default()
    }

    /// Splits `payload` into the chunks of an echo reply. An empty payload is one empty chunk.
    fn chunked(&self, payload: &[u8]) -> Vec<Reply> {
        let mut chunks: Vec<&[u8]> = payload.chunks(self.chunk.get()).collect();
        if chunks.is_empty() {
            chunks.push(&[]);
        }
        // At most `MAX_ECHO` chunks of at least one byte each, so this always fits.
        let Ok(total) = u8::try_from(chunks.len()) else {
            return Vec::new();
        };
        chunks
            .iter()
            .enumerate()
            .filter_map(|(i, chunk)| {
                Some(Reply::EchoChunk {
                    idx: u8::try_from(i).ok()?,
                    total,
                    chunk: chunk.to_vec(),
                })
            })
            .collect()
    }

    /// The next heartbeat notification; the counter goes up by one each call.
    pub fn heartbeat(&mut self) -> (Channel, Bytes) {
        self.heartbeat = self.heartbeat.wrapping_add(1);
        let counter = self.heartbeat;
        (EVT, Unsolicited::Heartbeat { counter }.encode())
    }

    /// A battery notification carrying `percent` verbatim.
    ///
    /// A value above 100 is sent as-is: the host must reject it, and tests check that it does.
    #[must_use]
    pub fn battery(percent: u8) -> (Channel, Bytes) {
        (EVT, fixed(Cmd::Battery, [percent]))
    }
}

impl Default for FakePeer {
    fn default() -> Self {
        Self::new()
    }
}
