"""Push live CPU load / temperature to the ST7735 panel over USB serial.

    python live_serial.py                       # COM10, 1 Hz, LibreHardwareMonitor
    python live_serial.py --source demo         # sine wave, no LHM needed
    python live_serial.py --interval 0.5 --seconds 30
    python live_serial.py --once                # one frame and exit

What goes over the wire (firmware/st7735-text/src/main.rs):

    "<top row>|<bottom row>\\n"          ->  the board answers
    "FRAME 160x80 crc=0x........ bytes=25600\\n"

The CRC is of the 25600 bytes the board actually pushed, so every single frame
is checked against what this script rendered locally. A mismatch is reported as
BAD and the number is never assumed to be "close enough".

What this file deliberately does NOT do:
  * own the sensor readings -- pcmon/lhm.py does, this only formats them
  * invent numbers when a sensor is missing: PROTOCOL.md s.9, missing shows "--"
  * decide anything about the protocol or the layout system -- this is a serial
    bridge for P1 hand-testing; M6 moves the same thin-device idea onto TCP 7802
  * retry forever on a dead port -- it says so and exits non-zero

Pure ASCII source on purpose (TASKS.md coding rule 1).
"""

import argparse
import math
import os
import sys
import time
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import preview_st7735 as P  # noqa: E402
from pcmon import serialport  # noqa: E402

FG = 0xFFFF
BG = 0x0000

#: How long to wait for the board's FRAME line after sending one frame.
ACK_TIMEOUT_S = 1.0


def two_lines(cpu, temp):
    """The two rows: fixed labels, right-aligned values, '--' when missing."""
    first = "CPU  %5.1f%%" % cpu if cpu is not None else "CPU     --"
    second = "TEMP %4dC" % round(temp) if temp is not None else "TEMP    --"
    return first, second


def expected_crc(lines):
    """CRC32 of the frame those two lines should produce on the glass."""
    rows = P.render_page(lines, FG, BG)
    return zlib.crc32(P.to_bytes(rows)) & 0xFFFFFFFF


def wait_for_frame(port, timeout=ACK_TIMEOUT_S):
    """Read until a 'FRAME ...' line arrives; returns that line or None."""
    buf = b""
    deadline = time.time() + timeout
    while time.time() < deadline:
        chunk = port.read()
        if chunk:
            buf += chunk
            if b"\n" in buf:
                for raw in buf.split(b"\n"):
                    line = raw.decode("ascii", "replace").strip()
                    if line.startswith("FRAME "):
                        return line
        else:
            time.sleep(0.01)
    return None


def drain(port, seconds):
    """Throw away whatever the board printed while it was booting."""
    deadline = time.time() + seconds
    while time.time() < deadline:
        if not port.read():
            time.sleep(0.02)


def crc_from_line(line):
    """'FRAME 160x80 crc=0x1A2B3C4D bytes=25600' -> 0x1A2B3C4D."""
    for token in line.split():
        if token.startswith("crc=0x"):
            return int(token[6:], 16)
    return None


class DemoSource(object):
    """Sine-wave stand-in so the link can be tested without LibreHardwareMonitor."""

    def __init__(self):
        self.started = time.time()

    def start(self):
        pass

    def stop(self):
        pass

    def snapshot(self, now=None):
        t = time.time() - self.started
        return {
            "cpu.total": 45.0 + 40.0 * math.sin(t * 0.7),
            "cpu.temp": 42.0 + 6.0 * math.sin(t * 0.3),
        }


def make_source(name):
    if name == "demo":
        return DemoSource()
    from pcmon.lhm import LhmSource

    return LhmSource()


def main(argv=None):
    ap = argparse.ArgumentParser(description="push live CPU/temp to the ST7735 panel")
    ap.add_argument("--port", default="COM10")
    ap.add_argument("--interval", type=float, default=1.0, help="seconds between frames")
    ap.add_argument("--seconds", type=float, default=0.0, help="stop after N seconds (0 = forever)")
    ap.add_argument("--source", choices=("lhm", "demo"), default="lhm")
    ap.add_argument("--once", action="store_true", help="send one frame and exit")
    ap.add_argument("--reset", action="store_true",
                    help="restart the board first (only needed to see the boot banner;\n"
                         "the live link itself works without a reset)")
    args = ap.parse_args(argv)

    source = make_source(args.source)
    source.start()
    try:
        port = serialport.Port(args.port)
    except OSError as exc:
        source.stop()
        print("FAIL cannot open %s: %s" % (args.port, exc))
        return 1

    try:
        if args.reset:
            port.reset_board()
            drain(port, 2.0)  # boot banner + READY; nothing to draw yet
        time.sleep(0.2)

        frames = 0
        bad = 0
        started = time.time()
        while True:
            tick = time.time()
            snapshot = source.snapshot(time.monotonic())
            lines = two_lines(snapshot.get("cpu.total"), snapshot.get("cpu.temp"))
            if port.write_line("%s|%s" % lines) == 0:
                print("FAIL write to %s failed" % args.port)
                return 1

            reply = wait_for_frame(port)
            want = expected_crc(lines)
            frames += 1
            if reply is None:
                bad += 1
                print("%-20s %-10s no answer from the board" % lines)
            else:
                got = crc_from_line(reply)
                ok = got == want
                if not ok:
                    bad += 1
                print("%-20s %-10s crc 0x%08X %s" % (lines[0], lines[1], got,
                                                     "ok" if ok else "BAD (want 0x%08X)" % want))

            if args.once:
                break
            if args.seconds and time.time() - started >= args.seconds:
                break
            time.sleep(max(0.0, args.interval - (time.time() - tick)))
    finally:
        port.close()
        source.stop()

    print("%d frames, %d bad" % (frames, bad))
    return 0 if bad == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
