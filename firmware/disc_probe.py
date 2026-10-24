"""验 T5.5:板子自己找到 daemon,不靠写死的 IP。

用法(在 H:\\newwork 下):
    python firmware\\disc_probe.py

它做的事:
  1. 开 COM10,给板子发复位脉冲 —— 这一下会重启板子(正常,不是错误)
  2. 把启动后 15 秒内的 DISC / LINK / WIFI 行原样打出来
  3. 给结论 PASS / FAIL

判定:
  PASS = 看到 DISC here(daemon 报价了)+ LINK catalog(握手完成了)
  FAIL = DISC none(三轮广播无人应答)或压根没连上
           —— 顺便打印是不是防火墙/daemon 没起
"""

import argparse
import socket
import subprocess
import sys
import time

sys.path.insert(0, r"H:\newwork")
from pcmon.serialport import Port  # noqa: E402

SHOW = ("DISC", "LINK", "WIFI", "PANIC")


def say(msg):
    print(msg, flush=True)


def kick(port, seconds=9.0, tries=4):
    """Pulse EN until the board prints something.

    The CH343 on this desk ignores roughly one pulse in three, and a missed
    pulse is indistinguishable from a dead board -- the app only talks at boot.
    """
    for attempt in range(1, tries + 1):
        port.reset_board()
        port.release_board()
        buf = ""
        start = time.time()
        while time.time() - start < seconds:
            chunk = port.read(4096)
            if chunk:
                buf += chunk.decode("utf-8", "replace")
        if buf.strip():
            return buf, attempt
        say("  (reset attempt %d was ignored, pulsing again)" % attempt)
    return "", tries


def daemon_up():
    out = subprocess.run(
        ["powershell.exe", "-NoProfile", "-Command",
         "(Get-NetTCPConnection -LocalPort 7802 -State Listen -EA SilentlyContinue).OwningProcess"],
        capture_output=True, text=True).stdout.split()
    return int(out[0]) if out else None


def udp_allowed():
    """Is there an inbound allow rule for UDP 7802?

    Both halves matter: a TCP-only rule lets the link work while every
    discovery datagram is silently dropped, which is exactly what happened
    here on 2026-10-23.
    """
    out = subprocess.run(
        ["powershell.exe", "-NoProfile", "-Command",
         "(Get-NetFirewallPortFilter | Where-Object { $_.LocalPort -eq 7802 } |"
         " ForEach-Object { ($_ | Get-NetFirewallRule) } |"
         " Where-Object { $_.Direction -eq 'Inbound' -and $_.Action -eq 'Allow' -and $_.Enabled }"
         " | Measure-Object).Count"],
        capture_output=True, text=True).stdout.strip()
    return out.isdigit() and int(out) >= 2


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", default="COM10")
    args = ap.parse_args()

    pid = daemon_up()
    say("daemon      : %s" % ("pid=%d" % pid if pid else "NOT RUNNING  <- 先跑 H:\\newwork\\start-daemon.bat"))
    say("udp 7802    : %s" % ("rule present" if udp_allowed() else "NO UDP ALLOW RULE  <- 广播会被防火墙丢掉"))
    if pid is None:
        return 2

    say("talking to %s (this resets the board)" % args.port)
    port = Port(args.port, 115200)
    try:
        text, attempts = kick(port)
    finally:
        port.close()

    if not text:
        say("FAIL the board never printed anything -- check the cable, or the port number")
        return 2

    say("---- board said (%d reset attempt(s)) ----" % attempts)
    seen = []
    for line in text.splitlines():
        line = line.strip()
        if any(k in line for k in SHOW):
            seen.append(line)
            say("  " + line)

    discovery = [l for l in seen if l.startswith("DISC here")]
    gave_up = [l for l in seen if l.startswith("DISC none")]
    catalog = [l for l in seen if "LINK catalog" in l]
    peer = [l for l in seen if l.startswith("LINK tcp")]

    say("---- verdict ----")
    say("broadcast answered : %s" % ("yes" if discovery else "NO"))
    say("used a written-in  : %s" % ("yes (DISC none -> fallback)" if gave_up else "no"))
    say("handshake          : %s" % ("done" if catalog else "NOT DONE"))

    if discovery and catalog and not gave_up:
        say("PASS the board found the daemon by itself")
        if peer:
            say("     " + peer[0])
        return 0
    say("FAIL see the lines above")
    return 1


if __name__ == "__main__":
    sys.exit(main())
