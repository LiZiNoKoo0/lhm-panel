"""Ask the daemon for its first two catalog lines, using the exact `HELLO`
bytes the firmware sends.

Why this exists: on the board the first catalog printed `LINK catalog n=0`.
That is either the truth (the daemon publishes an empty catalog until the
sensor bridge resolves its ids) or a bug in `wire::count_ids`. A number on the
serial line only becomes evidence when the bytes behind it are on record, so
this script dumps what the daemon actually writes.

Run:  python firmware/probe_catalog.py [port]
"""

import json
import socket
import sys

# Must stay byte-identical to panel_core::wire::HELLO (the test suite pins its
# shape; this copy is the PC-side echo of it).
HELLO = (
    '{"t":"hello","proto":1,"token":"",'
    '"client_id":"st7735-160x80","role":"thin",'
    '"accept":["ui","data"],'
    '"display":{"w":160,"h":80,"depth":16},'
    '"charset":"ascii",'
    '"primitives":["rect","text","num","path"],'
    '"fonts":[16],'
    '"input":[],'
    '"limits":{"max_nodes":32,"max_json_bytes":1024,"max_text_len":20}}'
)


def main() -> int:
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 7802
    sock = socket.create_connection(("127.0.0.1", port), timeout=5)
    sock.sendall(HELLO.encode("ascii") + b"\n")
    print("sent hello %d bytes" % (len(HELLO) + 1))

    buf = b""
    seen = 0
    sock.settimeout(6)
    try:
        while seen < 2:
            chunk = sock.recv(4096)
            if not chunk:
                print("daemon closed the connection")
                break
            buf += chunk
            while b"\n" in buf:
                raw, buf = buf.split(b"\n", 1)
                msg = json.loads(raw.decode("ascii"))
                if msg.get("t") != "catalog":
                    continue
                seen += 1
                metrics = msg.get("metrics") or []
                ids = [m.get("id") for m in metrics]
                print("catalog ver=%s metrics=%d ids=%s"
                      % (msg.get("ver"), len(metrics), ",".join(map(str, ids)) or "-"))
                print("  raw: %d bytes, first 120: %r" % (len(raw), raw[:120]))
    except socket.timeout:
        print("timeout waiting for a second catalog")
    finally:
        sock.close()
    return 0 if seen else 1


if __name__ == "__main__":
    raise SystemExit(main())
