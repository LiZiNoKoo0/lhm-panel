//! SHA-256 and HMAC-SHA256 in `no_std`, no allocation, no dependencies.
//!
//! Why hand-rolled instead of a crate: the protocol needs exactly one use of
//! this (sign one 32-byte nonce per connection). Pulling a crypto crate into an
//! `esp-hal 1.1` build risks a second dependency tree for a device that has
//! 21 KB of stack and a very specific toolchain; and a second implementation is
//! precisely what this project keeps paying for. Forty lines of well-known
//! arithmetic, pinned by NIST vectors, is the smaller risk.
//!
//! What this does NOT do: no SHA-224/384/512, no incremental API for callers
//! outside `auth`, no constant-time guarantees beyond the MAC comparison in
//! `auth::verify_srv` (nothing here branches on secret data anyway).

use core::convert::TryInto;

/// FIPS 180-4 round constants.
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// Incremental SHA-256 state. `update` may be called any number of times.
pub struct Sha256 {
    state: [u32; 8],
    buf: [u8; 64],
    buf_len: usize,
    total: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub const fn new() -> Self {
        Self {
            state: H0,
            buf: [0u8; 64],
            buf_len: 0,
            total: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);

        // Top up a partially filled block first.
        if self.buf_len > 0 {
            let need = 64 - self.buf_len;
            let take = if data.len() < need { data.len() } else { need };
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == 64 {
                let block = self.buf;
                self.compress(&block);
                self.buf_len = 0;
            }
        }

        // Whole blocks straight out of the caller's buffer.
        while data.len() >= 64 {
            let (block, rest) = data.split_at(64);
            self.compress(block);
            data = rest;
        }

        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }

    pub fn finish(mut self) -> [u8; 32] {
        let bit_len = self.total.wrapping_mul(8);

        // Pad: 0x80, zeros, then the 64-bit big-endian length.
        self.buf[self.buf_len] = 0x80;
        self.buf_len += 1;
        if self.buf_len > 56 {
            for byte in self.buf[self.buf_len..].iter_mut() {
                *byte = 0;
            }
            let block = self.buf;
            self.compress(&block);
            self.buf_len = 0;
        }
        for byte in self.buf[self.buf_len..56].iter_mut() {
            *byte = 0;
        }
        self.buf[56..64].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.buf;
        self.compress(&block);

        let mut out = [0u8; 32];
        for (i, word) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            let word: [u8; 4] = block[i * 4..i * 4 + 4].try_into().unwrap();
            w[i] = u32::from_be_bytes(word);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = self.state[0];
        let mut b = self.state[1];
        let mut c = self.state[2];
        let mut d = self.state[3];
        let mut e = self.state[4];
        let mut f = self.state[5];
        let mut g = self.state[6];
        let mut h = self.state[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }
}

/// One-shot SHA-256.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finish()
}

const BLOCK: usize = 64;

/// Incremental HMAC-SHA256 (RFC 2104), so callers can sign `nonce || client_id`
/// without building the concatenation in RAM.
pub struct HmacSha256 {
    inner: Sha256,
    outer_key: [u8; BLOCK],
}

impl HmacSha256 {
    pub fn new(key: &[u8]) -> Self {
        let mut block = [0u8; BLOCK];
        if key.len() > BLOCK {
            block[..32].copy_from_slice(&sha256(key));
        } else {
            block[..key.len()].copy_from_slice(key);
        }

        let mut inner = Sha256::new();
        let mut ipad = [0u8; BLOCK];
        let mut outer_key = [0u8; BLOCK];
        for i in 0..BLOCK {
            ipad[i] = block[i] ^ 0x36;
            outer_key[i] = block[i] ^ 0x5c;
        }
        inner.update(&ipad);
        Self { inner, outer_key }
    }

    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    pub fn finish(self) -> [u8; 32] {
        let inner_digest = self.inner.finish();
        let mut outer = Sha256::new();
        outer.update(&self.outer_key);
        outer.update(&inner_digest);
        outer.finish()
    }
}

/// One-shot HMAC-SHA256.
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new(key);
    mac.update(msg);
    mac.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> [u8; 64] {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut out = [0u8; 64];
        for (i, byte) in bytes.iter().enumerate() {
            out[i * 2] = DIGITS[(byte >> 4) as usize];
            out[i * 2 + 1] = DIGITS[(byte & 0x0f) as usize];
        }
        out
    }

    fn hex_str(bytes: &[u8]) -> std::string::String {
        let raw = hex(bytes);
        core::str::from_utf8(&raw).unwrap().to_string()
    }

    #[test]
    fn sha256_nist_vectors() {
        assert_eq!(
            hex_str(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex_str(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex_str(&sha256(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn sha256_streaming_matches_one_shot() {
        let data: [u8; 200] = core::array::from_fn(|i| (i * 7) as u8);
        let one_shot = sha256(&data);
        for chunk in [1usize, 3, 63, 64, 65, 127] {
            let mut hasher = Sha256::new();
            for piece in data.chunks(chunk) {
                hasher.update(piece);
            }
            assert_eq!(hasher.finish(), one_shot, "chunk size {}", chunk);
        }
    }

    #[test]
    fn hmac_rfc4231_vectors() {
        // Case 1
        let key = [0x0bu8; 20];
        assert_eq!(
            hex_str(&hmac_sha256(&key, b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        // Case 2
        assert_eq!(
            hex_str(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // Case 6: a key longer than the block size must be hashed first.
        let long_key = [0xaau8; 131];
        assert_eq!(
            hex_str(&hmac_sha256(
                &long_key,
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn hmac_streaming_matches_one_shot() {
        let key = b"0123456789abcdef";
        let mut mac = HmacSha256::new(key);
        mac.update(b"abab");
        mac.update(b"cdef");
        assert_eq!(mac.finish(), hmac_sha256(key, b"ababcdef"));
    }
}
