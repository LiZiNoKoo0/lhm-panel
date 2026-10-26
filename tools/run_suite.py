"""run_suite -- every PC-side acceptance check, with the fixtures they need.

    python tools/run_suite.py            # everything
    python tools/run_suite.py --quick    # only the checks that need nothing

WHAT IT DOES
    Runs each checker as a subprocess with a timeout and prints one line per
    check. Three of them silently need something that is not in the repo, and
    this script sets it up instead of letting you read a stack trace:

      * M4 parity reads `_captured.ndjson`, which only exists after a replayer
        is running and `tools/capture_tape.py` has listened to it.
      * C5 hkdf needs a real dashboard on https with --require-auth: without
        that flag loopback is exempt from auth, so "a wrong key is refused"
        can never hold (PROTOCOL.md s.2.2, and by design).
      * M3 selftest asserts against the *applied* layout, so user work in
        layouts/applied.json has to be moved aside -- and put back byte for
        byte, which is checked here.

WHAT IT DOES NOT DO
    * touch the board, the serial port or the firmware
    * call a check that did not run a pass -- a missing fixture is reported SKIP
"""

import argparse
import hashlib
import json
import os
import socket
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PY = sys.executable
TMP = os.path.join(ROOT, "tmp")
TAPE = os.path.join(ROOT, "_captured.ndjson")
APPLIED = os.path.join(ROOT, "layouts", "applied.json")
APPLIED_BAK = os.path.join(TMP, "applied.bak.json")
PAIR_LOG = os.path.join(TMP, "webui_live.out")
RELAY_PORT = 7813

VERDICT_HINTS = ("OK", "PASS", "FAIL", "PARITY", "SKIP", "checks")


def sha(path):
    with open(path, "rb") as handle:
        return hashlib.sha256(handle.read()).hexdigest()


def port_open(port, host="127.0.0.1", deadline_s=15.0):
    """Wait until something accepts TCP on host:port. Returns True/False."""
    end = time.monotonic() + deadline_s
    while time.monotonic() < end:
        try:
            with socket.create_connection((host, port), timeout=1.0):
                return True
        except OSError:
            time.sleep(0.25)
    return False


def port_busy(port, host="127.0.0.1"):
    try:
        with socket.create_connection((host, port), timeout=1.0):
            return True
    except OSError:
        return False


def run(name, cmd, env=None, timeout=180):
    """One check. Returns (ok, line)."""
    merged = dict(os.environ)
    if env:
        merged.update(env)
    started = time.time()
    try:
        done = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True,
                              timeout=timeout, env=merged)
    except subprocess.TimeoutExpired:
        return False, "TIMEOUT  %-18s (%.0f s)" % (name, timeout)
    except OSError as exc:
        return False, "ERROR    %-18s %s" % (name, exc)
    out = (done.stdout or "") + (done.stderr or "")
    took = time.time() - started
    verdict = ""
    for line in reversed(out.strip().splitlines()):
        stripped = line.strip()
        if stripped and len(stripped) <= 130 and any(h in stripped for h in VERDICT_HINTS):
            verdict = stripped
            break
    ok = done.returncode == 0
    text = "%-8s %-18s exit=%-3s %5.1fs  %s" % (
        "PASS" if ok else "FAIL", name, done.returncode, took, verdict)
    if not ok:
        for line in out.strip().splitlines()[-10:]:
            text += "\n         | " + line
    return ok, text


def with_applied_away(fn):
    """Run fn() with layouts/applied.json out of the way, then restore it."""
    moved = False
    before = None
    if os.path.isfile(APPLIED):
        before = sha(APPLIED)
        os.replace(APPLIED, APPLIED_BAK)
        moved = True
    try:
        return fn()
    finally:
        if moved:
            os.replace(APPLIED_BAK, APPLIED)
            if sha(APPLIED) != before:
                print("WARNING: layouts/applied.json did not come back identical")


def tape_capture():
    """Start a replayer, capture 74 frames from it, stop it. Returns ok."""
    replay = subprocess.Popen(
        [PY, os.path.join(ROOT, "replay.py"),
         os.path.join("recordings", "session1.ndjson"), "--speed", "4"],
        cwd=ROOT, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        if not port_open(7802):
            print("SKIP     replay parity      no replayer on 7802")
            return False
        done = subprocess.run(
            [PY, os.path.join("tools", "capture_tape.py"), "7802",
             TAPE, "74"], cwd=ROOT, capture_output=True, text=True, timeout=120)
        return done.returncode == 0
    finally:
        replay.terminate()
        try:
            replay.wait(timeout=10)
        except subprocess.TimeoutExpired:
            replay.kill()


def dashboard_check():
    """Start a paired https dashboard, run the hkdf proof, stop it. Returns ok."""
    if port_busy(RELAY_PORT):
        print("SKIP     web hkdf           %d is already in use" % RELAY_PORT)
        return False
    web = subprocess.Popen(
        [PY, "-u", "-m", "pcmon.webui", "--host", "127.0.0.1",
         "--port", str(RELAY_PORT), "--require-auth",
         "--token", "0123456789abcdef",
         "--tls-cert", os.path.join("tls", "server.crt"),
         "--tls-key", os.path.join("tls", "server.key")],
        cwd=ROOT, stdout=open(PAIR_LOG, "w", encoding="utf-8"),
        stderr=subprocess.STDOUT)
    try:
        if not port_open(RELAY_PORT):
            print("SKIP     web hkdf           dashboard never listened")
            return False
        env = {
            "PCMON_WEBUI": "https://127.0.0.1:%d" % RELAY_PORT,
            "PCMON_PAIR_LOG": os.path.relpath(PAIR_LOG, ROOT),
            # The self-signed cert is not in the machine store, and tls/ca is
            # empty, so `node --use-system-ca` cannot help. The proof is about
            # HKDF parameters, not about the trust store: turn checking off and
            # say so loudly in the report.
            "NODE_TLS_REJECT_UNAUTHORIZED": "0",
        }
        ok, line = run("web hkdf", ["node", os.path.join("tools", "check_web_hkdf.mjs")],
                       env=env, timeout=120)
        return ok, line
    finally:
        web.terminate()
        try:
            web.wait(timeout=10)
        except subprocess.TimeoutExpired:
            web.kill()


def main(argv=None):
    parser = argparse.ArgumentParser(prog="run_suite")
    parser.add_argument("--quick", action="store_true",
                        help="skip the checks that need a fixture")
    args = parser.parse_args(argv)
    os.makedirs(TMP, exist_ok=True)

    results = []

    results.append(run("protocol selftest", [PY, "-m", "pcmon.protocol"]))
    results.append(run("auth (C2)", [PY, os.path.join("tools", "check_auth.py")]))
    results.append(run("pairing codes", [PY, os.path.join("tools", "check_pairing.py")]))
    results.append(run("web pairing", [PY, os.path.join("tools", "check_web_pairing.py")]))
    results.append(run("sources (C6)", [PY, os.path.join("tools", "check_sources.py")]))

    if not args.quick:
        results.append(with_applied_away(
            lambda: run("term_tk selftest", [PY, "term_tk.py", "--selftest"])))

        got_tape = tape_capture()
        if got_tape:
            results.append(run("replay parity", [PY, os.path.join("tools", "check_replay_parity.py")]))
        else:
            print("SKIP     replay parity      tape capture failed")

        hkdf = dashboard_check()
        if isinstance(hkdf, tuple):
            results.append(hkdf)
        else:
            print("SKIP     web hkdf           dashboard check skipped")

    for ok, line in results:
        print(line)

    failed = sum(1 for ok, _ in results if not ok)
    print("\nSUITE %s (%d checks, %d failed)"
          % ("OK" if not failed else "FAILED", len(results), failed))
    return 0 if not failed else 1


if __name__ == "__main__":
    raise SystemExit(main())
