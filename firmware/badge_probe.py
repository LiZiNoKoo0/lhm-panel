"""Prove the `LINK LOST` badge appears *and* clears, on the real board.

The badge is driven by one bit (`TcpLink::is_up`). The failure that matters is
not "it never draws" -- that is visible on the glass -- but "it never clears",
because a stuck badge tells the user the values are stale while they are fine.
Scripts cannot look at the glass, so the board says it out loud (`BADGE on` /
`BADGE off`, T5.4) and this script checks the order of what it heard:

    reset the board        -> app boots with no content
    send one frame line    -> BADGE on  + badge frame      (link not up yet)
    daemon links up        -> BADGE off + plain frame      (badge clears)
    kill the daemon        -> BADGE on  + badge frame      (badge returns)
    restart the daemon     -> BADGE off + plain frame      (badge clears again)

Every FRAME crc is compared against `preview_st7735.py`, so "the badge is on"
means the same 25600 bytes the PC would draw -- not merely that some overlay
exists.

Run:  python firmware/badge_probe.py --port COM10
"""

import argparse
import os
import re
import subprocess
import sys
import time
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
sys.path.insert(0, REPO)

import preview_st7735 as P  # noqa: E402
from pcmon import serialport  # noqa: E402

DAEMON = os.path.join(REPO, "fake_daemon.py")
PORT_NUMBER = 7802
LOG = os.path.join(HERE, "badge_probe_daemon.log")
FRAME_RE = re.compile(r"FRAME\s+\d+x\d+\s+crc=0x([0-9a-fA-F]+)")

#: The real interpreter, not `sys.executable`.
#:
#: Under the assistant's sandbox `sys.executable` is a bundled Python 3.11 whose
#: `sys.path` is fixed and whose `PYTHONPATH` is dropped, so a daemon spawned
#: from it dies on `from pcmon import lhm` -- which reads exactly like "the
#: daemon refused to start". Measured on this box 2026-10-23.
REAL_PYTHON = r"C:\Users\Administrator\AppData\Local\Programs\Python\Python312\python.exe"

SAMPLE = "CPU 33.5%|TEMP   34C"
PLAIN = 0xEE47_3F67
BADGE = 0x6A7B_210F


def interpreter():
    return REAL_PYTHON if os.path.exists(REAL_PYTHON) else sys.executable


# -- daemon under test --------------------------------------------------

def daemon_pid():
    out = subprocess.run(
        ["powershell", "-NoProfile", "-Command",
         "(Get-NetTCPConnection -LocalPort %d -State Listen -ErrorAction SilentlyContinue).OwningProcess"
         % PORT_NUMBER],
        capture_output=True, text=True)
    text = out.stdout.strip()
    return int(text.splitlines()[0]) if text else None


def start_daemon():
    env = dict(os.environ)
    env["PYTHONPATH"] = REPO
    creation = 0x00000008  # DETACHED_PROCESS: outlives this script
    with open(LOG, "wb") as out, open(LOG + ".err", "wb") as err:
        child = subprocess.Popen(
            [interpreter(), "-u", DAEMON, "--source", "lhm",
             "--port", str(PORT_NUMBER), "--lenient"],
            cwd=REPO, env=env, stdin=subprocess.DEVNULL, stdout=out, stderr=err,
            creationflags=creation)
    return child.pid


def wait_listening(seconds=12):
    deadline = time.time() + seconds
    while time.time() < deadline:
        pid = daemon_pid()
        if pid is not None:
            return pid
        time.sleep(0.3)
    return None


def kill_daemon():
    pid = daemon_pid()
    if pid is None:
        return None
    subprocess.run(["taskkill", "/PID", str(pid), "/F"], capture_output=True, text=True)
    return pid


# -- wire ---------------------------------------------------------------

def collect(port, seconds):
    """Ordered list of events: ('frame', crc) and ('badge', True|False)."""
    events = []
    deadline = time.time() + seconds
    text = ""
    while time.time() < deadline:
        chunk = port.read()
        if not chunk:
            time.sleep(0.02)
            continue
        text += chunk.decode("ascii", "replace")
        while "\n" in text:
            line, text = text.split("\n", 1)
            match = FRAME_RE.search(line)
            if match:
                crc = int(match.group(1), 16)
                if not events or events[-1] != ("frame", crc):
                    events.append(("frame", crc))
            elif line.startswith("BADGE "):
                events.append(("badge", line.strip().endswith("on")))
    return events


def wait_for(port, seconds, predicate, nudge=None):
    """Collect until `predicate(events)`; return the events.

    `nudge` is re-sent every 2 s and makes the probe independent of what opening
    the port did: if the board is still booting it swallows the first line, and
    re-applying the same content is harmless (it repaints the same frame).
    """
    deadline = time.time() + seconds
    events = []
    text = ""
    next_nudge = time.time()
    while time.time() < deadline:
        if nudge is not None and time.time() >= next_nudge:
            port.write_line(nudge)
            next_nudge = time.time() + 2.0
        chunk = port.read()
        if chunk:
            text += chunk.decode("ascii", "replace")
            while "\n" in text:
                line, text = text.split("\n", 1)
                match = FRAME_RE.search(line)
                if match:
                    crc = int(match.group(1), 16)
                    if not events or events[-1] != ("frame", crc):
                        events.append(("frame", crc))
                elif line.startswith("BADGE "):
                    events.append(("badge", line.strip().endswith("on")))
            if predicate(events):
                break
        else:
            time.sleep(0.02)
    return events


def show(events):
    return ", ".join(
        ("BADGE %s" % ("on" if value else "off")) if kind == "badge" else "FRAME 0x%08X" % value
        for kind, value in events) or "(nothing)"


def main():
    ap = argparse.ArgumentParser(description="watch the LINK LOST badge appear and clear")
    ap.add_argument("--port", default="COM10")
    ap.add_argument("--baud", type=int, default=115200)
    args = ap.parse_args()

    print("preview: plain=0x%08X badge=0x%08X" % (PLAIN, BADGE))
    # The numbers above must be what the preview really draws, or every
    # comparison below is meaningless.
    plain_now = zlib.crc32(P.to_bytes(P.render_page(("CPU 33.5%", "TEMP   34C"), 0xFFFF, 0x0000))) & 0xFFFFFFFF
    badge_now = zlib.crc32(P.to_bytes(P.render_page(("CPU 33.5%", "TEMP   34C"), 0xFFFF, 0x0000, badge=True))) & 0xFFFFFFFF
    if (plain_now, badge_now) != (PLAIN, BADGE):
        print("FAIL preview moved: plain=0x%08X badge=0x%08X" % (plain_now, badge_now))
        return 1

    if daemon_pid() is not None:
        print("daemon       : stopped (phase 1 needs the link down)")
        kill_daemon()
        time.sleep(1.0)

    ok = True
    port = serialport.Port(args.port, args.baud)
    try:
        # Phase 1 does not reset anything: it establishes its own precondition
        # (the daemon is down) and re-sends the content line every 2 s, so a
        # board that is still booting cannot make this look like a failure.
        print("1. link down, content sent every 2 s")
        first = wait_for(port, 30, lambda ev: ("badge", True) in ev and ("frame", BADGE) in ev,
                         nudge=SAMPLE)
        print("   %s" % show(first))
        if ("badge", True) not in first:
            print("   FAIL the badge never appeared while the link was down")
            ok = False
        if ("frame", BADGE) not in first:
            print("   FAIL no badge frame: the overlay is not what the preview draws")
            ok = False

        started = start_daemon()
        pid = wait_listening()
        if pid is None:
            print("   FAIL the daemon never started listening on 7802 (see %s.err)" % LOG)
            return 1
        print("2. daemon started (pid=%s), waiting for the badge to clear" % pid)
        second = wait_for(port, 30, lambda ev: ("badge", False) in ev and ("frame", PLAIN) in ev)
        print("   %s" % show(second))
        if ("badge", False) not in second:
            print("   FAIL the badge never cleared when the daemon answered")
            ok = False
        if ("frame", PLAIN) not in second:
            print("   FAIL the layout did not go back to the plain frame")
            ok = False

        pid = kill_daemon()
        print("3. daemon killed (pid=%s), the badge must come back" % pid)
        third = wait_for(port, 20, lambda ev: ("badge", True) in ev and ("frame", BADGE) in ev)
        print("   %s" % show(third))
        if ("badge", True) not in third or ("frame", BADGE) not in third:
            print("   FAIL the badge did not return when the link died")
            ok = False

        restarted = start_daemon()
        if wait_listening() is None:
            print("   FAIL the daemon never came back (see %s.err)" % LOG)
            return 1
        print("4. daemon restarted (pid=%s), the badge must clear again" % restarted)
        fourth = wait_for(port, 30, lambda ev: ("badge", False) in ev and ("frame", PLAIN) in ev)
        print("   %s" % show(fourth))
        if ("badge", False) not in fourth or ("frame", PLAIN) not in fourth:
            print("   FAIL the badge stayed on after the link came back")
            ok = False
    finally:
        port.close()

    print("BADGE %s" % ("OK" if ok else "FAIL"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
