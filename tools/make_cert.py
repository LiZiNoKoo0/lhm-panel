"""Make a self-signed certificate for the dashboard, with the SANs it needs.

Why a script and not "bring your own PEM": the first thing anybody does when
they expose port 7803 is paste whatever certificate they have, and the failure
mode of a wrong SAN is a browser warning that people click through. So the
certificate is generated with the names this machine actually answers to.

Why openssl and not Python: generating a certificate needs an X.509 builder, and
the project has no third-party dependencies on the PC side. openssl is already
on this machine (D:\\msy64\\mingw64\\bin\\openssl.exe) and its CLI is stable.

The key is written next to the certificate and both are ignored by git: a
private key in a repository is a private key everybody has.

  python tools/make_cert.py                 # localhost + this machine's LAN IPs
  python tools/make_cert.py --extra 10.0.0.5
  python tools/make_cert.py --out tls --days 825
"""

import argparse
import datetime
import os
import shutil
import socket
import subprocess
import sys

DEFAULT_DIR = "tls"
DEFAULT_DAYS = 825          # the longest lifetime browsers accept for a leaf
DEFAULT_CN = "pcmon-dashboard"


def find_openssl():
    """PATH first, then the copy this machine actually has."""
    found = shutil.which("openssl")
    if found:
        return found
    for candidate in (r"D:\msy64\mingw64\bin\openssl.exe",
                      r"C:\Program Files\OpenSSL-Win64\bin\openssl.exe"):
        if os.path.isfile(candidate):
            return candidate
    return None


def local_ipv4_addresses():
    """Every IPv4 this machine answers to, minus loopback and link-local."""
    addresses = set()
    try:
        for info in socket.getaddrinfo(socket.gethostname(), None, socket.AF_INET):
            addresses.add(info[4][0])
    except OSError:
        pass
    # A UDP connect sends nothing but tells the stack which local address would
    # be used for the default route -- the one a phone on the same LAN reaches.
    probe = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        probe.connect(("192.0.2.1", 9))   # TEST-NET-1, never routed
        addresses.add(probe.getsockname()[0])
    except OSError:
        pass
    finally:
        probe.close()
    keep = []
    for addr in sorted(addresses):
        if addr.startswith("127.") or addr.startswith("169.254."):
            continue
        keep.append(addr)
    return keep


def main(argv=None):
    parser = argparse.ArgumentParser(description="self-signed TLS pair for pcmon.webui")
    parser.add_argument("--out", default=DEFAULT_DIR, help="output directory (default tls/)")
    parser.add_argument("--name", default=DEFAULT_CN, help="common name (default %s)" % DEFAULT_CN)
    parser.add_argument("--days", type=int, default=DEFAULT_DAYS)
    parser.add_argument("--extra", action="append", default=[],
                        help="extra SAN entry, e.g. --extra 10.0.0.5 or --extra panel.lan")
    parser.add_argument("--force", action="store_true", help="overwrite an existing pair")
    args = parser.parse_args(argv)

    openssl = find_openssl()
    if not openssl:
        raise SystemExit("make_cert: no openssl on PATH and none at the known path")

    cert = os.path.join(args.out, "server.crt")
    key = os.path.join(args.out, "server.key")
    if os.path.exists(cert) and not args.force:
        raise SystemExit("make_cert: %s exists (use --force to replace it)" % cert)
    os.makedirs(args.out, exist_ok=True)

    entries = ["DNS:localhost", "DNS:%s" % args.name, "IP:127.0.0.1"]
    for addr in local_ipv4_addresses():
        entries.append("IP:%s" % addr)
    for extra in args.extra:
        entries.append("DNS:%s" % extra if not extra.count(".") == 3 else "IP:%s" % extra)

    # De-duplicate while keeping the order readable in the printed summary.
    seen = set()
    sans = [e for e in entries if not (e in seen or seen.add(e))]

    cmd = [openssl, "req", "-x509", "-newkey", "rsa:2048", "-nodes",
           "-keyout", key, "-out", cert,
           "-days", str(args.days),
           "-subj", "/CN=%s" % args.name,
           "-addext", "subjectAltName=" + ",".join(sans)]
    print("make_cert: %s" % " ".join(cmd[:3]) + " ...")
    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode != 0:
        sys.stderr.write(result.stdout or "")
        sys.stderr.write(result.stderr or "")
        raise SystemExit("make_cert: openssl failed (%d)" % result.returncode)

    expires = datetime.date.today() + datetime.timedelta(days=args.days)
    print("")
    print("cert : %s" % os.path.abspath(cert))
    print("key  : %s   (keep this private; it is gitignored)" % os.path.abspath(key))
    print("sans : %s" % ", ".join(sans))
    print("until: %s" % expires.isoformat())
    print("")
    print("Run the dashboard with:")
    print("  python -m pcmon.webui --host 0.0.0.0 --port 7803 \\")
    print("      --tls-cert %s --tls-key %s --require-auth" % (cert, key))
    print("")
    print("The certificate is self-signed: a phone will warn until you accept")
    print("it once. That warning is the whole trust decision -- do it on a")
    print("network you control, and never click through it on a strange one.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
