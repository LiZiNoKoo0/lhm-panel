import sys, time
sys.path.insert(0, r"H:\newwork")
from pcmon import serialport
p = serialport.Port("COM10", 115200)
p.release_board()
time.sleep(1.0)
print("queued before:", p.queued())
print("write ->", p.write_line("CPU 33.5%|TEMP   34C"))
time.sleep(1.5)
data = b""
for _ in range(40):
    data += p.read()
    time.sleep(0.05)
print("read %d bytes: %r" % (len(data), data[-300:]))
p.close()
