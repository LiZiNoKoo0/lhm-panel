"""Print the real SSLError when a client's handshake fails (diagnostic only).

Why this exists: pcmon.webui wraps the *listening* socket in TLS, so a failed
handshake surfaces as an exception inside socket.accept(). socketserver catches
OSError there and returns quietly -- and ssl.SSLError is an OSError subclass. The
effect is a connection reset with nothing in any log, which is exactly what a
phone hitting the dashboard produced.

This listens with the same certificate and prints the exception instead of
swallowing it. It serves no HTTP and writes no files.

Usage
  python net/../tls_probe.py 7808 H:\\newwork\\tls\\server.crt H:\\newwork\\tls\\server.key
"""

import socket
import ssl
import sys
import time


def main(argv):
    port = int(argv[1]) if len(argv) > 1 else 7808
    cert = argv[2] if len(argv) > 2 else "H:\\newwork\\tls\\server.crt"
    key = argv[3] if len(argv) > 3 else "H:\\newwork\\tls\\server.key"

    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(cert, key)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_2

    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("0.0.0.0", port))
    srv.listen(8)
    print("probe listening on %d, cert=%s" % (port, cert), flush=True)

    while True:
        try:
            conn, addr = srv.accept()
        except OSError as exc:
            print("accept failed: %r" % (exc,), flush=True)
            continue

        peer = "%s:%s" % addr
        try:
            tls = ctx.wrap_socket(conn, server_side=True)
            print("%s HANDSHAKE OK %s %s" % (
                time.strftime("%H:%M:%S"), peer, tls.version()), flush=True)
            try:
                tls.recv(1024)
            except Exception:
                pass
            tls.close()
        except Exception as exc:
            # the whole point: name the failure instead of resetting silently
            print("%s HANDSHAKE FAILED %s -> %r" % (
                time.strftime("%H:%M:%S"), peer, exc), flush=True)
            try:
                conn.close()
            except Exception:
                pass


if __name__ == "__main__":
    main(sys.argv)
