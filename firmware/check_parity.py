"""Prove that the panel shows what the preview shows -- byte for byte, no camera.

The firmware composes the 160x80 frame in RAM and prints a CRC32 of those exact
25600 bytes over the serial port. This script renders the same frame with
preview_st7735.py, hashes it the same way, and compares. Same number = the
firmware and the PC put identical pixels on the glass.

    python firmware/check_parity.py                     # COM10, auto reset
    python firmware/check_parity.py --port COM7
    python firmware/check_parity.py --no-reset          # just listen

TEXT, CENTER_ME, FG and BG are read out of src/main.rs, so there is one source
of truth: whatever you flash is what gets checked.
"""
import argparse
import os
import re
import sys
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
sys.path.insert(0, REPO)

import preview_st7735 as P  # noqa: E402

MAIN_RS = os.path.join(HERE, "st7735-text", "src", "main.rs")
FRAME_RE = re.compile(r"FRAME\s+(\d+)x(\d+)\s+crc=0x([0-9A-Fa-f]{8})\s+bytes=(\d+)")


def firmware_settings(path=MAIN_RS):
    """Pull the constants out of main.rs so the check follows the firmware."""
    text = open(path, encoding="utf-8").read()

    def const(name, pattern, cast=str):
        match = re.search(r"const\s+%s\s*:\s*[^=]+=\s*%s" % (name, pattern), text)
        if not match:
            raise SystemExit("cannot find %s in %s" % (name, path))
        return cast(match.group(1))

    line = const("TEXT", r'"([^"]*)"')
    center = const("CENTER_ME", r"(true|false)") == "true"
    margin = const("MARGIN_X", r"(\d+)", int)
    fg = const("FG", r"0x([0-9A-Fa-f]{4})", lambda v: int(v, 16))
    bg = const("BG", r"0x([0-9A-Fa-f]{4})", lambda v: int(v, 16))
    return line, center, margin, fg, bg


def expected_frame(line, center, margin, fg_word, bg_word):
    """The frame the firmware should be pushing, plus its CRC."""
    fitted = P.prepare_text(line, P.COLS)
    width = len(fitted) * P.font.CELL_W
    x = (P.PANEL_W - width) // 2 if center and width < P.PANEL_W else margin
    y = (P.PANEL_H - P.font.CELL_H) // 2
    rows = P.render(fitted, fg_word, bg_word, x=x, y=y)
    data = P.to_bytes(rows)
    return fitted, x, y, data, zlib.crc32(data) & 0xFFFFFFFF


def read_serial(port, baud, seconds, reset, until=None):
    """Collect serial text; optionally pulse the auto-reset lines first.

    Uses the repo's ctypes port (pcmon/serialport.py) instead of pyserial: the
    project ships without pip dependencies.
    """
    from pcmon import serialport
    return serialport.read_for(port, baud=baud, seconds=seconds, reset=reset, until=until)


def main():
    ap = argparse.ArgumentParser(description="compare the panel's frame CRC with the preview")
    ap.add_argument("--port", default="COM10")
    ap.add_argument("--baud", type=int, default=115200)
    ap.add_argument("--seconds", type=float, default=12.0)
    ap.add_argument("--no-reset", action="store_true", help="do not pulse DTR/RTS")
    ap.add_argument("--dump", default=None, metavar="FILE", help="write the expected raw frame")
    args = ap.parse_args()

    line, center, margin, fg, bg = firmware_settings()
    fitted, x, y, data, crc = expected_frame(line, center, margin, fg, bg)
    print("firmware says: text=%r centre=%s margin=%d fg=0x%04X bg=0x%04X"
          % (line, center, margin, fg, bg))
    print("PC renders   : %r x=%d y=%d w=%dpx crc=0x%08X"
          % (fitted, x, y, len(fitted) * P.font.CELL_W, crc))
    if args.dump:
        with open(args.dump, "wb") as handle:
            handle.write(data)
        print("wrote %s (%d bytes)" % (args.dump, len(data)))

    text = read_serial(args.port, args.baud, args.seconds, not args.no_reset,
                       until=lambda b: "READY" in b)
    print("--- serial ---")
    print(text.strip()[-1200:] if text.strip() else "(nothing)")
    print("-------------")

    match = FRAME_RE.search(text)
    if not match:
        print("FAIL no FRAME line: is the board flashed and on %s?" % args.port)
        return 1
    width, height, board_crc, nbytes = (int(match.group(1)), int(match.group(2)),
                                        int(match.group(3), 16), int(match.group(4)))
    ok = True
    if (width, height) != (P.PANEL_W, P.PANEL_H):
        print("FAIL panel is %dx%d, preview is %dx%d" % (width, height, P.PANEL_W, P.PANEL_H))
        ok = False
    if nbytes != len(data):
        print("FAIL frame is %d bytes, preview is %d" % (nbytes, len(data)))
        ok = False
    if board_crc != crc:
        print("FAIL crc board=0x%08X preview=0x%08X" % (board_crc, crc))
        ok = False
    if "PANIC" in text:
        print("FAIL the firmware panicked")
        ok = False
    print("PARITY %s (crc=0x%08X, %d bytes)" %
          ("OK" if ok else "FAIL", board_crc, nbytes))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
