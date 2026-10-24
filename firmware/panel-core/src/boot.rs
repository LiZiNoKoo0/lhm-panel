//! The boot card: what the glass shows while the board is still assembling
//! itself, and until the PC sends a layout.
//!
//! What this does NOT do: no I/O, no timing, no idea which step is done. It
//! paints a given state; the BSP decides what the state is. That split is what
//! lets the PC render the same card and compare CRCs (the same discipline the
//! bench rows and the `LINK LOST` badge follow).
//!
//! Why it exists at all: a board that has just been reset used to show black
//! until a `ui` arrived, and "black" is indistinguishable from "dead" -- the
//! exact ambiguity this project keeps paying for. A self-test page that fills in
//! as stages complete is honest *and* readable from across the desk.
//!
//! Geometry: the panel is 160x80 with a fixed 8x16 cell, so it holds exactly
//! 20 cells by 5 rows. One title row plus four step rows is 80px with nothing
//! left over -- the layout is the panel, not a taste decision.

use crate::canvas::{Canvas, BG, FG};
use crate::font8x16 as font;

/// Steps shown on the card. Order is the boot order, top to bottom.
pub const STEPS: usize = 4;
pub const LABELS: [&str; STEPS] = ["PANEL", "STORE", "WIFI", "LINK"];

/// Index of each step, so callers do not pass bare numbers.
pub const PANEL: usize = 0;
pub const STORE: usize = 1;
pub const WIFI: usize = 2;
pub const LINK: usize = 3;

pub const TITLE: &str = "SELF TEST";

/// The title is the only coloured thing on the card: it is the one row that is
/// decoration rather than information. Sakura pink, the same ink the standalone
/// st7735 project uses on this panel, so the two boards look related.
pub const TITLE_COLOUR: u16 = 0xFDB8;

/// Pending rows are visible but recessive -- if they were white the card would
/// look finished before it is.
pub const PENDING_COLOUR: u16 = 0x8410;
/// Failed rows have to be impossible to misread as "still working".
pub const FAIL_COLOUR: u16 = 0xF800;

/// One row of the checklist.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// Not started: shown as `--`.
    Pending,
    /// In progress: shown as `...`.
    Busy,
    /// Done: shown as `ok`.
    Ok,
    /// Done and broken: shown as `fail`.
    Fail,
}

impl Step {
    /// The three-to-four character word for this state.
    pub fn text(&self) -> &'static [u8] {
        match self {
            Step::Pending => b"--",
            Step::Busy => b"...",
            Step::Ok => b"ok",
            Step::Fail => b"fail",
        }
    }

    /// Colour for both the label and the status word.
    pub fn colour(&self) -> u16 {
        match self {
            Step::Pending => PENDING_COLOUR,
            Step::Busy => PENDING_COLOUR,
            Step::Ok => FG,
            Step::Fail => FAIL_COLOUR,
        }
    }

    /// Compact code for the BSP's atomics (see `bootstate.rs`): the firmware
    /// publishes state from three different tasks, so it needs something that
    /// fits in an `AtomicU8` and cannot be half-written.
    pub fn code(&self) -> u8 {
        match self {
            Step::Pending => 0,
            Step::Busy => 1,
            Step::Ok => 2,
            Step::Fail => 3,
        }
    }

    pub fn from_code(code: u8) -> Step {
        match code {
            1 => Step::Busy,
            2 => Step::Ok,
            3 => Step::Fail,
            _ => Step::Pending,
        }
    }
}

/// All steps as they start: nothing has run yet.
pub const START: [Step; STEPS] = [Step::Pending; STEPS];

/// Paint the card. Clears first, so it is a whole frame like every other render.
pub fn draw(canvas: &mut Canvas<'_>, steps: &[Step; STEPS]) {
    canvas.clear(BG);
    draw_centred(canvas, TITLE.as_bytes(), 0, TITLE_COLOUR);

    for (i, &step) in steps.iter().enumerate() {
        // Row 0 is the title; step `i` sits on row `i + 1`.
        let y = (i + 1) * font::CELL_H;
        let colour = step.colour();
        canvas.text(font::CELL_W, y, LABELS[i].as_bytes(), colour);

        // Status is right-aligned against the last cell, so the column of
        // words stays a column whatever their length.
        let word = step.text();
        let cells = word.len().min(COLS);
        let x = (COLS - cells) * font::CELL_W;
        canvas.text(x, y, &word[..cells], colour);
    }
}

/// Cells across the panel: 160 / 8.
pub const COLS: usize = 20;

fn draw_centred(canvas: &mut Canvas<'_>, text: &[u8], y: usize, colour: u16) {
    let cells = text.len().min(COLS);
    let width = cells * font::CELL_W;
    let x = (canvas.width() - width) / 2;
    canvas.text(x, y, &text[..cells], colour);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(steps: &[Step; STEPS]) -> Canvas<'static> {
        // 160x80 RGB565 lives in a leaked buffer: the test only reads it back.
        let buf: &'static mut [u8] = Box::leak(vec![0u8; 160 * 80 * 2].into_boxed_slice());
        let mut canvas = Canvas::new(buf, 160, 80);
        draw(&mut canvas, steps);
        canvas
    }

    #[test]
    fn every_state_has_a_short_word() {
        for step in [Step::Pending, Step::Busy, Step::Ok, Step::Fail] {
            let word = step.text();
            assert!(!word.is_empty() && word.len() <= 4, "{:?} -> {:?}", step, word);
            assert!(word.iter().all(|&b| (font::FIRST..=font::LAST).contains(&b)));
        }
        assert_eq!(Step::Pending.text(), b"--");
        assert_eq!(Step::Busy.text(), b"...");
        assert_eq!(Step::Ok.text(), b"ok");
        assert_eq!(Step::Fail.text(), b"fail");
    }

    #[test]
    fn codes_round_trip_through_the_atomic() {
        for step in [Step::Pending, Step::Busy, Step::Ok, Step::Fail] {
            assert_eq!(Step::from_code(step.code()), step);
        }
        // 0 is the initial value of a zeroed atomic, so it must mean Pending.
        assert_eq!(Step::from_code(0), Step::Pending);
    }

    #[test]
    fn the_card_fills_the_panel_exactly() {
        // Five rows of 16px: one title + four steps. If STEPS ever grows, this
        // test is the thing that says the layout no longer fits.
        assert_eq!((STEPS + 1) * font::CELL_H, 80);
        assert_eq!(COLS * font::CELL_W, 160);
    }

    #[test]
    fn each_state_paints_a_different_picture() {
        let a = frame(&[Step::Pending; STEPS]).digest();
        let b = frame(&[Step::Busy; STEPS]).digest();
        let c = frame(&[Step::Ok; STEPS]).digest();
        let d = frame(&[Step::Fail; STEPS]).digest();
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(c, d);
        assert_ne!(a, c);
    }

    #[test]
    fn a_step_changing_repaints_only_its_row_but_changes_the_frame() {
        let before = frame(&[Step::Pending; STEPS]).digest();
        let after = frame(&[Step::Ok, Step::Pending, Step::Pending, Step::Pending]).digest();
        assert_ne!(before, after, "a finished step must be visible");
    }
}
