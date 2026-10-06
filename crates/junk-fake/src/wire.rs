//! The fakering wire format: `[cmd:u8][len:u8][payload; len]`, typed in both directions.

use alloc::vec::Vec;

use junk_core::{Bytes, Percent, ProtoError};

/// The most bytes an Echo command, and therefore one echo chunk, may carry.
pub const MAX_ECHO: usize = 253;

/// The command byte of every frame fakering knows.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Cmd {
    /// Host → device: are you there?
    Ping = 0x01,
    /// Host → device: read a value.
    Get = 0x02,
    /// Host → device: send these bytes back.
    Echo = 0x03,
    /// Device → host: reply to [`Cmd::Ping`].
    Pong = 0x81,
    /// Device → host: reply to [`Cmd::Get`].
    Value = 0x82,
    /// Device → host: one chunk of the reply to [`Cmd::Echo`].
    EchoChunk = 0x83,
    /// Device → host, unsolicited: still alive, with a counter.
    Heartbeat = 0x90,
    /// Device → host, unsolicited: battery level.
    Battery = 0x91,
}

impl Cmd {
    /// The command with this byte, if there is one.
    const fn from_byte(byte: u8) -> Option<Cmd> {
        match byte {
            0x01 => Some(Cmd::Ping),
            0x02 => Some(Cmd::Get),
            0x03 => Some(Cmd::Echo),
            0x81 => Some(Cmd::Pong),
            0x82 => Some(Cmd::Value),
            0x83 => Some(Cmd::EchoChunk),
            0x90 => Some(Cmd::Heartbeat),
            0x91 => Some(Cmd::Battery),
            _ => None,
        }
    }
}

/// A host → device frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Are you there?
    Ping,
    /// Read the value stored under `key`.
    Get {
        /// Which value.
        key: u8,
    },
    /// Send `payload` back, in chunks.
    Echo {
        /// At most [`MAX_ECHO`] bytes.
        payload: Bytes,
    },
}

impl Command {
    /// The bytes to write on the command channel.
    ///
    /// # Errors
    ///
    /// [`ProtoError::Unsupported`] if an Echo payload exceeds [`MAX_ECHO`].
    pub fn encode(&self) -> Result<Bytes, ProtoError> {
        match self {
            Command::Ping => Ok(fixed(Cmd::Ping, [])),
            Command::Get { key } => Ok(fixed(Cmd::Get, [*key])),
            Command::Echo { payload } => {
                if payload.len() > MAX_ECHO {
                    return Err(ProtoError::Unsupported("echo: payload too long"));
                }
                variable(Cmd::Echo, payload)
            }
        }
    }
}

/// A device → host frame that answers a [`Command`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// Answer to [`Command::Ping`].
    Pong,
    /// Answer to [`Command::Get`].
    Value {
        /// The key that was asked for.
        key: u8,
        /// What is stored under it.
        value: u32,
    },
    /// One chunk of the answer to [`Command::Echo`]; the last one has `idx == total - 1`.
    EchoChunk {
        /// Position of this chunk, from zero.
        idx: u8,
        /// How many chunks there are.
        total: u8,
        /// The bytes of this chunk, at most [`MAX_ECHO`] of them.
        chunk: Bytes,
    },
}

impl Reply {
    /// The bytes the device notifies on the event channel.
    ///
    /// # Errors
    ///
    /// [`ProtoError::Unsupported`] if an echo chunk exceeds [`MAX_ECHO`].
    pub fn encode(&self) -> Result<Bytes, ProtoError> {
        match self {
            Reply::Pong => Ok(fixed(Cmd::Pong, [])),
            Reply::Value { key, value } => {
                let [v0, v1, v2, v3] = value.to_le_bytes();
                Ok(fixed(Cmd::Value, [*key, v0, v1, v2, v3]))
            }
            Reply::EchoChunk { idx, total, chunk } => {
                if chunk.len() > MAX_ECHO {
                    return Err(ProtoError::Unsupported("echo: chunk too long"));
                }
                let mut payload = Vec::with_capacity(2 + chunk.len());
                payload.push(*idx);
                payload.push(*total);
                payload.extend_from_slice(chunk);
                variable(Cmd::EchoChunk, &payload)
            }
        }
    }
}

/// A device → host frame nobody asked for.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Unsolicited {
    /// Still alive.
    Heartbeat {
        /// Goes up by one each time.
        counter: u32,
    },
    /// Battery level. A percent above 100 on the wire is not a frame at all.
    Battery {
        /// Charge.
        percent: Percent,
    },
}

impl Unsolicited {
    /// The bytes the device notifies on the event channel. Cannot fail: both layouts are fixed.
    #[must_use]
    pub fn encode(&self) -> Bytes {
        match self {
            Unsolicited::Heartbeat { counter } => fixed(Cmd::Heartbeat, counter.to_le_bytes()),
            Unsolicited::Battery { percent } => fixed(Cmd::Battery, [percent.get()]),
        }
    }
}

/// Any fakering frame, in either direction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    /// Host → device.
    Command(Command),
    /// Device → host, answering a command.
    Reply(Reply),
    /// Device → host, on its own initiative.
    Unsolicited(Unsolicited),
}

impl Frame {
    /// Decodes one frame, or `None` if `bytes` is not one. Never panics (invariant 5).
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Option<Frame> {
        let (&cmd, rest) = bytes.split_first()?;
        let (&len, payload) = rest.split_first()?;
        if payload.len() != usize::from(len) {
            return None;
        }
        match Cmd::from_byte(cmd)? {
            Cmd::Ping => payload.is_empty().then_some(Frame::Command(Command::Ping)),
            Cmd::Get => {
                let [key] = payload else { return None };
                Some(Frame::Command(Command::Get { key: *key }))
            }
            Cmd::Echo => (payload.len() <= MAX_ECHO).then(|| {
                Frame::Command(Command::Echo {
                    payload: payload.to_vec(),
                })
            }),
            Cmd::Pong => payload.is_empty().then_some(Frame::Reply(Reply::Pong)),
            Cmd::Value => {
                let [key, value @ ..] = payload else {
                    return None;
                };
                let value = u32::from_le_bytes(<[u8; 4]>::try_from(value).ok()?);
                Some(Frame::Reply(Reply::Value { key: *key, value }))
            }
            Cmd::EchoChunk => {
                // `len` is a byte, so the chunk is at most 253 bytes: within `MAX_ECHO`.
                let [idx, total, chunk @ ..] = payload else {
                    return None;
                };
                Some(Frame::Reply(Reply::EchoChunk {
                    idx: *idx,
                    total: *total,
                    chunk: chunk.to_vec(),
                }))
            }
            Cmd::Heartbeat => {
                let counter = u32::from_le_bytes(<[u8; 4]>::try_from(payload).ok()?);
                Some(Frame::Unsolicited(Unsolicited::Heartbeat { counter }))
            }
            Cmd::Battery => {
                let [percent] = payload else { return None };
                let percent = Percent::new(*percent)?;
                Some(Frame::Unsolicited(Unsolicited::Battery { percent }))
            }
        }
    }

    /// The bytes of this frame.
    ///
    /// # Errors
    ///
    /// [`ProtoError::Unsupported`] if an Echo payload or an echo chunk exceeds [`MAX_ECHO`].
    pub fn encode(&self) -> Result<Bytes, ProtoError> {
        match self {
            Frame::Command(command) => command.encode(),
            Frame::Reply(reply) => reply.encode(),
            Frame::Unsolicited(unsolicited) => Ok(unsolicited.encode()),
        }
    }
}

/// `[cmd][N][payload]` for a payload whose length is fixed by its type.
pub(crate) fn fixed<const N: usize>(cmd: Cmd, payload: [u8; N]) -> Bytes {
    const { assert!(N <= 255, "a fixed payload must fit the length byte") };
    let mut bytes = Vec::with_capacity(2 + N);
    bytes.push(cmd as u8);
    // The assertion above makes the fallback unreachable.
    bytes.push(u8::try_from(N).unwrap_or(u8::MAX));
    bytes.extend_from_slice(&payload);
    bytes
}

/// `[cmd][len][payload]` for a payload whose length is only known at run time.
fn variable(cmd: Cmd, payload: &[u8]) -> Result<Bytes, ProtoError> {
    let len =
        u8::try_from(payload.len()).map_err(|_| ProtoError::Unsupported("payload too long"))?;
    let mut bytes = Vec::with_capacity(2 + payload.len());
    bytes.push(cmd as u8);
    bytes.push(len);
    bytes.extend_from_slice(payload);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn round_trip(frame: Frame, bytes: &[u8]) {
        assert_eq!(frame.encode().as_deref(), Ok(bytes));
        assert_eq!(Frame::parse(bytes), Some(frame));
    }

    #[test]
    fn every_kind_round_trips() {
        round_trip(Frame::Command(Command::Ping), &[0x01, 0x00]);
        round_trip(Frame::Command(Command::Get { key: 7 }), &[0x02, 0x01, 7]);
        round_trip(
            Frame::Command(Command::Echo {
                payload: vec![1, 2, 3],
            }),
            &[0x03, 0x03, 1, 2, 3],
        );
        round_trip(
            Frame::Command(Command::Echo { payload: vec![] }),
            &[0x03, 0x00],
        );
        round_trip(Frame::Reply(Reply::Pong), &[0x81, 0x00]);
        round_trip(
            Frame::Reply(Reply::Value {
                key: 7,
                value: 0x0403_0201,
            }),
            &[0x82, 0x05, 7, 1, 2, 3, 4],
        );
        round_trip(
            Frame::Reply(Reply::EchoChunk {
                idx: 1,
                total: 2,
                chunk: vec![9],
            }),
            &[0x83, 0x03, 1, 2, 9],
        );
        round_trip(
            Frame::Unsolicited(Unsolicited::Heartbeat {
                counter: 0x0403_0201,
            }),
            &[0x90, 0x04, 1, 2, 3, 4],
        );
        round_trip(
            Frame::Unsolicited(Unsolicited::Battery {
                percent: Percent::FULL,
            }),
            &[0x91, 0x01, 100],
        );
    }

    #[test]
    fn rejects_bad_layouts() {
        assert_eq!(Frame::parse(&[]), None);
        assert_eq!(Frame::parse(&[0x01]), None);
        assert_eq!(Frame::parse(&[0x01, 0x01]), None);
        assert_eq!(Frame::parse(&[0x01, 0x00, 0x00]), None);
        assert_eq!(Frame::parse(&[0x01, 0x01, 0x00]), None);
        assert_eq!(Frame::parse(&[0x04, 0x00]), None);
        assert_eq!(Frame::parse(&[0x02, 0x00]), None);
        assert_eq!(Frame::parse(&[0x82, 0x04, 1, 2, 3, 4]), None);
        assert_eq!(Frame::parse(&[0x83, 0x01, 0]), None);
        assert_eq!(Frame::parse(&[0x90, 0x03, 1, 2, 3]), None);
        assert_eq!(Frame::parse(&[0x91, 0x01, 101]), None);
        assert_eq!(Frame::parse(&[0x91, 0x02, 1, 2]), None);

        let mut long_echo = vec![0x03, 0xfe];
        long_echo.extend_from_slice(&[0; 254]);
        assert_eq!(Frame::parse(&long_echo), None);
        long_echo[1] = 0xfd;
        long_echo.pop();
        assert!(Frame::parse(&long_echo).is_some());
    }

    #[test]
    fn encode_refuses_oversized_payloads() {
        assert_eq!(
            Command::Echo {
                payload: vec![0; 254]
            }
            .encode(),
            Err(ProtoError::Unsupported("echo: payload too long"))
        );
        assert!(
            Command::Echo {
                payload: vec![0; 253]
            }
            .encode()
            .is_ok()
        );
        assert_eq!(
            Reply::EchoChunk {
                idx: 0,
                total: 1,
                chunk: vec![0; 254],
            }
            .encode(),
            Err(ProtoError::Unsupported("echo: chunk too long"))
        );
    }
}
