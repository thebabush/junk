//! Transactions: one typed state machine per request kind (SPEC §3.3).
//!
//! Both of this family's requests are a single write answered by a single packet, so a
//! transaction is no more than "which command's reply am I waiting for, and what do I make
//! of it". The shape is `junk_colmi::proto::Txn`'s, with the multi-packet and read-a-
//! characteristic cases it has left out rather than stubbed.

use alloc::vec;
use alloc::vec::Vec;

use junk_core::{Bytes, ProtoError};

use crate::proto::{DeviceInfo, Req, Resp};
use crate::wire::{Command, Packet};

/// What feeding one reply did to a transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Finished with this answer.
    Done(Resp),
    /// Finished with this error.
    Fail(ProtoError),
    /// Not something this transaction is waiting for. Nothing changed.
    Ignored,
}

/// A transaction and the bytes that start it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Started {
    /// The transaction to put in flight.
    pub txn: Txn,
    /// The packet to write.
    pub bytes: Bytes,
}

/// The in-flight state of one request.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Txn {
    /// `0x0101`: the device-info reply, decoded.
    DeviceInfo,
    /// Any command: the first reply carrying it, as it came.
    Raw {
        /// The command the reply must carry.
        command: Command,
    },
}

impl Txn {
    /// The transaction `req` starts and the packet that starts it.
    ///
    /// # Errors
    ///
    /// [`ProtoError::Unsupported`] for a [`Req::Raw`] whose payload does not fit the
    /// protocol's `u16` length field.
    pub fn start(req: Req) -> Result<Started, ProtoError> {
        let (txn, command, payload) = match req {
            Req::DeviceInfo => (Txn::DeviceInfo, Command::GetDeviceInfo, Vec::new()),
            Req::Raw { command, payload } => (Txn::Raw { command }, command, payload),
        };
        Ok(Started {
            txn,
            bytes: encode(command, &payload)?,
        })
    }

    /// The command whose reply this transaction is waiting for.
    #[must_use]
    pub const fn wants(self) -> Command {
        match self {
            Txn::DeviceInfo => Command::GetDeviceInfo,
            Txn::Raw { command } => command,
        }
    }

    /// Feeds one decoded packet. `raw` is the same packet as it came off the stream, which
    /// is what [`Resp::Raw`] carries.
    #[must_use]
    pub fn feed(self, packet: &Packet<'_>, raw: &[u8]) -> Step {
        if packet.command.raw() != self.wants().raw() {
            return Step::Ignored;
        }
        match self {
            Txn::DeviceInfo => match DeviceInfo::parse(packet.payload) {
                Ok(info) => Step::Done(Resp::DeviceInfo(info)),
                Err(err) => Step::Fail(err),
            },
            Txn::Raw { .. } => Step::Done(Resp::Raw(raw.to_vec())),
        }
    }
}

/// `command` and `payload` as a host packet.
fn encode(command: Command, payload: &[u8]) -> Result<Bytes, ProtoError> {
    let packet = Packet::host(command, payload);
    let mut bytes = vec![0; packet.encoded_len()];
    packet
        .encode_into(&mut bytes)
        // The buffer is exactly `encoded_len()`, so the only failure left is a payload too
        // long for the `u16` length field.
        .map_err(|_| ProtoError::Unsupported("raw: payload too long for the length field"))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::captured::{PAYLOAD, REPLY, REQUEST};
    use alloc::vec;

    #[test]
    fn device_info_starts_with_the_ten_bytes_the_probe_sent() {
        let started = Txn::start(Req::DeviceInfo).expect("no payload to overflow");
        assert_eq!(started.txn, Txn::DeviceInfo);
        assert_eq!(started.bytes, REQUEST);
    }

    #[test]
    fn device_info_answers_on_the_captured_reply() {
        let packet = Packet::decode(&REPLY).expect("the speaker's own reply");
        let Step::Done(Resp::DeviceInfo(info)) = Txn::DeviceInfo.feed(&packet, &REPLY) else {
            panic!("the reply carries 0x0101");
        };
        assert_eq!(info.firmware, "3.0.4");
    }

    #[test]
    fn device_info_fails_on_a_reply_it_cannot_read() {
        let mut payload = PAYLOAD;
        payload[1] = 9;
        let packet = Packet::device(Command::GetDeviceInfo, &payload);
        let mut bytes = vec![0; packet.encoded_len()];
        let encoded = packet.encode_into(&mut bytes).expect("fits");
        assert_eq!(
            Txn::DeviceInfo.feed(&packet, encoded),
            Step::Fail(ProtoError::Malformed(
                "device info: battery is not a count of fifths"
            ))
        );
    }

    #[test]
    fn a_transaction_ignores_another_commands_reply() {
        let packet = Packet::device(Command::NotifyBatteryInfo, &[4]);
        assert_eq!(Txn::DeviceInfo.feed(&packet, &[]), Step::Ignored);
        assert_eq!(
            Txn::Raw {
                command: Command::GetEqualizer
            }
            .feed(&packet, &[]),
            Step::Ignored
        );
    }

    #[test]
    fn raw_answers_with_the_packet_as_it_came() {
        let started = Txn::start(Req::Raw {
            command: Command::GetEqualizer,
            payload: vec![],
        })
        .expect("no payload to overflow");
        assert_eq!(
            started.txn,
            Txn::Raw {
                command: Command::GetEqualizer
            }
        );
        assert_eq!(
            started.bytes,
            [0x08, 0xee, 0, 0, 0, 0x02, 0x89, 0x0a, 0, 0x8b]
        );

        let packet = Packet::device(Command::GetEqualizer, &[1, 2, 3]);
        assert_eq!(
            started.txn.feed(&packet, &[0xde, 0xad]),
            Step::Done(Resp::Raw(vec![0xde, 0xad]))
        );
    }

    #[test]
    fn a_raw_payload_too_long_for_the_length_field_is_refused() {
        let err = Txn::start(Req::Raw {
            command: Command::Other(0x1234),
            payload: vec![0; usize::from(u16::MAX)],
        })
        .expect_err("ten header bytes plus 65535 does not fit a u16");
        assert_eq!(
            err,
            ProtoError::Unsupported("raw: payload too long for the length field")
        );
    }
}
