"""plugins -- out-of-process metric sources on TCP 7801 (PROTOCOL.md s.1).

    from pcmon import plugins
    hub = plugins.PluginHub(log=print)
    hub.start()                      # listens on 127.0.0.1:7801
    hub.snapshot(now)                # {metric_id: value | None}
    hub.metrics()                    # catalog entries contributed so far
    hub.stop()

WHY A SOCKET AND NOT AN IMPORT
    C6 made "a value source" a contract (start/stop/tick/snapshot/metrics) that
    lives in this process. A plugin is the same contract on the other side of a
    socket: it can be written in any language, restarted on its own, and cannot
    take the daemon down with it. The daemon does not care which one it holds --
    CompositeSource makes a hub look exactly like a built-in source.

WIRE FORMAT (newline-delimited JSON, exactly like every other port here)

    plugin -> daemon
      {"t":"plugin","k":"hello","id":"weather","metrics":[{...catalog...}]}
      {"t":"plugin","k":"values","v":{"weather.temp":21.4},"ts":1761000000.0}
      {"t":"plugin","k":"bye","why":"shutting down"}

    daemon -> plugin
      {"t":"plugin","k":"hello","ok":true}          accepted
      {"t":"plugin","k":"error","why":"..."}         then the socket is closed

RULES THAT ARE NOT NEGOTIABLE
    * loopback only. 7801 binds 127.0.0.1 and refuses anything else, because a
      plugin can rewrite what the screen shows. Device-key auth is the follow-up
      (the same challenge-response the terminals use); v1 trusts the machine.
    * a plugin that goes quiet is not a plugin that knows nothing: after
      interval_ms x 3 without a value its metrics answer None, so the panel shows
      -- and never the previous reading (PROTOCOL.md s.9).
    * a metric id stays in the catalog after the plugin exits. Removing it would
      bump `ver` and make the page rearrange itself; a dead plugin should look
      like "--", not like a different dashboard.
    * one thread per plugin, a bounded line buffer, and no lock shared with the
      send path: a plugin that stops reading must not stall the daemon.

WHAT THIS FILE DOES NOT DO
    * decide what a metric means -- it forwards ids and numbers
    * write catalog `source` (s.6.1): plugin metrics are what the PC measures,
      so they are `remote`, and that is the daemon's catalog to fill in
"""

import json
import socket
import threading
import time

#: The port the architecture diagram has always promised plugins (PROTOCOL.md s.1).
PLUGIN_PORT = 7801

#: Loopback only, and that is a security decision, not a default.
PLUGIN_HOST = "127.0.0.1"

#: A plugin that stops reading must not grow a queue in our memory.
MAX_BUFFER = 64 * 1024

#: How many times interval_ms a metric may stay silent before it reads None.
STALE_FACTOR = 3.0

#: Backlog for the listen socket; plugins are few.
LISTEN_BACKLOG = 8


class _Plugin:
    """One connected plugin: what it promised, and what it last said."""

    def __init__(self, plugin_id, addr):
        self.id = plugin_id
        self.addr = addr
        self.metrics = []          # catalog entries it declared
        self.values = {}           # metric_id -> (value, when)
        self.connected_at = time.monotonic()

    def accept_values(self, payload, now):
        """Merge one values frame. Returns how many ids it carried."""
        if not isinstance(payload, dict):
            return 0
        taken = 0
        for metric_id, value in payload.items():
            if not isinstance(metric_id, str):
                continue
            if value is None or isinstance(value, (int, float)):
                self.values[metric_id] = (value, now)
                taken += 1
        return taken


class PluginHub:
    """Accepts plugin connections and answers the source contract.

    Everything here is defensive: a malformed line, a plugin that never says
    hello, a socket that dies mid-frame -- each is counted and forgotten. The
    daemon must not notice any of it.
    """

    name = "plugins"

    def __init__(self, host=PLUGIN_HOST, port=PLUGIN_PORT, log=None,
                 clock=None, stale_factor=STALE_FACTOR):
        self.host = host
        self.port = int(port)
        self.stale_factor = float(stale_factor)
        self.stats = {}
        self.last_error = ""
        self._log = log if callable(log) else None
        self._clock = clock if clock is not None else time.monotonic
        self._lock = threading.Lock()
        self._plugins = {}         # plugin_id -> _Plugin (live or remembered)
        self._live = set()         # plugin ids currently connected
        self._sock = None
        self._thread = None
        self._stop = threading.Event()

    # -- logging ------------------------------------------------------

    def _count(self, key, by=1):
        self.stats[key] = self.stats.get(key, 0) + by

    def _note(self, text):
        self.last_error = text
        if self._log is None:
            return
        try:
            self._log(text)
        except Exception:          # noqa: BLE001 - logging must never hurt
            self._count("log_errors")

    # -- lifecycle ----------------------------------------------------

    def start(self):
        """Bind and accept in the background. True when listening."""
        with self._lock:
            if self._thread is not None and self._thread.is_alive():
                return False
        try:
            sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            sock.bind((self.host, self.port))
            sock.listen(LISTEN_BACKLOG)
            sock.settimeout(0.5)
            # port=0 means "pick one": the bound port is the real answer, and
            # tests rely on being able to read it back.
            self.port = sock.getsockname()[1]
        except OSError as exc:
            self._count("bind_errors")
            self._note("plugin port %d: %s" % (self.port, exc))
            return False
        with self._lock:
            self._sock = sock
            self._stop.clear()
            thread = threading.Thread(target=self._accept_loop, daemon=True,
                                      name="plugin-accept")
            self._thread = thread
        thread.start()
        self._count("starts")
        return True

    def stop(self, timeout=2.0):
        """Close the listener and every plugin socket. Idempotent."""
        self._stop.set()
        with self._lock:
            sock, self._sock = self._sock, None
            thread = self._thread
            self._thread = None
        if sock is not None:
            try:
                sock.close()
            except OSError:
                pass
        if thread is not None and thread.is_alive():
            try:
                thread.join(timeout)
            except RuntimeError:
                pass
        self._count("stops")
        return True

    def running(self):
        thread = self._thread
        return thread is not None and thread.is_alive()

    def plugin_ids(self):
        with self._lock:
            return sorted(self._plugins)

    def live_ids(self):
        with self._lock:
            return sorted(self._live)

    # -- accept / read ------------------------------------------------

    def _accept_loop(self):
        while not self._stop.is_set():
            with self._lock:
                sock = self._sock
            if sock is None:
                break
            try:
                conn, addr = sock.accept()
            except socket.timeout:
                continue
            except OSError:
                if self._stop.is_set():
                    break
                self._count("accept_errors")
                continue
            self._count("connections")
            thread = threading.Thread(target=self._serve, args=(conn, addr),
                                      daemon=True, name="plugin-conn")
            thread.start()

    def _serve(self, conn, addr):
        """One plugin, from hello to EOF. Never raises."""
        plugin = None
        buf = b""
        try:
            conn.settimeout(1.0)
            while not self._stop.is_set():
                try:
                    chunk = conn.recv(4096)
                except socket.timeout:
                    continue
                except OSError:
                    break
                if not chunk:
                    break
                buf += chunk
                if len(buf) > MAX_BUFFER:
                    self._count("buffer_overruns")
                    self._note("plugin %s exceeded %d bytes without a newline"
                               % (addr[0], MAX_BUFFER))
                    break
                while b"\n" in buf:
                    line, buf = buf.split(b"\n", 1)
                    if not line.strip():
                        continue
                    plugin = self._handle_line(conn, addr, plugin, line)
                    if plugin is None:
                        return
        except Exception as exc:   # noqa: BLE001 - a plugin must not kill us
            self._count("serve_errors")
            self._note("plugin %s failed: %s: %s"
                       % (addr[0], type(exc).__name__, exc))
        finally:
            try:
                conn.close()
            except OSError:
                pass
            if plugin is not None:
                with self._lock:
                    self._live.discard(plugin.id)
                self._count("disconnects")
                self._note("plugin %s disconnected" % plugin.id)

    def _handle_line(self, conn, addr, plugin, line):
        """Decode one line. Returns the plugin (None means: close this socket)."""
        self._count("lines")
        try:
            msg = json.loads(line.decode("utf-8", "replace"))
        except ValueError:
            self._count("bad_json")
            return plugin
        if not isinstance(msg, dict) or msg.get("t") != "plugin":
            self._count("bad_frames")
            return plugin
        kind = msg.get("k")
        now = self._clock()

        if kind == "hello":
            return self._on_hello(conn, addr, msg)
        if plugin is None:
            self._count("values_before_hello")
            return None
        if kind == "values":
            with self._lock:
                taken = plugin.accept_values(msg.get("v"), now)
            self._count("values_frames")
            self._count("values", taken)
            return plugin
        if kind == "bye":
            self._note("plugin %s said bye (%s)"
                       % (plugin.id, msg.get("why", "")))
            return None
        self._count("unknown_kind")
        return plugin

    def _on_hello(self, conn, addr, msg):
        plugin_id = msg.get("id")
        metrics = msg.get("metrics")
        if not isinstance(plugin_id, str) or not plugin_id.strip():
            self._send(conn, {"t": "plugin", "k": "error", "why": "no id"})
            self._count("hello_rejected")
            return None
        if not isinstance(metrics, list) or not metrics:
            self._send(conn, {"t": "plugin", "k": "error",
                              "why": "hello without metrics"})
            self._count("hello_rejected")
            return None
        clean = [entry for entry in metrics
                 if isinstance(entry, dict) and isinstance(entry.get("id"), str)]
        if not clean:
            self._send(conn, {"t": "plugin", "k": "error",
                              "why": "no usable metric entries"})
            self._count("hello_rejected")
            return None

        with self._lock:
            known = self._plugins.get(plugin_id)
            if known is None:
                known = _Plugin(plugin_id, addr)
                self._plugins[plugin_id] = known
                self._count("plugins_seen")
            known.metrics = clean
            known.connected_at = time.monotonic()
            self._live.add(plugin_id)
        self._send(conn, {"t": "plugin", "k": "hello", "ok": True})
        self._count("hellos")
        self._note("plugin %s hello: %s" % (
            plugin_id, ",".join(str(entry.get("id")) for entry in clean)))
        return known

    @staticmethod
    def _send(conn, payload):
        try:
            conn.sendall((json.dumps(payload, separators=(",", ":")) + "\n")
                         .encode("utf-8"))
        except OSError:
            pass

    # -- the source contract ------------------------------------------

    def metrics(self):
        """Every metric any plugin ever declared, deduped by id.

        A dead plugin keeps its entries on purpose: dropping them would bump the
        catalog version and rearrange the page (see the header).
        """
        seen = {}
        with self._lock:
            for plugin_id in sorted(self._plugins):
                for entry in self._plugins[plugin_id].metrics:
                    metric_id = entry.get("id")
                    if metric_id and metric_id not in seen:
                        seen[metric_id] = dict(entry)
        return list(seen.values())

    def snapshot(self, now=None):
        """Values from every live plugin, None where a metric has gone stale.

        A metric with no `interval_ms` is treated as fresh for one minute; that
        is the plugin's own declaration to make, not ours to invent.
        """
        now = float(self._clock() if now is None else now)
        out = {}
        with self._lock:
            for plugin in self._plugins.values():
                for entry in plugin.metrics:
                    metric_id = entry.get("id")
                    interval_ms = entry.get("interval_ms")
                    try:
                        interval_s = max(0.1, float(interval_ms) / 1000.0)
                    except (TypeError, ValueError):
                        interval_s = 60.0
                    stamp = plugin.values.get(metric_id)
                    if stamp is None:
                        out[metric_id] = None
                        continue
                    value, when = stamp
                    if now - when > interval_s * self.stale_factor:
                        self._count("stale", 1)
                        out[metric_id] = None
                    else:
                        out[metric_id] = value
        return out

    def tick(self, now=None):
        """Nothing to do: plugins push, they are not polled."""
        return None


def selftest():
    """Contract checks that need no plugin process. Returns (ok, lines)."""
    lines = []
    ok = True

    def check(condition, text):
        nonlocal ok
        ok = ok and bool(condition)
        lines.append(("ok   " if condition else "FAIL ") + text)

    clock = [1000.0]
    hub = PluginHub(port=0, clock=lambda: clock[0])
    check(isinstance(hub.snapshot(1000.0), dict),
          "an empty hub answers {} rather than raising")
    check(hub.metrics() == [], "an empty hub contributes no catalog entries")
    check(hub.tick(1000.0) is None, "tick() is a no-op")

    plugin = _Plugin("weather", ("127.0.0.1", 0))
    plugin.metrics = [{"id": "weather.temp", "label": "T", "unit": "C",
                       "interval_ms": 1000}]
    plugin.accept_values({"weather.temp": 21.5}, 1000.0)
    with hub._lock:
        hub._plugins["weather"] = plugin
    check(hub.snapshot(1000.0)["weather.temp"] == 21.5,
          "a fresh value is republished")
    clock[0] = 1004.0
    check(hub.snapshot(1004.0)["weather.temp"] is None,
          "3 s of silence on a 1 s metric becomes None, never the old value")
    check(hub.metrics()[0]["id"] == "weather.temp",
          "a silent plugin keeps its catalog entry")
    return ok, lines


def _main():
    ok, lines = selftest()
    for line in lines:
        print(line)
    print("PLUGINS %s (%d checks)" % ("OK" if ok else "FAILED", len(lines)))
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(_main())
