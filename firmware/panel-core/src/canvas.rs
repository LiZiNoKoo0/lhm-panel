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

    /// One pixel, or `None` outside the canvas. Used by tests that assert *what*
    /// was drawn rather than only that something changed; the firmware itself
    /// is write-only.
    pub fn get(&self, x: usize, y: usize) -> Option<u16> {
        if x >= self.w || y >= self.h {
            return None;
        }
        let at = (y * self.w + x) * 2;
        if at + 1 >= self.buf.len() {
            return None;
        }
        Some(((self.buf[at] as u16) << 8) | self.buf[at + 1] as u16)
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

    /// Fill a rectangle. Clipped like `put`, so a caller cannot paint outside
    /// the panel by arithmetic (SPEC D-3). Used by the `LINK LOST` badge, whose
    /// legibility depends on the block being solid before the text goes on it.
    pub fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, colour: u16) {
        for py in y..y.saturating_add(h) {
            for px in x..x.saturating_add(w) {
                self.put(px, py, colour);
            }
        }
    }

    /// Draw `text` with the 8x16 font at (x, y). Caller is responsible for
    /// truncation and centring; this only clips.
    pub fn text(&mut self, x: usize, y: usize, text: &[u8], colour: u16) {
        self.text_scaled(x, y, text, colour, 1)
    }

    /// Draw `text` at an integer magnification of the 8x16 font.
    ///
    /// `scale` 1 is the 8x16 cell, 2 is 16x32, 3 is 24x48. This is how the
    /// panel grows a second font without a second glyph table: one 1520-byte
    /// table covers every size by pixel duplication, which matters because B-1
    /// showed that any new static array is paid for out of the stack.
    ///
    /// Non-integer sizes are deliberately impossible. A 1.5x glyph would need
    /// some pixels doubled and others not, and at 8px tall that reads as a
    /// damaged font rather than a small one. Callers pick from `FONT_SCALES`.
    ///
    /// Cost: `w * h * scale^2` calls to `put`, but only for set pixels, and the
    /// panel is 160x80 -- a full 48px line is still a few thousand pixels.
    pub fn text_scaled(&mut self, x: usize, y: usize, text: &[u8], colour: u16, scale: usize) {
        let scale = scale.max(1);
        let cell_w = font::CELL_W * scale;
        for (cell, &ch) in text.iter().enumerate() {
            let idx = if (font::FIRST..=font::LAST).contains(&ch) {
                (ch - font::FIRST) as usize
            } else {
                // Not in the table: render the space glyph rather than nothing,
                // so a bad byte cannot shift the line.
                (b' ' - font::FIRST) as usize
            };
            let origin_x = x + cell * cell_w;
            for row in 0..font::CELL_H {
                let bits = font::GLYPHS[idx][row];
                for col in 0..font::CELL_W {
                    if (bits >> (7 - col)) & 1 != 1 {
                        continue;
                    }
                    // One set source pixel becomes a scale x scale block, so the
                    // glyph is magnified with hard edges and no resampling.
                    let px = origin_x + col * scale;
                    let py = y + row * scale;
                    for dy in 0..scale {
                        for dx in 0..scale {
                            self.put(px + dx, py + dy, colour);
                        }
                    }
                }
            }
        }
    }

    /// Width in pixels of `count` characters at `scale`.
    pub fn text_width(count: usize, scale: usize) -> usize {
        font::CELL_W * scale.max(1) * count
    }

    /// Height in pixels of one line at `scale`.
    pub fn text_height(scale: usize) -> usize {
        font::CELL_H * scale.max(1)
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
