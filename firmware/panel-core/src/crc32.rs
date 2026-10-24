//! CRC-32/IEEE, bitwise.
//!
//! What this does NOT do: no table, no allocation, no hardware CRC unit.
//! Same polynomial, init and final xor as `zlib.crc32` on the PC side, so the
//! number printed over the wire can be compared with a Python one directly.
//!
//! Swapping in a 256-entry table later is safe: it produces the *same* values
//! (same polynomial), it is only ~8x faster. Verify with the tests in this file
//! before you do that.

const POLY: u32 = 0xEDB8_8320;
const INIT: u32 = 0xFFFF_FFFF;

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = Crc32::new();
    crc.update(data);
    crc.finish()
}

/// The same CRC, fed in pieces.
///
/// Exists so a frame whose CRC field sits *in front of* the bytes it covers does
/// not have to be copied into scratch memory to be sealed or checked: hashing
/// `header` then `payload` in two calls gives the value that hashing the
/// concatenation would give (there is a test for exactly that, because "should
/// be the same" is how CRC bugs get shipped).
#[derive(Clone, Copy)]
pub struct Crc32 {
    state: u32,
}

impl Crc32 {
    pub fn new() -> Self {
        Self { state: INIT }
    }

    pub fn update(&mut self, data: &[u8]) {
        let mut crc = self.state;
        for &byte in data {
            crc ^= byte as u32;
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (POLY & mask);
            }
        }
        self.state = crc;
    }

    pub fn finish(self) -> u32 {
        !self.state
    }
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{crc32, Crc32};

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

    #[test]
    fn piecewise_equals_oneshot() {
        let data: Vec<u8> = (0..=255u8).chain(b"PCFG and a payload".iter().copied()).collect();
        let one = crc32(&data);
        for split in [0, 1, 7, 13, 15, 200, data.len()] {
            let mut c = Crc32::new();
            c.update(&data[..split]);
            c.update(&data[split..]);
            assert_eq!(c.finish(), one, "split at {split}");
        }
    }
}
