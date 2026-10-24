//! The A/B sector frame: two copies, one `seq`, and a rule for which one wins.
//!
//! What this does NOT do: no flash, no erase, no write, no sector addresses, no
//! `Store`. It is the bytes and the tie-break only, so the whole "which copy
//! would I believe after a power cut" question is answerable by `cargo test`
//! instead of by pulling the cable and hoping.
//!
//! Layout (little-endian), `firmware/SPEC.md` §6 S-1:
//!
//! ```text
//! off  len  what
//!   0    4   magic "PCFG"
//!   4    1   ver = 1
//!   5    2   len = payload length (0..=1024)
//!   7    4   seq, one higher on every save
//!  11    4   crc32(header[0..11] || payload)
//!  15    n   payload
//! ```
//!
//! Two guarantees, and they are the reason this file exists:
//!
//! 1. **A frame is all-or-nothing.** `parse` accepts a frame only if the CRC
//!    covers every byte the length field promised. A write that was cut in half
//!    therefore reads back as *absent*, never as a half-truth.
//! 2. **Newer wins, and "newer" is `seq`, not position.** So the writer may
//!    touch the sector that does not hold the truth (SPEC S-1: write the
//!    inactive sector, verify, let `seq` switch), and a power cut in the middle
//!    of that leaves the old frame as the newest *complete* one.

use crate::crc32::Crc32;

/// First four bytes of every frame.
pub const MAGIC: [u8; 4] = *b"PCFG";
/// Framing version. Bumped only if the layout above changes.
pub const VER: u8 = 1;
/// Bytes before the payload.
pub const HEADER_LEN: usize = 15;
/// Largest payload a frame may carry (SPEC §6: values up to 1 KB).
pub const MAX_PAYLOAD: usize = 1024;
/// Largest frame there can be: what a caller's buffer has to hold.
pub const FRAME_MAX: usize = HEADER_LEN + MAX_PAYLOAD;

const VER_AT: usize = 4;
const LEN_AT: usize = 5;
const SEQ_AT: usize = 7;
const CRC_AT: usize = 11;

/// A frame that passed every check, borrowing the bytes it was parsed from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Frame<'a> {
    /// Monotonic counter; the higher one wins.
    pub seq: u32,
    /// The bytes the caller sealed.
    pub payload: &'a [u8],
}

/// Wrap one payload into a frame at the front of `out`.
///
/// Returns the number of bytes to hand the flash driver, or `None` if `out` is
/// too small or the payload is over [`MAX_PAYLOAD`] (nothing is written then).
pub fn seal(seq: u32, payload: &[u8], out: &mut [u8]) -> Option<usize> {
    if payload.len() > MAX_PAYLOAD {
        return None;
    }
    let total = HEADER_LEN + payload.len();
    if out.len() < total {
        return None;
    }

    out[0..4].copy_from_slice(&MAGIC);
    out[VER_AT] = VER;
    out[LEN_AT..LEN_AT + 2].copy_from_slice(&(payload.len() as u16).to_le_bytes());
    out[SEQ_AT..SEQ_AT + 4].copy_from_slice(&seq.to_le_bytes());
    out[HEADER_LEN..total].copy_from_slice(payload);

    // The CRC field sits between the bytes it covers, so it is hashed in two
    // pieces: everything before it, then the payload. Concatenating a 1 KB
    // payload into scratch just to hash it is not worth 1 KB of a 34 KB stack.
    let mut crc = Crc32::new();
    crc.update(&out[..CRC_AT]);
    crc.update(payload);
    out[CRC_AT..HEADER_LEN].copy_from_slice(&crc.finish().to_le_bytes());

    Some(total)
}

/// Read a frame out of `buf`, or `None` if it is anything less than complete.
///
/// `buf` may be longer than the frame (the callers hand over whole sectors or
/// whole reads); the length field decides where the frame ends.
pub fn parse(buf: &[u8]) -> Option<Frame<'_>> {
    if buf.len() < HEADER_LEN || buf[0..4] != MAGIC || buf[VER_AT] != VER {
        return None;
    }
    let len = u16::from_le_bytes([buf[LEN_AT], buf[LEN_AT + 1]]) as usize;
    if len > MAX_PAYLOAD {
        return None;
    }
    let total = HEADER_LEN + len;
    if buf.len() < total {
        return None;
    }

    let want = u32::from_le_bytes([
        buf[CRC_AT],
        buf[CRC_AT + 1],
        buf[CRC_AT + 2],
        buf[CRC_AT + 3],
    ]);
    let mut crc = Crc32::new();
    crc.update(&buf[..CRC_AT]);
    crc.update(&buf[HEADER_LEN..total]);
    if crc.finish() != want {
        return None;
    }

    let seq = u32::from_le_bytes([
        buf[SEQ_AT],
        buf[SEQ_AT + 1],
        buf[SEQ_AT + 2],
        buf[SEQ_AT + 3],
    ]);
    Some(Frame {
        seq,
        payload: &buf[HEADER_LEN..total],
    })
}

/// Is `a` the later save than `b`?
///
/// Wrapping comparison, so the counter rolling over from `u32::MAX` to 0 is not
/// a "the old copy won" event. A counter 2^31 saves behind is out of scope and
/// would be a bad trade to guard against.
pub fn is_newer(a: u32, b: u32) -> bool {
    a.wrapping_sub(b) as i32 > 0
}

/// The frame a reader should believe: the one with the later `seq`.
pub fn newest<'a>(a: Option<Frame<'a>>, b: Option<Frame<'a>>) -> Option<Frame<'a>> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if is_newer(a.seq, b.seq) { a } else { b }),
        (a, b) => a.or(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAYLOAD: &[u8] = b"st7735 says hello";

    fn sealed(seq: u32, payload: &[u8]) -> ([u8; FRAME_MAX], usize) {
        let mut buf = [0u8; FRAME_MAX];
        let n = seal(seq, payload, &mut buf).expect("fits");
        (buf, n)
    }

    #[test]
    fn round_trip_keeps_seq_and_payload() {
        let (buf, n) = sealed(7, PAYLOAD);
        let frame = parse(&buf[..n]).expect("a sealed frame parses");
        assert_eq!(frame.seq, 7);
        assert_eq!(frame.payload, PAYLOAD);
        // A whole sector is a legal input to `parse`: the length field decides.
        assert_eq!(parse(&buf).map(|f| f.seq), Some(7));
    }

    #[test]
    fn empty_payload_is_legal_and_distinguishable_from_a_blank_sector() {
        let (buf, n) = sealed(1, b"");
        let frame = parse(&buf[..n]).expect("an empty payload still has a header");
        assert_eq!(frame.payload.len(), 0);
        // A blank sector must not parse: 0xFF is not our magic.
        assert!(parse(&[0xFFu8; 64]).is_none());
        assert!(parse(&[0x00u8; 64]).is_none());
    }

    #[test]
    fn a_write_cut_in_half_reads_as_absent() {
        // This is the power-cut case, at byte level: the header promised `len`
        // bytes and only half arrived. Half a record must never be accepted.
        let (buf, n) = sealed(3, PAYLOAD);
        for cut in [HEADER_LEN, HEADER_LEN + 1, n - 1] {
            assert!(
                parse(&buf[..cut]).is_none(),
                "a frame truncated to {cut}/{n} bytes was accepted"
            );
        }
    }

    #[test]
    fn every_single_bit_flip_is_rejected() {
        let (buf, n) = sealed(9, PAYLOAD);
        for bit in 0..(n * 8) {
            let mut damaged = buf;
            damaged[bit / 8] ^= 1 << (bit % 8);
            assert!(
                parse(&damaged[..n]).is_none(),
                "damage to bit {bit} survived the CRC"
            );
        }
    }

    #[test]
    fn wrong_version_or_magic_is_rejected() {
        let (buf, n) = sealed(2, PAYLOAD);
        let mut bad = buf;
        bad[VER_AT] = VER + 1;
        assert!(parse(&bad[..n]).is_none());
        let mut bad = buf;
        bad[0] = b'X';
        assert!(parse(&bad[..n]).is_none());
    }

    #[test]
    fn newer_seq_wins_regardless_of_argument_order() {
        let (a, an) = sealed(4, b"old");
        let (b, bn) = sealed(5, b"new");
        let fa = parse(&a[..an]);
        let fb = parse(&b[..bn]);
        assert_eq!(newest(fa, fb).map(|f| f.payload), Some(&b"new"[..]));
        assert_eq!(newest(fb, fa).map(|f| f.payload), Some(&b"new"[..]));
    }

    #[test]
    fn a_missing_copy_loses_to_the_one_that_survived() {
        let (a, an) = sealed(1, b"only");
        assert_eq!(newest(parse(&a[..an]), None).map(|f| f.seq), Some(1));
        assert_eq!(newest(None, parse(&a[..an])).map(|f| f.seq), Some(1));
        assert!(newest(None, None).is_none());
    }

    #[test]
    fn the_counter_rolling_over_does_not_hand_the_win_to_the_old_copy() {
        let (a, an) = sealed(u32::MAX, b"before wrap");
        let (b, bn) = sealed(0, b"after wrap");
        assert_eq!(
            newest(parse(&a[..an]), parse(&b[..bn])).map(|f| f.payload),
            Some(&b"after wrap"[..])
        );
    }

    #[test]
    fn equal_seq_prefers_the_second_argument_and_is_newer_is_strict() {
        // Two slots can only hold the same seq if something went very wrong;
        // the tie-break must still be a decision, not an accident of ordering.
        assert!(!is_newer(5, 5));
        assert!(is_newer(6, 5));
        assert!(!is_newer(5, 6));
    }

    #[test]
    fn over_long_payload_is_refused_without_writing() {
        let payload = [0u8; MAX_PAYLOAD + 1];
        let mut buf = [0u8; FRAME_MAX + 1];
        assert!(seal(1, &payload, &mut buf).is_none());
        assert_eq!(buf[0], 0, "a refused seal must not have touched the buffer");

        // And a buffer one byte short is refused too.
        let mut small = [0u8; HEADER_LEN + 4];
        assert!(seal(1, b"12345", &mut small).is_none());
    }
}
