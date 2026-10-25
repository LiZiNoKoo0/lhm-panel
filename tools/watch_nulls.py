"""Watch `data` frames on 7802 and report when values turn null.

This is the M5 acceptance, measured on the wire rather than on a screen:
killing lhm_bridge.exe must make the daemon publish null for every metric
within STALE_AFTER_S (3 s), because PROTOCOL 9 says null means "show --" and an
omitted key would leave the old value on the glass forever.

Usage: python tools/watch_nulls.py [seconds]
"""

import json
import socket
import sys
import time

HOST = "127.0.0.1"
PORT = 7802
TOKEN = open("daemon.token", encoding="ascii").read().strip()
DURATION = float(sys.argv[1]) if len(sys.argv) > 1 else 20.0


def main():
    sock = socket.create_connection((HOST, PORT), timeout=5.0)
    hello = {
        "t": "hello", "proto": 1, "token": TOKEN, "client_id": "null-watch",
        "role": "thick", "accept": ["data"], "charset": "ascii",
        "display": {"w": 160, "h": 80, "depth": 16},
        "primitives": [], "fonts": [16],
    }
    sock.sendall((json.dumps(hello) + "\n").encode())

    buf = b""
    started = time.monotonic()
    last_report = 0.0
    sock.settimeout(0.5)

    while time.monotonic() - started < DURATION:
        try:
            chunk = sock.recv(8192)
        except socket.timeout:
            continue
        if not chunk:
            print("daemon closed the connection")
            break
        buf += chunk
        while b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            if not line.strip():
                continue
            try:
                msg = json.loads(line)
            except ValueError:
                continue
            if msg.get("t") != "data":
                continue
            elapsed = time.monotonic() - started
            values = msg.get("v") or {}
            live = {k: v for k, v in values.items() if v is not None}
            dead = [k for k, v in values.items() if v is None]
            if elapsed - last_report >= 1.0:
                last_report = elapsed
                print("t=%5.1fs  live=%-2d null=%-2d  %s"
                      % (elapsed, len(live), len(dead),
                         ", ".join("%s=%s" % (k, v) for k, v in sorted(live.items()))[:80]))
    sock.close()


if __name__ == "__main__":
    main()
