//! Transactions: one typed state machine per request kind (SPEC §3.3).

use core::mem;

use junk_core::{Bytes, ProtoError};

use crate::wire::{Command, Reply};
use crate::{Req, Resp};

/// What feeding one reply did to a transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Accepted; more replies are needed.
    Continue,
    /// Finished with this answer.
    Done(Resp),
    /// Finished with this error.
    Fail(ProtoError),
    /// A reply of a kind this transaction is not waiting for. Nothing changed.
    Ignored,
}

/// The in-flight state of one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Txn {
    /// Waiting for [`Reply::Pong`].
    Ping,
    /// Waiting for [`Reply::Value`] with this key.
    Get {
        /// The key that was asked for.
        key: u8,
    },
    /// Collecting [`Reply::EchoChunk`]s.
    Echo(EchoTxn),
}

impl Txn {
    /// Begins the transaction for `req`: its state machine and the command bytes to write.
    ///
    /// # Errors
    ///
    /// [`ProtoError::Unsupported`] if an Echo payload exceeds [`MAX_ECHO`](crate::MAX_ECHO).
    pub fn start(req: Req) -> Result<(Txn, Bytes), ProtoError> {
        let (txn, command) = match req {
            Req::Ping => (Txn::Ping, Command::Ping),
            Req::Get(key) => (Txn::Get { key }, Command::Get { key }),
            Req::Echo(payload) => (Txn::Echo(EchoTxn::default()), Command::Echo { payload }),
        };
        Ok((txn, command.encode()?))
    }

    /// Feeds one reply from the device.
    pub fn feed(&mut self, reply: &Reply) -> Step {
        match (self, reply) {
            (Txn::Ping, Reply::Pong) => Step::Done(Resp::Pong),
            (Txn::Get { key }, Reply::Value { key: got, value }) => {
                if got == key {
                    Step::Done(Resp::Value(*value))
                } else {
                    Step::Fail(ProtoError::Malformed("get: key mismatch"))
                }
            }
            (Txn::Echo(echo), Reply::EchoChunk { idx, total, chunk }) => {
                echo.feed(*idx, *total, chunk)
            }
            (Txn::Ping, Reply::Value { .. } | Reply::EchoChunk { .. })
            | (Txn::Get { .. }, Reply::Pong | Reply::EchoChunk { .. })
            | (Txn::Echo(_), Reply::Pong | Reply::Value { .. }) => Step::Ignored,
        }
    }
}

/// The state of an Echo transaction: chunks received so far.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EchoTxn {
    /// Everything received so far, in order.
    received: Bytes,
    /// The `idx` the next chunk must carry.
    next: u8,
    /// The `total` the first chunk announced; every later chunk must agree.
    total: Option<u8>,
}

impl EchoTxn {
    /// Feeds one chunk.
    fn feed(&mut self, idx: u8, total: u8, chunk: &[u8]) -> Step {
        let Some(last) = total.checked_sub(1) else {
            return Step::Fail(ProtoError::Malformed("echo: empty"));
        };
        if self.total.is_some_and(|announced| announced != total) {
            return Step::Fail(ProtoError::Malformed("echo: total changed"));
        }
        if idx != self.next {
            return Step::Fail(ProtoError::Malformed("echo: out of order"));
        }
        self.total = Some(total);
        self.received.extend_from_slice(chunk);
        if idx == last {
            Step::Done(Resp::Echo(mem::take(&mut self.received)))
        } else {
            // `idx == next <= last <= 254` here, so this cannot overflow.
            self.next = idx + 1;
            Step::Continue
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn chunk(idx: u8, total: u8, chunk: &[u8]) -> Reply {
        Reply::EchoChunk {
            idx,
            total,
            chunk: chunk.to_vec(),
        }
    }

    #[test]
    fn start_builds_the_command_bytes() {
        assert_eq!(Txn::start(Req::Ping), Ok((Txn::Ping, vec![0x01, 0x00])));
        assert_eq!(
            Txn::start(Req::Get(9)),
            Ok((Txn::Get { key: 9 }, vec![0x02, 0x01, 9]))
        );
        assert_eq!(
            Txn::start(Req::Echo(vec![1, 2])),
            Ok((Txn::Echo(EchoTxn::default()), vec![0x03, 0x02, 1, 2]))
        );
        assert_eq!(
            Txn::start(Req::Echo(vec![0; 254])),
            Err(ProtoError::Unsupported("echo: payload too long"))
        );
    }

    #[test]
    fn echo_collects_chunks_in_order() {
        let mut txn = Txn::Echo(EchoTxn::default());
        assert_eq!(txn.feed(&chunk(0, 3, &[1, 2])), Step::Continue);
        assert_eq!(txn.feed(&chunk(1, 3, &[3])), Step::Continue);
        assert_eq!(
            txn.feed(&chunk(2, 3, &[])),
            Step::Done(Resp::Echo(vec![1, 2, 3]))
        );
    }

    #[test]
    fn echo_rejects_bad_sequences() {
        let mut txn = Txn::Echo(EchoTxn::default());
        assert_eq!(
            txn.feed(&chunk(0, 0, &[])),
            Step::Fail(ProtoError::Malformed("echo: empty"))
        );

        let mut txn = Txn::Echo(EchoTxn::default());
        assert_eq!(
            txn.feed(&chunk(1, 2, &[])),
            Step::Fail(ProtoError::Malformed("echo: out of order"))
        );

        let mut txn = Txn::Echo(EchoTxn::default());
        assert_eq!(txn.feed(&chunk(0, 3, &[])), Step::Continue);
        assert_eq!(
            txn.feed(&chunk(1, 2, &[])),
            Step::Fail(ProtoError::Malformed("echo: total changed"))
        );
    }

    #[test]
    fn wrong_kind_is_ignored_and_changes_nothing() {
        let mut txn = Txn::Get { key: 1 };
        assert_eq!(txn.feed(&Reply::Pong), Step::Ignored);
        assert_eq!(txn.feed(&chunk(0, 1, &[])), Step::Ignored);
        assert_eq!(txn, Txn::Get { key: 1 });
        assert_eq!(
            txn.feed(&Reply::Value { key: 2, value: 0 }),
            Step::Fail(ProtoError::Malformed("get: key mismatch"))
        );

        let mut txn = Txn::Ping;
        assert_eq!(txn.feed(&Reply::Value { key: 0, value: 0 }), Step::Ignored);
        assert_eq!(txn.feed(&Reply::Pong), Step::Done(Resp::Pong));
    }
}
