#!/usr/bin/env python3
"""Answer "which symbol owns this address?" for a crash dump.

Feed it the PC/A1/A2... from a panic TrapFrame and it prints the containing
symbol plus the offset, so a raw hex number becomes "bt_wifi_chan_data+0x4".

Usage:
    python firmware/elf_lookup.py <elf> 0x4000c2e4 0x3ffdce40 ...
"""

import sys

import elf_syms


def load(path):
    blob = open(path, "rb").read()
    secs = elf_syms.read_sections(blob)
    syms = []
    for tab in [s for s in secs if s["type"] in (2, 11)]:
        strtab = secs[tab["link"]]
        for i in range(tab["size"] // 16):
            off = tab["offset"] + i * 16
            import struct

            name_off, value, size, info, other, shndx = struct.unpack_from("<IIIBBH", blob, off)
            if name_off == 0 or shndx == 0:
                continue
            name = elf_syms.cstr(blob, strtab["offset"] + name_off)
            syms.append((value, max(size, 1), name))
    syms.sort()
    return syms


def main(argv):
    if len(argv) < 3:
        print(__doc__)
        return 2
    syms = load(argv[1])
    for raw in argv[2:]:
        addr = int(raw, 0)
        hit = None
        for value, size, name in syms:
            if value <= addr < value + size:
                hit = (value, name)
            elif value > addr:
                break
        if hit:
            print("0x%08x  %s+0x%x" % (addr, hit[1], addr - hit[0]))
        else:
            print("0x%08x  -- no symbol (ROM or gap)" % addr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
