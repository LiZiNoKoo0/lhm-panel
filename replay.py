"""replay -- offline replayer for a recorded PC-Monitor session (milestone M4).

    python replay.py recordings/session1.ndjson
    python replay.py recordings/session1.ndjson --speed 2.0 --loop
    python replay.py recordings/session1.ndjson --session 1
    python replay.py recordings/session1.ndjson --selftest

`fake_daemon.py --record FILE` writes every frame it sends into FILE as raw
NDJSON. This program is the other half: it listens on TCP 7802 (PROTOCOL.md
s.1), waits for a terminal to connect and say hello, and then hands that
terminal the recorded frames -- in the recorded order, at the recorded pace,
byte for byte -- so term_tk.py (or an ESP32 with no PC anywhere near it) shows
the same picture without a daemon, without plugins and without waiting for the
machine to get busy. That is what makes fast firmware iteration possible (M6).

Recording and playback meet on a single rule: the replayer never edits a frame.
No second pass through fit_nodes, no seq reassignment, no refreshed values, no
synthesized replies. What went out of the daemon goes back onto the wire, so a
terminal cannot tell the replay from the daemon that produced the recording.

Pacing (PROTOCOL.md s.9 "ts", s.4 "s"):
  The recorder stores frames without a clock, so the player rebuilds one:
    * the first frame carrying "ts" (epoch seconds) defines t=0
    * every later "ts" advances the clock by its delta; a backwards delta is
      clamped to 0 so a clock change cannot rewind the playback
    * frames without a "ts" inherit the current clock, so the burst after a
      hello (catalog + scenes + ui + data) stays a burst, and the ui + cmd
      pair of a page change still arrives together
    * one wait is capped at --max-gap seconds (default 5), so a coffee break in
      the middle of a recording is not replayed literally
    * a recording with no "ts" anywhere is paced like a live daemon: --interval
      seconds per "data" frame (default 1.0), structural frames immediately

What this file deliberately does NOT do:
  * record anything -- it is the playback half of `fake_daemon.py --record`
  * authenticate: whatever token a terminal sends is accepted, because the
    session being replayed already happened (s.2 is the daemon's business)
  * answer events. A page request or a key press is counted and logged, but the
    recording keeps playing; an interactive terminal wants fake_daemon.py
  * refresh stale values from a live source -- a replay is a fixed tape. Past
    the end of the recording the link stays open and heartbeats are still
    answered, but no new values arrive: the terminal holds the last frame and
    its own freshness badge (s.13.5) climbs, which is the truth about a tape
    that has run out
  * untangle a multi-terminal tape. `--record` appends every client's frames to
    one file, and such a tape replays as it was recorded; --session 1 can pick
    a single connection out of it (the daemon sends "catalog" once per hello,
    which marks each new connection), but frames already interleaved inside one
    connection cannot be separated
  * trim for capabilities or enforce limits -- the recorded bytes were already
    trimmed for the terminal that was connected at record time
  * judge metric semantics, draw anything, or open a window
  * die on a bad connection: every per-connection error is swallowed and
    counted (TASKS.md coding rule 3)

Source is pure ASCII on purpose (TASKS.md coding rule 1: PowerShell 5.1 on
this host mangles GBK/UTF-8 console output). Python 3.12 stdlib only.
"""

import argparse
import collections
import os
import select
import socket
import sys
import tempfile
import threading
import time
from pathlib import Path

from pcmon import protocol

# ---------------------------------------------------------------------------
# Constants -- every magic number lives here (TASKS.md coding rule 4)
# ---------------------------------------------------------------------------

BIND_HOST = "0.0.0.0"
DEFAULT_PORT = protocol.DEFAULT_PORT          # 7802
SOCKET_BACKLOG = 8
ACCEPT_TIMEOUT_S = 0.5                        # accept() poll slice for Ctrl-C
READ_SLICE_S = 0.1                            # select() slice while waiting
RECV_CHUNK_BYTES = 4096
MAX_LINE_BYTES = 65536                        # oversized line => not NDJSON
SEND_TIMEOUT_S = 0.2                          # PROTOCOL.md s.16 red line 1

DEFAULT_SPEED = 1.0                           # 1.0 = original pace
MAX_GAP_S = 5.0                               # cap for one recorded pause
FALLBACK_INTERVAL_S = 1.0                     # pace when there is no "ts" at all
LOOP_RESTART_S = 1.0                          # pause before a --loop lap
SESSION_JOIN_S = 1.0                          # teardown grace period
STAT_LOG_INTERVAL_S = 5.0                     # throttle for terminal "stat"

#: Human labels for frames of an unknown "t" (forward compatibility, s.20).
UNKNOWN_LABEL = "unknown"


# ---------------------------------------------------------------------------
# Tiny helpers
# ---------------------------------------------------------------------------


def _is_number(value):
    """True for int/float but not bool (bool is an int subclass)."""
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def _number_or_none(value):
    return float(value) if _is_number(value) else None


_LOG_LOCK = threading.Lock()


def log(text):
    """One timestamped console line; never raises into a session."""
    try:
        with _LOG_LOCK:
            sys.stdout.write("[" + time.strftime("%H:%M:%S") + "] " + text + "\n")
            sys.stdout.flush()
    except (OSError, ValueError):
        pass


class ReplayError(Exception):
    """A problem the user has to fix (missing file, busy port)."""


# ---------------------------------------------------------------------------
# Recording model -- one frame, one loaded file
# ---------------------------------------------------------------------------


class Frame(object):
    """One recorded line: the exact bytes plus what the loader learned.

    `raw` is what goes back onto the wire (the original line plus its newline).
    `at` is a virtual timestamp in recorded seconds, `delay` the capped wait in
    front of this frame (speed is applied at play time, not here).
    """

    __slots__ = ("raw", "msg", "kind", "ts", "at", "delay")

    def __init__(self, raw, msg):
        self.raw = raw
        self.msg = msg
        self.kind = protocol.kind(msg)
        self.ts = _number_or_none(msg.get("ts"))
        self.at = 0.0
        self.delay = 0.0

    def label(self):
        return self.kind if self.kind else UNKNOWN_LABEL

    def seq(self):
        value = self.msg.get("s")
        return int(value) if _is_number(value) else None


class Recording(object):
    """A loaded NDJSON file: the frames we can replay and what we skipped."""

    __slots__ = ("path", "frames", "stats", "kinds")

    def __init__(self, path, frames, stats, kinds):
        self.path = path
        self.frames = frames
        self.stats = stats
        self.kinds = kinds

    def duration(self):
        if not self.frames:
            return 0.0
        return max(0.0, self.frames[-1].at - self.frames[0].at)

    def describe(self):
        kinds = ", ".join("%s=%d" % (name, count)
                          for name, count in sorted(self.kinds.items()))
        return "%d frames (%s), %.1fs of traffic" % (
            len(self.frames), kinds or "none", self.duration())


def load_recording(path):
    """Read a recorded NDJSON file into a Recording.

    Only content problems are forgiving (a corrupt line is counted and
    skipped); a file that cannot be read at all raises ReplayError, because
    there is nothing sensible left to do.
    """
    try:
        blob = Path(path).read_bytes()
    except OSError as exc:
        raise ReplayError("cannot read %s: %s" % (path, exc))

    frames = []
    stats = collections.Counter()
    for line in blob.splitlines():
        stats["lines"] += 1
        if not line.strip():
            stats["blank_lines"] += 1
            continue
        msg = protocol.decode(line)
        if msg is None:
            stats["bad_lines"] += 1
            continue
        frames.append(Frame(line.strip() + b"\n", msg))
        stats["frames"] += 1

    kinds = collections.Counter()
    for frame in frames:
        kinds[frame.label()] += 1
    return Recording(str(path), frames, stats, kinds)


def assign_clock(frames, interval=FALLBACK_INTERVAL_S):
    """Give every frame a virtual timestamp in seconds (see module docstring).

    Anchored mode: the first frame with "ts" defines t=0, every later one
    advances the clock by its delta, everything in between inherits the clock.
    Fallback mode (no "ts" anywhere): one `interval` per "data" frame, the way
    a live daemon paces itself.
    """
    clock = 0.0
    anchor = None
    for frame in frames:
        if frame.ts is not None:
            if anchor is None:
                anchor = frame.ts
            else:
                step = frame.ts - anchor
                anchor = frame.ts
                if step > 0.0:
                    clock += step
        frame.at = clock

    if anchor is None:
        clock = 0.0
        for frame in frames:
            frame.at = clock
            if frame.kind == "data":
                clock += interval


def assign_delays(frames, max_gap=MAX_GAP_S):
    """Convert virtual timestamps into capped waits (speed applied by player)."""
    previous = 0.0
    for frame in frames:
        gap = frame.at - previous
        previous = frame.at
        if gap < 0.0:
            gap = 0.0
        if max_gap and gap > max_gap:
            frame.delay = max_gap
        else:
            frame.delay = gap


def split_sessions(frames):
    """Split a tape into per-connection groups, in recording order.

    A daemon sends "catalog" exactly once per accepted hello (PROTOCOL.md s.16
    red line 4: everything is resent after every hello), so a "catalog" frame
    after the first group has begun marks the next terminal connection. That is
    the only session boundary a tape contains: hellos are inbound and are never
    recorded.
    """
    groups = []
    for frame in frames:
        if frame.kind == "catalog" and groups and groups[-1]:
            groups.append([])
        elif not groups:
            groups.append([])
        groups[-1].append(frame)
    return groups


def select_session(recording, number):
    """Return a Recording holding connection `number` (1-based, 0 = all)."""
    if number <= 0:
        return recording
    groups = split_sessions(recording.frames)
    if number > len(groups):
        raise ReplayError("%s holds %d recorded session(s); --session %d is out "
                          "of range" % (recording.path, len(groups), number))
    frames = groups[number - 1]
    kinds = collections.Counter()
    for frame in frames:
        kinds[frame.label()] += 1
    return Recording(recording.path, frames, recording.stats, kinds)


# ---------------------------------------------------------------------------
# Session -- one connected terminal playing the tape
# ---------------------------------------------------------------------------


class Session(object):
    """One terminal: wait for hello, then feed frames, answer heartbeats.

    A single thread owns the socket for both directions. While it waits for the
    next frame it keeps reading inbound lines through select(), so a ping is
    answered on time and a closed peer is noticed immediately. Nothing is ever
    sent from a second thread, so an NDJSON line can never be interleaved with
    another one (that would desynchronize the terminal's parser forever).
    """

    def __init__(self, server, sock, addr):
        self.server = server
        self.sock = sock
        self.addr = addr
        try:
            self.label = "%s:%s" % (addr[0], addr[1])
        except (TypeError, IndexError):
            self.label = "?"
        self.client_id = None
        self.hello = None
        self.greeted = False
        self.buf = b""
        self.sent = 0
        self.pongs = 0
        self.events = 0
        self.ignored = 0
        self.bad_lines = 0
        self.errors = 0
        self.laps = 0
        self.stat_logged_at = float("-inf")
        self._dead = False
        self._closed = False
        self._stop = threading.Event()

    # -- public API -------------------------------------------------------

    def start(self):
        self._thread = threading.Thread(target=self.run, daemon=True,
                                        name="replay-" + self.label)
        self._thread.start()

    def stop(self, reason="server stop"):
        """Ask the session to finish and tear its socket down (idempotent)."""
        if self._stop.is_set():
            return
        self.server.log("%s: stopping (%s)" % (self.label, reason))
        self._stop.set()
        self._shutdown_socket()

    @property
    def alive(self):
        """True while this session's thread is still running (used by tests)."""
        thread = getattr(self, "_thread", None)
        return thread is not None and thread.is_alive()

    # -- lifecycle --------------------------------------------------------

    def run(self):
        try:
            self.server.log("connection from " + self.label)
            if not self._await_hello():
                return
            self._play()
        except Exception as exc:  # noqa: BLE001 - one session, not one dead process
            self.errors += 1
            self.server.log("%s: session error: %s: %s"
                            % (self.label, type(exc).__name__, exc))
        finally:
            self.close()

    def close(self):
        if self._closed:
            return
        self._closed = True
        self._stop.set()
        self._shutdown_socket()
        self.server.forget(self)
        self.server.log("closed %s: sent=%d (pongs=%d) events=%d ignored=%d "
                        "bad_lines=%d laps=%d errors=%d"
                        % (self.label, self.sent, self.pongs, self.events,
                           self.ignored, self.bad_lines, self.laps, self.errors))

    def _shutdown_socket(self):
        try:
            self.sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        try:
            self.sock.close()
        except OSError:
            pass

    # -- phase 1: wait for hello -----------------------------------------

    def _await_hello(self):
        """Read until the terminal says hello. False if the link died first.

        Time is sliced rather than blocking on "until the link dies" so the
        hello that just arrived is actually noticed -- waiting for a message
        and waiting for a disconnect are different waits.
        """
        while not (self._stop.is_set() or self._dead):
            if self.greeted:
                return True
            self._pump(READ_SLICE_S)
        return self.greeted and not self._dead

    # -- phase 2: play ----------------------------------------------------

    def _play(self):
        frames = self.server.frames
        if not frames:
            self.server.log("%s: the recording has no frames, idling" % self.label)
            self._idle()
            return

        speed = self.server.speed
        index = 0
        while not (self._stop.is_set() or self._dead):
            if index >= len(frames):
                if not self.server.loop:
                    self._finish()
                    return
                index = 0
                self.laps += 1
                self.server.log("%s: looping back to frame 0 (lap %d)"
                                % (self.label, self.laps + 1))
                if not self._pump(LOOP_RESTART_S / speed):
                    return
                continue

            frame = frames[index]
            if frame.delay > 0.0 and not self._pump(frame.delay / speed):
                return
            if not self._send(frame.raw):
                return
            self._trace(frame, frame.delay / speed)
            index += 1

    def _finish(self):
        """The tape ran out: hold the last picture, keep answering heartbeats."""
        self.server.log("%s: recording finished (%d frames); holding the last "
                        "frame, Ctrl-C to stop, --loop to repeat"
                        % (self.label, len(self.server.frames)))
        self._idle()

    def _idle(self):
        while not (self._stop.is_set() or self._dead):
            self._pump(None)

    def _trace(self, frame, waited):
        if self.server.quiet:
            return
        seq = frame.seq()
        self.server.log("%s -> %-7s %5d B  +%5.2fs  s=%s"
                        % (self.label, frame.label(), len(frame.raw),
                           waited, "-" if seq is None else seq))

    # -- transport --------------------------------------------------------

    def _send(self, payload):
        """Send one frame. False means the connection must die.

        A send timeout is fatal on purpose: sendall() may already have pushed
        part of the line, and a half-written frame would desynchronize the
        NDJSON stream for this terminal forever.
        """
        if self._dead or self._stop.is_set():
            return False
        try:
            self.sock.settimeout(SEND_TIMEOUT_S)
            self.sock.sendall(payload)
        except OSError as exc:
            self.errors += 1
            self.server.log("%s: send failed (%dms): %s"
                            % (self.label, int(SEND_TIMEOUT_S * 1000), exc))
            self._dead = True
            return False
        self.sent += 1
        return True

    def _pump(self, timeout):
        """Read and dispatch inbound lines for up to `timeout` seconds.

        timeout None blocks until the link dies or we are stopped. Returns
        False when the link died or the session is stopping.
        """
        deadline = None if timeout is None else time.monotonic() + timeout
        while not (self._stop.is_set() or self._dead):
            if deadline is None:
                slice_s = READ_SLICE_S
            else:
                slice_s = min(READ_SLICE_S, deadline - time.monotonic())
                if slice_s <= 0.0:
                    return True
            try:
                ready, _, _ = select.select([self.sock], [], [], slice_s)
            except (OSError, ValueError):
                self._dead = True
                break
            if not ready:
                continue
            try:
                chunk = self.sock.recv(RECV_CHUNK_BYTES)
            except TimeoutError:
                continue
            except OSError:
                self._dead = True
                break
            if not chunk:
                self.server.log("%s: peer closed" % self.label)
                self._dead = True
                break
            self._feed(chunk)
        return not (self._dead or self._stop.is_set())

    def _feed(self, chunk):
        self.buf += chunk
        while True:
            cut = self.buf.find(b"\n")
            if cut < 0:
                break
            line, self.buf = self.buf[:cut], self.buf[cut + 1:]
            self._on_line(line)
        if len(self.buf) > MAX_LINE_BYTES:
            self.errors += 1
            self.server.log("%s sent %d bytes without a newline; dropping it"
                            % (self.label, len(self.buf)))
            self.buf = b""

    # -- inbound messages -------------------------------------------------

    def _on_line(self, line):
        msg = protocol.decode(line)
        if msg is None:
            self.bad_lines += 1
            self.server.log("%s: unparseable line, ignored (%r)"
                            % (self.label, line[:80]))
            return
        kind = protocol.kind(msg)
        if kind is None:
            self.ignored += 1           # s.20: unknown "t" is not fatal
            return
        if kind == "hello":
            self._on_hello(msg)
        elif kind == "ping":
            self.pongs += 1
            self._send(protocol.encode({"t": "pong"}))
        elif kind == "event":
            self.events += 1
            self.server.log("%s: event %r (a replay cannot answer it)"
                            % (self.label, msg))
        elif kind == "stat":
            self._on_stat(msg)
        else:
            self.ignored += 1

    def _on_hello(self, msg):
        self.hello = msg
        self.client_id = msg.get("client_id")
        display = msg.get("display")
        display = display if isinstance(display, dict) else {}
        if self.greeted:
            self.server.log("%s: hello again, ignored (the tape keeps playing)"
                            % self.label)
            return
        self.greeted = True
        self.server.log("hello from %s role=%s %sx%s charset=%s "
                        "(any token is accepted: the session is already recorded)"
                        % (msg.get("client_id"), msg.get("role"),
                           display.get("w"), display.get("h"), msg.get("charset")))

    def _on_stat(self, msg):
        now = time.monotonic()
        if now - self.stat_logged_at < STAT_LOG_INTERVAL_S:
            return
        self.stat_logged_at = now
        self.server.log("%s stat rssi=%s heap=%s rx=%s drop=%s uptime=%s"
                        % (self.label, msg.get("rssi"), msg.get("heap"),
                           msg.get("rx"), msg.get("drop"), msg.get("uptime")))


# ---------------------------------------------------------------------------
# Server -- listening socket, accept loop, session registry
# ---------------------------------------------------------------------------


class ReplayServer(object):
    """Listen, accept, and give every terminal the same tape from frame 0."""

    def __init__(self, frames, host=BIND_HOST, port=DEFAULT_PORT,
                 speed=DEFAULT_SPEED, loop=False, quiet=False):
        self.frames = frames
        self.host = host
        self.port = int(port)
        self.speed = float(speed) if _is_number(speed) and speed > 0 else DEFAULT_SPEED
        self.loop = bool(loop)
        self.quiet = bool(quiet)
        self._srv = None
        self._stop = threading.Event()
        self._stopped = False
        self._lock = threading.Lock()
        self._sessions = set()
        self.connections = 0

    # -- logging ----------------------------------------------------------

    def log(self, text):
        log(text)

    # -- lifecycle --------------------------------------------------------

    def open(self):
        """Bind and listen. Returns the port actually used (0 => ephemeral)."""
        srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        try:
            if os.name != "nt":
                # POSIX needs this to rebind a port left in TIME_WAIT. On
                # Windows SO_REUSEADDR would let a second server silently
                # hijack the port of the one already running, so skip it.
                srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            srv.bind((self.host, self.port))
            srv.listen(SOCKET_BACKLOG)
            srv.settimeout(ACCEPT_TIMEOUT_S)
        except OSError as exc:
            try:
                srv.close()
            except OSError:
                pass
            raise ReplayError("cannot listen on %s:%d: %s"
                              % (self.host, self.port, exc))
        self._srv = srv
        try:
            self.port = srv.getsockname()[1]
        except OSError:
            pass
        return self.port

    def serve_forever(self):
        """Accept connections until stop(). Safe to call in a thread."""
        if self._srv is None:
            self.open()
        try:
            while not self._stop.is_set():
                try:
                    conn, addr = self._srv.accept()
                except TimeoutError:
                    continue
                except OSError as exc:
                    if self._stop.is_set():
                        break
                    self.log("accept failed: " + str(exc))
                    continue
                self._spawn(conn, addr)
        finally:
            self.stop()

    def stop(self):
        """Stop accepting, stop every session, close the socket. Idempotent."""
        with self._lock:
            if self._stopped:
                return
            self._stopped = True
        self._stop.set()
        srv, self._srv = self._srv, None
        if srv is not None:
            try:
                srv.close()
            except OSError:
                pass
        with self._lock:
            sessions = list(self._sessions)
        for session in sessions:
            session.stop("server shutdown")
        deadline = time.monotonic() + SESSION_JOIN_S
        for session in sessions:
            thread = getattr(session, "_thread", None)
            if thread is None:
                continue
            thread.join(timeout=max(0.0, deadline - time.monotonic()))

    # -- sessions ---------------------------------------------------------

    def _spawn(self, conn, addr):
        try:
            conn.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        except OSError:
            pass
        try:
            conn.settimeout(SEND_TIMEOUT_S)
        except OSError:
            pass
        session = Session(self, conn, addr)
        with self._lock:
            self._sessions.add(session)
            self.connections += 1
        try:
            session.start()
        except RuntimeError as exc:
            self.log("cannot start a thread for %s: %s" % (session.label, exc))
            self.forget(session)
            session.close()

    def forget(self, session):
        with self._lock:
            self._sessions.discard(session)


# ---------------------------------------------------------------------------
# Selftest -- deterministic, no daemon, no window
# ---------------------------------------------------------------------------


class _Probe(object):
    """A minimal terminal stand-in: hello, then record what arrives when."""

    def __init__(self, host, port, timeout=2.0):
        self.sock = socket.create_connection((host, port), timeout=timeout)
        self.sock.settimeout(READ_SLICE_S / 2.0)
        self.buf = b""
        self.lines = []                     # [(arrival_s, raw_line), ...]
        self.t0 = time.monotonic()
        self.dead = False

    def hello(self, token="replay-selftest"):
        self.send(protocol.hello("replay-probe", "thin", 240, 320, token=token,
                                 charset="utf8", accept=["ui", "data"],
                                 limits={"max_nodes": 48, "max_json_bytes": 65536},
                                 primitives=["rect", "path", "text", "num"],
                                 fonts=[12, 16, 24, 32]))

    def send(self, msg):
        try:
            self.sock.sendall(protocol.encode(msg))
        except OSError:
            self.dead = True

    def read_until(self, seconds, until=None):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if until is not None and until(self):
                return True
            try:
                chunk = self.sock.recv(RECV_CHUNK_BYTES)
            except TimeoutError:
                continue
            except OSError:
                self.dead = True
                return bool(until(self)) if until is not None else False
            if not chunk:
                self.dead = True
                return bool(until(self)) if until is not None else False
            self.buf += chunk
            while True:
                cut = self.buf.find(b"\n")
                if cut < 0:
                    break
                line, self.buf = self.buf[:cut], self.buf[cut + 1:]
                self.lines.append((time.monotonic() - self.t0,
                                   line.strip() + b"\n"))
        return bool(until(self)) if until is not None else True

    def frames(self):
        """Recorded frames seen, in arrival order (pongs / other noise dropped)."""
        out = []
        for arrival, raw in self.lines:
            msg = protocol.decode(raw)
            if protocol.kind(msg) == "pong":
                continue
            out.append((arrival, raw))
        return out

    def pongs(self):
        out = []
        for arrival, raw in self.lines:
            if protocol.kind(protocol.decode(raw)) == "pong":
                out.append((arrival, raw))
        return out

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass


def _sample_messages():
    """A small, deterministic session used by the selftest."""
    metrics = [{"id": "cpu.total", "label": "CPU", "unit": "%", "type": "number",
                "min": 0, "max": 100, "interval_ms": 1000}]
    msgs = [
        protocol.catalog(1, metrics),
        {"t": "scenes", "list": [{"i": 0, "title": "CPU"}]},
        protocol.ui(0, 1, [
            {"type": "text", "x": 8, "y": 8, "text": "CPU", "size": 16,
             "color": "#605E5C"},
            {"type": "num", "x": 8, "y": 26, "bind": "cpu.total", "dec": 0,
             "unit": "%", "size": 32},
        ], bg="#F7F7F7"),
        protocol.data({"cpu.total": 10.0}),
        protocol.data({"cpu.total": 20.0}),
        protocol.data({"cpu.total": 30.0}),
        {"t": "brand_new", "payload": [1, 2, 3]},   # s.20: must be forwarded
    ]
    base_ts = 1700000000
    data_offset = 0
    for index, msg in enumerate(msgs):
        if protocol.is_pc_to_term(msg):
            msg["s"] = index + 1
        if msg.get("t") == "data":
            msg["ts"] = base_ts + data_offset
            data_offset += 1
    return msgs


def _sample_file(path, raw_lines):
    """Write the sample as NDJSON with one blank and one corrupt line mixed in."""
    pieces = []
    for index, raw in enumerate(raw_lines):
        pieces.append(raw)
        if index == 1:
            pieces.append(b"   \n")                      # blank: ignored
        if index == 3:
            pieces.append(b'{"t":"data","v":\n')         # truncated: ignored
    Path(path).write_bytes(b"".join(pieces))


def run_selftest(argv=None):
    """Prove the loader, the pacing math and the player, then exit 0 or 1."""
    checks = []

    def check(name, ok, detail=""):
        checks.append((name, bool(ok), detail))
        line = ("PASS " if ok else "FAIL ") + name
        if detail:
            line += ": " + detail
        print(line)
        sys.stdout.flush()

    stamp = "%d_%d" % (os.getpid(), int(time.time() * 1000) % 1000000)
    rec_path = os.path.join(tempfile.gettempdir(), "replay_selftest_" + stamp + ".ndjson")
    servers = []
    probes = []

    msgs = _sample_messages()
    raw_lines = [protocol.encode(msg) for msg in msgs]

    try:
        # -- loader ------------------------------------------------------
        _sample_file(rec_path, raw_lines)
        recording = load_recording(rec_path)
        check("load_frames", len(recording.frames) == len(raw_lines),
              "%d of %d" % (len(recording.frames), len(raw_lines)))
        check("load_bad_lines", recording.stats["bad_lines"] == 1,
              "bad_lines=%d" % recording.stats["bad_lines"])
        check("load_blank_lines", recording.stats["blank_lines"] == 1,
              "blank_lines=%d" % recording.stats["blank_lines"])
        check("load_raw_verbatim",
              [frame.raw for frame in recording.frames] == raw_lines)
        check("load_unknown_t_kept",
              [frame.label() for frame in recording.frames][-1] == UNKNOWN_LABEL,
              "kinds=%r" % ([frame.label() for frame in recording.frames],))
        try:
            load_recording(os.path.join(tempfile.gettempdir(), "no_such_" + stamp))
        except ReplayError:
            check("load_missing_raises", True)
        else:
            check("load_missing_raises", False, "missing file must raise ReplayError")

        # -- pacing math -------------------------------------------------
        assign_clock(recording.frames)
        got_clock = [round(frame.at, 6) for frame in recording.frames]
        check("clock_from_ts", got_clock == [0.0, 0.0, 0.0, 0.0, 1.0, 2.0, 2.0],
              "clock=%r" % (got_clock,))

        assign_delays(recording.frames, max_gap=0.0)
        got_delays = [round(frame.delay, 6) for frame in recording.frames]
        check("delays_uncapped", got_delays == [0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0],
              "delays=%r" % (got_delays,))

        assign_delays(recording.frames, max_gap=0.25)
        got_delays = [round(frame.delay, 6) for frame in recording.frames]
        check("delays_capped", got_delays == [0.0, 0.0, 0.0, 0.0, 0.25, 0.25, 0.0],
              "delays=%r" % (got_delays,))

        no_ts = []
        for msg in ({"t": "ui", "scene": 0, "nodes": []},
                    {"t": "data", "v": {}},
                    {"t": "data", "v": {}}):
            raw = protocol.encode(msg)
            no_ts.append(Frame(raw, protocol.decode(raw)))
        assign_clock(no_ts, interval=0.5)
        check("clock_without_ts",
              [round(frame.at, 6) for frame in no_ts] == [0.0, 0.0, 0.5],
              "clock=%r" % ([frame.at for frame in no_ts],))

        # -- multi-session tapes (--session) -----------------------------
        two = []
        for kind_msg in ({"t": "catalog", "s": 1, "ver": 1, "metrics": []},
                         {"t": "ui", "s": 2, "scene": 0, "nodes": []},
                         {"t": "data", "s": 3, "v": {}, "ts": 1000},
                         {"t": "deny", "reason": "bad token"},
                         {"t": "catalog", "s": 1, "ver": 1, "metrics": []},
                         {"t": "ui", "s": 2, "scene": 0, "nodes": []},
                         {"t": "data", "s": 3, "v": {}, "ts": 2000}):
            raw = protocol.encode(kind_msg)
            two.append(Frame(raw, protocol.decode(raw)))
        groups = split_sessions(two)
        check("split_sessions", [len(g) for g in groups] == [4, 3],
              "groups=%r" % ([len(g) for g in groups],))
        tape = Recording("memory", two, recording.stats, recording.kinds)
        second = select_session(tape, 2)
        check("select_session", len(second.frames) == 3
              and second.frames[0].kind == "catalog"
              and second.frames[-1].ts == 2000.0,
              "frames=%d" % len(second.frames))
        check("select_session_all", len(select_session(tape, 0).frames) == 7,
              "0 must mean the whole tape")
        try:
            select_session(tape, 3)
        except ReplayError:
            check("select_session_range", True)
        else:
            check("select_session_range", False, "an out-of-range session must raise")

        assign_delays(recording.frames, max_gap=MAX_GAP_S)

        # -- playback ----------------------------------------------------
        server = ReplayServer(recording.frames, host="127.0.0.1", port=0,
                             speed=4.0, loop=False, quiet=True)
        servers.append(server)
        port = server.open()
        threading.Thread(target=server.serve_forever, daemon=True,
                         name="replay-selftest-srv").start()

        probe = _Probe("127.0.0.1", port)
        probes.append(probe)
        probe.hello()
        wanted = len(raw_lines)
        probe.read_until(4.0, until=lambda p: len(p.frames()) >= wanted)
        got = probe.frames()
        check("play_all_frames", len(got) >= wanted,
              "got=%d wanted=%d" % (len(got), wanted))
        check("play_bytes_identical",
              [raw for _at, raw in got][:wanted] == raw_lines,
              "first mismatch=%r"
              % ([(a, b) for a, b in zip([raw for _t, raw in got], raw_lines)
                  if a != b][:1],))
        if len(got) >= wanted:
            arrivals = [at for at, _raw in got]
            check("play_burst_at_start", arrivals[3] < 0.5,
                  "4th frame at %.2fs" % arrivals[3])
            gap = arrivals[5] - arrivals[4]
            check("play_pacing", 0.12 <= gap <= 0.75,
                  "gap=%.3fs (expected ~0.25s at speed 4)" % gap)

        # heartbeat: the terminal's ping must be answered even mid-tape
        probe.send({"t": "ping"})
        probe.read_until(2.0, until=lambda p: bool(p.pongs()))
        check("ping_answered", bool(probe.pongs()),
              "pongs=%d" % len(probe.pongs()))

        # a silent connection and a closed one must not hurt the server
        silent = socket.create_connection(("127.0.0.1", port), timeout=2.0)
        silent.close()
        probe.close()
        second = _Probe("127.0.0.1", port)
        probes.append(second)
        second.hello()
        second.read_until(4.0, until=lambda p: len(p.frames()) >= 2)
        check("second_session_restarts",
              [raw for _at, raw in second.frames()][:2] == raw_lines[:2],
              "got=%r" % ([raw for _at, raw in second.frames()][:2],))
        second.close()

        # -- loop mode ---------------------------------------------------
        loop_server = ReplayServer(recording.frames, host="127.0.0.1", port=0,
                                   speed=8.0, loop=True, quiet=True)
        servers.append(loop_server)
        loop_port = loop_server.open()
        threading.Thread(target=loop_server.serve_forever, daemon=True,
                         name="replay-selftest-loop").start()
        looper = _Probe("127.0.0.1", loop_port)
        probes.append(looper)
        looper.hello()
        first_raw = raw_lines[0]
        seen = lambda p: len([1 for _a, r in p.frames() if r == first_raw]) >= 2
        looper.read_until(5.0, until=seen)
        repeats = len([1 for _a, r in looper.frames() if r == first_raw])
        check("loop_repeats", repeats >= 2, "laps seen=%d" % repeats)
        seq = [raw for _at, raw in looper.frames()]
        check("loop_sequence_repeats", seq[:len(raw_lines)] == raw_lines,
              "first lap=%r" % (seq[:len(raw_lines)],))
        looper.close()
    finally:
        for probe in probes:
            probe.close()
        for server in servers:
            server.stop()
        try:
            if os.path.isfile(rec_path):
                os.remove(rec_path)
        except OSError:
            pass

    failures = [item for item in checks if not item[1]]
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
        description="PC-Monitor offline replayer (M4): listen on the daemon "
                    "port and feed a recorded NDJSON session to a terminal.")
    parser.add_argument("path", nargs="?", default=None,
                        help="NDJSON file written by `fake_daemon.py --record`")
    parser.add_argument("--host", default=BIND_HOST,
                        help="bind address (default %s; a replay sends only what "
                             "was recorded, but it still speaks the protocol)"
                             % BIND_HOST)
    parser.add_argument("--port", type=int, default=DEFAULT_PORT,
                        help="TCP port (default %d, the daemon port)" % DEFAULT_PORT)
    parser.add_argument("--speed", type=float, default=DEFAULT_SPEED,
                        help="playback speed multiplier: 2 = twice as fast, "
                             "0.5 = half speed (default %g)" % DEFAULT_SPEED)
    parser.add_argument("--loop", action="store_true",
                        help="start over when the recording ends instead of "
                             "holding the last frame")
    parser.add_argument("--session", type=int, default=0,
                        help="replay only the Nth terminal connection recorded "
                             "in the file (1-based); 0 = the whole tape "
                             "(default 0)")
    parser.add_argument("--max-gap", type=float, default=MAX_GAP_S,
                        help="cap one inter-frame wait in recorded seconds; "
                             "0 = no cap (default %g)" % MAX_GAP_S)
    parser.add_argument("--interval", type=float, default=FALLBACK_INTERVAL_S,
                        help="pace for recordings without any \"ts\" field, in "
                             "seconds per data frame (default %g)"
                             % FALLBACK_INTERVAL_S)
    parser.add_argument("--quiet", action="store_true",
                        help="log lifecycle lines only, not every frame")
    parser.add_argument("--selftest", action="store_true",
                        help="run the built-in checks (no file, no window) and exit")
    args = parser.parse_args(argv)

    if args.selftest:
        return args
    if not args.path:
        parser.error("a recording path is required (or use --selftest)")
    if not (args.speed > 0.0):
        parser.error("--speed must be greater than 0")
    if args.max_gap < 0.0:
        parser.error("--max-gap must be 0 or greater")
    if not (args.interval > 0.0):
        parser.error("--interval must be greater than 0")
    if args.session < 0:
        parser.error("--session must be 0 (whole tape) or greater")
    return args


def banner(recording, args, sessions=0):
    """Explain what is about to be replayed, and why it may look odd."""
    has_clock = any(frame.ts is not None for frame in recording.frames)
    pacing = "speed %gx, max gap %s" % (
        args.speed, ("%gs" % args.max_gap) if args.max_gap else "uncapped")
    if not has_clock:
        pacing += ", no \"ts\" in the file: %gs per data frame" % args.interval
    log("PC-Monitor replay (PROTOCOL.md v0.4, recorded session only)")
    log("  file     : " + recording.path)
    if sessions:
        log("  sessions : %d recorded, replaying session %d"
            % (sessions, args.session))
    log("  content  : " + recording.describe())
    log("  skipped  : %d blank, %d undecodable lines"
        % (recording.stats["blank_lines"], recording.stats["bad_lines"]))
    log("  pacing   : " + pacing)
    log("  mode     : " + ("loop forever" if args.loop else "play once, then hold"))
    log("  listen   : %s:%d (TCP, NDJSON)" % (args.host, args.port))
    log("  auth     : any token (the session being replayed already happened)")
    log("  waiting for a terminal to say hello...")


def main(argv=None):
    args = parse_args(argv)
    if args.selftest:
        return run_selftest()

    try:
        recording = load_recording(args.path)
    except ReplayError as exc:
        print("replay: " + str(exc))
        return 2
    if not recording.frames:
        print("replay: %s holds no usable NDJSON frames" % args.path)
        return 1

    sessions = len(split_sessions(recording.frames))
    try:
        recording = select_session(recording, args.session)
    except ReplayError as exc:
        print("replay: " + str(exc))
        return 2

    assign_clock(recording.frames, interval=args.interval)
    assign_delays(recording.frames, max_gap=args.max_gap)

    server = ReplayServer(recording.frames, host=args.host, port=args.port,
                          speed=args.speed, loop=args.loop, quiet=args.quiet)
    banner(recording, args, sessions)
    try:
        server.open()
    except ReplayError as exc:
        print("replay: " + str(exc))
        return 1
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        log("Ctrl-C, shutting down")
    finally:
        server.stop()
    log("done: %d connection(s) served" % server.connections)
    return 0


if __name__ == "__main__":
    sys.exit(main())
