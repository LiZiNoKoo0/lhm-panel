//! The `LINK LOST` badge: the one thing the board may paint over a PC layout.
//!
//! SPEC 4.9: when the daemon link dies while a layout is on the glass, the
//! picture must **not** be cleared or blacked out -- the last values stay, and
//! this badge says why they are stale. It is drawn in the bottom-right corner,
//! white on black (inverted relative to the body text) so it reads as an
//! annotation rather than as data.
//!
//! What this does NOT do: it does not decide *when* to draw. "Link is down" is
//! the session's answer (L-1/L-3), "there is a layout worth keeping" is the
//! scene's, and both live outside this file. Nor does it avoid overlapping
//! bound nodes yet -- T6 owns node geometry, and until then the only scene is
//! the bench one, whose rows sit at y=20/44 and never reach y=64.

use crate::canvas::{Canvas, BG, FG};
use crate::font8x16 as font;

/// Nine characters, so the badge is the same width on every board this ships
/// to and its geometry is a constant rather than a measurement.
pub const TEXT: &str = "LINK LOST";

/// Badge size in pixels: one 8x16 cell per character.
pub const W: usize = TEXT.len() * font::CELL_W;
pub const H: usize = font::CELL_H;

/// Bottom-right, flush with the corner: x = 160-72 = 88, y = 80-16 = 64.
pub const X: usize = crate::scene::LCD_W - W;
pub const Y: usize = crate::scene::LCD_H - H;

/// Whether the badge fits on a `w` x `h` panel at all. A smaller board gets no
/// badge instead of a clipped one: a half-visible `LINK LOS` would read as
/// data, which is the opposite of the point.
pub const fn fits(w: usize, h: usize) -> bool {
    w >= W && h >= H
}

/// Paint the badge: a white block with the text punched out in black.
///
/// Order matters and is the whole design: filling first means the badge is
/// legible over any layout, including one that happens to be white there.
pub fn draw(canvas: &mut Canvas<'_>) {
    if !fits(canvas.width(), canvas.height()) {
        return;
    }
    canvas.fill_rect(X, Y, W, H, FG);
    canvas.text(X, Y, TEXT.as_bytes(), BG);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{LCD_H, LCD_W};

    fn frame() -> [u8; LCD_W * LCD_H * 2] {
        [0u8; LCD_W * LCD_H * 2]
    }

    fn pixel(canvas: &Canvas<'_>, x: usize, y: usize) -> u16 {
        let at = (y * LCD_W + x) * 2;
        let raw = canvas.raw();
        u16::from_be_bytes([raw[at], raw[at + 1]])
    }

    #[test]
    fn geometry_is_the_bottom_right_corner() {
        assert_eq!(W, 72, "9 cells of 8px");
        assert_eq!((X, Y), (88, 64));
        assert_eq!(X + W, LCD_W, "flush right");
        assert_eq!(Y + H, LCD_H, "flush bottom");
    }

    #[test]
    fn the_badge_is_white_with_black_letters() {
        // Inverted on purpose: on this panel the body is white-on-black, so an
        // ordinary white line down there would be read as a value.
        let mut buf = frame();
        let mut canvas = Canvas::new(&mut buf, LCD_W, LCD_H);
        draw(&mut canvas);

        // A corner of the block is background of the badge = white.
        assert_eq!(pixel(&canvas, X, Y), FG);
        assert_eq!(pixel(&canvas, X + W - 1, Y + H - 1), FG);
        // The 'L' spine is inside the box, so it must be ink-black.
        let mut dark = 0;
        for y in Y..Y + H {
            for x in X..X + W {
                if pixel(&canvas, x, y) == BG {
                    dark += 1;
                }
            }
        }
        assert!(dark > 40, "letters are punched out, got {} black px", dark);
        assert!(dark < W * H / 2, "the block must stay mostly white");
    }

    #[test]
    fn nothing_outside_the_badge_moves() {
        let mut buf = frame();
        let mut canvas = Canvas::new(&mut buf, LCD_W, LCD_H);
        canvas.clear(FG);
        draw(&mut canvas);
        // The rows the bench scene uses (SPEC 4.9: never cover data if avoidable).
        assert_eq!(pixel(&canvas, 0, crate::scene::TOP_Y), FG);
        assert_eq!(pixel(&canvas, 0, crate::scene::BOTTOM_Y), FG);
        assert_eq!(pixel(&canvas, X - 1, Y + H - 1), FG);
        assert_eq!(pixel(&canvas, X, Y - 1), FG);
    }

    #[test]
    fn a_too_small_panel_gets_no_badge_at_all() {
        assert!(fits(LCD_W, LCD_H));
        assert!(fits(128, 64), "72x16 still fits a 128x64 panel");
        assert!(!fits(64, 64), "72px wide does not fit 64px");
        assert!(!fits(160, 12), "16px tall does not fit 12px");
        // And drawing into one is a no-op rather than a clipped mess.
        let mut buf = [0u8; 64 * 64 * 2];
        let mut canvas = Canvas::new(&mut buf, 64, 64);
        let before = canvas.digest();
        draw(&mut canvas);
        assert_eq!(canvas.digest(), before);
    }
}
