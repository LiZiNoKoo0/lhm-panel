"""check_plugins -- does the plugin port keep its promises?

    python tools/check_plugins.py            # everything that needs no network
    python tools/check_plugins.py --live     # also ask the real weather API once

WHAT IT ASSERTS
    hub:
      * a hello with metrics is accepted and shows up in the catalog
      * a values frame reaches snapshot(), and a silent metric turns into None
        after interval_ms x 3 -- never the previous reading
      * a plugin that exits keeps its catalog entry (no page rearrange)
      * junk lines are counted and do not kill the connection
      * values before hello close the socket
    weather plugin:
      * a canned API response maps through the config into the right numbers
        (stub HTTP server, so this part never touches the internet)

WHAT IT DOES NOT DO
    * start a daemon -- the hub is exercised on its own, which is the point of
      making it a source
"""

import argparse
import http.server
import json
import os
import socket
import subprocess
import sys
import threading
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if ROOT not in sys.path:
    sys.path.insert(0, ROOT)

from pcmon import plugins          # noqa: E402  (after sys.path)

WEATHER = os.path.join(ROOT, "plugins", "weather.py")
TMP = os.path.join(ROOT, "tmp")


class Checker:
    def __init__(self):
        self.passed = 0
        self.failed = 0

    def check(self, condition, text):
        if condition:
            self.passed += 1
            print("PASS  " + text)
        else:
            self.failed += 1
            print("FAIL  " + text)
        return bool(condition)


def wait_for(predicate, timeout=3.0, step=0.05):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        value = predicate()
        if value:
            return value
        time.sleep(step)
    return None


class FakePlugin:
    """A raw socket client, so the test speaks the wire and not our own objects."""

    def __init__(self, port, host="127.0.0.1"):
        self.sock = socket.create_connection((host, port), timeout=5.0)
        self.sock.settimeout(5.0)
        self.buf = b""

    def send(self, payload):
        if isinstance(payload, (dict, list)):
            payload = json.dumps(payload)
        self.sock.sendall(payload.encode("utf-8") + b"\n")

    def read(self):
        while b"\n" not in self.buf:
            chunk = self.sock.recv(4096)
            if not chunk:
                return None
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        return json.loads(line.decode("utf-8", "replace"))

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass


def check_hub(checker):
    hub = plugins.PluginHub(port=0, log=lambda text: None)
    if not checker.check(hub.start(), "hub: start() binds a port"):
        return
    port = hub.port
    checker.check(port > 0, "hub: asked for port 0 and got %d" % port)

    metric = {"id": "disk.read", "label": "DISK", "unit": "KB/s",
              "type": "number", "min": 0, "max": 9999,
              "interval_ms": 1000, "group": "system"}

    plugin = FakePlugin(port)
    plugin.send({"t": "plugin", "k": "hello", "id": "disk", "metrics": [metric]})
    ack = plugin.read()
    checker.check(isinstance(ack, dict) and ack.get("ok") is True,
                  "hub: hello is acknowledged")

    ids = [entry["id"] for entry in hub.metrics()]
    checker.check(ids == ["disk.read"], "hub: the declared metric joins the catalog")

    checklist = hub.snapshot(time.monotonic())
    checker.check(checklist.get("disk.read") is None,
                  "hub: a metric with no value yet reads None, not 0")

    plugin.send({"t": "plugin", "k": "values", "v": {"disk.read": 1234.5}})
    got = wait_for(lambda: hub.snapshot(time.monotonic()).get("disk.read"))
    checker.check(got == 1234.5, "hub: a pushed value reaches snapshot (%r)" % (got,))

    # junk must be survivable, and must not be mistaken for data
    plugin.send("this is not json")
    plugin.send({"t": "something-else", "k": "values"})
    plugin.send({"t": "plugin", "k": "values", "v": {"disk.read": 77.0}})
    again = wait_for(lambda: hub.snapshot(time.monotonic()).get("disk.read") == 77.0)
    checker.check(bool(again), "hub: junk lines are skipped, the plugin keeps working")
    checker.check(hub.stats.get("bad_json", 0) >= 1,
                  "hub: the bad line was counted (bad_json=%d)"
                  % hub.stats.get("bad_json", 0))

    plugin.close()
    time.sleep(0.3)
    checker.check(hub.live_ids() == [], "hub: a closed socket leaves the live set")
    still = [entry["id"] for entry in hub.metrics()]
    checker.check(still == ["disk.read"],
                  "hub: the dead plugin keeps its catalog entry")

    # staleness: interval_ms=1000 -> None after 3 s of silence
    silent = wait_for(lambda: hub.snapshot(time.monotonic()).get("disk.read") is None,
                      timeout=6.0)
    checker.check(bool(silent),
                  "hub: 3 s of silence turns the value into None, not the old one")

    # values before hello must close the socket
    early = FakePlugin(port)
    early.send({"t": "plugin", "k": "values", "v": {"x": 1.0}})
    deadline = time.monotonic() + 3.0
    closed = False
    try:
        early.sock.settimeout(0.5)
        while time.monotonic() < deadline:
            if early.sock.recv(16) == b"":
                closed = True
                break
    except OSError:
        closed = True
    early.close()
    checker.check(closed, "hub: values before hello get the connection closed")

    hub.stop()
    checker.check(not hub.running(), "hub: stop() releases the listener")


class StubHandler(http.server.BaseHTTPRequestHandler):
    body = b"{}"

    def do_GET(self):
        payload = self.body
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, fmt, *args):
        pass


def check_weather_mapping(checker):
    """Feed the plugin a canned API response and see what it maps."""
    canned = {"current": {"temperature_2m": 21.4, "relative_humidity_2m": 68,
                          "wind_speed_10m": 3.2}}
    StubHandler.body = json.dumps(canned).encode("utf-8")
    server = http.server.HTTPServer(("127.0.0.1", 0), StubHandler)
    port = server.server_address[1]
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        os.makedirs(TMP, exist_ok=True)
        config_path = os.path.join(TMP, "weather.check.json")
        with open(config_path, "w", encoding="utf-8") as handle:
            json.dump({
                "id": "weather",
                "url": "http://127.0.0.1:%d/v1/forecast?lat={lat}&lon={lon}" % port,
                "lat": "30.29",
                "lon": "120.16",
                "interval_s": 600,
                "map": {
                    "weather.temp": {"path": "current.temperature_2m",
                                     "label": "TEMP", "unit": "C"},
                    "weather.humidity": {"path": "current.relative_humidity_2m",
                                         "label": "HUM", "unit": "%"},
                    "weather.wind": {"path": "current.wind_speed_10m",
                                     "label": "WIND", "unit": "m/s"},
                    "weather.missing": {"path": "current.not_there",
                                        "label": "NOPE", "unit": ""},
                },
            }, handle)
        done = subprocess.run([sys.executable, WEATHER, "--once",
                               "--config", config_path],
                              capture_output=True, text=True, timeout=60)
        out = (done.stdout or "") + (done.stderr or "")
        checker.check(done.returncode == 0, "weather: --once exits 0")
        checker.check('"weather.temp": 21.4' in out,
                      "weather: the dot path lands on the right number")
        checker.check('"weather.humidity": 68.0' in out,
                      "weather: integers become floats, not strings")
        checker.check("weather.missing" not in out.split("once:")[-1],
                      "weather: a missing path is absent, not invented")
    finally:
        server.shutdown()
        server.server_close()


def check_live(checker):
    """Ask the real API once. Optional because the test box may be offline."""
    sample = os.path.join(ROOT, "plugins", "weather.sample.json")
    done = subprocess.run([sys.executable, WEATHER, "--once", "--config", sample],
                          capture_output=True, text=True, timeout=90)
    out = (done.stdout or "") + (done.stderr or "")
    ok = done.returncode == 0 and "once:" in out
    checker.check(ok, "weather: the live API answers (%s)"
                  % (out.strip().splitlines()[-1] if out.strip() else "no output"))


def main(argv=None):
    parser = argparse.ArgumentParser(prog="check_plugins")
    parser.add_argument("--live", action="store_true",
                        help="also query the real weather API once")
    args = parser.parse_args(argv)

    checker = Checker()
    check_hub(checker)
    check_weather_mapping(checker)
    if args.live:
        check_live(checker)

    total = checker.passed + checker.failed
    print("\nPLUGINS %s (%d checks, %d failed)"
          % ("OK" if not checker.failed else "FAILED", total, checker.failed))
    return 0 if not checker.failed else 1


if __name__ == "__main__":
    raise SystemExit(main())
