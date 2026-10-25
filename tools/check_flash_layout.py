"""Verify T6c from the chip itself: is a saved page really in the layout sectors?

Serial is unusable during the WiFi phase on this board (the CH343 drops out at
the current spike), so the `PAGE saved` line never reaches me. Reading the two
sectors back and parsing them is stronger evidence anyway: it shows what the
next boot will find, not what the firmware claimed it wrote.

Format, from panel-core/src/{sector,saved}.rs:

  sector frame   magic "PCFG" | ver u8 | len u16 | seq u32 | crc32 u32 | payload
  payload        magic "PUI1" | ver u32 | scene u8 | bg u16 | count u8 | nodes | crc32 u32
"""

import struct
import sys
import zlib

PATH = r"H:\newwork\flash_layout.bin"
BASE = 0xB000
SECTOR = 0x1000


def parse_frame(blob, offset):
    magic, = struct.unpack_from("<4s", blob, offset)
    if magic != b"PCFG":
        return None
    ver = blob[offset + 4]
    length, = struct.unpack_from("<H", blob, offset + 5)
    seq, = struct.unpack_from("<I", blob, offset + 7)
    crc, = struct.unpack_from("<I", blob, offset + 11)
    if length > 1024:
        return None
    body = blob[offset + 15: offset + 15 + length]
    want = zlib.crc32(blob[offset: offset + 11] + body) & 0xFFFFFFFF
    if want != crc:
        return None
    return {"ver": ver, "len": length, "seq": seq, "payload": body}


def parse_page(payload):
    if payload[:4] != b"PUI1":
        return None
    ver, = struct.unpack_from("<I", payload, 4)
    scene = payload[8]
    bg, = struct.unpack_from("<H", payload, 9)
    count = payload[11]
    want, = struct.unpack_from("<I", payload, len(payload) - 4)
    got = zlib.crc32(payload[: len(payload) - 4]) & 0xFFFFFFFF
    if want != got:
        return None
    return {"ver": ver, "scene": scene, "bg": bg, "count": count}


def main():
    blob = open(PATH, "rb").read()
    print("dump: %d bytes (0x%X)" % (len(blob), len(blob)))

    found = []
    for name, off in (("A (0xB000)", 0), ("B (0xC000)", SECTOR)):
        frame = parse_frame(blob, off)
        if frame is None:
            print("%s: no valid frame" % name)
            continue
        page = parse_page(frame["payload"])
        print("%s: frame ok  seq=%d  payload=%dB" % (name, frame["seq"], frame["len"]))
        if page is None:
            print("    payload is not a page")
            continue
        print("    page: ver=%d scene=%d bg=0x%04X nodes=%d"
              % (page["ver"], page["scene"], page["bg"], page["count"]))
        found.append((frame["seq"], page))

    if not found:
        print("RESULT: FAIL -- nothing saved in the layout sectors")
        return 1

    best = max(found, key=lambda pair: pair[0])
    (seq, page), = [best]
    ok = page["count"] == 2 and page["ver"] >= 1
    print("RESULT: %s -- newest seq=%d is ver=%d with %d nodes"
          % ("PASS" if ok else "SUSPECT", seq, page["ver"], page["count"]))
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
