//! The wire text must not move during refactors either.
//!
//! `diag.rs` lives in the BSP crate (it writes to a real transport), but its
//! content is pure formatting. Including it here lets a plain `cargo test` on
//! the PC pin down the exact bytes -- otherwise "the banner still looks fine"
//! is the best we can do, and that is exactly the kind of claim this project
//! refuses to make about pixels or about logs.

use panel_core::link::{LinkError, Transport};

#[path = "../../st7735-text/src/diag.rs"]
mod diag;

struct Sink(Vec<u8>);

impl Transport for Sink {
    fn poll_line(&mut self, _out: &mut [u8]) -> Result<Option<usize>, LinkError> {
        Ok(None)
    }
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), LinkError> {
        self.0.extend_from_slice(bytes);
        Ok(())
    }
    fn is_up(&self) -> bool {
        true
    }
}

fn text_of(f: impl FnOnce(&mut dyn Transport)) -> String {
    let mut sink = Sink(Vec::new());
    f(&mut sink);
    String::from_utf8(sink.0).expect("diagnostics must stay ASCII")
}

#[test]
fn banner_is_unchanged() {
    assert_eq!(
        text_of(|t| diag::banner(t)),
        "\n=== st7735 160x80 live text / 8x16 dot matrix ===\n"
    );
}

#[test]
fn init_line_is_unchanged() {
    assert_eq!(
        text_of(|t| diag::init_done(t, 0x60, 0, 24, false, 20)),
        "init done: MADCTL=0x60 OFF=(0,24) INVERT=false SPI=20MHz\n"
    );
}

#[test]
fn ready_line_is_unchanged() {
    assert_eq!(text_of(|t| diag::ready(t)), "READY\n");
}

#[test]
fn frame_line_is_unchanged_and_lowercase() {
    assert_eq!(
        text_of(|t| diag::frame_line(t, 160, 80, 0x0046_644B, 25600)),
        "FRAME 160x80 crc=0x0046644b bytes=25600\n"
    );
    // all-nibbles case, to catch a missing leading zero or an upper case hex
    assert_eq!(
        text_of(|t| diag::frame_line(t, 160, 80, 0xFFFF_FFFF, 25600)),
        "FRAME 160x80 crc=0xffffffff bytes=25600\n"
    );
    assert_eq!(
        text_of(|t| diag::frame_line(t, 160, 80, 0x0000_0000, 25600)),
        "FRAME 160x80 crc=0x00000000 bytes=25600\n"
    );
}
