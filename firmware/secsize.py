import struct, sys
blob = open(sys.argv[1], "rb").read()
(e_shoff,) = struct.unpack_from("<I", blob, 0x20)
e_shentsize, e_shnum, e_shstrndx = struct.unpack_from("<HHH", blob, 0x2E)
def sec(i):
    off = e_shoff + i * e_shentsize
    return struct.unpack_from("<IIIIIIIIII", blob, off)
strtab = sec(e_shstrndx)
def name(idx):
    s = strtab[4] + idx
    return blob[s:blob.index(b"\0", s)].decode()
for i in range(e_shnum):
    f = sec(i)
    if name(f[0]) in (".bss", ".data"):
        print("%s size=%d" % (name(f[0]), f[5]))
