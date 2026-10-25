"""Capture exactly what a replay sends to one client, for the M4 parity check.

Written after the first attempt at this comparison reported a false failure: it
parsed the replay's *log*, which interleaves every client that ever said hello,
so the board reconnecting mid-tape looked like a frame-order mismatch. The log is
a debugging aid, not the wire.

This connects once, stays connected, and writes every received line verbatim.
The M4 contract is that a terminal sees the recorded frames, in order -- timing
is not part of it (`--speed` exists precisely to change timing).
"""

import json
import socket
import sys
import time

HOST = "127.0.0.1"
PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 7802
OUT = sys.argv[2] if len(sys.argv) > 2 else "_captured.ndjson"
WANT = int(sys.argv[3]) if len(sys.argv) > 3 else 74
TIMEOUT_S = 20.0

hello = {
    "t": "hello", "proto": 1, "token": "0123456789abcdef", "client_id": "tape-probe",
    "role": "thick", "accept": ["ui", "data"], "charset": "ascii",
    "display": {"w": 240, "h": 320, "depth": 16},
    "primitives": ["rect", "path", "text", "num"],
    "fonts": [12, 16, 24, 32],
    "limits": {"max_nodes": 32, "max_json_bytes": 4096},
}

sock = socket.create_connection((HOST, PORT), timeout=5.0)
sock.sendall((json.dumps(hello) + "\n").encode())

buf = b""
lines = []
started = time.monotonic()
sock.settimeout(0.5)

while time.monotonic() - started < TIMEOUT_S and len(lines) < WANT:
    try:
        chunk = sock.recv(16384)
    except socket.timeout:
        continue
    if not chunk:
        break
    buf += chunk
    while b"\n" in buf:
        line, buf = buf.split(b"\n", 1)
        line = line.strip()
        if line:
            lines.append(line)

sock.close()

with open(OUT, "wb") as fh:
    for line in lines:
        fh.write(line + b"\n")

print("captured %d frames -> %s" % (len(lines), OUT))
for line in lines[:3]:
    print("  " + line.decode("utf-8", "replace")[:110])
