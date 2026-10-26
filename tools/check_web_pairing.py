"""Acceptance for the browser pairing flow (PROTOCOL.md s.2.2, C5).

Starts a real `pcmon.webui` process, pairs like a browser would, and then
checks that the data endpoints actually demand the key it ended up with. The
pairing code is read from the server's *stdout*, which is the point: the code
only ever exists on the operator's console.

  python tools/check_web_pairing.py

Needs a daemon on 7802 (it uses the `webui` entry in devices.json to connect).
Skipped politely when that is not true.
"""

import argparse
import http.client as http_client
import json
import os
import re
import ssl
import subprocess
import sys
import time
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from pcmon import pairing  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PORT = 7833
BASE = "http://127.0.0.1:%d" % PORT
results = []
USE_TLS = False


def _context():
    """Self-signed certificate: we are checking behaviour, not the CA chain."""
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    return ctx


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))
    print("%-46s %s%s" % (name, "PASS" if ok else "FAIL",
                          ("  " + detail) if detail else ""))


def http(method, path, body=None, token=None, timeout=5.0):
    url = BASE + path
    data = None
    headers = {}
    if body is not None:
        data = json.dumps(body).encode("utf-8")
        headers["Content-Type"] = "application/json"
    if token:
        headers["Authorization"] = "Bearer " + token
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=timeout,
                                    context=_context() if USE_TLS else None) as resp:
            return resp.status, resp.read()
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read()
    except (urllib.error.URLError, TimeoutError, ConnectionError) as exc:
        return 0, str(exc).encode()


def stream_status(path):
    """Status of a streaming endpoint: headers only, body left alone."""
    if USE_TLS:
        conn = http_client.HTTPSConnection("127.0.0.1", PORT, timeout=4,
                                           context=_context())
    else:
        conn = http_client.HTTPConnection("127.0.0.1", PORT, timeout=4)
    try:
        conn.request("GET", path)
        resp = conn.getresponse()      # returns as soon as headers land
        return resp.status
    except (OSError, http_client.HTTPException):
        return 0
    finally:
        conn.close()


def webui_key():
    try:
        with open(os.path.join(ROOT, "devices.json"), encoding="utf-8") as handle:
            entry = json.load(handle)["devices"]["webui"]
        return entry["key"]
    except (OSError, ValueError, KeyError):
        return None


def main(argv=None):
    global PORT, BASE, USE_TLS
    parser = argparse.ArgumentParser(description="browser pairing acceptance")
    parser.add_argument("--port", type=int, default=PORT)
    parser.add_argument("--tls", action="store_true",
                        help="serve with tls/server.crt and test over HTTPS")
    args = parser.parse_args(argv)
    PORT = args.port
    USE_TLS = args.tls
    BASE = "%s://127.0.0.1:%d" % ("https" if USE_TLS else "http", PORT)

    key = webui_key()
    if not key:
        print("SKIP: devices.json has no `webui` entry (pair the dashboard first)")
        return 0
    if http("GET", "/", timeout=0.5)[0] != 0:
        print("SKIP: something is already listening on %d" % PORT)
        return 0

    sessions = os.path.join(ROOT, "tmp", "check_web_pairing_sessions.json")
    if os.path.exists(sessions):
        os.remove(sessions)

    cmd = [sys.executable, "-m", "pcmon.webui",
           "--host", "127.0.0.1", "--port", str(PORT),
           "--daemon", "127.0.0.1:7802", "--token", key,
           "--sessions", sessions, "--require-auth"]
    if USE_TLS:
        cmd += ["--tls-cert", os.path.join(ROOT, "tls", "server.crt"),
                "--tls-key", os.path.join(ROOT, "tls", "server.key")]
    proc = subprocess.Popen(cmd, cwd=ROOT, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, text=True, bufsize=1)
    code = None
    try:
        # Wait for the server, then ask for a pairing code.
        for _ in range(60):
            time.sleep(0.25)
            if http("GET", "/", timeout=0.5)[0]:
                break
        else:
            print("webui never started; output:")
            proc.kill()
            print((proc.stdout.read() or "")[-800:])
            return 1

        status, raw = http("POST", "/pair/begin", {"name": "acceptance"})
        check("POST /pair/begin returns 200", status == 200, "status=%s" % status)
        begin = json.loads(raw.decode("utf-8")) if status == 200 else {}
        check("begin hands out pair_id, client_id, daemon_nonce and the salt recipe",
              all(begin.get(k) for k in ("pair_id", "client_id", "daemon_nonce")),
              "keys=%s" % sorted(begin.keys()))

        # The code appears on the operator's console, nowhere else. Read it
        # first, then assert that no value in the response equals it -- that is
        # what P-6 actually says, and it needs the code to compare against.
        for _ in range(40):
            line = proc.stdout.readline()
            if not line:
                break
            match = re.search(r"PAIR code=(\d{6})", line)
            if match:
                code = match.group(1)
                break
        check("the code is on the operator's console", code is not None,
              "code=%s" % code)
        if code is None:
            return 1
        leaked = [key for key, value in begin.items() if str(value) == code]
        check("begin never returns the code (P-6)", not leaked,
              "leaked=%s" % (leaked or "nothing"))

        # --- unauthenticated access is refused
        status, _ = http("GET", "/api/state")
        check("GET /api/state without a key is 401", status == 401, "status=%s" % status)

        # --- wrong code
        status, _ = http("POST", "/pair/claim", {
            "pair_id": begin["pair_id"],
            "code": "000000" if code != "000000" else "111111",
            "client_nonce": "a" * 32})
        check("a wrong code is refused", status == 403, "status=%s" % status)

        # --- right code, derive the key the way the browser would
        client_nonce = "b" * 32
        status, raw = http("POST", "/pair/claim", {
            "pair_id": begin["pair_id"], "code": code, "client_nonce": client_nonce})
        claim = json.loads(raw.decode("utf-8")) if status == 200 else {}
        check("the right code pairs the browser", status == 200, "status=%s" % status)
        check("the response carries no key (it was derived, not sent)",
              "key" not in claim)
        derived = pairing.derive_key(code, begin["client_id"], client_nonce,
                                    begin["daemon_nonce"])

        # --- reuse is refused
        status, _ = http("POST", "/pair/claim", {
            "pair_id": begin["pair_id"], "code": code, "client_nonce": client_nonce})
        check("the same code cannot be used twice", status in (401, 403),
              "status=%s" % status)

        # --- with the derived key, data flows
        status, raw = http("GET", "/api/state", token=derived)
        ok = status == 200
        detail = "status=%s" % status
        if ok:
            state = json.loads(raw.decode("utf-8"))
            detail = "link_up=%s metrics=%d" % (
                (state.get("link") or {}).get("up"),
                len((state.get("catalog") or {}).get("metrics") or []))
        check("the derived key opens /api/state", ok, detail)

        status, _ = http("GET", "/api/state", token="0" * 32)
        check("a wrong key is still 401", status == 401, "status=%s" % status)

        # /events is the one route that takes the key in the URL: EventSource
        # cannot set headers. Both directions are worth pinning.
        check("GET /events?key=<derived> opens the stream",
              stream_status("/events?key=" + derived) == 200)
        check("GET /events?key=<wrong> is 401",
              stream_status("/events?key=" + "0" * 32) == 401)

        # --- revocation is deleting the row
        if os.path.exists(sessions):
            with open(sessions, encoding="utf-8") as handle:
                table = json.load(handle)
            table["devices"] = {k: v for k, v in table["devices"].items()
                                if k != begin["client_id"]}
            with open(sessions, "w", encoding="utf-8") as handle:
                json.dump(table, handle)
        status, _ = http("GET", "/api/state", token=derived)
        check("revoking the browser takes effect immediately", status == 401,
              "status=%s" % status)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()

    failed = [r for r in results if not r[1]]
    print("")
    if failed:
        print("WEB PAIRING FAIL (%d/%d failed)" % (len(failed), len(results)))
        for name, _, detail in failed:
            print("  - %s %s" % (name, detail))
        return 1
    print("WEB PAIRING OK (%d checks)" % len(results))
    return 0


if __name__ == "__main__":
    sys.exit(main())