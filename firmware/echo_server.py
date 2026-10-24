#!/usr/bin/env python3
"""T5.2 acceptance rig: the dumbest possible TCP peer.

Why this exists instead of pointing the board at the daemon: T5.2 is about
bytes, and the daemon is about messages. If the board fails to reach a peer
that only echoes, the fault is in the socket path; if it reaches this and fails
against the daemon, the fault is in the protocol. One variable at a time.

It echoes each received line back, unchanged, one line at a time. That is the
entire contract -- an echo server that "helpfully" reformats is a liar.

Expected board output (bench build, `--features bench-echo`):

    LINK tcp peer=192.168.1.100:7802
    LINK up
    LINK tx n=5
    LINK rx n=5 text=PING

Usage:
    python firmware/echo_server.py                 # 0.0.0.0:7802
    python firmware/echo_server.py --port 7802 --once
"""

import argparse
import socket
import sys
import time

BIND_HOST = "0.0.0.0"
DEFAULT_PORT = 7802
RECV_CHUNK = 512


def serve(host, port, once):
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind((host, port))
    srv.listen(1)
    print("echo listening on %s:%d (TCP, newline framed)" % (host, port))
    sys.stdout.flush()

    while True:
        conn, addr = srv.accept()
        peer = "%s:%d" % (addr[0], addr[1])
        print("%s connected" % peer)
        sys.stdout.flush()
        buf = b""
        try:
            while True:
                chunk = conn.recv(RECV_CHUNK)
                if not chunk:
                    print("%s closed" % peer)
                    break
                buf += chunk
                while b"\n" in buf:
                    line, buf = buf.split(b"\n", 1)
                    stamp = time.strftime("%H:%M:%S")
                    print("%s rx %d bytes: %r" % (stamp, len(line), line))
                    sys.stdout.flush()
                    conn.sendall(line + b"\n")
        except OSError as exc:
            print("%s error: %s" % (peer, exc))
        finally:
            conn.close()
        if once:
            return


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--host", default=BIND_HOST)
    parser.add_argument("--port", type=int, default=DEFAULT_PORT)
    parser.add_argument("--once", action="store_true", help="exit after one connection")
    args = parser.parse_args(argv)
    try:
        serve(args.host, args.port, args.once)
    except KeyboardInterrupt:
        return 0
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
