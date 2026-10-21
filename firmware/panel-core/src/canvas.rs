//! Write-only RGB565 canvas over caller-owned memory.
//!
//! What this does NOT do: it has no idea which panel it is drawing into, it
//! never allocates, never talks SPI, and never keeps a second copy. The caller
//! owns the buffer (the firmware keeps one full frame in static RAM).
//!
//! Byte order is big-endian per pixel, matching what the panel expects on the
//! wire, so `raw()` can be pushed to SPI without a conversion pass.

use crate::crc32::crc32;
use crate::font8x16 as font;

/// Opaque black, the panel's "nothing here" colour.
pub const BG: u16 = 0x0000;
/// Opaque white, the ink colour.
pub const FG: u16 = 0xFFFF;

pub struct Canvas<'a> {
    buf: &'a mut [u8],
    w: usize,
    h: usize,
}

impl<'a> Canvas<'a> {
    /// `buf` must hold exactly `w * h * 2` bytes. Extra bytes are ignored.
    pub fn new(buf: &'a mut [u8], w: usize, h: usize) -> Self {
        Self { buf, w, h }
    }

    pub fn width(&self) -> usize {
        self.w
    }

    pub fn height(&self) -> usize {
        self.h
    }

    /// One pixel. Out-of-range coordinates are dropped silently: clipping is a
    /// contract, not an error (SPEC D-3).
    pub fn put(&mut self, x: usize, y: usize, colour: u16) {
        if x >= self.w || y >= self.h {
            return;
        }
        let at = (y * self.w + x) * 2;
        if at + 1 >= self.buf.len() {
            return;
        }
        self.buf[at] = (colour >> 8) as u8;
        self.buf[at + 1] = (colour & 0xFF) as u8;
    }

    /// Fill the whole canvas. Byte order matches `put`.
    pub fn clear(&mut self, colour: u16) {
        let hi = (colour >> 8) as u8;
        let lo = (colour & 0xFF) as u8;
        let mut at = 0;
        while at + 1 < self.buf.len() {
            self.buf[at] = hi;
            self.buf[at + 1] = lo;
            at += 2;
        }
    }

    /// Draw `text` with the 8x16 font at (x, y). Caller is responsible for
    /// truncation and centring; this only clips.
    pub fn text(&mut self, x: usize, y: usize, text: &[u8], colour: u16) {
        for (cell, &ch) in text.iter().enumerate() {
            let idx = if (font::FIRST..=font::LAST).contains(&ch) {
                (ch - font::FIRST) as usize
            } else {
                // Not in the table: render the space glyph rather than nothing,
                // so a bad byte cannot shift the line.
                (b' ' - font::FIRST) as usize
            };
            for row in 0..font::CELL_H {
                let bits = font::GLYPHS[idx][row];
                for col in 0..font::CELL_W {
                    if (bits >> (7 - col)) & 1 == 1 {
                        self.put(x + cell * font::CELL_W + col, y + row, colour);
                    }
                }
            }
        }
    }

    pub fn raw(&self) -> &[u8] {
        self.buf
    }

    /// CRC of the pixels currently in the buffer. This is what the PC compares
    /// against: the digest covers the image (`w * h * 2` bytes), not the bytes
    /// that happened to be sent over the wire.
    pub fn digest(&self) -> u32 {
        let want = (self.w * self.h * 2).min(self.buf.len());
        crc32(&self.buf[..want])
    }
}
