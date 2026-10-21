//! Diagnostics that go up the wire: boot banner and the per-frame CRC line.
//!
//! What this does NOT do: no formatting of user data, no buffering, no retries.
//! Every line here is written once and forgotten -- if the link is down, the
//! information is lost on purpose, because a monitor that blocks on logging is
//! worse than one that is quiet.

use panel_core::link::Transport;

/// Greeting before the panel is touched. Byte-identical to the pre-refactor
/// firmware so existing logs stay comparable.
pub fn banner(t: &mut dyn Transport) {
    let _ = t.write_all(b"\n=== st7735 160x80 live text / 8x16 dot matrix ===\n");
}

/// What the panel was actually initialised with. Values are passed in, never
/// hardcoded here: if board.rs changes, this line changes with it.
pub fn init_done(
    t: &mut dyn Transport,
    madctl: u8,
    off_x: u16,
    off_y: u16,
    invert: bool,
    spi_mhz: u32,
) {
    let mut line = [0u8; 96];
    let mut at = 0;
    at += put(&mut line[at..], b"init done: MADCTL=0x");
    at += put_hex_u8(&mut line[at..], madctl);
    at += put(&mut line[at..], b" OFF=(");
    at += put_dec(&mut line[at..], off_x as u32);
    at += put(&mut line[at..], b",");
    at += put_dec(&mut line[at..], off_y as u32);
    at += put(&mut line[at..], b") INVERT=");
    at += put(&mut line[at..], if invert { b"true" } else { b"false" });
    at += put(&mut line[at..], b" SPI=");
    at += put_dec(&mut line[at..], spi_mhz);
    at += put(&mut line[at..], b"MHz\n");
    let _ = t.write_all(&line[..at]);
}

pub fn ready(t: &mut dyn Transport) {
    let _ = t.write_all(b"READY\n");
}

/// `FRAME 160x80 crc=0x........ bytes=25600\n`
///
/// This is the whole验收 mechanism of P1: the number is the CRC of the exact
/// bytes that were pushed, so the PC can prove the glass matches its preview
/// without a camera.
pub fn frame_line(t: &mut dyn Transport, w: usize, h: usize, crc: u32, bytes: usize) {
    let mut line = [0u8; 64];
    let mut at = 0;
    at += put(&mut line[at..], b"FRAME ");
    at += put_dec(&mut line[at..], w as u32);
    at += put(&mut line[at..], b"x");
    at += put_dec(&mut line[at..], h as u32);
    at += put(&mut line[at..], b" crc=0x");
    at += put_hex_u32(&mut line[at..], crc);
    at += put(&mut line[at..], b" bytes=");
    at += put_dec(&mut line[at..], bytes as u32);
    at += put(&mut line[at..], b"\n");
    let _ = t.write_all(&line[..at]);
}

// -- tiny formatting helpers (no alloc, no fmt machinery) ---------------

fn put(dst: &mut [u8], src: &[u8]) -> usize {
    let n = if src.len() < dst.len() { src.len() } else { dst.len() };
    dst[..n].copy_from_slice(&src[..n]);
    n
}

fn put_dec(dst: &mut [u8], mut value: u32) -> usize {
    let mut tmp = [0u8; 10];
    let mut at = tmp.len();
    if value == 0 {
        at -= 1;
        tmp[at] = b'0';
    }
    while value > 0 {
        at -= 1;
        tmp[at] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    put(dst, &tmp[at..])
}

fn put_hex_u32(dst: &mut [u8], value: u32) -> usize {
    let mut at = 0;
    for shift in [28u32, 24, 20, 16, 12, 8, 4, 0] {
        let nibble = ((value >> shift) & 0xF) as u8;
        at += put(
            &mut dst[at..],
            &[if nibble < 10 { b'0' + nibble } else { b'a' + nibble - 10 }],
        );
    }
    at
}

fn put_hex_u8(dst: &mut [u8], value: u8) -> usize {
    let mut at = 0;
    for shift in [4u32, 0] {
        let nibble = ((value >> shift) & 0xF) as u8;
        at += put(
            &mut dst[at..],
            &[if nibble < 10 { b'0' + nibble } else { b'a' + nibble - 10 }],
        );
    }
    at
}
