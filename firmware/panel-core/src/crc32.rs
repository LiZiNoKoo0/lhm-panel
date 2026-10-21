//! CRC-32/IEEE, bitwise.
//!
//! What this does NOT do: no table, no allocation, no hardware CRC unit.
//! Same polynomial, init and final xor as `zlib.crc32` on the PC side, so the
//! number printed over the wire can be compared with a Python one directly.
//!
//! Swapping in a 256-entry table later is safe: it produces the *same* values
//! (same polynomial), it is only ~8x faster. Verify with the tests in this file
//! before you do it.

const POLY: u32 = 0xEDB8_8320;

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (POLY & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::crc32;

    // Values from Python: zlib.crc32(b"...") & 0xFFFFFFFF
    #[test]
    fn empty_is_zero() {
        assert_eq!(crc32(b""), 0x0000_0000);
    }

    #[test]
    fn matches_zlib() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b"hello"), 0x3610_A686);
        assert_eq!(crc32(&[0u8; 32]), 0x190A_55AD);
    }
}
