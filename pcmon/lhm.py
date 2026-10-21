r"""pcmon.lhm -- real sensor intake for PC-Monitor (milestone M5).

    source = LhmSource(r"H:\tools\lhm\lhm_bridge.exe")
    source.start()
    ...
    source.snapshot(time.monotonic())   # {"cpu.total": 27.3, "gpu.temp": None}

This module owns exactly one job: keep a bridge console program running, read
the NDJSON *arrays* it writes to stdout, and remember the newest frame plus the
time it arrived. Everybody else just asks snapshot(now) and gets
{metric_id: value or None}. PROTOCOL.md s.9: None means "invalid right now ->
show --", never a stale copy of the last value.

What this file deliberately does NOT do:
  * speak the display protocol -- no sockets, no catalog, no data frames, no
    sequence numbers; pcmon/protocol.py and the daemon own all of that
  * collect sensors itself -- no WMI, no performance counters, no ctypes.
    LHM (through the bridge) is the only source of truth (TASKS.md M5)
  * start LibreHardwareMonitor.exe or touch its GUI/config -- the bridge is a
    plain console program whose only output is stdout
  * keep history -- one frame, one timestamp (PROTOCOL.md s.8: the terminal
    owns the ring buffer)
  * block the caller -- start() returns at once and all blocking happens on a
    private thread; snapshot() only takes a short mutex, no pipe reads
  * raise at the caller -- missing exe, garbage lines, a killed bridge are all
    swallowed and counted in self.stats; the daemon decides what to log
  * invent numbers -- an absent or stale sensor is None, never a fake value
  * decide layout, labels or colors beyond the catalog metadata table below

Pure ASCII source on purpose (TASKS.md coding rule 1: PowerShell 5.1 on this
host mangles GBK/UTF-8 console output). Python 3.12 stdlib only.

Sensor mapping (TASKS.md M5) matches on hwtype + name + kind, never on the
"path" string: several GPUs and several NICs share names, so a path match would
silently combine them.
"""

import collections
import json
import os
import subprocess
import sys
import threading
import time

# ---------------------------------------------------------------------------
# Constants -- every magic number lives here (TASKS.md coding rule 4)
# ---------------------------------------------------------------------------

#: Default location of the console bridge (see tools/lhm_bridge/README.md).
DEFAULT_BRIDGE_PATH = r"H:\tools\lhm\lhm_bridge.exe"

#: Bridge frame cadence; also what we advertise as catalog interval_ms.
BRIDGE_INTERVAL_MS = 1000

#: No new frame for this long => every metric is None (TASKS.md M5: 3 seconds).
STALE_AFTER_S = 3.0

#: Respawn ladder after the bridge dies, seconds (PROTOCOL.md s.1, last repeats).
BACKOFF_S = (1.0, 2.0, 4.0, 8.0, 10.0)

#: A bridge line longer than this is not one of ours; skip it instead of dying.
MAX_LINE_BYTES = 4 * 1024 * 1024

#: How long to wait for a terminated bridge before killing it outright.
KILL_WAIT_S = 2.0

#: How long stop() waits for the reader thread before giving up on it.
STOP_JOIN_S = 3.0

#: net.down is published in KB/s; the bridge reports B/s.
BYTES_PER_KB = 1024.0

#: Keep scaled values short so the wire frame stays readable.
ROUND_DIGITS = 3

#: Window state for the bridge child: hidden console, no popping window.
SW_HIDE = 0

#: Piped stdout needs buffering for readline() to be efficient.
BRIDGE_BUFSIZE = -1

#: Encoding of the bridge output (it sets UTF-8 on stdout itself).
BRIDGE_ENCODING = "utf-8"

#: Mapping table. Order is the catalog order and the tie-break priority:
#:   "names" are the sensor names, tried in the listed order (CPU Package wins
#:   over Core Max).  "hw_names" is an optional hardware-name filter, needed
#:   where two pieces of hardware share a sensor name: LHM publishes a Load
#:   sensor called "Memory" for both "Total Memory" and "Virtual Memory", and
#:   only the former is what mem.used means (TASKS.md M5 writes that triple as
#:   Memory / Total Memory / Load).
#:   "first_with_traffic" picks the first candidate that actually moved bytes
#:   (idle NICs all report 0 B/s, and picking one of those shows a dead line).
RULES = (
    {"id": "cpu.total", "hwtypes": ("Cpu",), "names": ("CPU Total",),
     "kind": "Load", "scale": 1.0,
     "label": "CPU", "unit": "%", "min": 0, "max": 100},
    {"id": "cpu.temp", "hwtypes": ("Cpu",), "names": ("CPU Package", "Core Max"),
     "kind": "Temperature", "scale": 1.0,
     "label": "TEMP", "unit": "C", "min": 0, "max": 110},
    {"id": "mem.used", "hwtypes": ("Memory",), "names": ("Memory", "Total Memory"),
     "hw_names": ("Total Memory",), "kind": "Load", "scale": 1.0,
     "label": "MEM", "unit": "%", "min": 0, "max": 100},
    {"id": "net.down", "hwtypes": ("Network",), "names": ("Download Speed",),
     "kind": "Throughput", "scale": 1.0 / BYTES_PER_KB, "first_with_traffic": True,
     "label": "NET", "unit": "KB/s", "min": 0, "max": 8000},
    {"id": "gpu.load", "hwtypes": ("GpuNvidia",), "names": ("GPU Core",),
     "kind": "Load", "scale": 1.0,
     "label": "GPU", "unit": "%", "min": 0, "max": 100},
    {"id": "gpu.temp", "hwtypes": ("GpuNvidia",), "names": ("GPU Core",),
     "kind": "Temperature", "scale": 1.0,
     "label": "GTEMP", "unit": "C", "min": 0, "max": 110},
)

#: Every metric id this module can ever publish, in catalog order.
METRIC_IDS = tuple(rule["id"] for rule in RULES)

#: Metric ids a machine without that hardware simply never publishes.
OPTIONAL_IDS = ("gpu.load", "gpu.temp")

# ---------------------------------------------------------------------------
# Sensor frame -> metric values
# ---------------------------------------------------------------------------


def _is_number(value):
    """True for int/float but not bool (bool is an int subclass)."""
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def _sensor_value(sensor):
    """The finite numeric value of one bridge sensor dict, else None."""
    value = sensor.get("value")
    if not _is_number(value):
        return None
    value = float(value)
    if value != value or value in (float("inf"), float("-inf")):
        return None
    return value


def _match_rule(rule, sensors):
    """Best matching sensor value for one rule, or None if nothing matches."""
    hw_names = rule.get("hw_names")
    best_rank = None
    best_value = None
    for sensor in sensors:
        if sensor.get("hwtype") not in rule["hwtypes"]:
            continue
        if hw_names and sensor.get("hw") not in hw_names:
            continue
        if sensor.get("kind") != rule["kind"]:
            continue
        name = sensor.get("name")
        if name not in rule["names"]:
            continue
        value = _sensor_value(sensor)
        if value is None:
            continue
        if rule.get("first_with_traffic") and value > 0.0:
            return value
        rank = rule["names"].index(name)
        if best_rank is None or rank < best_rank:
            best_rank = rank
            best_value = value
    return best_value


def resolve_frame(frame):
    """Map one bridge frame (a list of sensor dicts) to {metric_id: value}.

    Invalid sensors are simply absent from the frame (the bridge never emits
    null), so an id that does not resolve must not appear in the result --
    callers turn that absence into None and the terminal shows "--".
    Never raises: junk entries are skipped.
    """
    out = {}
    if not isinstance(frame, list):
        return out
    sensors = [s for s in frame if isinstance(s, dict)]
    if not sensors:
        return out
    for rule in RULES:
        value = _match_rule(rule, sensors)
        if value is None:
            continue
        if rule["scale"] == 1.0:
            out[rule["id"]] = value                       # already the protocol unit
        else:
            out[rule["id"]] = round(value * rule["scale"], ROUND_DIGITS)
    return out


def catalog_metric(rule, interval_ms=BRIDGE_INTERVAL_MS):
    """Catalog entry (PROTOCOL.md s.6) for one rule -- no sensor data in it."""
    return {
        "id": rule["id"],
        "label": rule["label"],
        "unit": rule["unit"],
        "type": "number",
        "min": rule["min"],
        "max": rule["max"],
        "interval_ms": int(interval_ms),
    }


def _rule_for(metric_id):
    for rule in RULES:
        if rule["id"] == metric_id:
            return rule
    return None


# ---------------------------------------------------------------------------
# Subprocess plumbing
# ---------------------------------------------------------------------------


def _hidden_startupinfo():
    """STARTUPINFO that hides the bridge's console window (None elsewhere).

    CREATE_NO_WINDOW is the usual shortcut, but under a restricted token
    CreateProcess fails outright with 0xC0000142, so hide the window instead.
    """
    factory = getattr(subprocess, "STARTUPINFO", None)
    if factory is None:
        return None
    try:
        info = factory()
        info.dwFlags |= subprocess.STARTF_USESHOWWINDOW
        info.wShowWindow = SW_HIDE
        return info
    except (AttributeError, ValueError, OSError):
        return None


def _spawn_bridge(bridge_path, interval_ms):
    """Start the bridge as a console child with a stdout pipe. May raise.

    cwd must be the directory holding the bridge: it loads
    LibreHardwareMonitorLib.dll and friends from next to itself.
    """
    directory = os.path.dirname(os.path.abspath(bridge_path))
    args = [str(bridge_path), "--interval=%d" % int(interval_ms)]
    return subprocess.Popen(
        args,
        cwd=directory or None,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        bufsize=BRIDGE_BUFSIZE,
        startupinfo=_hidden_startupinfo(),
    )


# ---------------------------------------------------------------------------
# LhmSource -- newest bridge frame + arrival time, nothing else
# ---------------------------------------------------------------------------


class LhmSource:
    """Thread-safe view of the newest LibreHardwareMonitor frame.

    Lifecycle: start() launches a private supervisor thread that spawns the
    bridge, reads it line by line, and respawns it with the s.1 backoff ladder
    whenever it exits. stop() terminates the child and joins the thread.

    Values follow three rules (TASKS.md M5):
      * a frame carries only the sensors that are valid right now;
      * a metric that was resolved before but is missing from the newest frame
        is None (PROTOCOL.md s.9: null, never the previous value);
      * once the newest frame is STALE_AFTER_S old, or the bridge is not
        running, every metric is None.
    """

    def __init__(self, bridge_path=DEFAULT_BRIDGE_PATH,
                 interval_ms=BRIDGE_INTERVAL_MS,
                 stale_after_s=STALE_AFTER_S,
                 backoff_s=BACKOFF_S,
                 clock=None,
                 log=None,
                 spawn=None):
        self.bridge_path = str(bridge_path)
        self.interval_ms = max(1, int(interval_ms))
        self.stale_after_s = float(stale_after_s)
        ladder = [float(step) for step in (backoff_s or ())
                  if _is_number(step) and step >= 0]
        self.backoff_s = tuple(ladder) if ladder else (0.0,)
        self._clock = clock if clock is not None else time.monotonic
        self._log = log if callable(log) else None
        self._spawn = spawn if callable(spawn) else _spawn_bridge
        self._lock = threading.Lock()
        self._values = {}
        self._seen = []
        self._last_frame_at = None
        self._alive = False
        self._stop = threading.Event()
        self._thread = None
        self._proc = None
        self.stats = collections.Counter()
        self.last_error = ""

    # -- logging ----------------------------------------------------------

    def _note(self, text):
        """Hand a message to the optional log callback; never raise."""
        if self._log is None:
            return
        try:
            self._log(text)
        except Exception:  # noqa: BLE001 - logging must never hurt the source
            self.stats["log_errors"] += 1

    def _fail(self, key, text):
        """Count an error, remember it, and report it once through _note."""
        self.stats[key] += 1
        self.last_error = text
        self._note(text)

    # -- lifecycle --------------------------------------------------------

    def start(self):
        """Spawn the bridge in the background. Returns True when started.

        Never blocks on the pipe and never raises: a missing exe is just a
        counted spawn error, and the supervisor keeps retrying.
        """
        with self._lock:
            if self._thread is not None and self._thread.is_alive():
                return False
            self._stop.clear()
            thread = threading.Thread(target=self._supervise, daemon=True,
                                      name="lhm-reader")
            self._thread = thread
        try:
            thread.start()
        except RuntimeError as exc:
            with self._lock:
                self._thread = None
            self._fail("thread_errors", "cannot start lhm-reader: %s" % exc)
            return False
        self.stats["starts"] += 1
        return True

    def stop(self, timeout=STOP_JOIN_S):
        """Terminate the bridge and stop reading. Idempotent, never raises."""
        self._stop.set()
        self._terminate()
        thread = self._thread
        if thread is not None and thread.is_alive():
            try:
                thread.join(timeout)
            except RuntimeError:
                pass
        with self._lock:
            self._alive = False
            self._last_frame_at = None
            self._thread = None
        self.stats["stops"] += 1
        return True

    def running(self):
        """True while the supervisor thread is alive."""
        thread = self._thread
        return thread is not None and thread.is_alive()

    def bridge_alive(self):
        """True once the current bridge process has delivered a frame."""
        with self._lock:
            return self._alive

    # -- supervisor / reader ----------------------------------------------

    def _backoff_for(self, attempt):
        index = min(max(attempt, 1) - 1, len(self.backoff_s) - 1)
        return self.backoff_s[index]

    def _supervise(self):
        """Spawn, read to EOF, wait per backoff ladder, spawn again."""
        attempt = 0
        try:
            while not self._stop.is_set():
                if attempt:
                    wait = self._backoff_for(attempt)
                    self.stats["restarts"] += 1
                    self._note("lhm bridge restart in %gs (attempt %d)"
                               % (wait, attempt))
                    if self._stop.wait(wait):
                        break
                proc = self._spawn_once()
                if proc is None:
                    attempt += 1
                    continue
                if self._stop.is_set():
                    self._terminate(proc)
                    break
                frames = self._pump(proc)
                self._mark_down(proc)
                if self._stop.is_set():
                    break
                attempt = 1 if frames else attempt + 1
        except Exception as exc:  # noqa: BLE001 - never take the daemon down
            self._fail("reader_errors", "lhm reader crashed: %s: %s"
                       % (type(exc).__name__, exc))
        finally:
            self._terminate()          # never orphan a bridge
            with self._lock:
                self._alive = False
                self._last_frame_at = None
                self._proc = None

    def _spawn_once(self):
        """One spawn attempt; returns the Popen or None (counted, never raises)."""
        try:
            proc = self._spawn(self.bridge_path, self.interval_ms)
        except Exception as exc:  # noqa: BLE001 - missing exe, bad cwd, ...
            self._fail("spawn_errors", "cannot start lhm bridge %r: %s: %s"
                       % (self.bridge_path, type(exc).__name__, exc))
            return None
        if proc is None or getattr(proc, "stdout", None) is None:
            self._fail("spawn_errors", "lhm bridge %r gave no stdout pipe"
                       % (self.bridge_path,))
            return None
        with self._lock:
            self._proc = proc
        self.stats["spawns"] += 1
        return proc

    def _pump(self, proc):
        """Read lines until EOF. Returns True if at least one frame arrived."""
        stream = proc.stdout
        got_frame = False
        while not self._stop.is_set():
            try:
                line = stream.readline()
            except (ValueError, OSError) as exc:
                if self._stop.is_set():
                    break
                self._fail("read_errors", "lhm bridge read failed: %s: %s"
                           % (type(exc).__name__, exc))
                break
            if not line:
                break                      # EOF: the bridge exited
            if len(line) > MAX_LINE_BYTES:
                self.stats["oversized_lines"] += 1
                continue
            if self._accept_line(line):
                got_frame = True
        return got_frame

    def _accept_line(self, line):
        """Parse and store one bridge line. Returns True if it was a frame."""
        if isinstance(line, (bytes, bytearray)):
            try:
                text = bytes(line).decode(BRIDGE_ENCODING, "replace")
            except Exception:  # noqa: BLE001 - undecodable garbage
                self.stats["bad_frames"] += 1
                return False
        else:
            text = str(line)
        text = text.strip()
        if not text:
            return False
        try:
            frame = json.loads(text)
        except (ValueError, RecursionError):
            self.stats["bad_frames"] += 1
            return False
        if not isinstance(frame, list):
            # A frame is always an array; anything else is not ours (s.20: ignore).
            self.stats["bad_frames"] += 1
            return False

        values = resolve_frame(frame)
        now = self._clock()
        with self._lock:
            self._values = values
            self._last_frame_at = now
            self._alive = True
            for metric_id in METRIC_IDS:
                if metric_id in values and metric_id not in self._seen:
                    self._seen.append(metric_id)
        self.stats["frames"] += 1
        if not values:
            self.stats["empty_frames"] += 1
        return True

    def _mark_down(self, proc):
        """Bridge is gone: everything is None until a new frame shows up."""
        with self._lock:
            if self._proc is proc:
                self._proc = None
            self._alive = False
            self._last_frame_at = None
            self._values = {}
        self.stats["exits"] += 1
        self._terminate(proc)

    def _terminate(self, proc=None):
        """Stop a child process (default: the current one). Never raises."""
        if proc is None:
            with self._lock:
                proc = self._proc
                self._proc = None
        if proc is None:
            return
        try:
            if proc.poll() is None:
                proc.terminate()
        except (OSError, ValueError):
            pass
        try:
            proc.wait(KILL_WAIT_S)
        except Exception:  # noqa: BLE001 - timeout or already gone
            try:
                proc.kill()
                proc.wait(KILL_WAIT_S)
            except Exception:  # noqa: BLE001
                pass
        stream = getattr(proc, "stdout", None)
        if stream is not None:
            try:
                stream.close()
            except (OSError, ValueError):
                pass

    # -- public views -----------------------------------------------------

    def available(self):
        """Metric ids resolved at least once, in catalog order."""
        with self._lock:
            return list(self._seen)

    def metrics(self):
        """Catalog entries for the metrics this bridge actually delivers.

        Only sensors that resolved at least once appear: a machine without a
        GpuNvidia card never sees gpu.load/gpu.temp in its catalog (s.6).
        """
        with self._lock:
            seen = list(self._seen)
        out = []
        for metric_id in seen:
            rule = _rule_for(metric_id)
            if rule is not None:
                out.append(catalog_metric(rule, self.interval_ms))
        return out

    def snapshot(self, now=None):
        """{metric_id: value or None} for every metric seen since start().

        now must come from the same clock as this object's clock (monotonic by
        default, which is what the daemon already uses).
        """
        with self._lock:
            seen = list(self._seen)
            if not seen:
                return {}
            if now is None:
                now = self._clock()
            fresh = (self._alive
                     and self._last_frame_at is not None
                     and _is_number(now)
                     and (float(now) - self._last_frame_at) < self.stale_after_s)
            if not fresh:
                return {metric_id: None for metric_id in seen}
            return {metric_id: self._values.get(metric_id) for metric_id in seen}

    def values(self):
        """snapshot() against the internal clock (fake-source compatible)."""
        return self.snapshot(self._clock())

    def tick(self, now=None):
        """No-op: the bridge pushes, so there is nothing to poll (kept so the
        daemon can drive either source the same way)."""
        return None


# ---------------------------------------------------------------------------
# Self-test: python -m pcmon.lhm   ->  prints ALL OK or the failing case names
# ---------------------------------------------------------------------------
# The bridge used here is a small local script that writes the same NDJSON
# arrays as the real one, so the test needs neither LibreHardwareMonitor nor
# admin rights.

_TEST_BRIDGE = '''import json
import sys
import time

FULL = [
    {"id": "/intelcpu/0/load/0", "path": "CPU / CPU Total", "name": "CPU Total",
     "hw": "Fake CPU", "hwtype": "Cpu", "kind": "Load", "value": 37.5, "unit": "%"},
    {"id": "/intelcpu/0/temperature/0", "path": "CPU / CPU Package",
     "name": "CPU Package", "hw": "Fake CPU", "hwtype": "Cpu",
     "kind": "Temperature", "value": 55.0, "unit": "C"},
    {"id": "/ram/load/0", "path": "Total Memory / Memory", "name": "Memory",
     "hw": "Total Memory", "hwtype": "Memory", "kind": "Load", "value": 42.0,
     "unit": "%"},
    {"id": "/nic/0/throughput/1", "path": "NIC0 / Download Speed",
     "name": "Download Speed", "hw": "Idle NIC", "hwtype": "Network",
     "kind": "Throughput", "value": 0.0, "unit": "B/s"},
    {"id": "/nic/1/throughput/1", "path": "NIC1 / Download Speed",
     "name": "Download Speed", "hw": "Busy NIC", "hwtype": "Network",
     "kind": "Throughput", "value": 2048.0, "unit": "B/s"},
]

NO_TEMP = [s for s in FULL if s["name"] != "CPU Package"]

def emit(frame):
    sys.stdout.write(json.dumps(frame) + "\\n")
    sys.stdout.flush()

for index in range(10):
    emit([] if index == 0 else (FULL if index < 5 else NO_TEMP))
    time.sleep(0.15)
sys.stdout.write("this line is not json\\n")
sys.stdout.flush()
time.sleep(0.05)
sys.exit(0)
'''


def _check(condition, message):
    if not condition:
        raise AssertionError(message)


def _poll(predicate, timeout, step=0.05):
    """Wait until predicate() is truthy; returns its value or None."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(step)
    return None


def _selftest_resolve():
    """The mapping table: hwtype + name + kind, never the path string."""
    frame = [
        {"id": "/intelcpu/0/load/0", "path": "Intel Xeon / CPU Total",
         "name": "CPU Total", "hw": "Intel Xeon", "hwtype": "Cpu",
         "kind": "Load", "value": 37.5, "unit": "%"},
        {"id": "/intelcpu/0/temperature/1", "name": "Core Max", "hw": "Intel",
         "hwtype": "Cpu", "kind": "Temperature", "value": 61.0, "unit": "C"},
        {"id": "/intelcpu/0/temperature/0", "name": "CPU Package", "hw": "Intel",
         "hwtype": "Cpu", "kind": "Temperature", "value": 59.0, "unit": "C"},
        {"id": "/ram/load/0", "name": "Memory", "hw": "Total Memory",
         "hwtype": "Memory", "kind": "Load", "value": 42.0, "unit": "%"},
        {"id": "/nic/0/throughput/1", "name": "Download Speed", "hw": "Idle",
         "hwtype": "Network", "kind": "Throughput", "value": 0.0, "unit": "B/s"},
        {"id": "/nic/1/throughput/1", "name": "Download Speed", "hw": "Busy",
         "hwtype": "Network", "kind": "Throughput", "value": 4096.0, "unit": "B/s"},
        # Decoys: right name, wrong hwtype / hw / kind, must never win.
        {"id": "/gpu0/load/0", "path": "CPU Total", "name": "CPU Total",
         "hw": "GPU", "hwtype": "GpuNvidia", "kind": "Load", "value": 99.0,
         "unit": "%"},
        {"id": "/intelcpu/0/load/1", "name": "CPU Total", "hw": "Intel",
         "hwtype": "Cpu", "kind": "Power", "value": 88.0, "unit": "W"},
        {"id": "/ram/load/1", "name": "Memory", "hw": "Virtual Memory",
         "hwtype": "Memory", "kind": "Load", "value": 71.0, "unit": "%"},
    ]
    out = resolve_frame(frame)
    _check(out.get("cpu.total") == 37.5, "cpu.total must come from Cpu/Load: " + repr(out))
    _check(out.get("cpu.temp") == 59.0,
           "cpu.temp must prefer CPU Package over Core Max: " + repr(out))
    _check(out.get("mem.used") == 42.0,
           "mem.used must be Total Memory's load, not Virtual Memory's: " + repr(out))
    _check(out.get("net.down") == 4.0,
           "net.down must take the busy NIC and convert B/s -> KB/s: " + repr(out))
    _check("gpu.load" not in out, "GPU decoy must not fill gpu.load: " + repr(out))

    gpu = [{"id": "/gpu-nvidia/0/load/0", "name": "GPU Core", "hw": "RTX",
            "hwtype": "GpuNvidia", "kind": "Load", "value": 12.0, "unit": "%"},
           {"id": "/gpu-nvidia/0/temperature/0", "name": "GPU Core", "hw": "RTX",
            "hwtype": "GpuNvidia", "kind": "Temperature", "value": 47.0,
            "unit": "C"}]
    gpu_out = resolve_frame(gpu)
    _check(gpu_out == {"gpu.load": 12.0, "gpu.temp": 47.0},
           "GPU sensors must map by kind: " + repr(gpu_out))

    _check(resolve_frame([]) == {}, "empty frame maps to nothing")
    _check(resolve_frame(None) == {}, "non-list frame maps to nothing")
    _check(resolve_frame([{"name": "CPU Total", "value": None}]) == {},
           "null values must not resolve")
    _check(resolve_frame([{"hwtype": "Cpu", "name": "CPU Total", "kind": "Load",
                           "value": float("nan")}]) == {},
           "NaN must not resolve")


def _selftest_catalog_rules():
    """Catalog metadata stays inside PROTOCOL.md s.6 limits."""
    _check(len(METRIC_IDS) == len(set(METRIC_IDS)), "metric ids must be unique")
    for rule in RULES:
        _check(len(rule["label"]) <= 6, "label must fit a small screen: " + rule["id"])
        _check(rule["label"].isascii() and rule["unit"].isascii(),
               "labels and units must be ASCII: " + rule["id"])
        metric = catalog_metric(rule)
        _check(metric["type"] == "number", "every mapped metric is numeric")
        _check(metric["interval_ms"] == BRIDGE_INTERVAL_MS, "interval_ms is the bridge cadence")
        _check(metric["min"] <= metric["max"], "range must make sense: " + rule["id"])
    _check("gpu.load" in OPTIONAL_IDS and "cpu.total" not in OPTIONAL_IDS,
           "optional ids are the GPU ones (no GPU => not in catalog)")


def _selftest_source():
    """End to end against a scripted bridge: frames, staleness, respawn, stop."""

    def spawn(_path, interval_ms):
        # No file on disk: the scripted bridge is handed to python with -c.
        return subprocess.Popen(
            [sys.executable, "-c", _TEST_BRIDGE, "--interval=%d" % interval_ms],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, bufsize=BRIDGE_BUFSIZE,
            startupinfo=_hidden_startupinfo())

    source = LhmSource("fake_bridge.py", interval_ms=100, spawn=spawn)
    try:
        _check(source.snapshot() == {}, "an unstarted source knows no metrics")
        _check(source.metrics() == [], "an unstarted source publishes no catalog")
        _check(source.start() is True, "start() must return True")
        _check(source.start() is False, "start() must not start a second reader")

        first = _poll(lambda: source.snapshot().get("cpu.total"), 5.0)
        _check(first is not None, "no cpu.total frame arrived within 5s")

        snap = source.snapshot()
        _check(snap.get("cpu.total") == 37.5, "cpu.total must be 37.5: " + repr(snap))
        _check(snap.get("mem.used") == 42.0, "mem.used must be 42.0: " + repr(snap))
        _check(snap.get("net.down") == 2.0,
               "net.down must be 2048 B/s -> 2 KB/s: " + repr(snap))
        _check("gpu.load" not in snap, "a GPU-less bridge must not publish gpu.load")

        ids = [metric["id"] for metric in source.metrics()]
        _check(ids == ["cpu.total", "cpu.temp", "mem.used", "net.down"],
               "catalog must follow METRIC_IDS order: " + repr(ids))

        # A missing sensor is None even though the frame itself is fresh.
        missing = _poll(lambda: (source.snapshot().get("cpu.total") is not None
                                 and source.snapshot().get("cpu.temp") is None), 5.0)
        _check(missing is not None,
               "an absent sensor must read as None while the frame is fresh")

        # Pure arithmetic staleness: no sleeping, no timing flakiness.
        fresh = _poll(lambda: any(value is not None
                                  for value in source.snapshot().values()), 6.0)
        _check(fresh is not None, "a fresh snapshot must carry values")
        now = time.monotonic()
        later = source.snapshot(now + STALE_AFTER_S + 10.0)
        _check(all(value is None for value in later.values()),
               "a stale snapshot must be all None: " + repr(later))

        # The scripted bridge exits after ~1.6s; the supervisor respawns it.
        dead = _poll(lambda: all(value is None
                                 for value in source.snapshot().values()), 4.0)
        _check(dead is not None, "values must go None when the bridge exits")
        back = _poll(lambda: source.snapshot().get("cpu.total") is not None, 6.0)
        _check(back is not None, "the bridge must be respawned automatically")
        _check(source.stats["exits"] >= 1, "the exit must be counted")
        _check(source.stats["bad_frames"] >= 1, "garbage lines must be counted")
        _check(source.stats["spawns"] >= 2, "a respawn must be counted")
        _check(source.last_error == "" or isinstance(source.last_error, str),
               "last_error stays a string")
    finally:
        source.stop()
        _check(source.running() is False, "stop() must join the reader thread")
        _check(all(value is None for value in source.snapshot().values()),
               "a stopped source must report None, never old values")


def _selftest_cases():
    return [
        ("resolve", _selftest_resolve),
        ("catalog_rules", _selftest_catalog_rules),
        ("source", _selftest_source),
    ]


def _run_selftest():
    cases = _selftest_cases()
    failures = []
    for name, case in cases:
        try:
            case()
        except Exception as exc:  # noqa: BLE001 - one bad case must not stop the rest
            failures.append((name, exc))
    for name, exc in failures:
        print("FAIL " + name + ": " + type(exc).__name__ + ": " + str(exc))
    if failures:
        print("FAILED " + str(len(failures)) + "/" + str(len(cases)))
        return 1
    print("ALL OK")
    return 0


if __name__ == "__main__":
    sys.exit(_run_selftest())
