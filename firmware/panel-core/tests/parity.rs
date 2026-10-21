//! Byte-for-byte parity against the PC preview, without any hardware.
//!
//! Every expected number below was produced by `preview_st7735.py`:
//!     zlib.crc32(P.to_bytes(P.render_page((top, bottom), 0xFFFF, 0x0000)))
//!
//! If this test passes, the core still draws exactly what the PC thinks it
//! draws; the panel only has to move the bytes. That is the whole验收 standard
//! of阶段1: refactor freely, CRC values must not move.

use panel_core::canvas::{Canvas, BG};
use panel_core::link::{backoff_ms, LinkError, Transport};
use panel_core::scene::{Rows, Scene, FRAME_BYTES, LCD_H, LCD_W};

/// Feed one wire line through the real scene and hash the resulting frame,
/// exactly like the firmware does before it pushes the pixels.
fn digest(line: &str) -> u32 {
    let mut buf = vec![0u8; FRAME_BYTES];
    let mut rows = Rows::new();
    rows.apply(line.as_bytes());
    assert!(rows.is_dirty(), "apply must mark the scene dirty");
    {
        let mut canvas = Canvas::new(&mut buf, LCD_W, LCD_H);
        rows.render(&mut canvas);
        let sum = canvas.digest();
        assert_eq!(canvas.raw().len(), FRAME_BYTES);
        return sum;
    }
}

#[test]
fn matches_the_pc_preview() {
    // (wire line, CRC32 the preview computes for the same two rows)
    let cases: &[(&str, u32)] = &[
        ("CPU 33.5%|TEMP   34C", 0xEE47_3F67),
        ("|", 0x0448_135D),
        ("CPU 100%|TEMP   --C", 0xB5EE_5FE9),
        ("AAAAAAAAAAAAAAAAAAAA|BBBBBBBBBBBBBBBBBBBB", 0xC843_CD86),
        ("x|y", 0x81AD_7B08),
        ("MEM 71%|NET 1.2MB/s", 0xFCD2_CA54),
        ("0123456789|", 0xB904_BFAC),
        ("|0123456789", 0x5C03_C9C9),
    ];
    for &(line, want) in cases {
        assert_eq!(digest(line), want, "frame CRC moved for {line:?}");
    }
}

#[test]
fn empty_input_still_paints_a_black_frame() {
    // A bare "\n" from the PC means "clear the glass", not "keep the last one".
    assert_eq!(digest(""), 0x0448_135D);
}

#[test]
fn missing_bottom_row_leaves_that_row_blank() {
    // "|" and "" must agree, otherwise trailing pipes would change the image.
    assert_eq!(digest("CPU 33.5%|"), digest("CPU 33.5%"));
}

#[test]
fn over_long_input_is_cut_at_twenty_cells() {
    // 20 columns is the panel width; the 21st character must not bleed into
    // the next row or push the line off centre.
    assert_eq!(digest("AAAAAAAAAAAAAAAAAAAAX|"), digest("AAAAAAAAAAAAAAAAAAAA|"));
}

#[test]
fn mark_clean_parks_the_scene() {
    let mut rows = Rows::new();
    rows.apply(b"a|b");
    rows.mark_clean();
    assert!(!rows.is_dirty());
}

#[test]
fn backoff_ladder_is_the_pc_ladder() {
    assert_eq!(backoff_ms(2), 2_000);
    assert_eq!(backoff_ms(99), 10_000);
    let _ = LinkError::Closed;
}

/// The link contract must be implementable without allocation, so a tiny
/// in-memory transport is enough to prove the trait is usable.
struct FakeLink {
    line: &'static [u8],
    taken: bool,
}

impl Transport for FakeLink {
    fn poll_line(&mut self, out: &mut [u8]) -> Result<Option<usize>, LinkError> {
        if self.taken {
            return Ok(None);
        }
        self.taken = true;
        let n = self.line.len().min(out.len());
        out[..n].copy_from_slice(&self.line[..n]);
        Ok(Some(n))
    }

    fn write_all(&mut self, _bytes: &[u8]) -> Result<(), LinkError> {
        Ok(())
    }

    fn is_up(&self) -> bool {
        true
    }
}

#[test]
fn transport_trait_is_object_safe() {
    let mut link = FakeLink { line: b"CPU 1%|TEMP 2C", taken: false };
    let transport: &mut dyn Transport = &mut link;
    let mut buf = [0u8; 48];
    let n = transport.poll_line(&mut buf).unwrap().unwrap();
    assert_eq!(&buf[..n], b"CPU 1%|TEMP 2C");
    assert!(transport.poll_line(&mut buf).unwrap().is_none());
    let _ = BG;
}
