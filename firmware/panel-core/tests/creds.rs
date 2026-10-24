//! The credential record, pinned byte for byte, in both versions.
//!
//! Every expected byte string below came from `zlib.crc32` on the PC side, the
//! same way `parity.rs` pins frames: the record that lands in flash is compared
//! with the one a Python script can produce, not with "whatever encode did last
//! time". `firmware/BACKLOG.md` T3 (v1) and T5.6 (v2) are the spec.
//!
//! The recipe, unchanged from T3 except for the token field:
//!
//! ```text
//! h = b"PWIF" + bytes([2, len(ssid), len(psk), len(token)])
//!   + ssid.ljust(32,b"\0") + psk.ljust(64,b"\0") + token.ljust(32,b"\0")
//! rec = h + (zlib.crc32(h) & 0xFFFFFFFF).to_bytes(4, "little")
//! ```

use panel_core::creds::{
    WifiCreds, MAGIC, PSK_MAX, RECORD_LEN, RECORD_LEN_V1, SSID_MAX, TOKEN_MAX, VER, VER_V1,
};
use panel_core::crc32::crc32;

/// The `ver = 1` record T4/T5 wrote: `("mywifi", "hunter2hunter2")`, no token.
/// Kept pinned forever -- reading it is what lets an upgraded board keep its
/// WiFi without asking the user for the password again.
const MYWIFI_V1: [u8; RECORD_LEN_V1] = [
    0x50, 0x57, 0x49, 0x46, 0x01, 0x06, 0x0e, 0x00, 0x6d, 0x79, 0x77, 0x69, 0x66, 0x69, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x68, 0x75, 0x6e, 0x74, 0x65, 0x72, 0x32, 0x68,
    0x75, 0x6e, 0x74, 0x65, 0x72, 0x32, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xae, 0x32, 0xff, 0xe4,
];

/// The `ver = 2` record this firmware writes: same pair plus the token
/// `0123456789abcdef` (what `secrets.token_hex(8)` produces).
const MYWIFI: [u8; RECORD_LEN] = [
    0x50, 0x57, 0x49, 0x46, 0x02, 0x06, 0x0e, 0x10, 0x6d, 0x79, 0x77, 0x69, 0x66, 0x69, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x68, 0x75, 0x6e, 0x74, 0x65, 0x72, 0x32, 0x68,
    0x75, 0x6e, 0x74, 0x65, 0x72, 0x32, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37,
    0x38, 0x39, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x82, 0x52, 0x0f, 0xf0,
];

const TOKEN: &str = "0123456789abcdef";

fn paired() -> WifiCreds {
    WifiCreds::new("mywifi", "hunter2hunter2")
        .unwrap()
        .with_token(TOKEN)
        .unwrap()
}

fn encoded(creds: &WifiCreds) -> Vec<u8> {
    let mut buf = [0u8; RECORD_LEN];
    let n = creds.encode(&mut buf).expect("buffer is exactly one record");
    assert_eq!(n, RECORD_LEN);
    buf.to_vec()
}

#[test]
fn layout_constants_are_the_documented_numbers() {
    assert_eq!(SSID_MAX, 32);
    assert_eq!(PSK_MAX, 64);
    assert_eq!(TOKEN_MAX, 32);
    assert_eq!(RECORD_LEN, 140);
    assert_eq!(RECORD_LEN_V1, 108);
    assert_eq!(&MAGIC, b"PWIF");
    assert_eq!(VER, 2);
    assert_eq!(VER_V1, 1);
}

#[test]
fn roundtrip_keeps_all_three_fields() {
    let back = WifiCreds::decode(&encoded(&paired())).unwrap();
    assert_eq!(back.ssid(), "mywifi");
    assert_eq!(back.psk(), "hunter2hunter2");
    assert_eq!(back.token(), TOKEN);
    assert!(back.has_token());
    assert_eq!(back, paired());
}

#[test]
fn encoded_bytes_match_the_pc_reference() {
    // Byte-for-byte, including the right padding to 32 + 64 + 32 and the
    // trailing little-endian CRC. If this fails, flash and the Python tool
    // disagree about the record.
    assert_eq!(encoded(&paired()), MYWIFI);
    assert_eq!(&MYWIFI[136..140], &[0x82, 0x52, 0x0f, 0xf0]);
    assert_eq!(
        u32::from_le_bytes([MYWIFI[136], MYWIFI[137], MYWIFI[138], MYWIFI[139]]),
        0xf00f_5282
    );
    assert_eq!(crc32(&MYWIFI[..136]), 0xf00f_5282);
}

#[test]
fn a_v1_record_still_reads_and_simply_has_no_token() {
    // The upgrade path: a board flashed with T4/T5 firmware has this on flash.
    let back = WifiCreds::decode(&MYWIFI_V1).expect("v1 must stay readable");
    assert_eq!(back.ssid(), "mywifi");
    assert_eq!(back.psk(), "hunter2hunter2");
    assert_eq!(back.token(), "");
    assert!(!back.has_token());
    // ...and re-encoding it produces the v2 layout, so "the next save" is the
    // upgrade -- no separate migration pass to get wrong.
    assert_eq!(encoded(&back).len(), RECORD_LEN);
}

#[test]
fn encode_zeroes_padding_even_into_a_dirty_buffer() {
    let mut buf = [0xAAu8; RECORD_LEN];
    assert_eq!(paired().encode(&mut buf), Some(RECORD_LEN));
    assert_eq!(buf, MYWIFI);
}

#[test]
fn encode_leaves_the_rest_of_the_sector_alone() {
    let mut sector = [0x5Au8; 4096];
    assert_eq!(paired().encode(&mut sector), Some(RECORD_LEN));
    assert_eq!(&sector[..RECORD_LEN], &MYWIFI);
    assert!(sector[RECORD_LEN..].iter().all(|&b| b == 0x5A));
}

#[test]
fn encode_rejects_a_short_buffer_without_writing() {
    let mut tiny = [0u8; RECORD_LEN - 1];
    assert_eq!(paired().encode(&mut tiny), None);
    assert!(tiny.iter().all(|&b| b == 0), "nothing may be written");
    let mut empty: [u8; 0] = [];
    assert_eq!(paired().encode(&mut empty), None);
    let mut exact = [0u8; RECORD_LEN];
    assert_eq!(paired().encode(&mut exact), Some(RECORD_LEN));
}

#[test]
fn an_empty_token_writes_zero_length_and_is_still_a_v2_record() {
    let creds = WifiCreds::new("mywifi", "hunter2hunter2").unwrap();
    let bytes = encoded(&creds);
    assert_eq!(bytes[4], VER);
    assert_eq!(bytes[7], 0, "token_len");
    let back = WifiCreds::decode(&bytes).unwrap();
    assert_eq!(back.token(), "");
    assert!(!back.has_token());
}

#[test]
fn tokens_longer_than_the_field_are_refused() {
    let base = WifiCreds::new("mywifi", "hunter2hunter2").unwrap();
    assert!(base.with_token(&"t".repeat(TOKEN_MAX)).is_some());
    assert!(base.with_token(&"t".repeat(TOKEN_MAX + 1)).is_none());
}

#[test]
fn tokens_that_would_break_the_json_are_refused() {
    // The token goes into `hello` verbatim, so these must fail at the door
    // rather than produce a frame the daemon cannot parse.
    let base = WifiCreds::new("mywifi", "hunter2hunter2").unwrap();
    assert!(base.with_token("has space").is_some(), "spaces are legal");
    for bad in ["with\"quote", "back\\slash", "line\nbreak", "tab\there", "nul\0byte"] {
        assert!(base.with_token(bad).is_none(), "{bad:?} should be refused");
    }
}

#[test]
fn an_oversized_token_len_in_the_record_is_refused_even_with_a_valid_crc() {
    let mut bytes = MYWIFI;
    bytes[7] = (TOKEN_MAX + 1) as u8;
    re_stamp_v2(&mut bytes);
    assert!(WifiCreds::decode(&bytes).is_none());
}

#[test]
fn empty_ssid_is_refused() {
    assert!(WifiCreds::new("", "").is_none());
    assert!(WifiCreds::new("", "hunter2hunter2").is_none());
}

#[test]
fn ssid_length_boundary_is_32() {
    assert!(WifiCreds::new(&"x".repeat(SSID_MAX), "").is_some());
    assert!(WifiCreds::new(&"x".repeat(SSID_MAX + 1), "").is_none());
    let wide = "é".repeat(17); // 34 bytes, 17 chars
    assert!(WifiCreds::new(&wide, "").is_none());
}

#[test]
fn psk_length_rules_are_empty_or_8_to_63() {
    assert!(WifiCreds::new("mywifi", "").is_some(), "open network");
    assert!(WifiCreds::new("mywifi", &"p".repeat(7)).is_none());
    assert!(WifiCreds::new("mywifi", &"p".repeat(8)).is_some());
    assert!(WifiCreds::new("mywifi", &"p".repeat(63)).is_some());
    assert!(
        WifiCreds::new("mywifi", &"p".repeat(PSK_MAX)).is_none(),
        "64 bytes fills the field and leaves no room for a terminator"
    );
}

#[test]
fn open_network_roundtrips_as_an_empty_psk() {
    let creds = WifiCreds::new("cafe-wifi", "").unwrap();
    let bytes = encoded(&creds);
    assert_eq!(bytes[5], 9, "ssid_len");
    assert_eq!(bytes[6], 0, "psk_len");
    let back = WifiCreds::decode(&bytes).unwrap();
    assert_eq!(back.ssid(), "cafe-wifi");
    assert_eq!(back.psk(), "");
}

#[test]
fn longest_accepted_everything_roundtrips() {
    let ssid = "x".repeat(SSID_MAX);
    let psk = "p".repeat(PSK_MAX - 1);
    let token = "t".repeat(TOKEN_MAX);
    let creds = WifiCreds::new(&ssid, &psk).unwrap().with_token(&token).unwrap();
    let back = WifiCreds::decode(&encoded(&creds)).unwrap();
    assert_eq!(back.ssid(), ssid);
    assert_eq!(back.psk(), psk);
    assert_eq!(back.token(), token);
}

#[test]
fn decode_rejects_a_slice_shorter_than_the_version_needs() {
    assert!(WifiCreds::decode(&[]).is_none());
    assert!(WifiCreds::decode(&[0x50, 0x57, 0x49, 0x46]).is_none(), "magic only");
    assert!(WifiCreds::decode(&MYWIFI[..RECORD_LEN - 1]).is_none(), "v2 cut short");
    assert!(WifiCreds::decode(&MYWIFI_V1[..RECORD_LEN_V1 - 1]).is_none(), "v1 cut short");
    // A whole sector is fine: trailing bytes are ignored.
    let mut sector = [0u8; 4096];
    sector[..RECORD_LEN].copy_from_slice(&MYWIFI);
    assert!(WifiCreds::decode(&sector).is_some());
}

#[test]
fn decode_rejects_a_bad_magic_or_an_unknown_version() {
    let mut bytes = MYWIFI;
    bytes[0] = b'X';
    assert!(WifiCreds::decode(&bytes).is_none());

    // A blank sector must not read as an empty network.
    assert!(WifiCreds::decode(&[0xFFu8; RECORD_LEN]).is_none());
    assert!(WifiCreds::decode(&[0u8; RECORD_LEN]).is_none());

    // 3 is not a version anyone has written.
    let mut bytes = MYWIFI;
    bytes[4] = 3;
    re_stamp_v2(&mut bytes);
    assert!(WifiCreds::decode(&bytes).is_none());
}

#[test]
fn decode_rejects_a_flipped_payload_byte() {
    // Every payload byte is covered by the CRC, so no single flip can pass.
    for at in [5usize, 6, 7, 8, 39, 40, 60, 103, 104, 135] {
        let mut bytes = MYWIFI;
        bytes[at] ^= 0x01;
        assert!(
            WifiCreds::decode(&bytes).is_none(),
            "byte {at} flipped but still decoded"
        );
    }
}

#[test]
fn decode_rejects_a_flipped_crc_in_either_version() {
    let mut bytes = MYWIFI;
    bytes[139] ^= 0x80;
    assert!(WifiCreds::decode(&bytes).is_none());

    let mut bytes = MYWIFI_V1;
    bytes[107] ^= 0x80;
    assert!(WifiCreds::decode(&bytes).is_none());
}

#[test]
fn decode_rejects_lengths_outside_the_layout_even_with_a_valid_crc() {
    // Re-stamped CRC on purpose: these must fail on the length check alone,
    // otherwise the test would only prove the CRC works.
    for mutate in [
        |b: &mut [u8; RECORD_LEN]| b[5] = 0,                    // no ssid
        |b: &mut [u8; RECORD_LEN]| b[5] = (SSID_MAX + 1) as u8, // ssid too long
        |b: &mut [u8; RECORD_LEN]| b[6] = 7,                    // illegal psk length
        |b: &mut [u8; RECORD_LEN]| b[6] = PSK_MAX as u8,        // psk runs past its field
        |b: &mut [u8; RECORD_LEN]| b[7] = (TOKEN_MAX + 1) as u8, // token too long
    ] {
        let mut bytes = MYWIFI;
        mutate(&mut bytes);
        re_stamp_v2(&mut bytes);
        assert!(WifiCreds::decode(&bytes).is_none(), "{bytes:?}");
    }
}

#[test]
fn decode_rejects_a_payload_that_is_not_utf8() {
    let mut bytes = MYWIFI;
    bytes[8] = 0xFF; // invalid UTF-8 start byte, CRC re-stamped
    re_stamp_v2(&mut bytes);
    assert!(WifiCreds::decode(&bytes).is_none());

    let mut bytes = MYWIFI;
    bytes[104] = 0xFF; // same, in the token field
    re_stamp_v2(&mut bytes);
    assert!(WifiCreds::decode(&bytes).is_none());
}

#[test]
fn debug_hides_both_secrets() {
    let text = std::format!("{:?}", paired());
    assert!(text.contains("mywifi"));
    assert!(!text.contains("hunter2"), "psk leaked into Debug: {text}");
    assert!(!text.contains(TOKEN), "token leaked into Debug: {text}");
}

/// Recompute the trailing CRC so a deliberately corrupted header still reaches
/// the check the test is about.
fn re_stamp_v2(bytes: &mut [u8; RECORD_LEN]) {
    let sum = crc32(&bytes[..RECORD_LEN - 4]);
    bytes[RECORD_LEN - 4..].copy_from_slice(&sum.to_le_bytes());
}
