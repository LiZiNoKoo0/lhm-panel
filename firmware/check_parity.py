"""Prove the panel shows what the PC sends -- byte for byte, no camera.

The firmware composes the 160x80 frame in RAM, prints a CRC32 of those exact
25600 bytes over the serial port, then pushes that same buffer to the glass.
This script renders the same two rows with preview_st7735.py, hashes it the same
way, and compares. Same number = identical pixels.

    python firmware/check_parity.py                     # COM10
    python firmware/check_parity.py --port COM7
    python firmware/check_parity.py --lines "CPU 12.3%|TEMP   51C"
    python firmware/check_parity.py --no-reset          # just talk, do not reset

Nothing is read out of main.rs any more: the panel draws whatever it is given,
so the sample below is the whole input.
"""
import argparse
import os
import sys
import time
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
sys.path.insert(0, REPO)

import preview_st7735 as P  # noqa: E402
from pcmon import serialport  # noqa: E402

FRAME_RE = r"FRAME\s+(\d+)x(\d+)"

#: The frame the check draws if the caller does not pass --lines.
SAMPLE = ("CPU 33.5%", "TEMP   34C")


def expected_frame(lines):
    """The frame the firmware should be pushing, plus its CRC."""
    rows = P.render_page(lines, 0xFFFF, 0x0000)
    data = P.to_bytes(rows)
    return data, zlib.crc32(data) & 0xFFFFFFFF


def parse_frame_line(text):
    """Find the last 'FRAME WxH crc=0x... bytes=N' line; returns (w, h, crc, n)."""
    found = None
    for raw in text.splitlines():
        line = raw.strip()
        if not line.startswith("FRAME "):
            continue
        width = height = crc = nbytes = None
        for token in line.split():
            if token.startswith("crc=0x"):
                crc = int(token[6:], 16)
            elif token.startswith("bytes="):
                nbytes = int(token[6:])
            elif "x" in token and token[0].isdigit():
                width, height = (int(part) for part in token.split("x", 1))
        if crc is not None:
            found = (width, height, crc, nbytes)
    return found


def main():
    ap = argparse.ArgumentParser(description="compare the panel's frame CRC with the preview")
    ap.add_argument("--port", default="COM10")
    ap.add_argument("--baud", type=int, default=115200)
    ap.add_argument("--seconds", type=float, default=6.0)
    ap.add_argument("--reset", action="store_true",
                    help="restart the board first (only to read the boot banner; "
                         "the FRAME reply does not need it)")
    ap.add_argument("--lines", default=None, metavar="TOP|BOTTOM",
                    help="two rows separated by '|', default %r" % ("|".join(SAMPLE),))
    ap.add_argument("--dump", default=None, metavar="FILE", help="write the expected raw frame")
    args = ap.parse_args()

    if args.lines:
        top, _, bottom = args.lines.partition("|")
        lines = (top, bottom)
    else:
        lines = SAMPLE

    data, crc = expected_frame(lines)
    print("PC renders   : %r / %r -> crc=0x%08X (%d bytes)"
          % (lines[0], lines[1], crc, len(data)))
    if args.dump:
        with open(args.dump, "wb") as handle:
            handle.write(data)
        print("wrote %s (%d bytes)" % (args.dump, len(data)))

    port = serialport.Port(args.port, args.baud)
    try:
        if args.reset:
            port.reset_board()
            deadline = time.time() + 2.0
            while time.time() < deadline:  # swallow the boot banner
                if not port.read():
                    time.sleep(0.02)
        time.sleep(0.2)

        port.write_line("%s|%s" % lines)

        text = ""
        deadline = time.time() + args.seconds
        while time.time() < deadline:
            chunk = port.read()
            if chunk:
                text += chunk.decode("ascii", "replace")
                if parse_frame_line(text) is not None:
                    break
            else:
                time.sleep(0.02)
    finally:
        port.close()

    print("--- serial ---")
    print(text.strip()[-800:] if text.strip() else "(nothing)")
    print("-------------")

    frame = parse_frame_line(text)
    if frame is None:
        print("FAIL no FRAME line: is the board flashed and on %s?" % args.port)
        return 1

    width, height, board_crc, nbytes = frame
    ok = True
    if (width, height) != (P.PANEL_W, P.PANEL_H):
        print("FAIL panel is %sx%s, preview is %dx%d" % (width, height, P.PANEL_W, P.PANEL_H))
        ok = False
    if nbytes is not None and nbytes != len(data):
        print("FAIL frame is %d bytes, preview is %d" % (nbytes, len(data)))
        ok = False
    if board_crc != crc:
        print("FAIL crc board=0x%08X preview=0x%08X" % (board_crc, crc))
        ok = False
    print("PARITY %s (crc=0x%08X, %s bytes)"
          % ("OK" if ok else "FAIL", board_crc, nbytes))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
