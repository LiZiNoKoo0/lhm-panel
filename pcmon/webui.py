"""webui -- browser dashboard for the PC-Monitor daemon (milestone M6, stdlib only).

    python -m pcmon.webui [--host 127.0.0.1] [--port 7803]
                          [--daemon 127.0.0.1:7802] [--token TOKEN]
                          [--size 160x80] [--open]

WHAT IT IS
    A *thick terminal* (PROTOCOL.md section 3) that happens to draw its picture
    in a browser instead of on a TFT. It connects to the daemon over the same
    TCP/NDJSON port every other terminal uses, says hello, and receives
    `catalog` + `ui` + `data` like anything else. Then it re-serves what it got
    over HTTP so a page can render it.

WHY A BRIDGE AND NOT A PAGE THAT TALKS TO THE DAEMON DIRECTLY
    A browser cannot open a raw TCP socket. Something has to sit between the
    page and port 7802, and this file is that something. The daemon therefore
    needs no changes at all: no admin port, no shared state, no import. The day
    the real daemon exists, this keeps working.

WHY SSE AND NOT WEBSOCKET
    The stdlib has no WebSocket *server*: the handshake is easy, but framing
    (masking, continuation, close) is code I would have to own and get right,
    for a channel that only ever carries 1 Hz telemetry one way. SSE is fifteen
    lines of a normal HTTP response and the browser reconnects by itself.
    Upstream traffic (an `event`) is a plain POST.

WHAT THIS FILE DELIBERATELY DOES NOT DO
    * invent numbers, formats or colors -- everything on screen came from a
      `catalog`/`ui`/`data` frame (the page formats, using SPEC 4.4/4.5 rules)
    * transform the protocol -- it forwards `ui.nodes` verbatim; if a node is
      clipped by the display size, the page says so instead of rescaling it
    * hold value history or sparkline state (PROTOCOL.md section 8: that is a
      device-side widget concern)
    * become a command channel -- POST /api/event accepts `event` frames only
      (terminals report events, section 5 rule 5); anything else is refused
"""

import argparse
import collections
import json
import os
import socket
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse

try:  # imported as a package (python -m pcmon.webui)
    from . import font8x16
    from . import layouts as layouts_mod
    from . import protocol
except ImportError:  # pragma: no cover - run as a loose script
    import font8x16
    import layouts as layouts_mod
    import protocol

STATIC_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "webstatic")

#: Reconnect ladder. Deliberately the same numbers as `pcmon/lhm.py` and the
#: firmware (SPEC L-2): if the two sides disagree, one of them hammers the other.
BACKOFF_S = (1.0, 2.0, 4.0, 8.0, 10.0)

#: PROTOCOL.md section 13.5: no frame for this long and the terminal is stale.
LINK_LOST_S = 15.0

#: How often the page is re-sent a snapshot. Data arrives at 1 Hz, so 2 Hz is
#: enough to make a change look immediate without pushing JSON for no reason.
PUSH_INTERVAL_S = 0.5

CONTENT_TYPES = {
    ".html": "text/html; charset=utf-8",
    ".js": "text/javascript; charset=utf-8",
    ".css": "text/css; charset=utf-8",
}


def _font_json():
    """The 8x16 atlas as plain numbers (95 glyphs x 16 bytes, ~5 KB)."""
    return json.dumps({
        "w": font8x16.CELL_W,
        "h": font8x16.CELL_H,
        "first": font8x16.FIRST,
        "glyphs": [list(g) for g in font8x16.GLYPHS],
    }, separators=(",", ":")).encode("utf-8")


class Link:
    """The daemon connection, and the last thing it said.

    One lock for the whole state: the HTTP threads read it, the reader thread
    writes it, and the snapshots are small enough that a copy under a lock is
    cheaper than any cleverer scheme.
    """

    def __init__(self, host, port, token, w, h, client_id="webui",
                 token_file=None):
        self.host = host
        self.port = port
        self.token = token
        #: The daemon mints a fresh token every time it starts. A dashboard that
        #: keeps the token it read at launch therefore dies the first time the
        #: daemon is restarted -- and looks like a broken UI, not a stale secret.
        #: With a file, the token is re-read on every reconnect attempt.
        self.token_file = token_file
        self.w = w
        self.h = h
        self.client_id = client_id

        self._lock = threading.Lock()
        self._sock = None
        self._stopped = threading.Event()
        self._thread = None

        #: Everything the page is allowed to see. Written only by the reader.
        self._up = False
        self._err = ""
        self._catalog_ver = 0
        self._catalog = []
        self._ui = None
        self._values = {}          # id -> {"v": value, "ts": local seconds}
        #: What the *panel* declared in its hello, as reported by the daemon.
        #: Empty until one arrives; the editor uses it to grey out controls the
        #: hardware cannot honour (today: any font size but 16).
        self._terminal = {}
        self._hello = None
        self._last_rx = 0.0
        self._since = 0.0
        self.counts = collections.Counter()

    # -- lifetime ------------------------------------------------------

    def start(self):
        self._thread = threading.Thread(target=self._run, name="webui-link",
                                        daemon=True)
        self._thread.start()

    def stop(self):
        self._stopped.set()
        sock = self._sock
        if sock is not None:
            try:
                sock.shutdown(2)
            except OSError:
                pass

    def _run(self):
        failures = 0
        while not self._stopped.is_set():
            try:
                self._session()
            except OSError as exc:
                with self._lock:
                    self._err = "%s: %s" % (type(exc).__name__, exc)
            finally:
                with self._lock:
                    self._up = False
                    self._sock = None
            if self._stopped.is_set():
                return
            delay = BACKOFF_S[min(failures, len(BACKOFF_S) - 1)]
            failures += 1
            self._stopped.wait(delay)

    def _current_token(self):
        if self.token_file:
            try:
                with open(self.token_file, "r", encoding="utf-8") as handle:
                    fresh = handle.read().strip()
                if fresh:
                    self.token = fresh
            except OSError:
                pass  # keep the last known token
        return self.token

    def _session(self):
        """One connection, from hello to EOF. Returns when the daemon hangs up."""
        sock = _connect((self.host, self.port))
        self._sock = sock
        hello = protocol.hello(self.client_id, "thick", self.w, self.h,
                               auth=list(protocol.AUTH_ALGS), accept=["ui", "data"],
                               fw="webui-1")
        # Kept so the editor can gate its controls on what a *real* terminal
        # accepts. The dashboard's own hello is a thick one and says nothing
        # useful about the panel's fonts; the panel's hello does, and the daemon
        # logs it. Filled in by `_adopt_capabilities` from the daemon's banner
        # and the board's own hello when they are visible.
        with self._lock:
            self._hello = hello
        sock.sendall(protocol.encode(hello))
        buf = b""
        while not self._stopped.is_set():
            chunk = sock.recv(4096)
            if not chunk:
                raise OSError("daemon closed the connection")
            buf += chunk
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                if line.strip():
                    self._adopt(line)

    # -- inbound -------------------------------------------------------

    def _answer_challenge(self, msg):
        """Answer the daemon's nonce (s.2.1), after checking its own proof."""
        nonce = msg.get("nonce")
        key = self._current_token()
        if not isinstance(nonce, str) or not nonce or not key:
            self._fail_session("challenge without nonce or key")
            return
        srv = msg.get("srv")
        if isinstance(srv, str) and not protocol.verify_srv(key, nonce, srv):
            self._fail_session("daemon failed the srv check (impostor?)")
            return
        payload = protocol.encode(protocol.auth(
            protocol.mac_for(key, nonce, self.client_id)))
        try:
            self._sock.sendall(payload)
        except OSError as exc:
            self._fail_session("auth send failed: %s" % exc)
            return
        with self._lock:
            self.counts["auth_ok"] = self.counts.get("auth_ok", 0) + 1

    def _fail_session(self, reason):
        """Drop this connection; the reconnect loop will decide what to do next."""
        with self._lock:
            self._err = reason
            self.counts["auth_failed"] = self.counts.get("auth_failed", 0) + 1
        try:
            self._sock.close()
        except OSError:
            pass

    def _adopt(self, line):
        msg = protocol.decode(line)
        if msg is None:
            with self._lock:
                self.counts["bad_lines"] += 1
            return
        if msg.get("t") == "challenge":
            self._answer_challenge(msg)
            return
        kind = protocol.kind(msg)
        now = time.time()
        with self._lock:
            self.counts["frames"] += 1
            self._last_rx = now
            self._up = True
            self._err = ""
            if self._since == 0.0:
                self._since = now
            if kind == "catalog":
                self._catalog_ver = int(msg.get("ver") or 0)
                metrics = msg.get("metrics")
                self._catalog = list(metrics) if isinstance(metrics, list) else []
                self.counts["catalog"] += 1
            elif kind == "ui":
                self._ui = msg
                self.counts["ui"] += 1
            elif kind == "data":
                values = msg.get("v")
                if isinstance(values, dict):
                    for key, value in values.items():
                        self._values[key] = {"v": value, "ts": now}
                self.counts["data"] += 1
            elif kind == "deny":
                self._err = "denied: %s" % msg.get("reason")
                self.counts["deny"] += 1
            elif kind == "ping":
                self.counts["ping"] += 1
            else:
                self.counts["other"] += 1

    # -- upstream ------------------------------------------------------

    def send(self, msg):
        """Send one frame to the daemon. Raises OSError when the link is down."""
        sock = self._sock
        if sock is None:
            raise OSError("not connected")
        sock.sendall(protocol.encode(msg))

    # -- read side -----------------------------------------------------

    def snapshot(self):
        now = time.time()
        with self._lock:
            up = self._up
            age = (now - self._last_rx) if self._last_rx else None
            return {
                "now": now,
                "link": {
                    # A link that still has a socket but has gone quiet is the
                    # case worth showing: the badge is what tells a human that
                    # the numbers on screen stopped being true.
                    "up": bool(up and age is not None and age < LINK_LOST_S),
                    "socket": self._sock is not None,
                    "lost": bool(age is not None and age >= LINK_LOST_S),
                    "err": self._err,
                    "age_s": None if age is None else round(age, 1),
                    "since_s": None if not self._since else round(now - self._since, 1),
                    "host": self.host,
                    "port": self.port,
                    "client_id": self.client_id,
                },
                "display": {"w": self.w, "h": self.h},
                "catalog": {"ver": self._catalog_ver, "metrics": list(self._catalog)},
                "ui": self._ui,
                "values": {k: dict(v) for k, v in self._values.items()},
                "counts": dict(self.counts),
            }


def _connect(address):
    family = socket.AF_INET6 if ":" in address[0] else socket.AF_INET
    sock = socket.socket(family, socket.SOCK_STREAM)
    sock.settimeout(5.0)
    sock.connect(address)
    sock.settimeout(None)
    return sock


def _read_token(explicit, daemon_host):
    """Token precedence: flag, then ./daemon.token, then a clear error.

    Returns (token, path). The path is kept so reconnects can pick up a token
    the daemon has since rewritten (it mints a new one on every start).
    """
    if explicit:
        return explicit, None
    path = os.path.join(os.getcwd(), "daemon.token")
    if os.path.exists(path):
        with open(path, "r", encoding="utf-8") as handle:
            text = handle.read().strip()
        if text:
            return text, path
    raise SystemExit(
        "webui: no token. Pass --token, or run from the directory that holds "
        "daemon.token (the daemon writes it next to itself); daemon=%s"
        % (daemon_host,))


class Handler(BaseHTTPRequestHandler):
    server_version = "pcmon-webui"
    link = None  # set by serve()

    def log_message(self, fmt, *args):  # keep the console for our own lines
        pass

    # -- helpers -------------------------------------------------------

    def _json(self, payload, status=200):
        body = payload if isinstance(payload, bytes) else \
            json.dumps(payload, separators=(",", ":")).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def _static(self, name):
        path = os.path.join(STATIC_DIR, name)
        if not os.path.isfile(path):
            self._json({"error": "not found"}, 404)
            return
        with open(path, "rb") as handle:
            body = handle.read()
        ext = os.path.splitext(name)[1]
        self.send_response(200)
        self.send_header("Content-Type", CONTENT_TYPES.get(ext, "application/octet-stream"))
        self.send_header("Content-Length", str(len(body)))
        # No caching: this is a development dashboard, and a stale app.js that
        # silently keeps running old logic is worse than 20 KB of re-reads.
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    # -- routes --------------------------------------------------------

    def do_GET(self):
        path = urlparse(self.path).path
        if path in ("/", "/index.html"):
            self._static("index.html")
            return
        if path == "/api/font":
            self._json(_font_json())
            return
        if path == "/api/state":
            self._json(self.link.snapshot())
            return
        if path == "/api/layout":
            ver, scenes = self.layouts.snapshot()
            self._json({
                "ver": ver,
                "scenes": {str(k): v for k, v in sorted(scenes.items())},
            })
            return
        if path == "/events":
            self._events()
            return
        # Any other path is a static file, but only if it really is one: the
        # handler must not be a file server for the whole disk.
        name = path.lstrip("/")
        if name and ".." not in name and "/" not in name and "\\" not in name:
            candidate = os.path.join(STATIC_DIR, name)
            if os.path.isfile(candidate) and os.path.splitext(name)[1] in CONTENT_TYPES:
                self._static(name)
                return
        self._json({"error": "not found"}, 404)

    def do_POST(self):
        path = urlparse(self.path).path
        if path == "/api/layout/apply":
            self._apply_layout()
            return
        if path != "/api/event":
            self._json({"error": "not found"}, 404)
            return
        try:
            length = int(self.headers.get("Content-Length") or 0)
        except ValueError:
            length = 0
        raw = self.rfile.read(length) if length else b""
        msg = protocol.decode(raw)
        # Only events go upstream. A page must not be able to turn itself into
        # a command channel just because it speaks HTTP (PROTOCOL.md section 5
        # rule 5: terminals report events, the daemon decides what that means).
        if protocol.kind(msg) != "event":
            self._json({"ok": False, "error": "only event frames are accepted"}, 400)
            return
        try:
            self.link.send(msg)
        except OSError as exc:
            self._json({"ok": False, "error": str(exc)}, 503)
            return

    def _apply_layout(self):
        """Save one applied layout set and bump `ver` (PROTOCOL 22.5).

        This is the only write path the editor has. It is deliberately not a
        "save draft" endpoint: a draft has nowhere to live, because the moment
        the daemon could resend a draft it would be able to push unfinished
        work onto a device nobody is looking at. Closing the tab discards it,
        which is what a draft is.

        The response carries the new ver and the daemon's own view of the file,
        so the browser shows what the terminals will actually get rather than
        what it hoped it sent.
        """
        raw = self._read_body()
        try:
            payload = json.loads(raw.decode("utf-8") or "{}")
        except (UnicodeDecodeError, ValueError) as exc:
            self._json({"ok": False, "error": "不是合法 JSON: %s" % exc}, 400)
            return
        if not isinstance(payload, dict):
            self._json({"ok": False, "error": "请求体必须是对象"}, 400)
            return
        scenes = payload.get("scenes")
        try:
            ver = self.layouts.apply(scenes)
        except layouts_mod.LayoutError as exc:
            # A refused apply changes nothing on disk and tells the editor which
            # field was wrong -- the alternative is a layout that silently
            # ignores half of what the user typed.
            self._json({"ok": False, "error": str(exc)}, 400)
            return
        except OSError as exc:
            self._json({"ok": False, "error": "写入失败: %s" % exc}, 500)
            return

        # Push it to the daemon so terminals see it without waiting for their
        # next handshake. A failure here is reported but does not undo the
        # apply: the file is the source of truth and the daemon re-reads it.
        pushed = True
        try:
            self.link.send({"t": "event", "k": "apply", "ver": ver})
        except OSError:
            pushed = False

        current, applied = self.layouts.snapshot()
        self._json({
            "ok": True,
            "ver": current,
            "pushed": pushed,
            "scenes": {str(k): v for k, v in sorted(applied.items())},
        })

    def _read_body(self):
        try:
            length = int(self.headers.get("Content-Length") or 0)
        except ValueError:
            length = 0
        return self.rfile.read(length) if length else b""
        self._json({"ok": True})

    def _events(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream; charset=utf-8")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Connection", "keep-alive")
        self.end_headers()
        try:
            while True:
                payload = self.link.snapshot()
                body = json.dumps(payload, separators=(",", ":")).encode("utf-8")
                self.wfile.write(b"data: " + body + b"\n\n")
                self.wfile.flush()
                time.sleep(PUSH_INTERVAL_S)
        except (BrokenPipeError, ConnectionResetError, OSError):
            return  # browser closed the tab; nothing to clean up


class WebServer(ThreadingHTTPServer):
    """A listener that refuses to share port 7803 with another process.

    On Windows `SO_REUSEADDR` does not mean "reuse a socket in TIME_WAIT" the
    way it does on Unix: it lets a *second* process bind the same port, and
    then the kernel hands connections to whichever socket it likes. The symptom
    is a dashboard that keeps serving old code no matter how many times you
    restart it. One clear "port in use" beats that.
    """

    daemon_threads = True
    allow_reuse_address = os.name != "nt"

    def server_bind(self):
        if os.name == "nt":
            self.socket.setsockopt(socket.SOL_SOCKET, socket.SO_EXCLUSIVEADDRUSE, 1)
        super().server_bind()


def serve(host, port, link, layout_path=None):
    Handler.link = link
    #: The same object the daemon holds, when they share a file. The editor
    #: writes through `apply`, which bumps `ver` and saves atomically; the daemon
    #: sees the new ver the next time it reads, so no socket is needed between
    #: the two processes (PROTOCOL 22.5).
    Handler.layouts = layouts_mod.AppliedLayouts(
        layout_path or os.path.join("layouts", "applied.json"))
    return WebServer((host, port), Handler)


def main(argv=None):
    here = os.getcwd()
    parser = argparse.ArgumentParser(prog="pcmon.webui",
                                     description="browser dashboard for the daemon")
    parser.add_argument("--host", default="127.0.0.1",
                        help="HTTP bind address (default 127.0.0.1: localhost only)")
    parser.add_argument("--port", type=int, default=7803)
    parser.add_argument("--daemon", default="127.0.0.1:7802",
                        help="daemon address host:port")
    parser.add_argument("--token", default=None,
                        help="daemon token (default: ./daemon.token)")
    parser.add_argument("--size", default="160x80",
                        help="display size to declare in hello (default 160x80)")
    parser.add_argument("--client-id", default="webui")
    parser.add_argument("--layouts", default=os.path.join("layouts", "applied.json"),
                        help="applied layout file written by the daemon "
                             "(--layouts); the editor reads and applies through it")
    parser.add_argument("--open", action="store_true",
                        help="open the dashboard window when it is ready")
    args = parser.parse_args(argv)

    try:
        w_text, h_text = args.size.lower().split("x")
        w, h = int(w_text), int(h_text)
    except ValueError:
        raise SystemExit("webui: --size must look like 160x80, got %r" % args.size)

    host, _, port_text = args.daemon.partition(":")
    token, token_path = _read_token(args.token, args.daemon)
    link = Link(host or "127.0.0.1", int(port_text or 7802), token, w, h,
                client_id=args.client_id, token_file=token_path)
    link.start()

    httpd = serve(args.host, args.port, link, args.layouts)
    url = "http://%s:%d/" % (args.host, args.port)
    print("webui: %s  (daemon %s:%s, %dx%d, client_id=%s)"
          % (url, host, port_text or "7802", w, h, args.client_id))
    print("webui: cwd=%s  Ctrl-C to stop" % here)
    if args.open:
        import webbrowser
        threading.Timer(0.4, lambda: webbrowser.open(url)).start()
    try:
        httpd.serve_forever(poll_interval=0.5)
    except KeyboardInterrupt:
        print("webui: stopped")
    finally:
        link.stop()
        httpd.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
