//! The terminal half of PROTOCOL.md s.2.1: answer a challenge, check the daemon.
//!
//! One definition, shared by the firmware and (byte for byte) by
//! `pcmon/protocol.py::mac_for`. The vectors in the tests below are copied from
//! the Python acceptance tool on purpose: if either side ever changes what it
//! signs -- hexdecoding the nonce, adding a separator, dropping `client_id` --
//! one of the two test suites goes red instead of a board quietly failing to
//! authenticate in the field.
//!
//! What this does NOT do: no I/O, no clock, no knowledge of sockets. Given a
//! key and a nonce it produces 32 hex characters, which is all the wire needs.
//!
//! Why no allocation: `nonce || client_id` is signed with a streaming HMAC, so
//! the concatenation never has to exist in one buffer (the board's DRAM budget
//! is the same 192 KB the stack comes out of -- see B-1).

use crate::sha256::HmacSha256;

/// Algorithm name exactly as it appears in `hello.auth` and `challenge.alg`.
pub const AUTH_ALG: &str = "hmac-sha256/128";

/// Truncated HMAC length in bytes, and the hex length on the wire.
pub const MAC_BYTES: usize = 16;
pub const MAC_HEX: usize = MAC_BYTES * 2;

/// The daemon's half: 64 bits, 16 hex characters.
pub const SRV_BYTES: usize = 8;
pub const SRV_HEX: usize = SRV_BYTES * 2;

/// A nonce is 16 random bytes, i.e. 32 hex characters *as ASCII*.
pub const NONCE_HEX: usize = 32;

/// Label that keeps the daemon's proof from ever colliding with a terminal's
/// (a mac and an srv are both HMACs under the same key).
const SRV_LABEL: &[u8] = b"srv";

const DIGITS: &[u8; 16] = b"0123456789abcdef";

fn write_hex(bytes: &[u8], out: &mut [u8]) -> bool {
    if out.len() < bytes.len() * 2 {
        return false;
    }
    for (i, byte) in bytes.iter().enumerate() {
        out[i * 2] = DIGITS[(byte >> 4) as usize];
        out[i * 2 + 1] = DIGITS[(byte & 0x0f) as usize];
    }
    true
}

/// `mac = HMAC-SHA256(K, nonce_ascii || client_id)[0:16]`, as 32 hex chars.
///
/// `nonce` is the ASCII of the hex string off the wire, **not** the decoded
/// bytes: both ends must sign the same characters, and the wire text is the
/// thing they both actually have.
pub fn mac_into(key: &str, nonce: &str, client_id: &str, out: &mut [u8; MAC_HEX]) -> bool {
    if key.is_empty() || nonce.is_empty() || client_id.is_empty() {
        return false;
    }
    let mut mac = HmacSha256::new(key.as_bytes());
    mac.update(nonce.as_bytes());
    mac.update(client_id.as_bytes());
    let digest = mac.finish();
    write_hex(&digest[..MAC_BYTES], out)
}

/// `srv = HMAC-SHA256(K, "srv" || nonce)[0:8]`, as 16 hex chars.
pub fn srv_into(key: &str, nonce: &str, out: &mut [u8; SRV_HEX]) -> bool {
    if key.is_empty() || nonce.is_empty() {
        return false;
    }
    let mut mac = HmacSha256::new(key.as_bytes());
    mac.update(SRV_LABEL);
    mac.update(nonce.as_bytes());
    let digest = mac.finish();
    write_hex(&digest[..SRV_BYTES], out)
}

/// Constant-time-ish check of the daemon's proof.
///
/// Length is public (it is on the wire), so early-out on length is fine; the
/// byte comparison itself must not stop at the first mismatch, or a fake daemon
/// could learn its guess one character at a time.
pub fn verify_srv(key: &str, nonce: &str, srv: &str) -> bool {
    let mut expected = [0u8; SRV_HEX];
    if !srv_into(key, nonce, &mut expected) {
        return false;
    }
    let given = srv.as_bytes();
    if given.len() != SRV_HEX {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..SRV_HEX {
        diff |= expected[i] ^ given[i];
    }
    diff == 0
}

/// The `auth` line to send once the challenge is answered.
///
/// Builds `{"t":"auth","mac":"<32 hex>"}` into `out`; returns its length, or
/// `None` when `out` is too small (the caller sizes it with [`AUTH_LINE_CAP`]).
pub fn auth_into(mac: &[u8; MAC_HEX], out: &mut [u8]) -> Option<usize> {
    const HEAD: &[u8] = b"{\"t\":\"auth\",\"mac\":\"";
    const TAIL: &[u8] = b"\"}";
    let total = HEAD.len() + MAC_HEX + TAIL.len();
    if out.len() < total {
        return None;
    }
    out[..HEAD.len()].copy_from_slice(HEAD);
    out[HEAD.len()..HEAD.len() + MAC_HEX].copy_from_slice(mac);
    out[HEAD.len() + MAC_HEX..total].copy_from_slice(TAIL);
    Some(total)
}

/// Why a `KW` line was refused. The words go on the wire, so they are short
/// and stable -- an operator greps them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KwError {
    /// Wrong number of characters after `KW `.
    Length,
    /// Right length, but not lowercase hex.
    Hex,
}

impl KwError {
    pub fn word(self) -> &'static str {
        match self {
            KwError::Length => "length",
            KwError::Hex => "hex",
        }
    }
}

/// One line off the UART, classified for pairing (PROTOCOL.md s.2.3).
pub enum KwLine<'a> {
    /// Not a pairing line at all; the caller should treat it as content.
    NotKw,
    /// Starts with `KW` but the key is unusable.
    Bad(KwError),
    /// `KW <32 hex>`: here is the key.
    Key(&'a str),
}

/// Parse `KW <32 hex>`.
///
/// Deliberately strict: exactly one space, exactly [`KEY_HEX_CHARS`] characters,
/// lowercase hex only. A pairing line that "almost" works is worse than one that
/// is refused, because the tool on the other end is waiting for `KOK` and a
/// fuzzy match would leave the board storing one string and the daemon another.
pub fn kw_line(line: &[u8]) -> KwLine<'_> {
    if line.len() < 2 || line[0] != b'K' || line[1] != b'W' {
        return KwLine::NotKw;
    }
    if line.len() < 3 || line[2] != b' ' {
        return KwLine::Bad(KwError::Length);
    }
    let key = &line[3..];
    if key.len() != KEY_HEX_CHARS {
        return KwLine::Bad(KwError::Length);
    }
    for byte in key {
        let ok = matches!(byte, b'0'..=b'9' | b'a'..=b'f');
        if !ok {
            return KwLine::Bad(KwError::Hex);
        }
    }
    match core::str::from_utf8(key) {
        Ok(text) => KwLine::Key(text),
        Err(_) => KwLine::Bad(KwError::Hex),
    }
}

/// Length of a key as text: 32 hex characters (128 bits).
pub const KEY_HEX_CHARS: usize = 32;

/// Upper bound for [`auth_into`]'s buffer: head + mac + tail.
pub const AUTH_LINE_CAP: usize = AUTH_HEAD_LEN + MAC_HEX + 2;

const AUTH_HEAD_LEN: usize = b"{\"t\":\"auth\",\"mac\":\"".len();

#[cfg(test)]
mod tests {
    use super::*;

    /// The same key, nonce and client_id as `pcmon/protocol.py::_selftest_auth`.
    const KEY: &str = "0123456789abcdef";
    const NONCE: &str = "abababababababababababababababab";
    const CLIENT: &str = "esp32-a";
    const EXPECT_MAC: &str = "a45fdad8c13e4ab02e140693b88e4005";
    const EXPECT_SRV: &str = "4f8c924f99c82f32";

    fn mac_buf() -> [u8; MAC_HEX] {
        let mut buf = [0u8; MAC_HEX];
        assert!(mac_into(KEY, NONCE, CLIENT, &mut buf));
        buf
    }

    fn mac_str() -> std::string::String {
        core::str::from_utf8(&mac_buf()).unwrap().to_string()
    }

    #[test]
    fn mac_matches_the_python_side() {
        assert_eq!(mac_str(), EXPECT_MAC, "firmware and daemon disagree about mac");
    }

    #[test]
    fn srv_matches_the_python_side() {
        let mut buf = [0u8; SRV_HEX];
        assert!(srv_into(KEY, NONCE, &mut buf));
        assert_eq!(core::str::from_utf8(&buf).unwrap(), EXPECT_SRV);
    }

    #[test]
    fn srv_verification_rejects_impostors() {
        assert!(verify_srv(KEY, NONCE, EXPECT_SRV));
        assert!(!verify_srv(KEY, NONCE, "0000000000000000"));
        assert!(!verify_srv(KEY, "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd", EXPECT_SRV));
        assert!(!verify_srv("", NONCE, EXPECT_SRV));
        assert!(!verify_srv(KEY, NONCE, "4f8c924f99c82f3"), "short srv");
        assert!(!verify_srv(KEY, NONCE, "4f8c924f99c82f32ff"), "long srv");
    }

    #[test]
    fn mac_is_bound_to_the_client_id() {
        let mut other = [0u8; MAC_HEX];
        assert!(mac_into(KEY, NONCE, "esp32-b", &mut other));
        assert_ne!(core::str::from_utf8(&other).unwrap(), mac_str());
    }

    #[test]
    fn mac_is_bound_to_the_nonce() {
        let mut other = [0u8; MAC_HEX];
        assert!(mac_into(KEY, "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd", CLIENT, &mut other));
        assert_ne!(core::str::from_utf8(&other).unwrap(), mac_str());
    }

    #[test]
    fn empty_inputs_are_refused_rather_than_signed() {
        let mut buf = [0u8; MAC_HEX];
        assert!(!mac_into("", NONCE, CLIENT, &mut buf));
        assert!(!mac_into(KEY, "", CLIENT, &mut buf));
        assert!(!mac_into(KEY, NONCE, "", &mut buf));
    }

    #[test]
    fn auth_line_is_exactly_what_the_protocol_shows() {
        let mac = mac_buf();
        let mut line = [0u8; AUTH_LINE_CAP];
        let len = auth_into(&mac, &mut line).unwrap();
        let text = core::str::from_utf8(&line[..len]).unwrap();
        assert_eq!(
            text,
            "{\"t\":\"auth\",\"mac\":\"a45fdad8c13e4ab02e140693b88e4005\"}"
        );
        assert_eq!(AUTH_LINE_CAP, len);
        // Too small a buffer is a compile-time-sized mistake, not a panic.
        let mut small = [0u8; 8];
        assert!(auth_into(&mac, &mut small).is_none());
    }

    #[test]
    fn kw_line_accepts_exactly_the_documented_shape() {
        let key = "5e1f7c3a9b2d48e6a0c4f8b1d3e50729";
        let line = format!("KW {}", key);
        match kw_line(line.as_bytes()) {
            KwLine::Key(parsed) => assert_eq!(parsed, key),
            _ => panic!("a valid KW line must parse"),
        }
    }

    #[test]
    fn kw_line_refuses_everything_else() {
        // Not a pairing line: the bench renderer owns these.
        assert!(matches!(kw_line(b"CPU 33.5%|TEMP 34C"), KwLine::NotKw));
        assert!(matches!(kw_line(b"KX 5e1f7c3a9b2d48e6a0c4f8b1d3e50729"), KwLine::NotKw));
        // Wrong length (the classic: a 16-character v0.5 token).
        assert!(matches!(kw_line(b"KW 0123456789abcdef"), KwLine::Bad(KwError::Length)));
        assert!(matches!(kw_line(b"KW"), KwLine::Bad(KwError::Length)));
        assert!(matches!(kw_line(b"KW "), KwLine::Bad(KwError::Length)));
        // Right length, not hex -- and uppercase is refused on purpose, because
        // the tool and the firmware must agree byte for byte.
        let mut bad = [b'0'; 35];
        bad[..3].copy_from_slice(b"KW ");
        bad[10] = b'g';
        assert!(matches!(kw_line(&bad), KwLine::Bad(KwError::Hex)));
        assert!(matches!(
            kw_line(b"KW 5E1F7C3A9B2D48E6A0C4F8B1D3E50729"),
            KwLine::Bad(KwError::Hex)
        ));
    }
}
