import sys, time
sys.path.insert(0, r"H:\newwork")
from pcmon import serialport
p = serialport.Port("COM10", 115200)
p.release_board()
buf = b""
t0 = time.time()
sent_at = None
while time.time() - t0 < 12:
    chunk = p.read()
    if chunk:
        buf += chunk
    if sent_at is None and time.time() - t0 > 1.0:
        p.write_line("CPU 33.5%|TEMP   34C"); sent_at = time.time()
    time.sleep(0.02)
text = buf.decode("ascii", "replace")
import re
for line in text.splitlines():
    if any(k in line for k in ("FRAME", "BADGE", "LINK", "READY", "rst:", "panic")):
        print(line.strip()[:160])
print("total bytes", len(buf))
p.close()
