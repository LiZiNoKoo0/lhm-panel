"""fake_daemon -- synthetic PC-Monitor daemon (milestone M2).

    python fake_daemon.py [--record recordings/session1.ndjson]

It speaks PROTOCOL.md v0.4 over TCP/NDJSON (default 0.0.0.0:7802) to any
number of display terminals at once and pushes invented CPU / memory / network
numbers at 1 Hz. Every byte it encodes or decodes goes through pcmon.protocol,
and every layout goes through pcmon.protocol.fit_nodes, so this file contains
no protocol knowledge of its own beyond the layouts it authors.

What this file deliberately does NOT do:
  * collect any real system data -- CPU/RAM/NET are invented (M5 does reality)
  * accept commands from terminals -- terminals only report events (s.5)
  * keep value history for sparklines -- the terminal owns that ring buffer
    (s.8); we only ever send the newest value
  * answer UDP "discover" broadcasts or serve the HTTP admin UI (port 7803)
  * trust any terminal cache: after every hello catalog+scenes+ui+data are
    resent unconditionally (s.16 red line 4)
  * enforce an idle timeout -- the 15s "LINK LOST" detection is firmware work
    (s.13.5), and a terminal that never pings must not be dropped here
  * scale layouts -- pages are authored for 240x320 and only trimmed to the
    client's capabilities, never re-flowed
  * let one terminal block another: one outbox per connection, 200ms send
    timeout, and the data frame is a single slot (newest wins, never queued)
  * hold a global lock around sends -- the registry lock is only ever held to
    snapshot the client list, so a slow client cannot stall the others
  * die on a bad connection -- every per-connection error is swallowed and
    counted (TASKS.md coding rule 3)

Source is pure ASCII on purpose (TASKS.md coding rule 1: PowerShell 5.1 on
this host mangles GBK/UTF-8 console output). Python 3.12 stdlib only.
"""

import argparse
import collections
import math
import os
import random
import secrets
import select
import socket
import sys
import threading
import time
from pathlib import Path

from pcmon import protocol

# ---------------------------------------------------------------------------
# Constants -- every magic number lives here (TASKS.md coding rule 4)
# ---------------------------------------------------------------------------

BIND_HOST = "0.0.0.0"
DEFAULT_PORT = protocol.DEFAULT_PORT          # 7802
SOCKET_BACKLOG = 16
ACCEPT_TIMEOUT_S = 0.5                        # accept() poll slice for Ctrl-C

RECV_CHUNK_BYTES = 4096
RECV_TIMEOUT_S = 0.5                          # select() slice for shutdown checks
MAX_LINE_BYTES = 65536                        # oversized line => not NDJSON

SEND_TIMEOUT_S = 0.2                          # PROTOCOL.md s.16 red line 1
MAX_STRUCT_BACKLOG = 16                       # structural frames we refuse to drop
TICK_INTERVAL_S = 0.2                         # value jitter / housekeeping tick
DATA_INTERVAL_S = 1.0                         # one data frame per second
STAT_LOG_INTERVAL_S = 5.0                     # throttle for stat chatter
SEQ_SLACK_BYTES = 8                           # room for the "s":<seq> we add at send

#: Message types that carry the PC -> terminal sequence number (s.4).
SEQ_KINDS = ("catalog", "scenes", "ui", "data", "cmd", "time", "debug")

SCREEN_W = 240                                # authored layout size, portrait
SCREEN_H = 320
DEFAULT_SCENE = 0
UI_VER = 1                                    # bump when a layout changes
CATALOG_VER = 1                               # bump when METRICS changes
BG_COLOR = "#F7F7F7"
TOKEN_BYTES = 8
EXAMPLE_CLIENT_ID = "sample-thin"

#: Fake signal shaping (only used to invent numbers).
TAU = 2.0 * math.pi
CPU_PERIOD_S = 20.0
CPU_BASE = 50.0
CPU_AMP = 30.0
CPU_NOISE = 4.0
TEMP_PERIOD_S = 90.0
TEMP_BASE = 60.0
TEMP_AMP = 15.0
TEMP_NOISE = 1.0
MEM_CYCLE_S = 150.0
MEM_FLOOR = 35.0
MEM_CEIL = 95.0
MEM_NOISE = 1.0
NET_SPIKE_P = 0.15
NET_LOW_MAX = 400.0
NET_SPIKE_MIN = 2000.0
NET_SPIKE_MAX = 8000.0
NOISE_SMOOTH = 0.6

#: Catalog labels are short on purpose: small screens show them verbatim (s.6).
METRICS = [
    {"id": "cpu.total", "label": "CPU", "unit": "%", "type": "number",
     "min": 0, "max": 100, "interval_ms": 1000},
    {"id": "cpu.temp", "label": "TEMP", "unit": "C", "type": "number",
     "min": 0, "max": 110, "interval_ms": 1000},
    {"id": "mem.used", "label": "MEM", "unit": "%", "type": "number",
     "min": 0, "max": 100, "interval_ms": 1000},
    {"id": "net.down", "label": "NET", "unit": "KB/s", "type": "number",
     "min": 0, "max": 8000, "interval_ms": 1000},
]

#: Page list. The daemon owns it; terminals only know "which index" (s.10).
SCENE_TITLES = ["CPU", "MEMNET", "KEYS"]
FOCUS_SCENE = 2

# ---------------------------------------------------------------------------
# Layouts -- authored here, trimmed later by protocol.fit_nodes()
# ---------------------------------------------------------------------------
# Absolute coordinates, top-left origin, draw order == list order (s.7).
# Node types stay inside the v0.3 minimal set (rect / path / text / num) plus
# the optional "line" widget, which fit_nodes drops for terminals that do not
# declare it. "min"/"max" on a bind_w rect is an optional hint for terminals
# that want to scale; unknown fields are ignored by everyone else (s.20).

LAYOUT_CPU = [
    {"type": "text", "x": 8, "y": 8, "text": "CPU", "size": 16, "color": "#605E5C"},
    {"type": "num", "x": 8, "y": 26, "bind": "cpu.total", "dec": 0, "unit": "%",
     "size": 32, "color": "#1F1F1F"},
    {"type": "rect", "x": 8, "y": 70, "w": 224, "h": 10, "fill": "#E1DFDD"},
    {"type": "rect", "x": 8, "y": 70, "w": 224, "h": 10, "fill": "#0078D4",
     "bind_w": "cpu.total", "min": 0, "max": 100},
    {"type": "line", "x": 8, "y": 92, "w": 224, "h": 80, "bind": "cpu.total",
     "min": 0, "max": 100, "points": 60, "color": "#0078D4"},
    {"type": "path", "x": 8, "y": 178, "pts": [0, 0, 224, 0], "color": "#E1DFDD"},
    {"type": "text", "x": 8, "y": 186, "text": "TEMP", "size": 12, "color": "#605E5C"},
    {"type": "num", "x": 56, "y": 186, "bind": "cpu.temp", "dec": 0, "unit": "C",
     "size": 24, "color": "#1F1F1F"},
]

LAYOUT_MEMNET = [
    {"type": "text", "x": 8, "y": 8, "text": "MEM", "size": 16, "color": "#605E5C"},
    {"type": "num", "x": 8, "y": 26, "bind": "mem.used", "dec": 0, "unit": "%",
     "size": 32, "color": "#1F1F1F"},
    {"type": "rect", "x": 8, "y": 70, "w": 224, "h": 10, "fill": "#E1DFDD"},
    {"type": "rect", "x": 8, "y": 70, "w": 224, "h": 10, "fill": "#0078D4",
     "bind_w": "mem.used", "min": 0, "max": 100},
    {"type": "text", "x": 8, "y": 96, "text": "NET DOWN", "size": 12, "color": "#605E5C"},
    {"type": "num", "x": 8, "y": 112, "bind": "net.down", "dec": 0, "unit": "KB/s",
     "size": 24, "color": "#1F1F1F"},
    {"type": "rect", "x": 8, "y": 146, "w": 224, "h": 8, "fill": "#E1DFDD"},
    {"type": "rect", "x": 8, "y": 146, "w": 224, "h": 8, "fill": "#0078D4",
     "bind_w": "net.down", "min": 0, "max": 8000},
    {"type": "line", "x": 8, "y": 168, "w": 224, "h": 64, "bind": "net.down",
     "min": 0, "max": 8000, "points": 60, "color": "#0078D4"},
    {"type": "path", "x": 8, "y": 162, "pts": [0, 0, 224, 0], "color": "#E1DFDD"},
]

#: Focus navigation proof page (s.11.5): three selectable rows.
LAYOUT_KEYS = [
    {"type": "text", "x": 8, "y": 10, "text": "KEYS", "size": 16, "color": "#605E5C"},
    {"type": "text", "x": 8, "y": 34, "text": "UP/DOWN + ENTER", "size": 12,
     "color": "#605E5C"},
    {"type": "rect", "x": 8, "y": 64, "w": 224, "h": 34, "fill": "#E1DFDD",
     "focusable": True, "focus": 0},
    {"type": "text", "x": 16, "y": 74, "text": "BTN ONE", "size": 16, "color": "#1F1F1F"},
    {"type": "rect", "x": 8, "y": 108, "w": 224, "h": 34, "fill": "#E1DFDD",
     "focusable": True, "focus": 1},
    {"type": "text", "x": 16, "y": 118, "text": "BTN TWO", "size": 16, "color": "#1F1F1F"},
    {"type": "rect", "x": 8, "y": 152, "w": 224, "h": 34, "fill": "#E1DFDD",
     "focusable": True, "focus": 2},
    {"type": "text", "x": 16, "y": 162, "text": "BTN THREE", "size": 16,
     "color": "#1F1F1F"},
]

LAYOUTS = {
    0: LAYOUT_CPU,
    1: LAYOUT_MEMNET,
    2: LAYOUT_KEYS,
}


def example_hello(token_text):
    """A ready-to-paste hello line for the terminal-side smoke test."""
    msg = protocol.hello(
        EXAMPLE_CLIENT_ID,
        "thin",
        SCREEN_W,
        SCREEN_H,
        token=token_text,
        depth=16,
        charset="ascii",
        accept=["ui", "data"],
        limits={"max_nodes": 32, "max_line_points": 60,
                "max_text_len": 16, "max_json_bytes": 4096},
        primitives=["rect", "path", "text", "num"],
        widgets=[],
        fonts=[12, 16, 24, 32],
        input=["key"],
        features={"backlight": False, "rtc": False, "storage": False},
    )
    return protocol.encode(msg).decode("ascii").strip()


# ---------------------------------------------------------------------------
# Fake data source -- invents the four metrics, never reads the machine
# ---------------------------------------------------------------------------


class FakeSource:
    """Sine/sawtooth/spike generators producing the four catalog metrics."""

    def __init__(self, seed=None):
        self._rng = random.Random(seed)
        self._t0 = time.monotonic()
        self._noise = {m["id"]: 0.0 for m in METRICS}
        self._values = {m["id"]: None for m in METRICS}
        self.tick(self._t0)

    def tick(self, now):
        """Recompute values; called every TICK_INTERVAL_S (200ms jitter)."""
        t = max(0.0, now - self._t0)
        for key in self._noise:
            self._noise[key] = (NOISE_SMOOTH * self._noise[key]
                                + (1.0 - NOISE_SMOOTH) * self._rng.uniform(-1.0, 1.0))

        cpu = (CPU_BASE
               + CPU_AMP * math.sin(TAU * t / CPU_PERIOD_S)
               + CPU_NOISE * self._noise["cpu.total"])
        temp = (TEMP_BASE
                + TEMP_AMP * math.sin(TAU * t / TEMP_PERIOD_S)
                + TEMP_NOISE * self._noise["cpu.temp"])
        # Slow rise that snaps back: sawtooth, not a sine.
        cycle = (t % MEM_CYCLE_S) / MEM_CYCLE_S
        mem = (MEM_FLOOR + (MEM_CEIL - MEM_FLOOR) * cycle
               + MEM_NOISE * self._noise["mem.used"])
        if self._rng.random() < NET_SPIKE_P:
            net = self._rng.uniform(NET_SPIKE_MIN, NET_SPIKE_MAX)
        else:
            net = self._rng.uniform(0.0, NET_LOW_MAX)

        self._values = {
            "cpu.total": round(_clamp(cpu, 0.0, 100.0), 1),
            "cpu.temp": round(_clamp(temp, 0.0, 110.0), 1),
            "mem.used": round(_clamp(mem, 0.0, 100.0), 1),
            "net.down": round(_clamp(net, 0.0, 8000.0), 0),
        }

    def values(self):
        """Newest snapshot; the caller must not mutate the returned dict."""
        return dict(self._values)


def _clamp(value, low, high):
    if value < low:
        return low
    if value > high:
        return high
    return value


# ---------------------------------------------------------------------------
# Client -- one connection, one reader thread, one writer thread
# ---------------------------------------------------------------------------


class Client:
    """A connected terminal.

    Outbox shape (PROTOCOL.md s.16 red line 2): structural frames (catalog /
    scenes / ui / cmd / ...) wait in a short bounded deque, while the data
    frame lives in a single slot that the newest frame overwrites. Nothing is
    ever queued without bound, and no lock here is shared with other clients.
    """

    def __init__(self, daemon, sock, addr):
        self.daemon = daemon
        self.sock = sock
        self.addr = addr
        self.client_id = None
        self.hello = None
        self.role = None
        self.scene = DEFAULT_SCENE
        self.max_json = 0
        self.accept = set()
        self.seq = 0
        self.sent = 0
        self.skipped = 0
        self.errors = 0
        self.trim_warned = False
        self.stat_logged_at = float("-inf")
        self._cond = threading.Condition()
        self._struct = collections.deque()
        self._data = None
        self._stop = threading.Event()
        self._close_lock = threading.Lock()
        self._closed = False
        self._drain_then_close = False
        self._close_reason = ""
        self._reader = threading.Thread(target=self._read_loop, daemon=True,
                                        name="rx-" + self.label())
        self._writer = threading.Thread(target=self._write_loop, daemon=True,
                                        name="tx-" + self.label())

    # -- identity ---------------------------------------------------------

    def label(self):
        if self.client_id:
            return str(self.client_id)
        try:
            return "%s:%s" % (self.addr[0], self.addr[1])
        except (TypeError, IndexError):
            return "?"

    def adopt_hello(self, hello_msg):
        """Remember the (already normalized) hello; it decides what we send."""
        self.hello = hello_msg
        self.client_id = hello_msg.get("client_id")
        self.role = hello_msg.get("role")
        limits = hello_msg.get("limits")
        limits = limits if isinstance(limits, dict) else {}
        max_json = limits.get("max_json_bytes")
        if isinstance(max_json, (int, float)) and not isinstance(max_json, bool) and max_json > 0:
            self.max_json = int(max_json)
        else:
            self.max_json = 0
        accepted = hello_msg.get("accept")
        self.accept = {a for a in accepted if isinstance(a, str)} if isinstance(accepted, list) else set()

    def accepts(self, what):
        return what in self.accept

    # -- lifecycle --------------------------------------------------------

    def start(self):
        self._reader.start()
        self._writer.start()

    def request_close(self, reason=""):
        """Send what is already queued (e.g. a deny), then hang up.

        The protocol requires {"t":"deny"} to reach the terminal *before* the
        socket dies (s.2), so dropping the outbox here would be wrong.
        """
        with self._cond:
            self._drain_then_close = True
            self._close_reason = reason
            self._cond.notify_all()

    def _drained_and_closing(self):
        with self._cond:
            return (self._drain_then_close
                    and not self._struct
                    and self._data is None)

    def close(self, reason=""):
        """Idempotent teardown; safe from any thread."""
        with self._close_lock:
            if self._closed:
                return
            self._closed = True
        self._stop.set()
        if not reason:
            reason = self._close_reason
        with self._cond:
            self._struct.clear()
            self._data = None
            self._cond.notify_all()
        try:
            self.sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        try:
            self.sock.close()
        except OSError:
            pass
        self.daemon.forget(self)
        self.daemon.log("closed %s (%s) sent=%d skipped=%d errors=%d"
                        % (self.label(), reason or "done",
                           self.sent, self.skipped, self.errors))

    # -- outbox -----------------------------------------------------------

    def push(self, msg, is_data=False):
        """Hand a message to the writer. Data frames collapse into one slot."""
        if self._stop.is_set():
            return
        with self._cond:
            if is_data:
                if self._data is not None:
                    self.skipped += 1
                self._data = msg
            else:
                if len(self._struct) >= MAX_STRUCT_BACKLOG:
                    self._struct.popleft()
                    self.skipped += 1
                self._struct.append(msg)
            self._cond.notify_all()

    def _take_frames(self):
        """Block until something is queued. Returns None once we are stopping."""
        with self._cond:
            while not self._stop.is_set() and not self._struct and self._data is None:
                self._cond.wait(DATA_INTERVAL_S)
            if self._stop.is_set():
                return None
            frames = list(self._struct)
            self._struct.clear()
            if self._data is not None:
                frames.append(self._data)
                self._data = None
            return frames

    # -- writer -----------------------------------------------------------

    def _write_loop(self):
        try:
            while not self._stop.is_set():
                frames = self._take_frames()
                if frames is None:
                    return
                for msg in frames:
                    if self._stop.is_set():
                        return
                    if not self._send_frame(msg):
                        self.close("send failed")
                        return
                if self._drained_and_closing():
                    return
        except Exception as exc:  # noqa: BLE001 - never kill the daemon
            self.errors += 1
            self.daemon.log("writer %s crashed: %s: %s"
                            % (self.label(), type(exc).__name__, exc))
        finally:
            self.close()

    def _send_frame(self, msg):
        """Send one frame. Returns False when the connection must die.

        A send timeout is fatal on purpose: sendall() may already have pushed
        part of the line, and a half-written frame would desynchronize the
        NDJSON stream for this terminal forever. Terminal-side reconnect with
        backoff (s.13.5) handles it.
        """
        frame = dict(msg)
        if frame.get("t") in SEQ_KINDS:
            self.seq += 1
            frame["s"] = self.seq
        try:
            payload = protocol.encode(frame)
        except Exception as exc:  # noqa: BLE001 - a bad frame is not fatal
            self.errors += 1
            self.daemon.log("encode failed for %s: %s: %s"
                            % (self.label(), type(exc).__name__, exc))
            return True
        try:
            self.sock.settimeout(SEND_TIMEOUT_S)
            self.sock.sendall(payload)
        except OSError as exc:
            self.errors += 1
            self.daemon.log("send to %s failed after %dms: %s"
                            % (self.label(), int(SEND_TIMEOUT_S * 1000), exc))
            return False
        self.sent += 1
        self.daemon.record(payload)
        return True

    # -- reader -----------------------------------------------------------

    def _read_loop(self):
        buf = b""
        try:
            while not self._stop.is_set():
                try:
                    ready, _, _ = select.select([self.sock], [], [], RECV_TIMEOUT_S)
                except (OSError, ValueError):
                    break
                if not ready:
                    continue
                try:
                    chunk = self.sock.recv(RECV_CHUNK_BYTES)
                except TimeoutError:
                    continue
                except OSError:
                    break
                if not chunk:
                    self.close("peer closed")
                    return
                buf += chunk
                if len(buf) > MAX_LINE_BYTES:
                    self.errors += 1
                    self.daemon.log("%s sent %d bytes without a newline; dropping it"
                                    % (self.label(), len(buf)))
                    self.close("oversized line")
                    return
                while True:
                    cut = buf.find(b"\n")
                    if cut < 0:
                        break
                    line = buf[:cut]
                    buf = buf[cut + 1:]
                    self.daemon.handle_line(self, line)
        except Exception as exc:  # noqa: BLE001 - never kill the daemon
            self.errors += 1
            self.daemon.log("reader %s crashed: %s: %s"
                            % (self.label(), type(exc).__name__, exc))
        finally:
            self.close()


# ---------------------------------------------------------------------------
# Daemon -- accept loop, registry, ticker, recording
# ---------------------------------------------------------------------------


class Daemon:
    """Owns the listening socket, the client registry and the fake ticker."""

    def __init__(self, host=BIND_HOST, port=DEFAULT_PORT, token=None,
                 record_path=None, lenient=False, seed=None):
        self.host = host
        self.port = port
        self.token = token or secrets.token_hex(TOKEN_BYTES)
        self.lenient = lenient
        self._record_path = record_path
        self._rec = None
        self._rec_lock = threading.Lock()
        self._clients = {}
        self._lock = threading.Lock()
        self._log_lock = threading.Lock()
        self._close_lock = threading.Lock()
        self._shutting_down = False
        self._stop = threading.Event()
        self._srv = None
        self._source = FakeSource(seed)
        self.stats = collections.Counter()

    # -- logging / recording ---------------------------------------------

    def log(self, text):
        with self._log_lock:
            sys.stdout.write("[" + time.strftime("%H:%M:%S") + "] " + text + "\n")
            sys.stdout.flush()

    def log_raw(self, text):
        with self._log_lock:
            sys.stdout.write(text + "\n")
            sys.stdout.flush()

    def open_recording(self):
        if not self._record_path:
            return
        path = Path(self._record_path)
        if path.parent and str(path.parent) not in ("", "."):
            path.parent.mkdir(parents=True, exist_ok=True)
        self._rec = open(path, "ab")
        self.log("recording outbound NDJSON to " + str(path))

    def record(self, payload):
        """Append one outbound frame verbatim (replay food for M4)."""
        if self._rec is None:
            return
        try:
            with self._rec_lock:
                if self._rec is None:
                    return
                self._rec.write(payload)
                self._rec.flush()
        except OSError as exc:
            self.stats["record_errors"] += 1
            self.log("record failed, recording disabled: " + str(exc))
            with self._rec_lock:
                try:
                    if self._rec is not None:
                        self._rec.close()
                except OSError:
                    pass
                self._rec = None

    # -- registry ---------------------------------------------------------

    def snapshot(self):
        with self._lock:
            return list(self._clients.values())

    def forget(self, client):
        with self._lock:
            if client.client_id and self._clients.get(client.client_id) is client:
                del self._clients[client.client_id]

    def register(self, client):
        """Register a client_id; kick the previous connection (s.16 red line 5)."""
        replaced = None
        with self._lock:
            old = self._clients.get(client.client_id)
            self._clients[client.client_id] = client
            if old is not None and old is not client:
                replaced = old
        if replaced is not None:
            self.log("client_id %s reconnected: kicking the old connection"
                     % client.client_id)
            self.stats["kicks"] += 1
            replaced.close("replaced by newer connection")

    # -- outbound ---------------------------------------------------------

    def _prepare(self, msg, client):
        """Trim a frame until it fits limits.max_json_bytes (s.5)."""
        limit = client.max_json
        if limit <= 0:
            return msg
        budget = limit - SEQ_SLACK_BYTES
        if budget <= 0:
            budget = limit
        if self._fits(msg, budget):
            return msg
        kind = msg.get("t")
        try:
            if kind == "ui":
                msg = self._trim_ui(msg, budget)
            elif kind == "data":
                msg = self._trim_data(msg, budget)
        except Exception as exc:  # noqa: BLE001
            self.stats["errors"] += 1
            self.log("trim failed: %s: %s" % (type(exc).__name__, exc))
        if not self._fits(msg, budget) and not client.trim_warned:
            client.trim_warned = True
            self.log("%s: %s still too big for max_json_bytes=%d after trimming"
                     % (client.label(), kind, limit))
        return msg

    @staticmethod
    def _fits(msg, budget):
        try:
            return len(protocol.encode(msg)) <= budget
        except Exception:  # noqa: BLE001
            return True

    def _trim_ui(self, msg, budget):
        nodes = msg.get("nodes")
        if not isinstance(nodes, list):
            return msg
        dropped = 0
        while nodes and not self._fits(msg, budget):
            nodes.pop()
            dropped += 1
        if dropped:
            self.stats["ui_nodes_trimmed"] += dropped
        return msg

    def _trim_data(self, msg, budget):
        values = msg.get("v")
        if not isinstance(values, dict):
            return msg
        copy_msg = dict(msg)
        copy_msg["v"] = dict(values)
        keys = list(values.keys())
        dropped = 0
        while copy_msg["v"] and keys and not self._fits(copy_msg, budget):
            copy_msg["v"].pop(keys.pop(), None)
            dropped += 1
        if dropped:
            self.stats["data_keys_trimmed"] += dropped
        return copy_msg

    def send_catalog(self, client):
        client.push(protocol.catalog(CATALOG_VER, METRICS))

    def send_scenes(self, client):
        client.push({"t": "scenes",
                     "list": [{"i": i, "title": t} for i, t in enumerate(SCENE_TITLES)]})

    def send_scene(self, client, scene):
        """Push one page, trimmed to the client's capabilities (s.5, s.16.3)."""
        if not client.accepts("ui"):
            return
        layout = LAYOUTS.get(scene)
        if layout is None:
            return
        nodes = protocol.fit_nodes(layout, client.hello or {})
        msg = protocol.ui(scene, UI_VER, nodes, bg=BG_COLOR)
        client.push(self._prepare(msg, client))

    def send_data(self, client):
        if not client.accepts("data"):
            return
        msg = protocol.data(self._source.values())
        msg["ts"] = int(time.time())
        client.push(self._prepare(msg, client), is_data=True)

    def broadcast_data(self):
        values = self._source.values()
        self.stats["data_frames"] += 1
        for client in self.snapshot():
            if not client.accepts("data"):
                continue
            msg = protocol.data(values)
            msg["ts"] = int(time.time())
            client.push(self._prepare(msg, client), is_data=True)

    # -- inbound ----------------------------------------------------------

    def handle_line(self, client, line):
        msg = protocol.decode(line)
        if msg is None:
            client.errors += 1
            self.stats["bad_lines"] += 1
            self.log("%s: unparseable line, ignored (%r)" % (client.label(), line[:80]))
            return
        try:
            self.handle_message(client, msg)
        except Exception as exc:  # noqa: BLE001 - one bad message, not one dead daemon
            client.errors += 1
            self.stats["errors"] += 1
            self.log("%s: handler error: %s: %s"
                     % (client.label(), type(exc).__name__, exc))

    def handle_message(self, client, msg):
        kind = protocol.kind(msg)
        if kind is None:
            # Unknown "t": ignore silently (s.20 forward compatibility).
            self.stats["unknown_msgs"] += 1
            return
        if kind == "hello":
            self.handle_hello(client, msg)
        elif kind == "ping":
            client.push({"t": "pong"})
        elif kind == "event":
            self.handle_event(client, msg)
        elif kind == "stat":
            self.handle_stat(client, msg)
        else:
            # discover / here are transport-level; the rest is ours to send.
            self.stats["ignored_msgs"] += 1

    def handle_hello(self, client, msg):
        reason = self._check_hello(msg)
        if reason is not None:
            self.stats["denied"] += 1
            self.log("%s: hello refused (%s)" % (client.label(), reason))
            client.push({"t": "deny", "reason": reason})
            client.request_close("hello refused: " + reason)
            return
        hello_msg = self._normalize_hello(msg)
        client.adopt_hello(hello_msg)
        self.register(client)
        self.stats["hellos"] += 1
        display = hello_msg.get("display") or {}
        self.log("hello from %s role=%s %sx%s charset=%s prims=%s fonts=%s"
                 % (client.client_id, hello_msg.get("role"),
                    display.get("w"), display.get("h"),
                    hello_msg.get("charset"),
                    ",".join(str(p) for p in hello_msg.get("primitives") or []),
                    ",".join(str(f) for f in hello_msg.get("fonts") or [])))
        # s.16 red line 4: resend everything, assume no cache is valid.
        client.scene = DEFAULT_SCENE
        self.send_catalog(client)
        self.send_scenes(client)
        self.send_scene(client, client.scene)
        self.send_data(client)

    def _check_hello(self, msg):
        """Return None when acceptable, else a deny reason string."""
        token = msg.get("token")
        if not self.lenient:
            if not isinstance(token, str) or token != self.token:
                return "bad token"
        elif isinstance(token, str) and token != self.token:
            self.stats["lenient_tokens"] += 1
            self.log("LENIENT mode: trusting unknown token (development only)")
        client_id = msg.get("client_id")
        if not isinstance(client_id, str) or not client_id:
            return "bad client_id"
        if msg.get("role") not in protocol.ROLES:
            return "bad role"
        proto = msg.get("proto")
        if proto is None:
            self.log("hello without proto; assuming %d" % protocol.PROTO_VERSION)
        elif not isinstance(proto, (int, float)) or isinstance(proto, bool):
            return "bad proto"
        elif int(proto) != protocol.PROTO_VERSION:
            self.log("proto mismatch: terminal says %r, daemon speaks %r"
                     % (proto, protocol.PROTO_VERSION))
            return "proto mismatch"
        return None

    def _normalize_hello(self, msg):
        """Fill defaults so downstream code always sees a complete hello."""
        display = msg.get("display")
        display = display if isinstance(display, dict) else {}
        width = display.get("w")
        height = display.get("h")
        width = int(width) if _is_number(width) and width > 0 else SCREEN_W
        height = int(height) if _is_number(height) and height > 0 else SCREEN_H
        depth = display.get("depth")
        depth = int(depth) if _is_number(depth) and depth > 0 else 16

        limits = dict(protocol.DEFAULT_LIMITS)
        given_limits = msg.get("limits")
        if isinstance(given_limits, dict):
            for key, value in given_limits.items():
                if _is_number(value):
                    limits[key] = int(value)

        fonts = [f for f in _as_list(msg.get("fonts")) if _is_number(f) and f > 0]
        primitives = [p for p in _as_list(msg.get("primitives")) if isinstance(p, str)]
        widgets = [w for w in _as_list(msg.get("widgets")) if isinstance(w, str)]
        accept = [a for a in _as_list(msg.get("accept")) if isinstance(a, str)]
        inputs = [i for i in _as_list(msg.get("input")) if isinstance(i, str)]
        features = msg.get("features")
        features = features if isinstance(features, dict) else {}

        normalized = protocol.hello(
            msg.get("client_id"),
            msg.get("role"),
            width,
            height,
            proto=protocol.PROTO_VERSION,
            token=msg.get("token") or "",
            depth=depth,
            charset=msg.get("charset") if isinstance(msg.get("charset"), str) else "ascii",
            accept=accept if isinstance(msg.get("accept"), list) else None,
            limits=limits,
            primitives=primitives if primitives else None,
            widgets=widgets,
            fonts=fonts if fonts else None,
            input=inputs if inputs else None,
            features=features,
        )
        for extra in ("fw", "model", "proto"):
            if extra in msg and extra not in normalized:
                normalized[extra] = msg[extra]
        return normalized

    def handle_stat(self, client, msg):
        """Terminal health report (s.15): count it, log it at most every 5s."""
        self.stats["stats"] += 1
        now = time.monotonic()
        if now - client.stat_logged_at < STAT_LOG_INTERVAL_S:
            return
        client.stat_logged_at = now
        self.log("%s stat rssi=%s heap=%s rx=%s drop=%s uptime=%s"
                 % (client.label(), msg.get("rssi"), msg.get("heap"),
                    msg.get("rx"), msg.get("drop"), msg.get("uptime")))

    def handle_event(self, client, msg):
        key = msg.get("k")
        if key == "page":
            self.handle_page_event(client, msg.get("want"))
        elif key == "key":
            ident = msg.get("id")
            if ident == "focus":
                value = msg.get("v")
                if _is_number(value):
                    self.stats["focus_events"] += 1
                    self.log_raw("FOCUS " + str(int(value)))
                else:
                    self.log("%s: focus event without numeric v, ignored" % client.label())
            else:
                # A key we have no mapping for: report only, never guess (s.11).
                self.log("%s: key event id=%r v=%r (no action configured)"
                         % (client.label(), ident, msg.get("v")))
        elif key == "tap":
            self.log("%s: tap event id=%r (no action configured)"
                     % (client.label(), msg.get("id")))
        elif key == "state":
            self.log("%s: state event id=%r v=%r"
                     % (client.label(), msg.get("id"), msg.get("v")))
        else:
            self.stats["unknown_events"] += 1
            self.log("%s: unknown event k=%r, ignored" % (client.label(), key))

    def handle_page_event(self, client, want):
        """Page request: "next", one index, or a preload list (s.11)."""
        if want == "next":
            client.scene = (client.scene + 1) % len(SCENE_TITLES)
            self._show_scene(client, client.scene)
            return
        if _is_number(want):
            scene = int(want)
            if scene in LAYOUTS:
                client.scene = scene
                self._show_scene(client, scene)
            else:
                self.log("%s asked for scene %d, ignored" % (client.label(), scene))
            return
        if isinstance(want, list):
            # Swipe preload: answer with one ui message per scene (s.16.6).
            asked = [int(w) for w in want if _is_number(w) and int(w) in LAYOUTS]
            if not asked:
                self.log("%s asked for scenes %r, ignored" % (client.label(), want))
                return
            for scene in asked:
                self.send_scene(client, scene)
            return
        self.log("%s: page event with unusable want=%r" % (client.label(), want))

    def _show_scene(self, client, scene):
        self.log("%s -> scene %d (%s)" % (client.label(), scene, SCENE_TITLES[scene]))
        self.send_scene(client, scene)
        if client.accepts("ui"):
            client.push(protocol.cmd("show", target=client.client_id, scene=scene))

    # -- main loop --------------------------------------------------------

    def _tick_loop(self):
        next_data = time.monotonic() + DATA_INTERVAL_S
        try:
            while not self._stop.is_set():
                now = time.monotonic()
                self._source.tick(now)
                if now >= next_data:
                    next_data = now + DATA_INTERVAL_S
                    try:
                        self.broadcast_data()
                    except Exception as exc:  # noqa: BLE001
                        self.stats["errors"] += 1
                        self.log("broadcast failed: %s: %s" % (type(exc).__name__, exc))
                self._stop.wait(TICK_INTERVAL_S)
        except Exception as exc:  # noqa: BLE001
            self.stats["errors"] += 1
            self.log("ticker crashed: %s: %s" % (type(exc).__name__, exc))

    def serve_forever(self):
        """Listen and accept forever. Returns False if the port is unusable."""
        self.open_recording()
        srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        try:
            if os.name != "nt":
                # POSIX needs this to rebind a port left in TIME_WAIT. On
                # Windows SO_REUSEADDR would let a second daemon silently
                # hijack the port of the one already running, so skip it.
                srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            srv.bind((self.host, self.port))
            srv.listen(SOCKET_BACKLOG)
            srv.settimeout(ACCEPT_TIMEOUT_S)
        except OSError as exc:
            self.log("cannot listen on %s:%d: %s" % (self.host, self.port, exc))
            try:
                srv.close()
            except OSError:
                pass
            self.shutdown()
            return False
        self._srv = srv
        self.log("listening on %s:%d (TCP, NDJSON)" % (self.host, self.port))
        ticker = threading.Thread(target=self._tick_loop, daemon=True, name="ticker")
        ticker.start()
        try:
            while not self._stop.is_set():
                try:
                    conn, addr = srv.accept()
                except TimeoutError:
                    continue
                except OSError as exc:
                    if self._stop.is_set():
                        break
                    self.stats["accept_errors"] += 1
                    self.log("accept failed: " + str(exc))
                    continue
                self._spawn(conn, addr)
        finally:
            ticker.join(timeout=1.0)
            self.shutdown()
            srv.close()
        return True

    def _spawn(self, conn, addr):
        try:
            conn.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        except OSError:
            pass
        try:
            conn.settimeout(SEND_TIMEOUT_S)
        except OSError:
            pass
        client = Client(self, conn, addr)
        self.stats["connections"] += 1
        self.log("connection from %s:%s" % (addr[0], addr[1]))
        try:
            client.start()
        except RuntimeError as exc:
            self.stats["errors"] += 1
            self.log("could not start threads for %s: %s" % (client.label(), exc))
            client.close("thread start failed")

    def shutdown(self):
        """Idempotent teardown: stop the ticker, drop clients, close recording."""
        with self._close_lock:
            if self._shutting_down:
                return
            self._shutting_down = True
        self._stop.set()
        if self._srv is not None:
            try:
                self._srv.close()
            except OSError:
                pass
        for client in self.snapshot():
            client.close("daemon shutting down")
        with self._rec_lock:
            if self._rec is not None:
                try:
                    self._rec.flush()
                    self._rec.close()
                except OSError:
                    pass
                self._rec = None
        self.log("stats: " + ", ".join("%s=%d" % (k, v) for k, v in sorted(self.stats.items())))

    def banner(self):
        lines = [
            "PC-Monitor fake daemon (PROTOCOL.md v0.4, fake data only)",
            "  listen   : %s:%d (TCP, NDJSON)" % (self.host, self.port),
            "  token    : " + self.token,
            "  metrics  : " + ", ".join(m["id"] for m in METRICS),
            "  scenes   : " + ", ".join("%d=%s" % (i, t) for i, t in enumerate(SCENE_TITLES)),
            "  cadence  : data %g/s, value jitter %g/s, send timeout %dms"
            % (1.0 / DATA_INTERVAL_S, 1.0 / TICK_INTERVAL_S, int(SEND_TIMEOUT_S * 1000)),
            "  record   : " + (str(self._record_path) if self._record_path else "(disabled)"),
            "  token check: " + ("LENIENT (dev only)" if self.lenient else "strict"),
            "  hello    : " + example_hello(self.token),
        ]
        for line in lines:
            self.log(line)


# ---------------------------------------------------------------------------
# Small helpers
# ---------------------------------------------------------------------------


def _is_number(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def _as_list(value):
    if value is None:
        return []
    if isinstance(value, (list, tuple)):
        return list(value)
    return [value]


def parse_args(argv=None):
    parser = argparse.ArgumentParser(
        description="PC-Monitor fake data daemon (M2): invented metrics over "
                    "TCP/NDJSON for terminal development.")
    parser.add_argument("--host", default=BIND_HOST,
                       help="bind address (default %s; use a LAN address, not 0.0.0.0, "
                            "when you are not on a trusted network)" % BIND_HOST)
    parser.add_argument("--port", type=int, default=DEFAULT_PORT,
                       help="TCP port (default %d)" % DEFAULT_PORT)
    parser.add_argument("--record", metavar="FILE", default=None,
                       help="append every outbound NDJSON frame verbatim to FILE "
                            "(food for the M4 replay tool)")
    parser.add_argument("--token", default=None,
                       help="fixed auth token; a random one is generated and printed otherwise")
    parser.add_argument("--lenient", action="store_true",
                       help="accept unknown tokens (development pairing mode, PROTOCOL.md s.2)")
    parser.add_argument("--fake", action="store_true",
                       help="fake data (this is the only mode; flag accepted for symmetry)")
    parser.add_argument("--seed", type=int, default=None,
                       help="seed the fake value generator for repeatable numbers")
    return parser.parse_args(argv)


def main(argv=None):
    args = parse_args(argv)
    daemon = Daemon(host=args.host, port=args.port, token=args.token,
                    record_path=args.record, lenient=args.lenient, seed=args.seed)
    daemon.banner()
    started = False
    try:
        started = daemon.serve_forever()
    except KeyboardInterrupt:
        daemon.log("Ctrl-C, shutting down")
        started = True
    finally:
        daemon.shutdown()
    return 0 if started else 1


if __name__ == "__main__":
    sys.exit(main())
