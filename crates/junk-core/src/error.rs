//! Why a request failed.

use core::fmt;

/// Why a request could not be answered.
///
/// Carried in [`Output::Done`](crate::Output::Done); never a panic (invariant 5).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ProtoError {
    /// The device did not answer in time. Raised by the driver from its own timer.
    Timeout,
    /// The link dropped while the request was in flight.
    Disconnected,
    /// A frame arrived with a bad checksum or CRC.
    Checksum,
    /// A frame did not have the layout the protocol expects; the text says which.
    Malformed(&'static str),
    /// The device, dialect or driver cannot do this; the text says what.
    Unsupported(&'static str),
    /// The driver cannot take this request right now.
    Busy,
}

impl fmt::Display for ProtoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtoError::Timeout => f.write_str("timed out waiting for the device"),
            ProtoError::Disconnected => f.write_str("link dropped while the request was in flight"),
            ProtoError::Checksum => f.write_str("checksum mismatch"),
            ProtoError::Malformed(what) => write!(f, "malformed frame: {what}"),
            ProtoError::Unsupported(what) => write!(f, "unsupported: {what}"),
            ProtoError::Busy => f.write_str("driver is busy"),
        }
    }
}

impl core::error::Error for ProtoError {}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeSet;
    use alloc::string::{String, ToString};

    #[test]
    fn display_is_non_empty_and_distinct() {
        let all = [
            ProtoError::Timeout,
            ProtoError::Disconnected,
            ProtoError::Checksum,
            ProtoError::Malformed("short"),
            ProtoError::Unsupported("big data"),
            ProtoError::Busy,
        ];
        let strings: BTreeSet<String> = all.iter().map(ToString::to_string).collect();
        assert_eq!(strings.len(), all.len());
        assert!(strings.iter().all(|s| !s.is_empty()));
        assert!(ProtoError::Malformed("short").to_string().contains("short"));
        assert!(
            ProtoError::Unsupported("big data")
                .to_string()
                .contains("big data")
        );
    }
}
