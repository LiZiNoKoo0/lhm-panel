"""ST7735 160x80 technical preview: one line of 8x16 text, real RGB565 pixels.

This is not a UI mock-up. It is the framebuffer the panel will hold: 160x80
pixels, RGB565 colour, glyphs from the same dot-matrix table the firmware can
compile in (pcmon/font8x16.py). Whatever you see here is what the glass shows,
only larger -- so "does it fit, does it read, is the colour right" is answered
before flashing anything.

    python preview_st7735.py                          # one line, default text
    python preview_st7735.py --text "CPU 33.5% 34C"   # the usual: a metric line
    python preview_st7735.py --bg 000000 --fg FFFF00  # two colours, RGB
    python preview_st7735.py --zoom 6 --raw out.bin   # 6x view + dump the bytes
    python preview_st7735.py --ppm out.ppm            # 1:1 file, for a diff
    python preview_st7735.py --selftest               # no window, checks only

Keys in the window: 1 2 4 6 8 zoom, s save, q or Esc quit.
The --raw dump is what you push over SPI (or memcmp against a panel grab):
160*80*2 = 25600 bytes, big-endian RGB565, row by row.
"""
import argparse
import os
import sys

BASE_DIR = os.path.dirname(os.path.abspath(__file__))
if BASE_DIR not in sys.path:
    sys.path.insert(0, BASE_DIR)

from pcmon import font8x16 as font  # noqa: E402

PANEL_W = 160
PANEL_H = 80
COLS = PANEL_W // font.CELL_W        # 20 characters per line
LINES = PANEL_H // font.CELL_H       # 5 lines of text, we use one
DEFAULT_TEXT = "PC-MONITOR 160x80"
OVERFLOW_MARK = ">"

RGB565_MAX = 0xFFFF


# -- colour -------------------------------------------------------------

def parse_color(text, fallback):
    """'#RRGGBB', 'RGB' shorthand or 'RRGGBB' -> (r, g, b), 0..255 each."""
    raw = (text or "").strip().lstrip("#")
    if not raw:
        return fallback
    if len(raw) == 3:
        raw = "".join(ch * 2 for ch in raw)
    if len(raw) != 6:
        raise ValueError("colour must be RRGGBB or RGB, got %r" % text)
    try:
        value = int(raw, 16)
    except ValueError:
        raise ValueError("colour must be hex, got %r" % text)
    return ((value >> 16) & 0xFF, (value >> 8) & 0xFF, value & 0xFF)


def to_rgb565(rgb):
    """8-bit per channel -> one RGB565 word (5 bits red, 6 green, 5 blue)."""
    r, g, b = rgb
    return ((r & 0xF8) << 8) | ((g & 0xFC) << 3) | (b >> 3)


def from_rgb565(word):
    """Back to 8-bit per channel, the way the panel actually shows it."""
    r = (word >> 11) & 0x1F
    g = (word >> 5) & 0x3F
    b = word & 0x1F
    return (r << 3 | r >> 2, g << 2 | g >> 4, b << 3 | b >> 2)


def quantised(rgb):
    """The colour the glass shows after RGB565 rounding, and whether it moved."""
    word = to_rgb565(rgb)
    back = from_rgb565(word)
    return back, (back != tuple(rgb)), word


# -- text ---------------------------------------------------------------

def prepare_text(text, cols=COLS):
    """Fit a line into `cols` cells: non-ASCII becomes '?', overflow is marked."""
    cleaned = []
    for ch in text:
        if ch == "\t":
            cleaned.extend(" " * 4)
        elif 32 <= ord(ch) <= 126:
            cleaned.append(ch)
        else:
            cleaned.append("?")
    line = "".join(cleaned)
    if len(line) > cols:
        line = line[:cols - 1] + OVERFLOW_MARK
    return line


def ink_runs(line):
    """[(row, x, length)] of lit pixels; runs keep the item count small."""
    runs = []
    for row in range(font.CELL_H):
        col = 0
        while col < len(line) * font.CELL_W:
            cell, bit = divmod(col, font.CELL_W)
            byte = font.glyph(line[cell])[row]
            if (byte >> (7 - bit)) & 1:
                start = col
                while col < len(line) * font.CELL_W:
                    cell, bit = divmod(col, font.CELL_W)
                    byte = font.glyph(line[cell])[row]
                    if not ((byte >> (7 - bit)) & 1):
                        break
                    col += 1
                runs.append((row, start, col - start))
            else:
                col += 1
    return runs


# -- framebuffer --------------------------------------------------------

def render(line, fg_word, bg_word, x=2, y=None):
    """Build the panel image: rows of RGB565 words, 160 words each."""
    if y is None:
        y = (PANEL_H - font.CELL_H) // 2
    rows = [[bg_word] * PANEL_W for _ in range(PANEL_H)]
    for row, start, length in ink_runs(line):
        py = y + row
        if not (0 <= py < PANEL_H):
            continue
        for px in range(x + start, x + start + length):
            if 0 <= px < PANEL_W:
                rows[py][px] = fg_word
    return rows


#: Where the live firmware puts its two rows (firmware/main.rs TOP_Y/BOTTOM_Y).
LINE_YS = (20, 44)


def render_page(lines, fg_word, bg_word, ys=LINE_YS):
    """Several centred rows in one frame -- the layout the live firmware draws.

    Mirrors draw_row() in firmware/st7735-text/src/main.rs: each line is
    truncated to COLS characters and centred, so the PC and the glass agree
    without either side knowing what the text means.
    """
    rows = [[bg_word] * PANEL_W for _ in range(PANEL_H)]
    for text, y in zip(lines, ys):
        fitted = prepare_text(text)
        width = len(fitted) * font.CELL_W
        x = (PANEL_W - width) // 2 if width < PANEL_W else 0
        for row, start, length in ink_runs(fitted):
            py = y + row
            if not (0 <= py < PANEL_H):
                continue
            for px in range(x + start, x + start + length):
                if 0 <= px < PANEL_W:
                    rows[py][px] = fg_word
    return rows


def to_bytes(rows, big_endian=True):
    """RGB565 words -> the byte stream SPI expects."""
    out = bytearray()
    for row in rows:
        for word in row:
            out += word.to_bytes(2, "big" if big_endian else "little")
    return bytes(out)


def to_ppm(rows):
    """A 1:1 PPM (P6) for eyeballing or diffing outside this window."""
    out = bytearray(b"P6\n%d %d\n255\n" % (PANEL_W, PANEL_H))
    for row in rows:
        for word in row:
            out += bytes(from_rgb565(word))
    return bytes(out)


# -- window -------------------------------------------------------------

def draw(canvas, rows, zoom, bezel=6):
    canvas.delete("all")
    w, h = PANEL_W * zoom, PANEL_H * zoom
    canvas.configure(width=w + bezel * 2, height=h + bezel * 2)
    canvas.create_rectangle(0, 0, w + bezel * 2, h + bezel * 2,
                            fill="#202020", outline="")
    canvas.create_rectangle(bezel, bezel, bezel + w, bezel + h,
                            fill="#000000", outline="#606060")
    cache = {}
    for y, row in enumerate(rows):
        x = 0
        while x < PANEL_W:
            word = row[x]
            start = x
            while x < PANEL_W and row[x] == word:
                x += 1
            colour = cache.get(word)
            if colour is None:
                r, g, b = from_rgb565(word)
                colour = "#%02x%02x%02x" % (r, g, b)
                cache[word] = colour
            canvas.create_rectangle(bezel + start * zoom, bezel + y * zoom,
                                    bezel + x * zoom - 1, bezel + (y + 1) * zoom - 1,
                                    fill=colour, outline="")


def show(line, fg_word, bg_word, zoom, dump_raw, dump_ppm, x=2, y=None):
    import tkinter as tk

    root = tk.Tk()
    root.title("ST7735 %dx%d - %r" % (PANEL_W, PANEL_H, line))
    rows = render(line, fg_word, bg_word, x=x, y=y)
    canvas = tk.Canvas(root, highlightthickness=0)
    canvas.pack()
    state = {"zoom": zoom}

    def redraw():
        draw(canvas, rows, state["zoom"])

    def save(_event=None):
        path = dump_raw or os.path.join(BASE_DIR, "tmp", "st7735_panel.bin")
        directory = os.path.dirname(os.path.abspath(path))
        if directory and not os.path.isdir(directory):
            os.makedirs(directory)
        with open(path, "wb") as handle:
            handle.write(to_bytes(rows))
        ppm = dump_ppm or (os.path.splitext(path)[0] + ".ppm")
        with open(ppm, "wb") as handle:
            handle.write(to_ppm(rows))
        print("saved %s (%d bytes) and %s" % (path, 160 * 80 * 2, ppm))

    for key in ("1", "2", "4", "6", "8"):
        root.bind(key, lambda _e, z=int(key): (state.update(zoom=z), redraw()))
    root.bind("s", save)
    root.bind("<Escape>", lambda _e: root.destroy())
    root.bind("q", lambda _e: root.destroy())
    redraw()
    print("panel %dx%d, %d cols x %d lines, font %dx%d, text %r (%d px wide)"
          % (PANEL_W, PANEL_H, COLS, LINES, font.CELL_W, font.CELL_H, line,
             len(line) * font.CELL_W))
    root.mainloop()


# -- self-test ----------------------------------------------------------

def _check(condition, message):
    if not condition:
        raise AssertionError(message)


def selftest():
    cases = []

    def case(fn):
        cases.append(fn)
        return fn

    @case
    def panel_geometry():
        _check(PANEL_W == 160 and PANEL_H == 80, "panel must be 160x80")
        _check(COLS == 20, "20 columns of 8px must fit: %d" % COLS)
        _check(LINES == 5, "5 lines of 16px must fit: %d" % LINES)

    @case
    def font_table():
        _check(len(font.GLYPHS) == 95, "95 printable glyphs: %d" % len(font.GLYPHS))
        for code, glyph in zip(range(32, 127), font.GLYPHS):
            _check(len(glyph) == font.CELL_H, "glyph %r row count" % chr(code))
            for byte in glyph:
                _check(0 <= byte <= 255, "glyph %r has a non-byte row" % chr(code))
        _check(font.glyph("A") != font.glyph(" "), "A must differ from space")
        _check(font.glyph("\u4e2d") == font.glyph(" "), "non-ASCII falls back to space")

    @case
    def colour_math():
        _check(to_rgb565((255, 255, 255)) == 0xFFFF, "white is 0xFFFF")
        _check(to_rgb565((0, 0, 0)) == 0x0000, "black is 0x0000")
        _check(to_rgb565((255, 0, 0)) == 0xF800, "red is 0xF800")
        _check(to_rgb565((0, 255, 0)) == 0x07E0, "green is 0x07E0")
        _check(to_rgb565((0, 0, 255)) == 0x001F, "blue is 0x001F")
        back, moved, word = quantised((255, 255, 255))
        _check(back == (255, 255, 255) and not moved, "white survives the round trip")
        back, moved, word = quantised((1, 1, 1))
        _check(moved and back == (0, 0, 0), "near-black must snap to black and say so")
        _check(parse_color("#0f0", None) == (0, 255, 0), "3-digit hex expands")
        _check(parse_color("FFFF00", None) == (255, 255, 0), "bare hex works")

    @case
    def text_fitting():
        _check(prepare_text("A" * 20) == "A" * 20, "exactly 20 chars must fit whole")
        long_line = prepare_text("B" * 25)
        _check(len(long_line) == 20, "overflow is cut to one line: %r" % long_line)
        _check(long_line.endswith(OVERFLOW_MARK), "overflow must be marked")
        _check(prepare_text("tab\there") == "tab    here", "tab expands to 4 spaces")
        _check(prepare_text("a\u00e9b") == "a?b", "non-ASCII becomes ?")

    @case
    def framebuffer():
        bg, fg = 0x0000, 0xFFFF
        rows = render("H", fg, bg)
        _check(len(rows) == PANEL_H, "height")
        _check(all(len(row) == PANEL_W for row in rows), "width")
        lit = sum(1 for row in rows for word in row if word == fg)
        _check(lit > 0, "the glyph must light pixels")
        _check(lit == sum(length for _row, _start, length in ink_runs("H")),
               "framebuffer must hold exactly the glyph's ink")
        top = min(y for y, row in enumerate(rows) if fg in row)
        _check(0 <= top < PANEL_H, "ink lands inside the panel")

        data = to_bytes(rows)
        _check(len(data) == PANEL_W * PANEL_H * 2, "raw dump is 25600 bytes")
        _check(data[0:2] == b"\x00\x00", "first pixel is the black corner, big-endian")
        ppm = to_ppm(rows)
        _check(ppm.startswith(b"P6\n160 80\n255\n"), "PPM header")
        _check(len(ppm) == len(b"P6\n160 80\n255\n") + PANEL_W * PANEL_H * 3,
               "PPM payload is 3 bytes per pixel")

    @case
    def clipping():
        rows = render(prepare_text("W" * 40), 0xFFFF, 0x0000, x=0)
        _check(all(len(row) == PANEL_W for row in rows),
               "an over-long line must not widen the framebuffer")

    failed = 0
    for fn in cases:
        try:
            fn()
            print("PASS %s" % fn.__name__)
        except AssertionError as exc:
            failed += 1
            print("FAIL %s: %s" % (fn.__name__, exc))
    print("%s (%d checks, %d failed)" %
          ("SELFTEST PASS" if not failed else "SELFTEST FAIL", len(cases), failed))
    return 0 if not failed else 1


# -- cli ----------------------------------------------------------------

def parse_args(argv=None):
    ap = argparse.ArgumentParser(
        description="ST7735 160x80 preview: one line of 8x16 text, real RGB565.")
    ap.add_argument("--text", default=DEFAULT_TEXT, help="the line to draw (default %r)"
                    % DEFAULT_TEXT)
    ap.add_argument("--fg", default="FFFFFF", help="text colour, RRGGBB or RGB")
    ap.add_argument("--bg", default="000000", help="background colour, RRGGBB or RGB")
    ap.add_argument("--x", type=int, default=2, help="left margin in pixels (default 2)")
    ap.add_argument("--center", action="store_true",
                    help="centre the line horizontally instead of the left margin")
    ap.add_argument("--y", type=int, default=None,
                    help="top of the text box (default: vertically centred, %d)"
                         % ((PANEL_H - font.CELL_H) // 2))
    ap.add_argument("--zoom", type=int, default=4, choices=[1, 2, 4, 6, 8],
                    help="on-screen magnification (default 4)")
    ap.add_argument("--raw", default=None, metavar="FILE",
                    help="write the RGB565 frame after the press of s")
    ap.add_argument("--ppm", default=None, metavar="FILE", help="same for a PPM")
    ap.add_argument("--selftest", action="store_true", help="run the checks and exit")
    ap.add_argument("--no-window", action="store_true",
                    help="render and write the dumps, but do not open a window")
    return ap.parse_args(argv)


def main(argv=None):
    args = parse_args(argv)
    if args.selftest:
        return selftest()

    fg_rgb = parse_color(args.fg, (255, 255, 255))
    bg_rgb = parse_color(args.bg, (0, 0, 0))
    fg_shown, fg_moved, fg_word = quantised(fg_rgb)
    bg_shown, bg_moved, bg_word = quantised(bg_rgb)
    for name, asked, shown, word, moved in (("fg", fg_rgb, fg_shown, fg_word, fg_moved),
                                            ("bg", bg_rgb, bg_shown, bg_word, bg_moved)):
        note = " (asked %s, panel shows %s: RGB565 rounding)" % (
            "#%02x%02x%02x" % asked, "#%02x%02x%02x" % shown) if moved else ""
        print("%s 0x%04X%s" % (name, word, note))

    line = prepare_text(args.text, COLS)
    if line != args.text:
        print("note: text adjusted for the panel: %r" % (line,))

    x = args.x
    if args.center:
        x = max(0, (PANEL_W - len(line) * font.CELL_W) // 2)
        print("centred: %d px of margin on each side" % x)

    if args.raw or args.ppm or args.no_window:
        rows = render(line, fg_word, bg_word, x=x, y=args.y)
        if args.raw:
            with open(args.raw, "wb") as handle:
                handle.write(to_bytes(rows))
            print("wrote %s (%d bytes)" % (args.raw, PANEL_W * PANEL_H * 2))
        if args.ppm:
            with open(args.ppm, "wb") as handle:
                handle.write(to_ppm(rows))
            print("wrote %s" % args.ppm)
        if args.no_window:
            print("panel %dx%d, font %dx%d, %d cols, text %r (%d px)"
                  % (PANEL_W, PANEL_H, font.CELL_W, font.CELL_H, COLS, line,
                     len(line) * font.CELL_W))
            return 0

    show(line, fg_word, bg_word, args.zoom, args.raw, args.ppm, x=x, y=args.y)
    return 0


if __name__ == "__main__":
    sys.exit(main())
