"""weather -- the first real plugin: an HTTP JSON source pushed onto port 7801.

    python plugins/weather.py --once                  # fetch once, print, exit
    python plugins/weather.py                         # read plugins/weather.json
    python plugins/weather.py --config my.json --port 7801 --verbose

WHY A PLUGIN AND NOT A BUILT-IN SOURCE
    It measures nothing on this machine. Making it a plugin is what keeps
    "where numbers come from" separate from "what the daemon sends": the daemon
    does not grow a weather dependency, and if the API dies the panel shows --.

CONFIGURATION IS DATA, NOT CODE
    Every weather API names its fields differently, so this file does not know
    any of them. The config carries a URL template and a mapping from our metric
    ids to dot paths in the response:

        {"url": "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}&current=temperature_2m",
         "lat": "30.29", "lon": "120.16",
         "interval_s": 600,
         "map": {"weather.temp": {"path": "current.temperature_2m",
                                  "label": "TEMP", "unit": "C",
                                  "min": -50, "max": 60}}}

    Swapping providers is editing that file. The token placeholders are {city},
    {key}, {lat}, {lon}; anything else in the string is left alone.

WHAT IT DELIBERATELY DOES NOT DO
    * retry hard. One attempt per interval; a failure means "no value this
      round", and the hub turns 3 silent rounds into -- (PROTOCOL.md s.9).
    * invent a value when a path is missing. Absent is absent.
    * write the API key anywhere except the config file (which is gitignored).
"""

import argparse
import json
import os
import socket
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_CONFIG = os.path.join(HERE, "weather.json")
DEFAULT_HOST = "127.0.0.1"
DEFAULT_PORT = 7801
FETCH_TIMEOUT_S = 15.0
USER_AGENT = "pcmon-weather-plugin/1"


def load_config(path):
    with open(path, "r", encoding="utf-8") as handle:
        config = json.load(handle)
    if not isinstance(config, dict):
        raise SystemExit("%s: top level must be an object" % path)
    for required in ("url", "map"):
        if not config.get(required):
            raise SystemExit("%s: missing %r" % (path, required))
    if not isinstance(config["map"], dict) or not config["map"]:
        raise SystemExit("%s: 'map' must be a non-empty object" % path)
    config.setdefault("interval_s", 600)
    config.setdefault("apikey", "")
    config.setdefault("city", "")
    config.setdefault("lat", "")
    config.setdefault("lon", "")
    config.setdefault("timeout_s", FETCH_TIMEOUT_S)
    return config


def build_url(config):
    """Fill the placeholders. Unknown placeholders are left alone on purpose."""
    values = {
        "city": urllib.parse.quote(str(config.get("city", ""))),
        "key": urllib.parse.quote(str(config.get("apikey", ""))),
        "lat": urllib.parse.quote(str(config.get("lat", ""))),
        "lon": urllib.parse.quote(str(config.get("lon", ""))),
    }
    url = config["url"]
    for name, value in values.items():
        url = url.replace("{" + name + "}", value)
    return url


def dig(payload, path):
    """Follow a dot path ('current.temperature_2m'). None when it is not there."""
    node = payload
    for part in str(path).split("."):
        if not part:
            continue
        if isinstance(node, dict):
            if part not in node:
                return None
            node = node[part]
        elif isinstance(node, list):
            try:
                node = node[int(part)]
            except (ValueError, IndexError):
                return None
        else:
            return None
    return node


def as_number(value):
    """A published value is a float or nothing. Bools and strings are noise."""
    if isinstance(value, bool):
        return None
    if isinstance(value, (int, float)):
        return float(value)
    return None


def hello_metrics(config):
    """Catalog entries the daemon should show, built from the mapping."""
    interval_ms = int(float(config["interval_s"]) * 1000)
    entries = []
    for metric_id, spec in config["map"].items():
        spec = spec if isinstance(spec, dict) else {}
        entries.append({
            "id": metric_id,
            "label": str(spec.get("label", metric_id)),
            "unit": str(spec.get("unit", "")),
            "type": "number",
            "min": spec.get("min", 0),
            "max": spec.get("max", 100),
            "interval_ms": interval_ms,
            "group": str(spec.get("group", "weather")),
        })
    return entries


def fetch(config, verbose=False):
    """One HTTP GET, mapped to {metric_id: float}. Raises on transport errors."""
    url = build_url(config)
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(request, timeout=float(config["timeout_s"])) as resp:
        body = resp.read()
    payload = json.loads(body.decode("utf-8", "replace"))
    values = {}
    missing = []
    for metric_id, spec in config["map"].items():
        path = spec.get("path") if isinstance(spec, dict) else None
        number = as_number(dig(payload, path))
        if number is None:
            missing.append("%s<-%s" % (metric_id, path))
            continue
        values[metric_id] = number
    if verbose:
        print("fetch: %d of %d metrics (%s)"
              % (len(values), len(config["map"]),
                 ", ".join("%s=%s" % item for item in sorted(values.items()))))
        for item in missing:
            print("fetch: no value for " + item)
    return values


class Connection:
    """A single line-oriented socket to the daemon's plugin port."""

    def __init__(self, host, port, verbose=False):
        self.verbose = verbose
        self.sock = socket.create_connection((host, port), timeout=10.0)
        self.sock.settimeout(10.0)
        self.buf = b""

    def send(self, payload):
        line = json.dumps(payload, separators=(",", ":")).encode("utf-8") + b"\n"
        self.sock.sendall(line)
        if self.verbose:
            print("send: " + line.decode("utf-8").rstrip())

    def read_line(self):
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


def run(config, host, port, verbose=False):
    """Hello once, then push a values frame every interval. Returns on error."""
    metrics = hello_metrics(config)
    conn = Connection(host, port, verbose=verbose)
    try:
        conn.send({"t": "plugin", "k": "hello", "id": config.get("id", "weather"),
                   "metrics": metrics})
        answer = conn.read_line()
        if not isinstance(answer, dict) or not answer.get("ok"):
            raise SystemExit("daemon refused the plugin: %r" % (answer,))
        print("weather: registered %d metrics, every %ss"
              % (len(metrics), config["interval_s"]))
        interval = max(5.0, float(config["interval_s"]))
        failures = 0
        while True:
            try:
                values = fetch(config, verbose=verbose)
            except (urllib.error.URLError, OSError, ValueError) as exc:
                failures += 1
                print("weather: fetch failed (%d in a row): %s" % (failures, exc))
            else:
                if values:
                    failures = 0
                    conn.send({"t": "plugin", "k": "values",
                               "v": values, "ts": time.time()})
                else:
                    print("weather: response carried none of the mapped paths")
            time.sleep(interval)
    finally:
        conn.close()


def main(argv=None):
    parser = argparse.ArgumentParser(prog="weather-plugin")
    parser.add_argument("--config", default=DEFAULT_CONFIG)
    parser.add_argument("--host", default=DEFAULT_HOST)
    parser.add_argument("--port", type=int, default=DEFAULT_PORT)
    parser.add_argument("--once", action="store_true",
                        help="fetch once, print the mapped values, exit")
    parser.add_argument("--verbose", action="store_true")
    args = parser.parse_args(argv)

    if not os.path.isfile(args.config):
        raise SystemExit("%s: no such config (copy weather.sample.json)"
                         % args.config)
    config = load_config(args.config)
    if args.once:
        values = fetch(config, verbose=True)
        print("once: " + json.dumps(values, sort_keys=True))
        return 0 if values else 1
    run(config, args.host, args.port, verbose=args.verbose)
    return 0


if __name__ == "__main__":
    sys.exit(main())
