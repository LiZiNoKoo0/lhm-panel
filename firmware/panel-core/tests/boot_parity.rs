//! The boot card's pixels, pinned on both sides.
//!
//! Every number here was produced twice: by `preview_st7735.py::boot_page()` on
//! the PC, and by this crate's `boot::draw()` in the tests below. The board then
//! printed the same four numbers over the serial port on 2026-10-23:
//!
//!     BOOT PANEL ok            FRAME 160x80 crc=0x8e9722aa
//!     BOOT STORE ok
//!     BOOT WIFI ok             FRAME 160x80 crc=0x5adc8dbd
//!     BOOT LINK ...            FRAME 160x80 crc=0xaffeb207
//!     BOOT LINK ok             FRAME 160x80 crc=0x188fddde
//!
//! So: PC preview == core drawing == what actually reached the glass. If any of
//! these moves, the card changed on one side only -- which is exactly the class
//! of bug a CRC is here to catch.

use panel_core::boot::{self, Step, STEPS};
use panel_core::canvas::Canvas;

fn frame(steps: &[Step; STEPS]) -> u32 {
    frame_of(steps)
}

fn frame_of(steps: &[Step; STEPS]) -> u32 {
    let mut buf = vec![0u8; 160 * 80 * 2];
    {
        let mut canvas = Canvas::new(&mut buf, 160, 80);
        boot::draw(&mut canvas, steps);
        canvas.digest()
    }
}
#[test]
fn the_four_painted_states_match_the_pc_and_the_board() {
    // 1. panel + store already settled, the rest still pending.
    assert_eq!(
        frame(&[Step::Ok, Step::Ok, Step::Pending, Step::Pending]),
        0x8E97_22AA
    );
    // 2. WiFi came up.
    assert_eq!(
        frame(&[Step::Ok, Step::Ok, Step::Ok, Step::Pending]),
        0x5ADC_8DBD
    );
    // 3. the daemon link is being attempted (`...`).
    assert_eq!(
        frame(&[Step::Ok, Step::Ok, Step::Ok, Step::Busy]),
        0xAFFE_B207
    );
    // 4. handshake done -- the card `check_parity.py --boot` compares against.
    assert_eq!(frame(&[Step::Ok; STEPS]), 0x188F_DDDE);
}

#[test]
fn a_refused_token_paints_the_link_row_red() {
    // T5.6, measured on the board: the daemon answered `LINK deny reason=bad
    // token`, the BSP marked LINK `fail`, and the glass showed 0x1EFEB12B.
    // Pinned because "denied" and "still waiting" must not look the same: the
    // first means fix the token, the second means wait.
    assert_eq!(
        frame(&[Step::Ok, Step::Ok, Step::Ok, Step::Fail]),
        0x1EFE_B12B
    );
    assert_ne!(
        frame(&[Step::Ok, Step::Ok, Step::Ok, Step::Fail]),
        0xAFFE_B207,
        "fail must not look like busy"
    );
}

#[test]
fn the_initial_card_is_not_any_of_the_settled_ones() {
    // A fresh boot paints `--` everywhere. It must not collide with a finished
    // card, or "nothing has run yet" would be indistinguishable from "all good".
    let start = frame(&boot::START);
    assert_ne!(start, 0x188F_DDDE);
    assert_ne!(start, 0x8E97_22AA);
}
