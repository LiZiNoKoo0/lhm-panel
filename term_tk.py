"""term_tk -- desktop simulator terminal for PC-Monitor (milestone M3).

    python term_tk.py [--host 127.0.0.1] [--port 7802] [--token TOKEN]
    python term_tk.py --selftest            # spawn a fake daemon, prove it works
    python term_tk.py --selftest --no-spawn # test against a daemon already up

It is a THIN terminal (PROTOCOL.md section 3): it connects to the PC daemon over
TCP/NDJSON, says hello with its capabilities, and then draws whatever "ui"
structure the daemon sends it with the four primitives from section 7
(rect / path / text / num) plus a focus highlight, an "N ms ago" freshness
label and a "LINK LOST" badge (section 13.5). All protocol bytes go through
pcmon.protocol.

What this file deliberately does NOT do:
  * parse metric semantics -- it knows "a num node is bound to this id", never
    that cpu.total means CPU (README rule 2). No unit conversion, no ranges
    beyond the optional min/max scale hint of a bind_w rect
  * author any layout -- every pixel comes from the last "ui" message; if the
    daemon sends nothing, this window is empty on purpose (M3 forbids hardcoded
    layouts)
  * send commands -- terminals only report events (section 5, rule 5), so
    cmd -> ui coupling is done by asking for a page, never by acting locally
  * keep value history, ring buffers or sparkline state (section 8: that is a
    device-side widget concern and this terminal does not declare "line")
  * answer UDP "discover", speak WebSocket/serial, or serve the admin UI
  * persist the UI to disk (section 13 is firmware flash business)
  * retry a "deny" forever -- a bad token is a configuration problem, so the
    link stops instead of hammering the daemon
  * run an animation loop beyond what the data frames and the freshness badge
    need (a 250ms badge tick; no timers drive drawing otherwise)
  * let networking block the UI: socket I/O lives in one background thread and
    reaches Tk only through a queue polled with after()

Visual style: flat Windows 10 light. No gradients, no rounded corners, no
shadows, no images, no emoji, no custom title bar (see the style constants).

Source is pure ASCII on purpose (TASKS.md coding rule 1: PowerShell 5.1 on
this host mangles GBK/UTF-8 console output). Python 3.12 stdlib only.
"""

import argparse
import collections
import os
import queue
import re
import select
import socket
import subprocess
import sys
import tempfile
import threading
import time
import tkinter as tk
from tkinter import font as tkfont

from pcmon import protocol

# ---------------------------------------------------------------------------
# Constants -- every magic number lives here (TASKS.md coding rule 4)
# ---------------------------------------------------------------------------

BASE_DIR = os.path.dirname(os.path.abspath(__file__))
DAEMON_SCRIPT = os.path.join(BASE_DIR, "fake_daemon.py")
TOKEN_FILE = os.path.join(BASE_DIR, "daemon.token")

DEFAULT_HOST = protocol.DEFAULT_DAEMON_HOST          # 127.0.0.1
DEFAULT_PORT = protocol.DEFAULT_PORT                 # 7802
DEFAULT_TOKEN = ""                                   # daemon token, see --token
CLIENT_ID = "term-tk"

#: What this terminal tells the daemon it can do (TASKS.md M3 hello).
DISPLAY_W = 240
DISPLAY_H = 320
DISPLAY_DEPTH = 16
CHARSET = "utf8"
MAX_NODES = 48
MAX_JSON_BYTES = 65536
PRIMITIVES = ["rect", "path", "text", "num"]
WIDGETS = []                                         # no bar/line/dot: v0.3 set
FONTS = [12, 16, 24, 32]
INPUTS = ["key"]
FW_TAG = "term-tk/1.0"

#: Transport timings.
CONNECT_TIMEOUT_S = 2.0
SOCK_POLL_S = 0.2
RECV_CHUNK_BYTES = 4096
MAX_LINE_BYTES = 65536
PING_INTERVAL_S = 10.0                               # heartbeat, section 1
BACKOFF_S = protocol.RECONNECT_BACKOFF_S             # 1,2,4,8,10 (section 1)

#: UI timings.
UI_POLL_MS = 50                                      # queue -> Tk drain rate
STATUS_TICK_MS = 250                                 # freshness badge refresh
LINK_LOST_S = protocol.HEARTBEAT_TIMEOUT_S           # 15s, section 13.5

#: Visual style -- Windows 10 light, flat, nothing decorative.
BG_COLOR = "#F7F7F7"
TEXT_COLOR = "#1F1F1F"
DIM_COLOR = "#605E5C"
ACCENT_COLOR = "#0078D4"
BORDER_COLOR = "#E1DFDD"
ALERT_COLOR = "#C42B1C"
FONT_FAMILY_PREFERRED = "Segoe UI"
OVERLAY_PX = 10
DEFAULT_FONT_PX = 12
FOCUS_BOX_PX = 2
FOCUS_PAD = 2

#: Node drawing defaults (used only when a node omits the field).
DEFAULT_RECT_WIDTH = 1
DEFAULT_BIND_MIN = 0.0
DEFAULT_BIND_MAX = 100.0
MAX_DECIMALS = 6
UNIT_SPACED_MIN_LEN = 3                              # "KB/s" gets a space, "%" not
TEXT_ALIGN_ANCHORS = {"left": "nw", "center": "n", "right": "ne"}
DEFAULT_ALIGN = "left"

#: Selftest knobs.
SELFTEST_TMP_DIR = os.path.join(BASE_DIR, "tmp")     # scratch dir, deleted again
SPAWN_WAIT_S = 6.0
SELFTEST_UI_WAIT_S = 5.0
SELFTEST_SETTLE_S = 0.4
SELFTEST_TOTAL_S = 4.0
#: The selftest daemon and this terminal share this key (PROTOCOL.md s.2.4).
#: Fixed on purpose: the selftest must be repeatable, and the key never leaves
#: this machine -- a real pairing mints a random one.
SELFTEST_KEY = "5e1f7c3a9b2d48e6a0c4f8b1d3e50729"
SELFTEST_DEVICES_NAME = "term_tk_selftest_devices.json"

_HEX_COLOR = re.compile(r"^#(?:[0-9A-Fa-f]{3}|[0-9A-Fa-f]{6}|[0-9A-Fa-f]{8})$")


def _is_number(value):
    """True for int/float but not bool (bool is an int subclass)."""
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def _num(value, default):
    return float(value) if _is_number(value) else default


def _color(value):
    """Return a usable "#RRGGBB" string or None (unknown colors are ignored)."""
    if isinstance(value, str):
        text = value.strip()
        if _HEX_COLOR.match(text):
            return text
    return None


def _format_value(value, dec, unit):
    """Format one bound value. None means "no data" -> "--" (README rule 3)."""
    if value is None:
        return "--"
    if isinstance(value, bool):
        value = 1 if value else 0
    if isinstance(value, str):
        return value                                # string metric: as-is
    if isinstance(value, (int, float)):
        digits = int(dec) if _is_number(dec) else 0
        digits = max(0, min(digits, MAX_DECIMALS))
        text = ("%." + str(digits) + "f") % float(value)
    else:
        return "--"
    if isinstance(unit, str) and unit:
        sep = " " if len(unit) >= UNIT_SPACED_MIN_LEN else ""
        text = text + sep + unit
    return text


def _bind_ratio(value, node):
    """Map a bound value onto 0..1 for a bind_w rect (optional min/max hint)."""
    if not _is_number(value):
        return 0.0
    low = _num(node.get("min"), DEFAULT_BIND_MIN)
    high = _num(node.get("max"), DEFAULT_BIND_MAX)
    if high == low:
        return 0.0
    ratio = (float(value) - low) / (high - low)
    return 0.0 if ratio < 0.0 else (1.0 if ratio > 1.0 else ratio)


# ---------------------------------------------------------------------------
# Net -- one background thread; the UI only ever reads a queue
# ---------------------------------------------------------------------------


class Net(threading.Thread):
    """Socket owner: connect, hello, read NDJSON, reconnect with backoff.

    Everything that can block lives here so the Tk event loop never stalls.
    Outbound frames are queued (never sent from the UI thread) and flushed by
    this thread between reads.
    """

    def __init__(self, events, host=DEFAULT_HOST, port=DEFAULT_PORT,
                 token=DEFAULT_TOKEN, client_id=CLIENT_ID):
        super().__init__(daemon=True, name="term-net")
        self.events = events
        self.host = host
        self.port = int(port)
        self.token = token
        self.client_id = client_id
        #: Do NOT call this `_stop`: Thread._stop() is internal, and
        #: overriding it breaks is_alive()/join() once the thread ends.
        self._halt = threading.Event()
        self._sock = None
        self._pending = collections.deque()
        self._pending_lock = threading.Lock()
        self._denied = False
        self.frames_sent = 0
        self.frames_received = 0
        self.errors = 0

    # -- public API (called from the Tk thread) ---------------------------

    def send(self, msg):
        """Queue one frame. Returns False if encoding failed or we are closed."""
        try:
            payload = protocol.encode(msg)
        except Exception:  # noqa: BLE001 - a bad frame must not kill the link
            self.errors += 1
            return False
        with self._pending_lock:
            self._pending.append(payload)
        return True

    def stop(self):
        """Ask the thread to finish and tear the socket down."""
        self._halt.set()
        self._kill_socket()

    # -- internals --------------------------------------------------------

    def _emit(self, event, **fields):
        fields["event"] = event
        try:
            self.events.put(fields)
        except Exception:  # noqa: BLE001 - a full queue is not fatal
            self.errors += 1

    def _kill_socket(self):
        sock, self._sock = self._sock, None
        if sock is None:
            return
        try:
            sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        try:
            sock.close()
        except OSError:
            pass

    def _hello(self):
        return protocol.hello(
            self.client_id,
            "thin",
            DISPLAY_W,
            DISPLAY_H,
            auth=list(protocol.AUTH_ALGS),
            depth=DISPLAY_DEPTH,
            charset=CHARSET,
            accept=["ui", "data"],
            limits={"max_nodes": MAX_NODES, "max_json_bytes": MAX_JSON_BYTES},
            primitives=PRIMITIVES,
            widgets=WIDGETS,
            fonts=FONTS,
            input=INPUTS,
            fw=FW_TAG,
        )

    def run(self):
        attempt = 0
        while not self._halt.is_set():
            if self._connect():
                attempt = 0
                reason = self._read_loop()
                if self._halt.is_set() or self._denied:
                    break
                if not reason:
                    reason = "connection closed"
            else:
                reason = "connect failed"
            delay = BACKOFF_S[min(attempt, len(BACKOFF_S) - 1)]
            attempt += 1
            self._emit("link", up=False, reason=reason, delay=delay,
                       host=self.host, port=self.port)
            if self._halt.wait(delay):
                break
        self._kill_socket()

    def _connect(self):
        self._emit("link", up=False, reason="connecting", delay=0,
                   host=self.host, port=self.port)
        try:
            sock = socket.create_connection((self.host, self.port),
                                            timeout=CONNECT_TIMEOUT_S)
        except OSError as exc:
            self.errors += 1
            self._emit("link", up=False, reason=str(exc), delay=0,
                       host=self.host, port=self.port)
            return False
        try:
            sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        except OSError:
            pass
        try:
            sock.settimeout(SOCK_POLL_S)
        except OSError:
            pass
        self._sock = sock
        with self._pending_lock:
            self._pending.clear()
        if not self.send(self._hello()):
            self._kill_socket()
            return False
        if not self._flush(sock):
            self._kill_socket()
            return False
        self._emit("link", up=True, reason="connected", delay=0,
                   host=self.host, port=self.port)
        return True

    def _flush(self, sock):
        """Send every queued frame. Returns False when the socket is dead."""
        while True:
            with self._pending_lock:
                if not self._pending:
                    return True
                payload = self._pending.popleft()
            try:
                sock.sendall(payload)
            except OSError as exc:
                self.errors += 1
                self._emit("link", up=False, reason="send failed: %s" % exc,
                           delay=0, host=self.host, port=self.port)
                return False
            self.frames_sent += 1

    def _read_loop(self):
        """Read until the link dies. Returns a human reason string."""
        sock = self._sock
        buf = b""
        last_ping = time.monotonic()
        while not self._halt.is_set():
            if time.monotonic() - last_ping >= PING_INTERVAL_S:
                last_ping = time.monotonic()
                self.send({"t": "ping"})     # keeps a half-dead link detectable
            if not self._flush(sock):
                return "send failed"
            try:
                ready, _, _ = select.select([sock], [], [], SOCK_POLL_S)
            except (OSError, ValueError):
                return "select failed"
            if not ready:
                continue
            try:
                chunk = sock.recv(RECV_CHUNK_BYTES)
            except TimeoutError:
                continue
            except OSError as exc:
                return "recv failed: %s" % exc
            if not chunk:
                return "peer closed"
            self.frames_received += 1
            buf += chunk
            if len(buf) > MAX_LINE_BYTES:
                return "oversized line"
            while True:
                cut = buf.find(b"\n")
                if cut < 0:
                    break
                line, buf = buf[:cut], buf[cut + 1:]
                self._handle_line(line)
                if self._denied:
                    return "denied"
        return "stopped"

    def _handle_line(self, line):
        msg = protocol.decode(line)
        if msg is None:
            self.errors += 1
            self._emit("bad_line")
            return
        kind = protocol.kind(msg)
        if kind is None:
            self._emit("unknown")           # section 20: ignore unknown "t"
            return
        if kind == "challenge":
            self._answer_challenge(msg)
            return
        if kind == "deny":
            # A bad token is a configuration problem: stop, do not hammer.
            self._denied = True
            reason = msg.get("reason")
            self._emit("deny", reason=reason if isinstance(reason, str) else "")
            return
        self._emit("msg", kind=kind, msg=msg)

    def _answer_challenge(self, msg):
        """PROTOCOL.md s.2.1: prove we hold the key, without sending it.

        `srv` is checked first. If it does not verify, the thing on the other
        end does not hold the key either, so answering would hand a valid mac
        to an impostor -- stop instead.
        """
        nonce = msg.get("nonce")
        if not isinstance(nonce, str) or not nonce:
            self._emit("bad_challenge")
            return
        srv = msg.get("srv")
        if isinstance(srv, str) and not protocol.verify_srv(self.token, nonce, srv):
            self._denied = True
            self._emit("deny", reason="daemon failed the srv check (impostor?)")
            return
        mac = protocol.mac_for(self.token, nonce, self.client_id)
        self.send(protocol.auth(mac))
        self._emit("auth_sent", alg=msg.get("alg"))


# ---------------------------------------------------------------------------
# TermApp -- canvas, primitives, focus navigation, status badges
# ---------------------------------------------------------------------------


class TermApp:
    """The whole terminal UI. Feed it via the Net queue; it never opens sockets."""

    def __init__(self, root, host=DEFAULT_HOST, port=DEFAULT_PORT,
                 token=DEFAULT_TOKEN, client_id=CLIENT_ID,
                 width=DISPLAY_W, height=DISPLAY_H):
        self.root = root
        self.host = host
        self.port = int(port)
        self.client_id = client_id
        self.display_w = int(width)
        self.display_h = int(height)

        self.events = queue.Queue()
        self.net = Net(self.events, host=host, port=port, token=token,
                       client_id=client_id)

        # -- state that the protocol maintains for us --
        self.values = {}                # metric id -> newest value (None = invalid)
        self.scene = None
        self.scene_titles = []
        self.ui_ver = None
        self.records = []               # canvas records of the current page
        self.by_bind = {}               # metric id -> [num record]
        self.width_records = []         # bind_w rect records
        self.focusables = []            # [{"index": n, "record": rec}]
        self.focus_pos = 0
        self.focus_item = None

        # -- link / freshness state --
        self.link_up = False
        self.link_reason = "not connected"
        self.deny_reason = ""
        self.debug_text = ""
        self.last_rx = time.monotonic()
        self.last_data = None
        self.frames = 0
        self.data_frames = 0
        self.ui_frames = 0
        self.ignored = 0
        self.errors = 0
        self.sent_events = []
        self._poll_job = None
        self._tick_job = None
        self._fonts = {}
        self._closing = False

        self.root.title("PC-Monitor terminal - " + self.client_id)
        self.root.configure(bg=BG_COLOR)
        self.root.resizable(False, False)
        self.font_family = self._pick_font_family()
        self.canvas = tk.Canvas(root, width=self.display_w, height=self.display_h,
                                bg=BG_COLOR, highlightthickness=0, bd=0)
        self.canvas.pack()
        self._bind_keys()
        self._draw_overlays()

    # -- construction helpers ---------------------------------------------

    def _pick_font_family(self):
        wanted = FONT_FAMILY_PREFERRED
        try:
            families = set(tkfont.families(self.root))
        except tk.TclError:
            families = set()
        if wanted in families:
            return wanted
        try:
            return tkfont.nametofont("TkDefaultFont").actual("family")
        except tk.TclError:
            return wanted

    def _font(self, size_px):
        """Font of the given pixel height (protocol "size" is a pixel height)."""
        px = int(size_px) if _is_number(size_px) and size_px > 0 else DEFAULT_FONT_PX
        px = max(6, min(px, 200))
        font = self._fonts.get(px)
        if font is None:
            font = tkfont.Font(family=self.font_family, size=-px)
            self._fonts[px] = font
        return font

    def _bind_keys(self):
        """Keyboard map: arrows move focus, Enter activates, n/p flip pages."""
        binds = {
            "<Up>": lambda: self._move_focus(-1),
            "<Left>": lambda: self._move_focus(-1),
            "<Down>": lambda: self._move_focus(1),
            "<Right>": lambda: self._move_focus(1),
            "<Return>": self._activate_focus,
            "<KP_Enter>": self._activate_focus,
            "<space>": self._activate_focus,
            "<n>": lambda: self._want_page("next"),
            "<N>": lambda: self._want_page("next"),
            "<p>": lambda: self._want_page("prev"),
            "<P>": lambda: self._want_page("prev"),
        }
        for sequence, handler in binds.items():
            self.root.bind(sequence, self._key_handler(handler))
        self.canvas.focus_set()

    def _key_handler(self, handler):
        """Wrap a zero-argument action in a Tk binding (event object unused)."""
        def on_key(_event):
            try:
                handler()
            except Exception:  # noqa: BLE001 - one bad key, not a dead UI
                self.errors += 1
            return "break"
        return on_key

    # -- lifecycle --------------------------------------------------------

    def start(self, poll=True):
        """Start the network thread and the two Tk timers."""
        self.net.start()
        if poll:
            self._poll_job = self.root.after(UI_POLL_MS, self._poll_queue)
            self._tick_job = self.root.after(STATUS_TICK_MS, self._status_tick)

    def close(self):
        if self._closing:
            return
        self._closing = True
        self.net.stop()
        for job in (self._poll_job, self._tick_job):
            if job is None:
                continue
            try:
                self.root.after_cancel(job)
            except tk.TclError:
                pass
        self._poll_job = None
        self._tick_job = None
        try:
            self.root.destroy()
        except tk.TclError:
            pass

    def pump_once(self):
        """Run one Tk event cycle (used by --selftest instead of mainloop)."""
        try:
            self.root.update()
        except tk.TclError:
            self.errors += 1

    # -- queue drain ------------------------------------------------------

    def _poll_queue(self):
        self._poll_job = None
        try:
            while True:
                try:
                    item = self.events.get_nowait()
                except queue.Empty:
                    break
                try:
                    self._handle_item(item)
                except Exception:  # noqa: BLE001 - one bad frame, not a dead UI
                    self.errors += 1
        finally:
            if not self._closing:
                self._poll_job = self.root.after(UI_POLL_MS, self._poll_queue)

    def _handle_item(self, item):
        event = item.get("event")
        if event == "msg":
            self._on_message(item.get("kind"), item.get("msg") or {})
        elif event == "link":
            self.link_up = bool(item.get("up"))
            if item.get("up"):
                self.link_reason = ""
                self.deny_reason = ""
                self.last_rx = time.monotonic()
            else:
                self.link_reason = str(item.get("reason") or "disconnected")
                delay = item.get("delay")
                if self.link_reason != "connecting" and _is_number(delay) and delay > 0:
                    self.link_reason += " (retry in %gs)" % float(delay)
            self._update_status()
        elif event == "deny":
            reason = str(item.get("reason") or "denied")
            self.deny_reason = reason
            self.link_up = False
            self.link_reason = "denied"
            self._update_status()
        elif event == "bad_line":
            self.errors += 1
        elif event == "unknown":
            self.ignored += 1

    def _on_message(self, kind, msg):
        self.frames += 1
        self.last_rx = time.monotonic()
        if kind == "ui":
            self._rebuild(msg)
        elif kind == "data":
            self._on_data(msg)
        elif kind == "cmd":
            self._on_cmd(msg)
        elif kind == "scenes":
            self._on_scenes(msg)
        elif kind == "debug":
            text = msg.get("text")
            self.debug_text = text if isinstance(text, str) else ""
            self._update_status()
        elif kind in ("catalog", "time", "pong"):
            pass                                     # nothing to draw
        else:
            self.ignored += 1                        # section 20

    # -- ui: full rebuild -------------------------------------------------

    def _rebuild(self, msg):
        """Rebuild the whole page from a ui message (section 7)."""
        self.canvas.delete("all")
        self.records = []
        self.by_bind = {}
        self.width_records = []
        self.focusables = []
        self.focus_item = None
        self.ui_frames += 1
        scene = msg.get("scene")
        self.scene = int(scene) if _is_number(scene) else self.scene
        ver = msg.get("ver")
        self.ui_ver = ver if _is_number(ver) else self.ui_ver

        bg = _color(msg.get("bg"))
        self.canvas.configure(bg=bg if bg else BG_COLOR)

        nodes = msg.get("nodes")
        nodes = nodes if isinstance(nodes, list) else []
        for node in nodes:
            if not isinstance(node, dict):
                continue
            record = self._draw_node(node)
            if record is None:
                continue
            self.records.append(record)
            if node.get("focusable") is True:
                self._add_focusable(node, record)

        self.focusables.sort(key=lambda item: item["index"])
        # Section 11.5: after a new ui an out-of-range focus index resets to 0.
        if self.focus_pos >= len(self.focusables):
            self.focus_pos = 0

        for record in self.width_records:
            self._apply_width(record)
        for record in self.records:
            if record["kind"] == "num":
                self._apply_num(record)

        self._draw_overlays()
        self._draw_focus()

    def _add_focusable(self, node, record):
        """Register a focusable node; "focus" absent => array-order numbering."""
        index = node.get("focus")
        if _is_number(index):
            index = int(index)
        else:
            index = len(self.focusables)
        self.focusables.append({"index": index, "record": record})

    # -- primitives -------------------------------------------------------

    def _draw_node(self, node):
        """Draw one primitive. Unknown types are ignored (section 20)."""
        kind = node.get("type")
        x = _num(node.get("x"), 0.0)
        y = _num(node.get("y"), 0.0)
        if kind == "rect":
            return self._draw_rect(node, x, y)
        if kind == "path":
            return self._draw_path(node, x, y)
        if kind == "text":
            return self._draw_text(node, x, y)
        if kind == "num":
            return self._draw_num(node, x, y)
        return None

    def _draw_rect(self, node, x, y):
        width = _num(node.get("w"), 0.0)
        height = _num(node.get("h"), 0.0)
        fill = _color(node.get("fill"))
        outline = _color(node.get("color"))
        thickness = _num(node.get("width"), DEFAULT_RECT_WIDTH)
        thickness = max(1, int(round(thickness)))
        # "r" (rounded corners) is ignored on purpose: flat rectangles only.
        if fill is not None:
            item = self.canvas.create_rectangle(
                x, y, x + width, y + height, fill=fill,
                outline=outline or fill, width=thickness if outline else 0)
        else:
            item = self.canvas.create_rectangle(
                x, y, x + width, y + height, fill="",
                outline=outline or BORDER_COLOR, width=thickness)
        record = {"kind": "rect", "node": node, "item": item,
                  "x": x, "y": y, "w": width, "h": height, "bind": None}
        bind = node.get("bind_w")
        if isinstance(bind, str) and bind:
            record["kind"] = "rect_w"
            record["bind"] = bind
            self.width_records.append(record)
        return record

    def _draw_path(self, node, x, y):
        pts = node.get("pts")
        if not isinstance(pts, list) or len(pts) < 4:
            return None
        coords = []
        for index in range(0, len(pts) - 1, 2):
            dx = pts[index]
            dy = pts[index + 1]
            if not _is_number(dx) or not _is_number(dy):
                continue
            coords.extend((x + float(dx), y + float(dy)))
        if len(coords) < 4:
            return None
        color = _color(node.get("color")) or ACCENT_COLOR
        thickness = max(1, int(round(_num(node.get("width"), DEFAULT_RECT_WIDTH))))
        fill = _color(node.get("fill"))
        if fill is not None or node.get("closed") is True:
            item = self.canvas.create_polygon(
                *coords, fill=fill or "", outline=color, width=thickness)
        else:
            item = self.canvas.create_line(
                *coords, fill=color, width=thickness, capstyle="butt")
        return {"kind": "path", "node": node, "item": item}

    def _draw_text(self, node, x, y):
        text = node.get("text")
        text = text if isinstance(text, str) else ""
        color = _color(node.get("color")) or TEXT_COLOR
        align = node.get("align")
        anchor = TEXT_ALIGN_ANCHORS.get(align if isinstance(align, str) else DEFAULT_ALIGN,
                                        TEXT_ALIGN_ANCHORS[DEFAULT_ALIGN])
        item = self.canvas.create_text(x, y, text=text, anchor=anchor,
                                       fill=color, font=self._font(node.get("size")))
        return {"kind": "text", "node": node, "item": item}

    def _draw_num(self, node, x, y):
        bind = node.get("bind")
        bind = bind if isinstance(bind, str) else ""
        align = node.get("align")
        anchor = TEXT_ALIGN_ANCHORS.get(align if isinstance(align, str) else DEFAULT_ALIGN,
                                        TEXT_ALIGN_ANCHORS[DEFAULT_ALIGN])
        color = _color(node.get("color")) or TEXT_COLOR
        record = {
            "kind": "num",
            "node": node,
            "bind": bind,
            "dec": node.get("dec"),
            "unit": node.get("unit"),
            "color": color,
            "item": self.canvas.create_text(x, y, text="--", anchor=anchor,
                                            fill=color, font=self._font(node.get("size"))),
        }
        if bind:
            self.by_bind.setdefault(bind, []).append(record)
        return record

    # -- data: in-place updates only (no full redraw) ----------------------

    def _on_data(self, msg):
        values = msg.get("v")
        values = values if isinstance(values, dict) else {}
        colors = msg.get("c")
        colors = colors if isinstance(colors, dict) else {}
        self.last_data = time.monotonic()
        self.data_frames += 1
        for metric, value in values.items():
            self.values[metric] = value
            for record in self.by_bind.get(metric, ()):
                self._apply_num(record)
            for record in self.width_records:
                if record["bind"] == metric:
                    self._apply_width(record)
        for metric, color in colors.items():
            self._apply_color(metric, color)
        self._update_status()

    def _apply_num(self, record):
        text = _format_value(self.values.get(record["bind"]),
                             record["dec"], record["unit"])
        try:
            self.canvas.itemconfigure(record["item"], text=text)
        except tk.TclError:
            self.errors += 1

    def _apply_width(self, record):
        ratio = _bind_ratio(self.values.get(record["bind"]), record["node"])
        x = record["x"]
        y = record["y"]
        w = record["w"] * ratio
        try:
            self.canvas.coords(record["item"], x, y, x + w, y + record["h"])
        except tk.TclError:
            self.errors += 1

    def _apply_color(self, metric, color):
        ink = _color(color)
        if ink is None:
            return
        for record in self.by_bind.get(metric, ()):
            try:
                self.canvas.itemconfigure(record["item"], fill=ink)
            except tk.TclError:
                self.errors += 1

    # -- scenes / cmd -----------------------------------------------------

    def _on_scenes(self, msg):
        titles = []
        items = msg.get("list")
        if isinstance(items, list):
            for item in items:
                if isinstance(item, dict):
                    title = item.get("title")
                    titles.append(title if isinstance(title, str) else "")
        self.scene_titles = titles
        self._update_status()

    def _on_cmd(self, msg):
        action = msg.get("action")
        if action == "show":
            scene = msg.get("scene")
            if _is_number(scene) and self.scene is not None and int(scene) != self.scene:
                self._send_event({"t": "event", "k": "page", "want": int(scene)})
        elif action == "clear":
            self._clear_page()
        elif action == "reset":
            self.values = {}
            self._clear_page()
            self.focus_pos = 0
            self._send_event({"t": "event", "k": "page", "want": 0})
        elif action == "brightness":
            pass                                     # ignored by design
        else:
            self.ignored += 1                        # reboot etc.: not ours

    def _clear_page(self):
        self.canvas.delete("all")
        self.records = []
        self.by_bind = {}
        self.width_records = []
        self.focusables = []
        self.focus_pos = 0
        self.focus_item = None
        self._draw_overlays()

    # -- focus navigation (section 11.5) ----------------------------------

    def _move_focus(self, delta):
        if not self.focusables:
            return
        self.focus_pos = (self.focus_pos + int(delta)) % len(self.focusables)
        self._draw_focus()

    def _activate_focus(self):
        if not self.focusables:
            return
        index = self.focusables[self.focus_pos]["index"]
        self._send_event({"t": "event", "k": "key", "id": "focus", "v": index})

    def _draw_focus(self):
        if self.focus_item is not None:
            try:
                self.canvas.delete(self.focus_item)
            except tk.TclError:
                pass
            self.focus_item = None
        if not self.focusables:
            return
        record = self.focusables[self.focus_pos]["record"]
        try:
            box = self.canvas.bbox(record["item"])
        except tk.TclError:
            box = None
        if not box:
            return
        pad = FOCUS_PAD
        self.focus_item = self.canvas.create_rectangle(
            box[0] - pad, box[1] - pad, box[2] + pad, box[3] + pad,
            outline=ACCENT_COLOR, width=FOCUS_BOX_PX, fill="")

    def _want_page(self, direction):
        if direction == "next":
            want = "next"
        else:
            want = 0 if self.scene is None else max(0, int(self.scene) - 1)
        self._send_event({"t": "event", "k": "page", "want": want})

    def _send_event(self, msg):
        if not self.net.send(msg):
            self.errors += 1
            return
        self.sent_events.append(msg)
        del self.sent_events[:-20]

    # -- overlays: freshness badge + link lost (section 13.5) -------------

    def _draw_overlays(self):
        """(Re)create the four small status items; they are updated in place."""
        small = self._font(OVERLAY_PX)
        self.age_item = self.canvas.create_text(
            self.display_w - 6, 6, text="", anchor="ne", fill=DIM_COLOR,
            font=small, tags=("overlay",))
        self.scene_item = self.canvas.create_text(
            6, self.display_h - 22, text="", anchor="sw", fill=DIM_COLOR,
            font=small, tags=("overlay",))
        self.debug_item = self.canvas.create_text(
            6, self.display_h - 6, text="", anchor="sw", fill=DIM_COLOR,
            font=small, tags=("overlay",))
        self.lost_item = self.canvas.create_text(
            self.display_w - 6, self.display_h - 6, text="", anchor="se",
            fill=ALERT_COLOR, font=small, tags=("overlay",))
        self._update_status()

    def _update_status(self):
        if self._closing:
            return
        now = time.monotonic()
        if self.last_data is None:
            age_text = "-- ms ago"
        else:
            age_text = "%d ms ago" % int(max(0.0, now - self.last_data) * 1000.0)
        if self.scene is None:
            scene_text = ""
        elif self.scene_titles and 0 <= int(self.scene) < len(self.scene_titles):
            scene_text = "%d/%d %s" % (int(self.scene) + 1, len(self.scene_titles),
                                       self.scene_titles[int(self.scene)])
        else:
            scene_text = "scene %d" % int(self.scene)
        if self.deny_reason:
            note = "DENIED: " + self.deny_reason
        elif not self.link_up:
            note = self.link_reason
        else:
            note = self.debug_text
        lost = "" if (now - self.last_rx) <= LINK_LOST_S else "LINK LOST"
        try:
            self.canvas.itemconfigure(self.age_item, text=age_text)
            self.canvas.itemconfigure(self.scene_item, text=scene_text)
            self.canvas.itemconfigure(self.debug_item, text=note)
            self.canvas.itemconfigure(self.lost_item, text=lost)
        except tk.TclError:
            self.errors += 1

    def _status_tick(self):
        self._tick_job = None
        try:
            self._update_status()
        finally:
            if not self._closing:
                self._tick_job = self.root.after(STATUS_TICK_MS, self._status_tick)


# ---------------------------------------------------------------------------
# Selftest -- prove the terminal works, then kill everything we started
# ---------------------------------------------------------------------------


def _pump(app, seconds, until=None, step=SELFTEST_SETTLE_S / 8.0):
    """Run the Tk event loop for a while; stop early when until() is true."""
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        app.pump_once()
        if until is not None and until():
            return True
        time.sleep(step)
    return bool(until()) if until is not None else True


def _press(app, keysym, until, timeout=3.0, slice_s=0.5):
    """Generate a real Tk key event, retrying until until() holds.

    Tk sometimes drops a synthetic key event on a withdrawn window, which
    made this selftest flaky (same build, same code, pass one run and fail
    the next). Pressing again is harmless and still exercises the real
    binding, unlike calling the handler directly.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        app.root.event_generate(keysym)
        if _pump(app, slice_s, until=until):
            return True
    try:
        return bool(until())
    except Exception:
        return False


def _port_open(host, port, timeout=0.3):
    try:
        sock = socket.create_connection((host, port), timeout=timeout)
    except OSError:
        return False
    try:
        sock.close()
    except OSError:
        pass
    return True


def _scratch_paths():
    """Two flat log files for the selftest daemon (no new directories)."""
    base = SELFTEST_TMP_DIR if os.path.isdir(SELFTEST_TMP_DIR) else tempfile.gettempdir()
    stamp = "%d_%d" % (os.getpid(), int(time.time() * 1000) % 1000000)
    return (os.path.join(base, "term_tk_selftest_" + stamp + ".out"),
            os.path.join(base, "term_tk_selftest_" + stamp + ".err"))


def _free_port(host, start, tries=50):
    """First port in [start, start+tries) nobody is listening on.

    The selftest spawns its own daemon; if the default port is already taken it
    would connect to a stranger and report that stranger's verdict as ours.
    That happened for real (a packaged PC-Monitor.exe held 7802) and cost an
    hour of blaming the handshake.
    """
    for offset in range(tries):
        port = start + offset
        probe = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        try:
            probe.bind((host, port))
        except OSError:
            continue
        finally:
            probe.close()
        return port
    return start


def _write_selftest_devices(path):
    """Pair the selftest client with the selftest daemon (s.2.4).

    A file, not an in-memory table: the daemon is a separate process and reads
    its device table from disk, exactly as a real deployment does.
    """
    from pcmon.devices import DeviceTable

    DeviceTable(path).add(CLIENT_ID + "-selftest", key=SELFTEST_KEY,
                          note="term_tk selftest")
    return path


def _spawn_daemon(host, port, out_path, err_path, devices_path):
    """Start fake_daemon.py as a child process (never Start-Process)."""
    if not os.path.isfile(DAEMON_SCRIPT):
        print("daemon script not found: " + DAEMON_SCRIPT)
        return None
    cmd = [sys.executable, DAEMON_SCRIPT, "--port", str(port),
           "--auth", "hmac", "--devices", devices_path, "--seed", "1"]
    try:
        out_file = open(out_path, "wb")
        err_file = open(err_path, "wb")
    except OSError as exc:
        print("cannot open daemon log files: " + str(exc))
        return None
    try:
        proc = subprocess.Popen(cmd, cwd=BASE_DIR, stdout=out_file,
                                stderr=err_file, stdin=subprocess.DEVNULL)
    finally:
        out_file.close()
        err_file.close()
    deadline = time.monotonic() + SPAWN_WAIT_S
    while time.monotonic() < deadline:
        if _port_open(host, port):
            return proc
        if proc.poll() is not None:
            return proc
        time.sleep(0.1)
    return proc


def _stop_process(proc, timeout=3.0):
    if proc is None:
        return
    if proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout)
        except subprocess.TimeoutExpired:
            proc.kill()
            try:
                proc.wait(timeout)
            except subprocess.TimeoutExpired:
                pass
    for stream in (proc.stdout, proc.stderr, proc.stdin):
        try:
            if stream is not None:
                stream.close()
        except OSError:
            pass


def _read_text(path):
    try:
        with open(path, "rb") as handle:
            return handle.read().decode("utf-8", "replace")
    except OSError:
        return ""


def _glyph_boxes(canvas):
    """Bounding boxes trimmed to the ink (ascent) part of every text item.

    Tk's bbox spans ascent+descent, so two labels separated by a 1 px gap in
    real ink still look overlapping when their blank descender rows meet.
    Trimming to the ascent height makes the comparison honest.
    """
    boxes = []
    for item in canvas.find_all():
        if canvas.type(item) != "text":
            continue
        text = canvas.itemcget(item, "text")
        if not text:
            continue
        box = canvas.bbox(item)
        if not box:
            continue
        try:
            font = tkfont.Font(root=canvas,
                               font=canvas.itemcget(item, "font"))
            ascent = font.metrics("ascent")
        except (tk.TclError, ValueError, TypeError):
            ascent = box[3] - box[1]
        boxes.append((text, (box[0], box[1], box[2],
                             min(box[3], box[1] + ascent))))
    return boxes


def find_text_overlaps(canvas):
    """Return [(text_a, text_b), ...] for text whose real ink intersects."""
    boxes = _glyph_boxes(canvas)
    hits = []
    for i in range(len(boxes)):
        for j in range(i + 1, len(boxes)):
            a, b = boxes[i][1], boxes[j][1]
            if a[0] < b[2] and b[0] < a[2] and a[1] < b[3] and b[1] < a[3]:
                hits.append((boxes[i][0], boxes[j][0]))
    return hits


def run_selftest(args):
    """Headless-ish end-to-end check. Prints PASS/FAIL lines, returns 0 or 1."""
    checks = []
    out_path = None
    err_path = None
    proc = None
    app = None
    root = None

    def check(name, ok, detail=""):
        checks.append((name, bool(ok), detail))
        line = ("PASS " if ok else "FAIL ") + name
        if detail:
            line += ": " + detail
        print(line)
        sys.stdout.flush()

    try:
        out_path, err_path = _scratch_paths()
        if not args.no_spawn:
            scratch = (SELFTEST_TMP_DIR if os.path.isdir(SELFTEST_TMP_DIR)
                       else tempfile.gettempdir())
            devices_path = _write_selftest_devices(
                os.path.join(scratch, SELFTEST_DEVICES_NAME))
            args.token = SELFTEST_KEY          # the key the daemon will check
            if _port_open(args.host, args.port):
                chosen = _free_port(args.host, args.port + 1)
                print("NOTE port %d is busy (someone else's daemon); "
                      "using %d for this selftest" % (args.port, chosen))
                args.port = chosen
            proc = _spawn_daemon(args.host, args.port, out_path, err_path,
                                 devices_path)
            reachable = _port_open(args.host, args.port)
            detail = "" if reachable else "daemon never opened %s:%d" % (args.host, args.port)
            check("daemon_reachable", reachable, detail)
            if not reachable:
                text = _read_text(err_path).strip()
                if text:
                    print("daemon stderr: " + text[-400:])
                return 1
        else:
            print("NOTE --no-spawn: using the daemon already on %s:%d (token %r)"
                  % (args.host, args.port, args.token))
            if not _port_open(args.host, args.port):
                check("daemon_reachable", False,
                      "nothing listening on %s:%d" % (args.host, args.port))
                return 1

        root = tk.Tk()
        app = TermApp(root, host=args.host, port=args.port, token=args.token,
                      client_id=CLIENT_ID + "-selftest")
        root.withdraw()                              # never leave a window open
        app.start()
        began = time.monotonic()

        got_ui = _pump(app, SELFTEST_UI_WAIT_S,
                       until=lambda: app.ui_ver is not None and app.records != [])
        check("connected", app.net.is_alive() and app.link_up,
              "link_up=%s reason=%r" % (app.link_up, app.link_reason))
        check("ui_frame", got_ui,
              "ui_frames=%d nodes=%d" % (app.ui_frames, len(app.records)))
        check("drawn_items", len(app.records) > 0,
              "records=%d" % len(app.records))

        overlaps = find_text_overlaps(app.canvas)
        check("no_text_overlap", not overlaps, "overlaps=%r" % (overlaps,))

        got_data = _pump(app, SELFTEST_UI_WAIT_S,
                         until=lambda: app.data_frames >= 2)
        check("data_frames", got_data, "data_frames=%d" % app.data_frames)

        num_records = [r for r in app.records if r["kind"] == "num"]
        texts = [str(app.canvas.itemcget(r["item"], "text")) for r in num_records]
        filled = [t for t in texts if t and t != "--"]
        check("num_updated", bool(filled), "num texts=%r" % (texts,))

        width_records = list(app.width_records)
        widths = [app.canvas.bbox(r["item"]) for r in width_records]
        scaled = False
        for record, box in zip(width_records, widths):
            if not box:
                continue
            drawn = box[2] - box[0]
            if 0 < drawn < record["w"] - 0.5:
                scaled = True
        check("bind_w_scaled", scaled,
              "rects=%d boxes=%r" % (len(width_records), widths))

        # Walk to the page that carries focusable nodes (the daemon's KEYS page)
        # using nothing but the protocol's page event, then check navigation.
        walked = 0
        deadline = time.monotonic() + SELFTEST_UI_WAIT_S * 2
        while len(app.focusables) < 2 and time.monotonic() < deadline:
            started_at = app.scene
            walked += 1
            _press(app, "<n>",
                   lambda b=started_at: (app.scene != b)
                   and len(app.focusables) >= 2, timeout=1.5)
        check("focusables_seen", len(app.focusables) >= 2,
              "focusables=%d pages_walked=%d scene=%r"
              % (len(app.focusables), walked, app.scene))

        if len(app.focusables) >= 2:
            before = app.focus_pos
            before_box = app.canvas.bbox(app.focus_item) if app.focus_item else None
            app.root.event_generate("<Down>")
            _pump(app, SELFTEST_SETTLE_S)
            after_box = app.canvas.bbox(app.focus_item) if app.focus_item else None
            moved = app.focus_pos != before
            check("focus_moves", moved, "pos %d -> %d" % (before, app.focus_pos))
            check("focus_highlight", before_box != after_box,
                  "box %r -> %r" % (before_box, after_box))
            target = app.focusables[app.focus_pos]["index"]
            sent_before = len(app.sent_events)
            _press(app, "<Return>",
                   lambda: len(app.sent_events) > sent_before)
            emitted = len(app.sent_events) > sent_before
            last = app.sent_events[-1] if app.sent_events else None
            check("focus_event_sent", emitted and last == {"t": "event", "k": "key",
                                                           "id": "focus", "v": target},
                  "sent=%r" % (last,))
        else:
            check("focus_moves", False, "no focusable nodes on any page")
            check("focus_event_sent", False, "no focusable nodes on any page")

        scene_before = app.scene
        switched = _press(app, "<n>", lambda: app.scene != scene_before)
        check("page_switch", switched,
              "scene %r -> %r" % (scene_before, app.scene))

        # Focus index must reset to 0 when a new page has fewer focusables.
        app.focus_pos = len(app.focusables) + 5
        app._rebuild({"t": "ui", "scene": app.scene, "ver": app.ui_ver,
                      "nodes": [{"type": "text", "x": 8, "y": 8, "text": "X",
                                 "size": 12},
                                {"type": "rect", "x": 8, "y": 40, "w": 40, "h": 20,
                                 "fill": "#0078D4", "focusable": True}]})
        check("focus_reset", app.focus_pos == 0, "focus_pos=%d" % app.focus_pos)

        # Freshness badge + LINK LOST rendering (time is simulated, not waited).
        app.last_data = time.monotonic() - 1.25
        app.last_rx = time.monotonic() - (LINK_LOST_S + 1.0)
        app._update_status()
        _pump(app, SELFTEST_SETTLE_S / 4.0)
        age_text = app.canvas.itemcget(app.age_item, "text")
        lost_text = app.canvas.itemcget(app.lost_item, "text")
        check("age_badge", "ms ago" in age_text, "text=%r" % age_text)
        check("link_lost_badge", lost_text == "LINK LOST", "text=%r" % lost_text)
        app.last_rx = time.monotonic()
        app._update_status()
        check("link_lost_clears",
              app.canvas.itemcget(app.lost_item, "text") == "",
              "text=%r" % app.canvas.itemcget(app.lost_item, "text"))

        # cmd handling: clear empties the page, show/reset stay on the wire.
        app._clear_page()
        check("cmd_clear", app.canvas.find_all() == () or
              len(app.records) == 0, "records=%d" % len(app.records))
        app._on_cmd({"t": "cmd", "target": "*", "action": "reset"})
        check("cmd_reset_requests_page",
              bool(app.sent_events) and app.sent_events[-1].get("want") == 0,
              "sent=%r" % (app.sent_events[-1] if app.sent_events else None))
        sent_before = len(app.sent_events)
        app._on_cmd({"t": "cmd", "target": "*", "action": "brightness", "value": 60})
        check("cmd_brightness_ignored", len(app.sent_events) == sent_before)

        elapsed = time.monotonic() - began
        check("within_budget", elapsed <= SELFTEST_TOTAL_S + SELFTEST_UI_WAIT_S * 3,
              "%.1fs" % elapsed)

        if proc is not None:
            _pump(app, SELFTEST_SETTLE_S)
            log_text = _read_text(out_path)
            focus_lines = [line for line in log_text.splitlines()
                           if line.startswith("FOCUS ")]
            check("daemon_saw_focus_event", bool(focus_lines),
                  "daemon log focus lines=%r" % (focus_lines[-3:],))
    finally:
        # The daemon must die even if closing the UI raises (a leaking child
        # keeps the port bound and poisons the next selftest run).
        try:
            if app is not None:
                app.close()
            elif root is not None:
                try:
                    root.destroy()
                except tk.TclError:
                    pass
        except Exception:
            pass
        finally:
            _stop_process(proc)
        for path in (out_path, err_path):
            try:
                if path and os.path.isfile(path):
                    os.remove(path)
            except OSError:
                pass

    failures = [c for c in checks if not c[1]]
    for name, _ok, detail in failures:
        print("FAILED " + name + (": " + detail if detail else ""))
    print("SELFTEST %s (%d checks, %d failed)"
          % ("PASS" if not failures else "FAIL", len(checks), len(failures)))
    return 0 if not failures else 1


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def parse_args(argv=None):
    parser = argparse.ArgumentParser(
        description="PC-Monitor thin desktop terminal (M3): draws the ui/data "
                    "frames of a PC daemon on a Tk canvas.")
    parser.add_argument("--host", default=DEFAULT_HOST,
                        help="daemon address (default %s)" % DEFAULT_HOST)
    parser.add_argument("--port", type=int, default=DEFAULT_PORT,
                        help="daemon TCP port (default %d)" % DEFAULT_PORT)
    parser.add_argument("--token", default=DEFAULT_TOKEN,
                        help="auth token from the daemon banner; a daemon started "
                             "with --lenient accepts any token (development)")
    parser.add_argument("--client-id", default=CLIENT_ID,
                        help="name this terminal reports (default %s)" % CLIENT_ID)
    parser.add_argument("--selftest", action="store_true",
                        help="start a fake daemon, pump the event loop, print "
                             "PASS/FAIL and exit (no window is left open)")
    parser.add_argument("--no-spawn", action="store_true",
                        help="with --selftest: use a daemon that is already running")
    return parser.parse_args(argv)


def read_token_file():
    """Pick up the token fake_daemon.py publishes next to the scripts.

    The daemon draws a fresh random token every run (PROTOCOL.md s.2), so
    hand-copying it out of the banner to start a terminal is pointless in
    development. An explicit --token always wins; --lenient daemons accept
    anything anyway.
    """
    try:
        with open(TOKEN_FILE, "r", encoding="ascii") as handle:
            return handle.read().strip()
    except OSError:
        return ""


def main(argv=None):
    args = parse_args(argv)
    if not args.selftest and not args.token:
        args.token = read_token_file()
    if args.selftest:
        return run_selftest(args)
    root = tk.Tk()
    app = TermApp(root, host=args.host, port=args.port, token=args.token,
                  client_id=args.client_id)
    root.protocol("WM_DELETE_WINDOW", app.close)
    app.start()
    try:
        root.mainloop()
    except KeyboardInterrupt:
        pass
    finally:
        app.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
