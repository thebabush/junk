//! The two integrity checks the rings use: a byte sum on 16-byte frames, CRC-16/MODBUS on
//! big-data bodies.

/// The wrapping sum of `bytes`: the last byte of a 16-byte frame is this over the first 15.
#[must_use]
pub const fn sum8(bytes: &[u8]) -> u8 {
    let mut sum: u8 = 0;
    let mut i = 0;
    while i < bytes.len() {
        sum = sum.wrapping_add(bytes[i]);
        i += 1;
    }
    sum
}

/// CRC-16/MODBUS of `bytes`: init `0xFFFF`, reflected polynomial `0xA001`, no final xor.
///
/// A big-data frame carries this over its body, little-endian; an empty body gives
/// `0xFFFF`, the init value.
#[must_use]
pub const fn crc16_modbus(bytes: &[u8]) -> u16 {
    let mut crc: u16 = 0xffff;
    let mut i = 0;
    while i < bytes.len() {
        crc ^= bytes[i] as u16;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0xa001
            };
            bit += 1;
        }
        i += 1;
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sum8_wraps() {
        assert_eq!(sum8(&[]), 0);
        assert_eq!(sum8(&[0x04, 0x01, 0x12]), 0x17);
        assert_eq!(sum8(&[0xff, 0x01]), 0x00);
        assert_eq!(sum8(&[0xff, 0xff, 0x03]), 0x01);
    }

    // Every vector is a body from the fixtures with the CRC the ring or the app sent.
    #[test]
    fn crc16_modbus_matches_the_fixtures() {
        assert_eq!(crc16_modbus(&[]), 0xffff);
        assert_eq!(crc16_modbus(&[0x00]), 0x40bf);
        assert_eq!(crc16_modbus(&[0x02]), 0x813e);
        assert_eq!(crc16_modbus(&[0xff]), 0x00ff);
        assert_eq!(crc16_modbus(&[0x06, 0x01]), 0xd0c3);
        assert_eq!(crc16_modbus(&[0x01, 0x01]), 0xe0c1);
        assert_eq!(crc16_modbus(&[0x4e, 0x74, 0x21, 0x69]), 0x688f);
    }

    const _: () = assert!(crc16_modbus(&[]) == 0xffff);
    const _: () = assert!(sum8(&[0x04, 0x01, 0x12]) == 0x17);
}
