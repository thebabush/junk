//! Steps: one command out, one reply back, and the rule that says which frame is the reply.

use junk_core::Bytes;

use crate::proto::slot::Slot;
use crate::wire::{DataType, Frame};

/// Which incoming frame answers a step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Expect {
    /// A frame of the step's data type whose payload starts with command id `id` and then
    /// the bytes of `key`: the group's inquired type, or for voice guidance two of them.
    Reply { id: u8, key: Bytes },
    /// The next frame of the step's data type, whatever it says: a raw request does not
    /// know what to expect.
    NextData,
}

/// One command and what it is waiting for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Step {
    /// The data type the command goes out with, and the reply must come back with.
    pub(crate) data_type: DataType,
    /// The command's payload.
    pub(crate) payload: Bytes,
    /// Which frame is the reply.
    pub(crate) expect: Expect,
    /// Where the reply goes.
    pub(crate) slot: Slot,
}

impl Step {
    /// A table-one GET: `payload` out, and the reply `reply` with `key` after it back.
    pub(crate) fn one(payload: &[u8], reply: u8, key: &[u8], slot: Slot) -> Self {
        Self::get(DataType::DataMdr, payload, reply, key, slot)
    }

    /// A table-two GET.
    pub(crate) fn two(payload: &[u8], reply: u8, key: &[u8], slot: Slot) -> Self {
        Self::get(DataType::DataMdrNo2, payload, reply, key, slot)
    }

    fn get(data_type: DataType, payload: &[u8], reply: u8, key: &[u8], slot: Slot) -> Self {
        Self {
            data_type,
            payload: payload.to_vec(),
            expect: Expect::Reply {
                id: reply,
                key: key.to_vec(),
            },
            slot,
        }
    }

    /// Whether `frame` is this step's reply.
    pub(crate) fn wants(&self, frame: &Frame) -> bool {
        if frame.data_type != self.data_type {
            return false;
        }
        match &self.expect {
            Expect::NextData => true,
            Expect::Reply { id, key } => match frame.payload.split_first() {
                Some((first, rest)) => first == id && rest.starts_with(key),
                None => false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn frame(data_type: DataType, payload: &[u8]) -> Frame {
        Frame::new(data_type, 0, payload.to_vec())
    }

    #[test]
    fn a_reply_must_match_type_id_and_key() {
        let step = Step::one(&[0x10, 0x00], 0x11, &[0x00], Slot::Battery);
        assert!(step.wants(&frame(DataType::DataMdr, &[0x11, 0x00, 0x50, 0x00])));
        assert!(!step.wants(&frame(DataType::DataMdr, &[0x13, 0x00, 0x50, 0x00])));
        assert!(!step.wants(&frame(DataType::DataMdr, &[0x11, 0x01, 0x50, 0x00])));
        assert!(!step.wants(&frame(DataType::DataMdrNo2, &[0x11, 0x00])));
        assert!(!step.wants(&frame(DataType::DataMdr, &[])));
        assert!(!step.wants(&frame(DataType::DataMdr, &[0x11])));
    }

    #[test]
    fn next_data_takes_any_frame_of_the_type() {
        let step = Step {
            data_type: DataType::DataMdr,
            payload: vec![0x99],
            expect: Expect::NextData,
            slot: Slot::CapRaw,
        };
        assert!(step.wants(&frame(DataType::DataMdr, &[])));
        assert!(step.wants(&frame(DataType::DataMdr, &[0x42, 0x01])));
        assert!(!step.wants(&frame(DataType::DataMdrNo2, &[0x42])));
    }
}
