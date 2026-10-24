#!/usr/bin/env python3
"""Print symbols from an ELF32 Xtensa image, filtered by a regex.

Why this exists: the Xtensa build has no `nm` on this box (espup installs the
Rust toolchain but not binutils), and the question "how much stack is left?"
is answered by two linker symbols. Reading them beats installing binutils.

Usage:
    python firmware/elf_syms.py <elf> [regex]
    python firmware/elf_syms.py target/.../st7735-text 'stack|heap'
"""

import re
import struct
import sys


def read_sections(blob):
    (e_shoff,) = struct.unpack_from("<I", blob, 0x20)
    e_shentsize, e_shnum, e_shstrndx = struct.unpack_from("<HHH", blob, 0x2E)
    secs = []
    for i in range(e_shnum):
        off = e_shoff + i * e_shentsize
        fields = struct.unpack_from("<IIIIIIIIII", blob, off)
        secs.append(
            {
                "name_off": fields[0],
                "type": fields[1],
                "addr": fields[3],
                "offset": fields[4],
                "size": fields[5],
                "link": fields[6],
                "entsize": fields[9],
            }
        )
    strtab = secs[e_shstrndx]
    for sec in secs:
        start = strtab["offset"] + sec["name_off"]
        end = blob.index(b"\0", start)
        sec["name"] = blob[start:end].decode("ascii", "replace")
    return secs


def cstr(blob, off):
    end = blob.index(b"\0", off)
    return blob[off:end].decode("ascii", "replace")


def main(argv):
    if len(argv) < 2:
        print(__doc__)
        return 2
    path = argv[1]
    pattern = re.compile(argv[2] if len(argv) > 2 else r".")
    blob = open(path, "rb").read()
    if blob[:4] != b"\x7fELF":
        print("not an ELF file")
        return 2
    if blob[4] != 1:
        print("not ELF32")
        return 2
    secs = read_sections(blob)
    wanted = [s for s in secs if s["type"] in (2, 11)]  # SYMTAB, DYNSYM
    if not wanted:
        print("no symtab (stripped?)")
        return 1

    rows = []
    for tab in wanted:
        strtab = secs[tab["link"]]
        count = tab["size"] // 16
        for i in range(count):
            off = tab["offset"] + i * 16
            name_off, value, size, info, other, shndx = struct.unpack_from(
                "<IIIBBH", blob, off
            )
            if name_off == 0:
                continue
            name = cstr(blob, strtab["offset"] + name_off)
            if not pattern.search(name):
                continue
            kind = "FUNC" if info & 0xF == 2 else ("OBJ" if info & 0xF == 1 else "-")
            rows.append((value, size, kind, name, shndx))

    print("%-10s %-12s %-6s %s" % ("value", "size", "kind", "name"))
    for value, size, kind, name, shndx in sorted(rows):
        tag = "" if shndx != 0 else " (undef)"
        print("%08x   %-12d %-6s %s%s" % (value, size, kind, name, tag))
    print("\n%d symbols matched" % len(rows))

    print("\n-- regions of interest --")
    for value, size, kind, name, _ in sorted(rows):
        if name in ("_stack_start", "_stack_end", "_bss_start", "_bss_end", "_heap_start", "_heap_end"):
            print("%-14s 0x%08x  end 0x%08x" % (name, value, value + size))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
