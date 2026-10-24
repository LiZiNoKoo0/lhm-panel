//! The Wi-Fi credential record: pure bytes, no flash, no WiFi types.
//!
//! What this does NOT do: no reading, no erasing, no writing, no retries, no
//! NVS, no allocation, no logging. It turns an `(ssid, psk)` pair into the 108
//! bytes that `store.rs` drops at offset 0 of one sector, and back -- and it
//! refuses anything it cannot round-trip. The flash side can fail on its own
//! (that is `store.rs`'s job); here the only question is "are these bytes a
//! record I would have written".
//!
//! Layout, little-endian where it matters (`firmware/BACKLOG.md` T3, T5.6):
//!
//! ```text
//! off  len  what
//!   0    4   magic "PWIF"
//!   4    1   ver = 2 (1 = the T4/T5 record, still readable)
//!   5    1   ssid_len (1..=32)
//!   6    1   psk_len  (0, or 8..=63)
//!   7    1   token_len (0..=32; in a `ver = 1` record this byte is 0/flags)
//!   8   32   ssid, right-padded with 0x00
//!  40   64   psk,  right-padded with 0x00 (plaintext on purpose, see SPEC §6)
//! 104   32   token, right-padded with 0x00 (T5.6; empty until the user pairs)
//! 136    4   crc32(0..136), little-endian
//! ```
//!
//! The `ver = 1` record is the same front 108 bytes with the CRC at offset 104
//! and no token field: reading it still works (the board comes up with WiFi and
//! no token), while every write produces `ver = 2`. So an upgrade is "the next
//! save", not a migration pass that could lose what the user typed.
//!
//! The record is a prefix of a 4096-byte sector: `encode` writes 140 bytes and
//! reports that length, it never touches the remaining 3956. Two records live
//! in two sectors (A/B) and only the winner changes; the bytes do not move.

use crate::crc32::crc32;

/// Longest SSID we accept, in bytes (not characters), and the size of the field.
pub const SSID_MAX: usize = 32;
/// Size of the psk field. A psk may be *empty* or up to 63 bytes in it.
pub const PSK_MAX: usize = 64;
/// Longest token we accept, in bytes, and the size of its field. The daemon
/// generates 16 hex characters (`secrets.token_hex(8)`); the slack is there so
/// a longer secret does not mean another storage-format change.
pub const TOKEN_MAX: usize = 32;
/// Record length written by this firmware (`ver = 2`): credentials + token.
pub const RECORD_LEN: usize = 140;
/// Length of the `ver = 1` record T4/T5 wrote: credentials only.
pub const RECORD_LEN_V1: usize = 108;
/// First four bytes. Chosen so a blank (0xFF or 0x00) sector is obviously not a record.
pub const MAGIC: [u8; 4] = *b"PWIF";
/// Format version we write. Bumped only if the layout below changes.
pub const VER: u8 = 2;
/// The version still accepted on read, so an upgraded board keeps its WiFi.
pub const VER_V1: u8 = 1;

/// Shortest non-empty psk. WPA2 passphrases are 8..=63 bytes; below that the
/// router cannot be configured and a "successful" save would be a lie.
const PSK_MIN: usize = 8;
/// Field limits, named so the arithmetic below has no bare numbers.
const CRC_AT: usize = RECORD_LEN - 4;
const CRC_AT_V1: usize = RECORD_LEN_V1 - 4;
const SSID_AT: usize = 8;
const PSK_AT: usize = SSID_AT + SSID_MAX;
const TOKEN_AT: usize = PSK_AT + PSK_MAX;

/// A credentials pair that is known to fit the record.
///
/// Only constructible through [`WifiCreds::new`] or [`WifiCreds::decode`], so a
/// value of this type always satisfies the length rules; that is what lets the
/// accessors be infallible.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct WifiCreds {
    ssid: [u8; SSID_MAX],
    psk: [u8; PSK_MAX],
    token: [u8; TOKEN_MAX],
    ssid_len: u8,
    psk_len: u8,
    token_len: u8,
}

impl WifiCreds {
    /// `None` if the pair cannot be stored: empty or over-long ssid, or a psk
    /// that is neither empty (open network) nor a legal WPA passphrase length.
    pub fn new(ssid: &str, psk: &str) -> Option<Self> {
        let s = ssid.as_bytes();
        let p = psk.as_bytes();
        if !lens_ok(s.len(), p.len()) {
            return None;
        }
        let mut creds = Self {
            ssid: [0u8; SSID_MAX],
            psk: [0u8; PSK_MAX],
            token: [0u8; TOKEN_MAX],
            ssid_len: s.len() as u8,
            psk_len: p.len() as u8,
            token_len: 0,
        };
        creds.ssid[..s.len()].copy_from_slice(s);
        creds.psk[..p.len()].copy_from_slice(p);
        Some(creds)
    }

    /// Same record with an auth token (T5.6). `None` if the token is longer
    /// than its field or carries a byte that cannot sit inside a JSON string:
    /// the token goes into `hello` verbatim, so a quote or a backslash here
    /// would corrupt the frame instead of failing it cleanly.
    pub fn with_token(mut self, token: &str) -> Option<Self> {
        let t = token.as_bytes();
        let printable = t
            .iter()
            .all(|b| (0x20..=0x7E).contains(b) && *b != b'"' && *b != b'\\');
        if t.len() > TOKEN_MAX || !printable {
            return None;
        }
        self.token = [0u8; TOKEN_MAX];
        self.token[..t.len()].copy_from_slice(t);
        self.token_len = t.len() as u8;
        Some(self)
    }

    pub fn ssid(&self) -> &str {
        // Never fails: `new`/`decode` only accept valid UTF-8 and only store
        // its length. `unwrap_or("")` keeps this total instead of panicking.
        core::str::from_utf8(&self.ssid[..self.ssid_len as usize]).unwrap_or("")
    }

    pub fn psk(&self) -> &str {
        core::str::from_utf8(&self.psk[..self.psk_len as usize]).unwrap_or("")
    }

    /// The auth token presented in `hello`. Empty means "not paired yet": the
    /// daemon answers `bad token` unless it was started lenient.
    pub fn token(&self) -> &str {
        core::str::from_utf8(&self.token[..self.token_len as usize]).unwrap_or("")
    }

    pub fn has_token(&self) -> bool {
        self.token_len > 0
    }

    /// Write the record into the front of `out`. Returns how many bytes were
    /// written (`RECORD_LEN`) so the caller passes exactly that to the flash
    /// driver, or `None` if `out` is too small (nothing is written then).
    pub fn encode(&self, out: &mut [u8]) -> Option<usize> {
        if out.len() < RECORD_LEN {
            return None;
        }
        let rec = &mut out[..RECORD_LEN];
        // Zero first: the padding must not inherit whatever the caller's buffer
        // held, and a stale byte would still pass the CRC if we only excluded it.
        for byte in rec.iter_mut() {
            *byte = 0;
        }
        rec[0..4].copy_from_slice(&MAGIC);
        rec[4] = VER;
        rec[5] = self.ssid_len;
        rec[6] = self.psk_len;
        rec[7] = self.token_len;
        rec[SSID_AT..SSID_AT + SSID_MAX].copy_from_slice(&self.ssid);
        rec[PSK_AT..PSK_AT + PSK_MAX].copy_from_slice(&self.psk);
        rec[TOKEN_AT..TOKEN_AT + TOKEN_MAX].copy_from_slice(&self.token);
        let sum = crc32(&rec[..CRC_AT]);
        rec[CRC_AT..RECORD_LEN].copy_from_slice(&sum.to_le_bytes());
        Some(RECORD_LEN)
    }

    /// Parse the front of `bytes`. Trailing bytes (the rest of the sector) are
    /// ignored, so the caller can hand over a whole 4 KiB read.
    ///
    /// Both record versions are accepted: `ver = 2` (with token) and
    /// `ver = 1` (T4/T5, no token field -- the result simply has no token, so a
    /// board upgraded from T4 keeps its WiFi and pairs later).
    ///
    /// `None` on: too short for its version, wrong magic, an unknown version, a
    /// length field outside the layout, a payload that is not UTF-8, or a CRC
    /// that does not cover the bytes we just read.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 8 || bytes[0..4] != MAGIC {
            return None;
        }
        match bytes[4] {
            VER => Self::decode_v2(bytes),
            VER_V1 => Self::decode_v1(bytes),
            _ => None,
        }
    }

    /// `ver = 2`: credentials + token, CRC over 0..136.
    fn decode_v2(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < RECORD_LEN {
            return None;
        }
        let rec = &bytes[..RECORD_LEN];
        let ssid_len = rec[5] as usize;
        let psk_len = rec[6] as usize;
        let token_len = rec[7] as usize;
        // Checked before slicing, and against the layout, not just the array
        // bounds: a record whose lengths we would not have written is corrupt
        // even if the CRC somehow matched.
        if !lens_ok(ssid_len, psk_len) || token_len > TOKEN_MAX {
            return None;
        }
        let mut sum = [0u8; 4];
        sum.copy_from_slice(&rec[CRC_AT..RECORD_LEN]);
        if u32::from_le_bytes(sum) != crc32(&rec[..CRC_AT]) {
            return None;
        }
        let ssid = core::str::from_utf8(&rec[SSID_AT..SSID_AT + ssid_len]).ok()?;
        let psk = core::str::from_utf8(&rec[PSK_AT..PSK_AT + psk_len]).ok()?;
        let token = core::str::from_utf8(&rec[TOKEN_AT..TOKEN_AT + token_len]).ok()?;
        // Rebuild through the constructors so a decoded value cannot differ
        // from a written one in any field this type owns.
        Self::new(ssid, psk)?.with_token(token)
    }

    /// `ver = 1`: credentials only, CRC over 0..104, no token field.
    fn decode_v1(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < RECORD_LEN_V1 {
            return None;
        }
        let rec = &bytes[..RECORD_LEN_V1];
        let ssid_len = rec[5] as usize;
        let psk_len = rec[6] as usize;
        if !lens_ok(ssid_len, psk_len) {
            return None;
        }
        let mut sum = [0u8; 4];
        sum.copy_from_slice(&rec[CRC_AT_V1..RECORD_LEN_V1]);
        if u32::from_le_bytes(sum) != crc32(&rec[..CRC_AT_V1]) {
            return None;
        }
        let ssid = core::str::from_utf8(&rec[SSID_AT..SSID_AT + ssid_len]).ok()?;
        let psk = core::str::from_utf8(&rec[PSK_AT..PSK_AT + psk_len]).ok()?;
        Self::new(ssid, psk)
    }
}

impl core::fmt::Debug for WifiCreds {
    /// SSID in the clear, psk and token only as lengths: this type ends up in
    /// board logs and neither secret belongs in a pasted terminal dump.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("WifiCreds")
            .field("ssid", &self.ssid())
            .field("psk", &format_args!("<hidden {} bytes>", self.psk_len))
            .field("token", &format_args!("<{} bytes>", self.token_len))
            .finish()
    }
}

/// The one place the length rules live, shared by `new` and `decode`.
fn lens_ok(ssid_len: usize, psk_len: usize) -> bool {
    if ssid_len == 0 || ssid_len > SSID_MAX {
        return false;
    }
    psk_len == 0 || (psk_len >= PSK_MIN && psk_len <= PSK_MAX - 1)
}
