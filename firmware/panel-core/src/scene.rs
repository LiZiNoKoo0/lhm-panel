//! Scenes: what ends up on the glass.
//!
//! What this does NOT do: no I/O, no protocol parsing beyond the one wire
//! format the current firmware speaks (`<top>|<bottom>`), no knowledge of the
//! panel, no allocation. A scene is a pure function of its state plus whatever
//! `apply` was last fed.
//!
//! Right now there is exactly one scene (`Rows`). The trait exists so that the
//! local fallback page and the protocol-driven layout can arrive later without
//! touching the main loop (SPEC E1).

use crate::canvas::{Canvas, BG, FG};
use crate::font8x16 as font;

// -- panel geometry -----------------------------------------------------
// These are facts about the module on the desk, not negotiation results
// (PROTOCOL: a 160x80 panel is dumb; the PC does the layout).

pub const LCD_W: usize = 160;
pub const LCD_H: usize = 80;
pub const FRAME_BYTES: usize = LCD_W * LCD_H * 2;

/// Where the two text rows sit: 2 x 16px inside 80px, with an 8px gutter.
pub const TOP_Y: usize = 20;
pub const BOTTOM_Y: usize = 44;

/// Characters per row: 160px / 8px.
pub const COLS: usize = LCD_W / font::CELL_W;

/// Longest line we accept from the PC, `|` included.
pub const LINE_MAX: usize = 48;

pub trait Scene {
    /// Feed one line of input. Must only change state -- never draw.
    fn apply(&mut self, line: &[u8]);
    fn is_dirty(&self) -> bool;
    /// Paint the whole canvas (clearing first). Called once per frame, not per
    /// input event.
    fn render(&mut self, canvas: &mut Canvas<'_>);
    fn mark_clean(&mut self);
}

/// The current firmware's only scene: `<top>|<bottom>`, both ASCII, both
/// centred, truncated to `COLS`. The board does not know what the text means.
pub struct Rows {
    buf: [u8; LINE_MAX],
    len: usize,
    dirty: bool,
}

impl Rows {
    pub const fn new() -> Self {
        Self { buf: [0; LINE_MAX], len: 0, dirty: false }
    }

    fn paint(&self, canvas: &mut Canvas<'_>) {
        canvas.clear(BG);
        let line = &self.buf[..self.len];
        let split = match line.iter().position(|&b| b == b'|') {
            Some(at) => at,
            None => line.len(),
        };
        let (top, rest) = line.split_at(split);
        let bottom = if rest.is_empty() { &rest[..0] } else { &rest[1..] };
        draw_row(canvas, top, TOP_Y);
        draw_row(canvas, bottom, BOTTOM_Y);
    }
}

impl Scene for Rows {
    fn apply(&mut self, line: &[u8]) {
        let n = if line.len() < LINE_MAX { line.len() } else { LINE_MAX };
        self.buf[..n].copy_from_slice(&line[..n]);
        self.len = n;
        self.dirty = true;
    }

    fn is_dirty(&self) -> bool {
        self.dirty
    }

    fn render(&mut self, canvas: &mut Canvas<'_>) {
        self.paint(canvas);
    }

    fn mark_clean(&mut self) {
        self.dirty = false;
    }
}

/// One centred row of up to `COLS` cells. Mirrors `render_page()` in
/// `preview_st7735.py` for inputs that fit, which is the whole point: the PC
/// and the glass agree by construction, not by eye.
fn draw_row(canvas: &mut Canvas<'_>, text: &[u8], y0: usize) {
    let cells = if text.len() < COLS { text.len() } else { COLS };
    let width = cells * font::CELL_W;
    let x0 = if width < LCD_W { (LCD_W - width) / 2 } else { 0 };
    canvas.text(x0, y0, &text[..cells], FG);
}
