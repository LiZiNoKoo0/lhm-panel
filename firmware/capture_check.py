#!/usr/bin/env python3
"""Verify the IP/TCP checksums of packets arriving from the panel.

Why: a SYN that arrives but gets no reply and leaves no firewall log is
the signature of a packet the receiving stack threw away. A wrong checksum
does exactly that, and nothing on the Windows side records it.

Needs Administrator (raw sockets). Usage:
    python firmware/capture_check.py [seconds] [peer_ip] [port]
"""

import socket
import struct
import sys
import time

SIO_RCVALL = 0x98000001


def csum(data):
    if len(data) % 2:
        data += b"\x00"
    total = 0
    for i in range(0, len(data), 2):
        total += (data[i] << 8) | data[i + 1]
    while total >> 16:
        total = (total & 0xFFFF) + (total >> 16)
    return (~total) & 0xFFFF


def main(argv):
    seconds = int(argv[1]) if len(argv) > 1 else 25
    peer = argv[2] if len(argv) > 2 else "192.168.1.53"
    port = int(argv[3]) if len(argv) > 3 else 7804

    s = socket.socket(socket.AF_INET, socket.SOCK_RAW, socket.IPPROTO_IP)
    s.bind(("192.168.1.128", 0))
    s.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 1 << 20)
    s.ioctl(SIO_RCVALL, 1)
    s.settimeout(1.0)

    seen = 0
    deadline = time.time() + seconds
    print("listening %ds for %s -> port %d" % (seconds, peer, port))
    while time.time() < deadline:
        try:
            pkt = s.recv(65535)
        except socket.timeout:
            continue
        ihl = (pkt[0] & 0xF) * 4
        total_len = struct.unpack_from("!H", pkt, 2)[0]
        proto = pkt[9]
        src = socket.inet_ntoa(pkt[12:16])
        dst = socket.inet_ntoa(pkt[16:20])
        if proto != 6 or src != peer:
            continue
        tcp = pkt[ihl:total_len]
        sport, dport = struct.unpack_from("!HH", tcp, 0)
        if dport != port:
            continue
        seen += 1
        flags = tcp[13]
        # the sender filled in the checksum; recompute over what arrived
        got = struct.unpack_from("!H", tcp, 16)[0]
        pseudo = socket.inet_aton(src) + socket.inet_aton(dst) + struct.pack("!BBH", 0, 6, len(tcp))
        want = csum(pseudo + tcp[:16] + b"\x00\x00" + tcp[18:])
        ip_got = struct.unpack_from("!H", pkt, 10)[0]
        ip_want = csum(pkt[:ihl])
        print(
            "  %s:%d -> %s:%d flags=0x%02x tcp_csum got=0x%04x want=0x%04x %s | ip_csum got=0x%04x want=0x%04x %s"
            % (
                src, sport, dst, dport, flags,
                got, want, "OK" if got == want else "BAD",
                ip_got, ip_want, "OK" if ip_got == ip_want else "BAD",
            )
        )
        if seen >= 6:
            break
    if seen == 0:
        print("  no TCP packet from %s to port %d arrived" % (peer, port))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
