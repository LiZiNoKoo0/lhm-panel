import sys, zlib
sys.path.insert(0, r"H:\newwork")
import preview_st7735 as P
for badge in (False, True):
    d = P.to_bytes(P.render_page(("CPU 33.5%", "TEMP   34C"), 0xFFFF, 0x0000, badge=badge))
    print("badge=%s crc=0x%08X" % (badge, zlib.crc32(d) & 0xFFFFFFFF))
