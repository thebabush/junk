//! Bluetooth device addresses, as `IOBluetooth` wants to be given them. Pure.

use crate::RfcommError;

/// The number of octets in a Bluetooth device address.
const OCTETS: usize = 6;

/// `address` in the one form `IOBluetooth` accepts, `XX:XX:XX:XX:XX:XX` in upper case.
///
/// Six hex octets separated by `:` or `-`, in either case, are all accepted and normalised;
/// anything else is rejected here rather than handed to the framework, which answers a
/// malformed address with a null device and no reason.
///
/// # Errors
///
/// [`RfcommError::BadAddress`] if `address` is not six separated hex octets.
pub fn normalise(address: &str) -> Result<String, RfcommError> {
    let bad = || RfcommError::BadAddress {
        got: address.to_owned(),
    };
    let mut octets = Vec::with_capacity(OCTETS);
    for part in address.split([':', '-']) {
        let [high, low] = part.as_bytes() else {
            return Err(bad());
        };
        if !high.is_ascii_hexdigit() || !low.is_ascii_hexdigit() {
            return Err(bad());
        }
        octets.push(part.to_ascii_uppercase());
    }
    if octets.len() != OCTETS {
        return Err(bad());
    }
    Ok(octets.join(":"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_speaker_is_accepted_as_written() {
        assert_eq!(
            normalise("F4:2B:7D:A1:B2:C3").as_deref(),
            Ok("F4:2B:7D:A1:B2:C3")
        );
    }

    #[test]
    fn case_and_separator_are_normalised() {
        for written in [
            "f4:2b:7d:a1:b2:c3",
            "F4-2B-7D-A1-B2-C3",
            "f4-2B-7d-a1-B2-c3",
        ] {
            assert_eq!(
                normalise(written).as_deref(),
                Ok("F4:2B:7D:A1:B2:C3"),
                "{written}"
            );
        }
    }

    #[test]
    fn anything_else_is_a_bad_address() {
        for written in [
            "",
            "F4:2B:7D:A1:B2",             // five octets
            "F4:2B:7D:A1:B2:C3:00",       // seven
            "F4:2B:7D:A1:B2:C",           // a half octet
            "F4:2B:7D:A1:B2:0C3",         // an octet and a half
            "F4:2B:7D:A1:B2:CG",          // not hex
            "F42B7DA1B2C3",               // no separators
            "F4 2B 7D 38 32 63",          // the wrong separator
            "soundcore Motion 300",       // a name, not an address
            "0cf12d31-fac3-4553-bd80-d6", // a UUID's opening
        ] {
            assert_eq!(
                normalise(written),
                Err(RfcommError::BadAddress {
                    got: written.to_owned()
                }),
                "{written:?} should not be an address"
            );
        }
    }
}
