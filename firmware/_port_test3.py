import sys, time
sys.path.insert(0, r"H:\newwork")
from pcmon import serialport
p = serialport.Port("COM10", 115200)
print("open queued:", p.queued())
p.write_line("CPU 33.5%|TEMP   34C")
buf = b""
t0 = time.time()
while time.time() - t0 < 6:
    c = p.read()
    if c: buf += c
    else: time.sleep(0.02)
print("bytes", len(buf))
print(buf.decode("ascii","replace")[-400:])
p.close()
