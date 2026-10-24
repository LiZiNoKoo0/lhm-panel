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
import json
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

#: One RTS pulse is swallowed about one time in three on this CH343 board.
RESET_ATTEMPTS = 4
#: How long to listen for a boot log after each pulse before retrying.
RESET_LISTEN_S = 1.5

#: How many times the frame line is re-sent if no `FRAME` answer comes back.
WRITE_ATTEMPTS = 4
#: How long each retry listens before sending again.
RETRY_LISTEN_S = 2.5

#: How long to wait for the board's `READY` before sending a page anyway.
BOARD_READY_S = 8.0
#: Bytes per write when trickling a long `ui` line (the firmware reads 64 per tick).
SEND_CHUNK = 48


def expected_frame(lines, badge=False):
    """The frame the firmware should be pushing, plus its CRC."""
    rows = P.render_page(lines, 0xFFFF, 0x0000, badge=badge)
    data = P.to_bytes(rows)
    return data, zlib.crc32(data) & 0xFFFFFFFF


def load_page(path):
    """Read an NDJSON fixture: one `ui` line plus any number of `data` lines.

    Values carry across a `ui` exactly like the firmware's `Page` does, so a
    two-page fixture compares against the picture the board would really hold.
    """
    layout = None
    values = {}
    lines = []
    with open(path, "r", encoding="utf-8") as handle:
        for raw in handle:
            line = raw.strip()
            if not line or line.startswith("#"):
                continue
            message = json.loads(line)
            kind = message.get("t")
            if kind == "ui":
                layout = message
            elif kind == "data":
                values = P.apply_data(values, message)
            lines.append(line)
    if layout is None:
        raise SystemExit("FAIL %s has no `ui` line" % path)
    return layout, values, lines


def check_ui(args):
    """Feed a fixture page to the board over the serial port, compare CRCs.

    The UART carries the same JSON the socket would: `Inbox` classifies a line
    by its first byte, so `{` means protocol no matter which wire it came from.
    That is why a page can be proven on a desk with no WiFi and no daemon.
    """
    layout, values, lines = load_page(args.ui)
    data = P.to_bytes(P.ui_page(layout, values, badge=args.link_lost))
    crc = zlib.crc32(data) & 0xFFFFFFFF
    print("PC renders   : ui scene=%s ver=%s nodes=%d%s -> crc=0x%08X (%d bytes)"
          % (layout.get("scene"), layout.get("ver"), len(layout.get("nodes", [])),
             " + LINK LOST badge" if args.link_lost else "", crc, len(data)))
    if args.dump:
        with open(args.dump, "wb") as handle:
            handle.write(data)
        print("wrote %s (%d bytes)" % (args.dump, len(data)))

    port = serialport.Port(args.port, args.baud)
    text = ""
    try:
        # Wait for the board to be in its loop before sending: an 861-byte line
        # handed to a board that is still printing its boot banner overflows the
        # 128-byte UART FIFO, and the line arrives as a corrupt fragment (which
        # `LineBuf` drops whole -- correct, and silent). `READY` is printed after
        # the panel is up, before the first card, so it is the marker.
        deadline = time.time() + BOARD_READY_S
        ready = False
        started = time.time()
        last_byte = started
        while time.time() < deadline:
            chunk = port.read()
            if chunk:
                last_byte = time.time()
                if b"READY" in chunk or b"FRAME " in chunk:
                    ready = True
                    break
            else:
                time.sleep(0.02)
                # A board that has been up for a while prints nothing until
                # something changes, so silence *is* the "ready" signal -- only
                # once it has had time to get through its boot banner.
                if time.time() - started > 1.5 and time.time() - last_byte > 1.0:
                    ready = True
                    break
        if not ready:
            print("note: no READY seen in %ds, sending anyway" % BOARD_READY_S)

        # Trickled, not blasted: the firmware reads 64 bytes per 5 ms tick, so a
        # chunk every 6 ms keeps the FIFO from ever filling. 115200 baud would
        # also work in theory; in practice the board is busy blitting SPI.
        for attempt in range(WRITE_ATTEMPTS):
            for line in lines:
                payload = line.encode("ascii") + b"\n"
                for at in range(0, len(payload), SEND_CHUNK):
                    port.write(payload[at:at + SEND_CHUNK])
                    time.sleep(0.006)
                time.sleep(0.05)
            deadline = time.time() + (args.seconds if attempt == 0 else RETRY_LISTEN_S)
            while time.time() < deadline:
                chunk = port.read()
                if chunk:
                    text += chunk.decode("ascii", "replace")
                    if "PAGE ui" in text and text.count("FRAME ") >= 1:
                        # One more read window so the `data` frame lands too.
                        pass
                else:
                    time.sleep(0.02)
            if "PAGE ui" in text:
                break
    finally:
        port.close()

    print("--- serial ---")
    print(text.strip()[-800:] if text.strip() else "(nothing)")
    print("-------------")

    frames = [line for line in text.splitlines() if line.strip().startswith("FRAME ")]
    if not frames:
        print("FAIL no FRAME line: is the board flashed and on %s?" % args.port)
        return 1
    seen = [parse_frame_line(line) for line in frames]
    seen = [frame for frame in seen if frame is not None]
    if not seen:
        print("FAIL could not parse %r" % frames[-1])
        return 1
    width, height, board_crc, nbytes = seen[-1]
    # Any frame in this run, not only the last one: a live daemon keeps streaming
    # its own `data` while the fixture is on the glass, so the final frame can be
    # the same page carrying the daemon's numbers instead of the fixture's. The
    # claim under test is "the board painted these exact bytes", and one such
    # frame is proof of it.
    matched = [frame for frame in seen if frame[2] == crc]
    ok = True
    if (width, height) != (P.PANEL_W, P.PANEL_H):
        print("FAIL panel is %sx%s, preview is %dx%d" % (width, height, P.PANEL_W, P.PANEL_H))
        ok = False
    if not matched:
        print("FAIL crc board=0x%08X preview=0x%08X (%d frames seen: %s)"
              % (board_crc, crc, len(seen), " ".join("0x%08X" % f[2] for f in seen[:8])))
        ok = False
    print("UI PARITY %s (crc=0x%08X, %s bytes, %d frames, %d matched)"
          % ("OK" if ok else "FAIL", matched[-1][2] if matched else board_crc,
             matched[-1][3] if matched else nbytes, len(seen), len(matched)))
    return 0 if ok else 1


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


def check_boot(args):
    """Compare the boot card on the glass with the card the PC draws.

    Needs a reset: the card is painted only when a row changes, so a board that
    settled ten minutes ago will not repaint for us. Four rows settling means
    four frames, and the *last* one is the finished card.
    """
    data = P.to_bytes(P.boot_page())
    crc = zlib.crc32(data) & 0xFFFFFFFF
    print("PC renders   : SELF TEST card, all rows ok -> crc=0x%08X (%d bytes)"
          % (crc, len(data)))

    port = serialport.Port(args.port, args.baud)
    text = ""
    try:
        for attempt in range(RESET_ATTEMPTS):
            port.reset_board()
            heard = ""
            deadline = time.time() + 20.0
            while time.time() < deadline:
                chunk = port.read()
                if chunk:
                    heard += chunk.decode("ascii", "replace")
                    # Stop once the last row has settled *and* been painted.
                    if "BOOT LINK ok" in heard and heard.rfind("FRAME") > heard.rfind("BOOT LINK ok"):
                        break
                else:
                    time.sleep(0.02)
            if heard.strip():
                text = heard
                if "BOOT" in heard:
                    break
            print("reset attempt %d/%d: no boot log, retrying" % (attempt + 1, RESET_ATTEMPTS))
    finally:
        port.close()

    print("--- serial ---")
    print(text.strip()[-800:] if text.strip() else "(nothing)")
    print("-------------")

    frames = [line for line in text.splitlines() if line.strip().startswith("FRAME ")]
    if not frames:
        print("FAIL no FRAME line: is the board flashed and idle (no layout) on %s?" % args.port)
        return 1

    last = parse_frame_line(frames[-1])
    if last is None:
        print("FAIL could not parse %r" % frames[-1])
        return 1
    width, height, board_crc, nbytes = last
    ok = True
    if (width, height) != (P.PANEL_W, P.PANEL_H):
        print("FAIL panel is %sx%s, preview is %dx%d" % (width, height, P.PANEL_W, P.PANEL_H))
        ok = False
    if board_crc != crc:
        print("FAIL crc board=0x%08X preview=0x%08X (%d frames seen)"
              % (board_crc, crc, len(frames)))
        ok = False
    print("BOOT PARITY %s (crc=0x%08X, %s bytes, %d frames)"
          % ("OK" if ok else "FAIL", board_crc, nbytes, len(frames)))
    return 0 if ok else 1


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
    ap.add_argument("--link-lost", action="store_true",
                    help="expect the LINK LOST badge: the daemon must be down, "
                         "and the board must already be showing a layout")
    ap.add_argument("--boot", action="store_true",
                    help="expect the boot self-test card with every row ok "
                         "(the board must have no layout yet); implies --reset")
    ap.add_argument("--ui", default=None, metavar="FILE",
                    help="send this NDJSON fixture (one `ui` + optional `data` lines) "
                         "to the board and compare the frame it paints")
    args = ap.parse_args()

    if args.ui:
        return check_ui(args)

    if args.boot:
        return check_boot(args)

    if args.lines:
        top, _, bottom = args.lines.partition("|")
        lines = (top, bottom)
    else:
        lines = SAMPLE

    data, crc = expected_frame(lines, badge=args.link_lost)
    print("PC renders   : %r / %r%s -> crc=0x%08X (%d bytes)"
          % (lines[0], lines[1], " + LINK LOST badge" if args.link_lost else "", crc, len(data)))
    if args.dump:
        with open(args.dump, "wb") as handle:
            handle.write(data)
        print("wrote %s (%d bytes)" % (args.dump, len(data)))

    port = serialport.Port(args.port, args.baud)
    try:
        if args.reset:
            # A swallowed RTS pulse leaves the chip in whatever state it was in
            # -- often the ROM loader right after espflash, which answers
            # nothing. Retry until real bytes come back, otherwise a working
            # firmware looks dead.
            for attempt in range(RESET_ATTEMPTS):
                port.reset_board()
                deadline = time.time() + RESET_LISTEN_S
                heard = b""
                while time.time() < deadline:  # swallow the boot banner
                    chunk = port.read()
                    if chunk:
                        heard += chunk
                    else:
                        time.sleep(0.02)
                if heard:
                    break
                print("reset attempt %d/%d: no boot log, retrying"
                      % (attempt + 1, RESET_ATTEMPTS))
        time.sleep(0.2)

        # The first line is not always heard. Opening a COM port asserts DTR/RTS
        # (IO0/EN on this board), so the board may still be booting when the
        # write lands, and a swallowed line looks exactly like dead firmware.
        # Re-sending is harmless: the same content just repaints the same frame.
        text = ""
        for attempt in range(WRITE_ATTEMPTS):
            port.write_line("%s|%s" % lines)
            deadline = time.time() + (args.seconds if attempt == 0 else RETRY_LISTEN_S)
            while time.time() < deadline:
                chunk = port.read()
                if chunk:
                    text += chunk.decode("ascii", "replace")
                    if parse_frame_line(text) is not None:
                        break
                else:
                    time.sleep(0.02)
            if parse_frame_line(text) is not None:
                break
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
