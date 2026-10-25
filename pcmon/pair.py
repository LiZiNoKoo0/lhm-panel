"""Pair a terminal: mint its key, keep the table, print the code (PROTOCOL s.2.2).

Why a CLI and not only a dialog: on a server there is no desktop to draw a
window on, so the *terminal the operator is typing in* is the pairing channel.
`--new` therefore prints the key to stdout, which is exactly as trustworthy as
the SSH session it appears in -- no more, no less.

What this does NOT do yet: the 6-digit one-time code flow (s.2.2 P-1..P-7).
That needs two nonces, one of which must come from the client over HTTP, so it
belongs to the webui work (card C5), not here. Until then a browser terminal is
paired with `--new` and the key is pasted once.

Usage
  python -m pcmon.pair --new --client phone
  python -m pcmon.pair --list
  python -m pcmon.pair --revoke phone
  python -m pcmon.pair --prune --days 30
  python -m pcmon.pair --serial COM10 --client esp32-a     (needs firmware C3)
"""

import argparse
import sys
import time

from pcmon import devices as devices_mod
from pcmon import protocol


def _fmt_ts(value):
    if not isinstance(value, (int, float)):
        return "-"
    return time.strftime("%Y-%m-%d %H:%M", time.localtime(value))


def cmd_new(table, client_id, note, force, key=None):
    if not client_id:
        raise SystemExit("pair: --new needs --client <id>")
    if table.exists(client_id) and not force:
        raise SystemExit("pair: %r is already paired (P-7 in s.2.2: one key per "
                         "device). Use --force to rotate it, or --revoke first."
                         % client_id)
    if key is not None and not devices_mod.looks_like_key(key):
        raise SystemExit("pair: --key must be %d lowercase hex chars, got %r"
                         % (devices_mod.KEY_HEX_CHARS, key))
    key = table.add(client_id, key=key, note=note)
    print("PAIR ok client=%s" % client_id)
    print("  key      : %s" % key)
    print("  devices  : %s" % table.path)
    print("")
    print("Type that key into the terminal ONCE. It is not recoverable from")
    print("the wire: hello carries no key, only an HMAC of the challenge.")
    if force:
        print("(rotated: any terminal still holding the old key will now be denied)")
    return 0


def cmd_list(table):
    entries = table.entries()
    if not entries:
        print("no devices paired (%s)" % table.path)
        return 0
    print("%-20s %-34s %-17s %-17s" % ("client_id", "key", "added", "last_seen"))
    for client_id in sorted(entries):
        entry = entries[client_id] or {}
        print("%-20s %-34s %-17s %-17s"
              % (client_id, entry.get("key", "?"),
                 _fmt_ts(entry.get("added_at")), _fmt_ts(entry.get("last_seen"))))
    return 0


def cmd_revoke(table, client_id):
    if not client_id:
        raise SystemExit("pair: --revoke needs --client <id>")
    if not table.revoke(client_id):
        print("pair: %r is not in %s" % (client_id, table.path))
        return 1
    print("REVOKE ok client=%s (effective on the next challenge)" % client_id)
    return 0


def cmd_prune(table, days):
    removed = table.prune(days)
    if not removed:
        print("pair: nothing to prune (older than %g days)" % days)
    else:
        print("PRUNE removed: %s" % ", ".join(sorted(removed)))
    return 0


def cmd_serial(table, port, client_id, baud, timeout):
    """Board pairing: mint a key, then hand it to the firmware over the wire.

    The firmware side (card C3) accepts one line `KW <32 hex>`. Until that lands
    this will time out -- it says so instead of pretending to have worked.
    """
    if not port or not client_id:
        raise SystemExit("pair: --serial needs --port and --client")
    try:
        from pcmon import serialport
    except ImportError as exc:  # pragma: no cover - module is part of the tree
        raise SystemExit("pair: cannot import pcmon.serialport: %s" % exc)

    key = table.add(client_id, note="serial %s" % port)
    print("PAIR mint client=%s key=%s" % (client_id, key))
    try:
        link = serialport.SerialLink(port, baud)
    except Exception as exc:  # noqa: BLE001 - report, do not traceback
        table.revoke(client_id)
        raise SystemExit("pair: cannot open %s: %s (key rolled back)" % (port, exc))
    try:
        link.write(("KW %s\n" % key).encode("ascii"))
        deadline = time.monotonic() + timeout
        seen = ""
        while time.monotonic() < deadline:
            chunk = link.read(0.2)
            if not chunk:
                continue
            seen += chunk.decode("ascii", "replace")
            if "KOK" in seen:
                print("serial: %s" % seen.strip().splitlines()[-1])
                print("PAIR ok (firmware accepted the key)")
                return 0
        table.revoke(client_id)
        print("serial: no KOK within %gs. The firmware in flash does not speak `KW`"
              % timeout)
        print("        yet (card C3). Key rolled back so the table has no ghost entry.")
        return 1
    finally:
        try:
            link.close()
        except Exception:  # noqa: BLE001
            pass


def parse_args(argv=None):
    parser = argparse.ArgumentParser(
        description="Pair a terminal with the daemon (PROTOCOL.md s.2.2/s.2.4).")
    parser.add_argument("--devices", metavar="FILE", default=devices_mod.DEFAULT_DEVICES_FILE,
                        help="device table (default %s)" % devices_mod.DEFAULT_DEVICES_FILE)
    parser.add_argument("--new", action="store_true", help="mint a key for --client")
    parser.add_argument("--list", action="store_true", help="show paired devices")
    parser.add_argument("--revoke", action="store_true", help="remove --client")
    parser.add_argument("--prune", action="store_true", help="drop devices unseen for --days")
    parser.add_argument("--days", type=float, default=30.0,
                        help="age for --prune, in days (default 30)")
    parser.add_argument("--client", default=None, help="client_id")
    parser.add_argument("--note", default="", help="free text stored beside the key")
    parser.add_argument("--key", default=None,
                        help="register an existing key (32 hex) instead of minting one; "
                             "use this to adopt a board whose flash already holds a key")
    parser.add_argument("--force", action="store_true", help="rotate an existing key")
    parser.add_argument("--serial", metavar="PORT", default=None,
                        help="also write the key to a board on PORT (e.g. COM10)")
    parser.add_argument("--baud", type=int, default=115200)
    parser.add_argument("--timeout", type=float, default=6.0)
    return parser.parse_args(argv)


def main(argv=None):
    args = parse_args(argv)
    table = devices_mod.DeviceTable(args.devices)
    print("protocol v%s, alg %s" % (protocol.PROTO_VERSION, protocol.AUTH_ALG))
    if args.list:
        return cmd_list(table)
    if args.revoke:
        return cmd_revoke(table, args.client)
    if args.prune:
        return cmd_prune(table, args.days)
    if args.new or args.serial:
        if args.serial:
            return cmd_serial(table, args.serial, args.client, args.baud, args.timeout)
        return cmd_new(table, args.client, args.note, args.force, args.key)
    print(__doc__ or "")
    return 1


if __name__ == "__main__":
    sys.exit(main())
