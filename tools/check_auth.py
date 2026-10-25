"""Prove the handshake, case by case (PROTOCOL.md s.2.1/s.2.5).

Every case below is one line of the threat model. Run it the way the other
acceptance tools run: it prints PASS/FAIL per case and exits non-zero if any
case fails, so it can gate a commit.

What it does not do: touch a board. This is the PC-side contract; the firmware
must answer the same mac, and that is card C3's `cargo test` + a flash.

  python tools/check_auth.py            # ~15 s, includes the 5 s answer deadline
"""

import os
import socket
import sys
import tempfile
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from pcmon import protocol                      # noqa: E402
from pcmon.devices import DeviceTable, new_key  # noqa: E402
import fake_daemon                              # noqa: E402

HOST = "127.0.0.1"
PORT = 7812
CLIENT = "esp32-test"
OTHER = "phone-test"

results = []


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))
    print("%-34s %s%s" % (name, "PASS" if ok else "FAIL",
                          ("  " + detail) if detail else ""))
    return ok


# ---------------------------------------------------------------------------
# minimal wire client
# ---------------------------------------------------------------------------


class Wire:
    """A socket that speaks just enough NDJSON to test a handshake."""

    def __init__(self, timeout=6.0):
        self.sock = socket.create_connection((HOST, PORT), timeout=timeout)
        self.sock.settimeout(0.4)
        self.buf = b""
        self.stack = []

    def send(self, msg):
        self.sock.sendall(protocol.encode(msg))

    def next_msg(self, deadline):
        """Return the next decoded message, or None when the deadline passes."""
        while time.monotonic() < deadline:
            cut = self.buf.find(b"\n")
            if cut >= 0:
                line, self.buf = self.buf[:cut], self.buf[cut + 1:]
                msg = protocol.decode(line)
                if msg is not None:
                    return msg
                continue
            try:
                chunk = self.sock.recv(4096)
            except TimeoutError:
                continue
            except OSError:
                return None
            if not chunk:
                return None
            self.buf += chunk
        return None

    def collect(self, seconds):
        """Everything that arrives within the window."""
        out = []
        deadline = time.monotonic() + seconds
        while True:
            msg = self.next_msg(deadline)
            if msg is None:
                return out
            out.append(msg)

    def wait_kind(self, kinds, seconds=6.0):
        deadline = time.monotonic() + seconds
        while True:
            msg = self.next_msg(deadline)
            if msg is None:
                return None
            if protocol.kind(msg) in kinds:
                return msg

    def hello(self, client_id=CLIENT, **kw):
        self.send(protocol.hello(client_id, "thin", 160, 80, **kw))

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass


def handshake(client_id, key, wire=None):
    """hello -> challenge -> auth. Returns (wire, challenge, auth_msg)."""
    wire = wire or Wire()
    wire.hello(client_id)
    ch = wire.wait_kind(("challenge", "deny"), seconds=3.0)
    if ch is None or protocol.kind(ch) != "challenge":
        return wire, ch, None
    mac = protocol.mac_for(key, ch["nonce"], client_id)
    wire.send(protocol.auth(mac))
    return wire, ch, mac


# ---------------------------------------------------------------------------
# cases
# ---------------------------------------------------------------------------


def case_happy_path(key):
    wire, ch, mac = handshake(CLIENT, key)
    if not isinstance(ch, dict) or protocol.kind(ch) != "challenge":
        check("happy path: challenge", False, repr(ch)[:60])
        wire.close()
        return
    ok = check("happy path: challenge arrives",
               ch.get("alg") == protocol.AUTH_ALG and len(ch.get("nonce", "")) == 32,
               "alg=%s nonce=%d chars" % (ch.get("alg"), len(ch.get("nonce") or "")))
    ok &= check("happy path: daemon proves itself (srv)",
                protocol.verify_srv(key, ch["nonce"], ch.get("srv")),
                "srv=%s" % ch.get("srv"))
    catalog = wire.wait_kind(("catalog", "deny"), seconds=4.0)
    ok &= check("happy path: catalog after auth",
                catalog is not None and protocol.kind(catalog) == "catalog",
                repr(catalog)[:60] if catalog else "nothing")
    data = wire.wait_kind(("data",), seconds=4.0)
    ok &= check("happy path: data frames flow", data is not None)
    wire.close()
    return mac


def case_replay(key, mac, nonce):
    """The same mac on a fresh connection must not work (A-1)."""
    wire = Wire()
    wire.hello(CLIENT)
    ch = wire.wait_kind(("challenge", "deny"), seconds=3.0)
    if ch is None or protocol.kind(ch) != "challenge":
        check("replay: challenge", False, repr(ch)[:60])
        wire.close()
        return
    wire.send(protocol.auth(mac))
    verdict = wire.wait_kind(("deny", "catalog"), seconds=4.0)
    check("replay: captured mac rejected",
          verdict is not None and protocol.kind(verdict) == "deny",
          "reason=%s" % (verdict or {}).get("reason"))
    wire.close()


def case_bad_mac(key):
    wire = Wire()
    wire.hello(CLIENT)
    ch = wire.wait_kind(("challenge", "deny"), seconds=3.0)
    wire.send(protocol.auth("0" * 32))
    verdict = wire.wait_kind(("deny", "catalog"), seconds=4.0)
    check("wrong mac: denied",
          verdict is not None and protocol.kind(verdict) == "deny",
          "reason=%s" % (verdict or {}).get("reason"))
    wire.close()


def case_wrong_client(key):
    """A mac bound to esp32-test must not open a connection as phone-test."""
    wire = Wire()
    wire.hello(OTHER)
    ch = wire.wait_kind(("challenge", "deny"), seconds=3.0)
    if ch is None or protocol.kind(ch) != "challenge":
        check("wrong client_id: unknown client denied",
              ch is not None and protocol.kind(ch) == "deny",
              "reason=%s" % (ch or {}).get("reason"))
        wire.close()
        return
    wire.send(protocol.auth(protocol.mac_for(key, ch["nonce"], CLIENT)))
    verdict = wire.wait_kind(("deny", "catalog"), seconds=4.0)
    check("wrong client_id: mac does not transfer",
          verdict is not None and protocol.kind(verdict) == "deny",
          "reason=%s" % (verdict or {}).get("reason"))
    wire.close()


def case_no_data_before_auth():
    """A-3: nothing but challenge may arrive before the answer."""
    wire = Wire()
    wire.hello(CLIENT)
    ch = wire.wait_kind(("challenge",), seconds=3.0)
    leaked = [protocol.kind(m) for m in wire.collect(1.5)]
    check("gate: no catalog/ui/data before auth",
          ch is not None and not leaked,
          "leaked=%s" % (leaked or "nothing"))
    wire.close()


def case_timeout():
    """A-2: no answer within 5 s -> deny auth timeout, connection gone."""
    wire = Wire(timeout=12.0)
    wire.hello(CLIENT)
    ch = wire.wait_kind(("challenge",), seconds=3.0)
    started = time.monotonic()
    verdict = wire.wait_kind(("deny",), seconds=9.0)
    elapsed = time.monotonic() - started
    check("deadline: silence gets denied",
          ch is not None and verdict is not None
          and (verdict or {}).get("reason") == "auth timeout",
          "after %.1fs reason=%s" % (elapsed, (verdict or {}).get("reason")))
    wire.close()


def case_lockout(key):
    """A-5: five failures in a minute lock the source out."""
    reasons = []
    for _ in range(6):
        wire = Wire()
        wire.hello(CLIENT)
        ch = wire.wait_kind(("challenge", "deny"), seconds=3.0)
        if ch is not None and protocol.kind(ch) == "challenge":
            wire.send(protocol.auth("f" * 32))
            verdict = wire.wait_kind(("deny",), seconds=4.0)
            reasons.append((verdict or {}).get("reason"))
        else:
            reasons.append((ch or {}).get("reason"))
        wire.close()
    check("lockout: 6th attempt sees 'locked'",
          reasons and reasons[-1] == "locked",
          "reasons=%s" % reasons)
    return reasons


def case_static_mode(token):
    """--auth static still accepts the v0.5 plain-token hello, and only that."""
    global PORT
    table_file = tempfile.mktemp(suffix="-devices.json")
    daemon = fake_daemon.Daemon(host=HOST, port=PORT + 1, token=token,
                                auth_mode="static", devices_file=table_file)
    daemon.log = lambda text: None
    thread = threading.Thread(target=daemon.serve_forever, daemon=True)
    thread.start()
    time.sleep(0.6)
    try:
        saved, PORT = PORT, PORT + 1
        wire = Wire()
        wire.send(protocol.hello(CLIENT, "thin", 160, 80, token=token))
        got = wire.wait_kind(("catalog", "deny", "challenge"), seconds=4.0)
        check("static mode: correct token accepted",
              got is not None and protocol.kind(got) == "catalog",
              "got=%s" % (protocol.kind(got) if got else "nothing"))
        wire.close()

        wire = Wire()
        wire.hello(CLIENT)          # hmac hello, no token
        got = wire.wait_kind(("catalog", "deny", "challenge"), seconds=4.0)
        check("static mode: hmac hello refused",
              got is not None and protocol.kind(got) == "deny",
              "reason=%s" % (got or {}).get("reason"))
        wire.close()
        PORT = saved
    finally:
        daemon.shutdown()


def main():
    table_file = os.path.join(tempfile.mkdtemp(prefix="pcmon-auth-"), "devices.json")
    table = DeviceTable(table_file)
    key = new_key()
    table.add(CLIENT, key=key, note="check_auth")
    table.add(OTHER, key=new_key(), note="check_auth")

    daemon = fake_daemon.Daemon(host=HOST, port=PORT, auth_mode="hmac",
                                source="fake", devices_file=table_file)
    daemon.log = lambda text: None
    daemon.log_raw = lambda text: None
    thread = threading.Thread(target=daemon.serve_forever, daemon=True)
    thread.start()
    time.sleep(0.6)

    print("auth: alg=%s devices=%s port=%d" % (protocol.AUTH_ALG, table_file, PORT))
    try:
        mac = case_happy_path(key)
        if mac:
            case_replay(key, mac, None)
        case_bad_mac(key)
        case_wrong_client(key)
        case_no_data_before_auth()
        case_timeout()
        case_lockout(key)
    finally:
        daemon.shutdown()

    time.sleep(0.3)
    case_static_mode("0123456789abcdef")

    failed = [r for r in results if not r[1]]
    print("")
    if failed:
        print("AUTH FAIL (%d/%d failed)" % (len(failed), len(results)))
        for name, _, detail in failed:
            print("  - %s %s" % (name, detail))
        return 1
    print("AUTH OK (%d checks)" % len(results))
    return 0


if __name__ == "__main__":
    sys.exit(main())
