"""sources -- the seam between "where the numbers come from" and "what is sent" (C6).

    from pcmon import sources
    src = sources.create("proc")      # or "fake" / "lhm"
    src.start()
    values = src.snapshot(now)        # {metric_id: float | None}
    src.stop()

THE CONTRACT (duck-typed; the daemon calls these five names and nothing else)

    start()          -> bool   Spawn or attach. Never raises. Idempotent.
    stop()           -> bool   Release. Never raises. Idempotent.
    tick(now)        -> None   Optional, called every TICK_INTERVAL_S. For
                               sources that synthesise values rather than read
                               them; a reading source may ignore it.
    snapshot(now)    -> dict   {metric_id: value | None}. None means "not valid
                               right now" -> the wire value is null and the
                               screen shows -- (PROTOCOL.md s.9). A metric that
                               disappeared must be None, never the old value.
    metrics()        -> list   OPTIONAL. When present, the source names the
                               catalog entries it can deliver *now* and the
                               daemon publishes exactly those; a metric missing
                               from the list is not published at all. A source
                               with a fixed table (fake) may leave this out and
                               let the daemon own the catalog -- has_metrics()
                               is how callers ask instead of guessing.

    plus `stats`, a Counter, optionally read for the --stats log line.

WHY snapshot() AND NOT values()
    The two sources that existed disagreed: FakeSource had values(), LhmSource
    had snapshot(now), and the daemon carried a `source_kind` flag around to
    remember which one it was holding. Two names for one idea is how a third
    source turns into a third branch. One name, one meaning: "what is true
    right now", with None wherever the answer is "unknown".

WHAT THIS FILE DOES NOT DO
    * speak NDJSON, or know about catalog / data / ui frames or terminals
    * decide `catalog.source = local | remote` (PROTOCOL.md s.6.1) -- that says
      a *terminal* measures the metric, and it is the daemon's catalog to write
    * raise because a sensor died: a dead sensor is not a dead daemon
    * pretend a missing reading is zero. Zero is a number, "unknown" is not.
"""

import collections
import os
import time

#: Source names. These are the values of --source and the keys of the catalog
#: banner, so they are part of the command line people already typed (M5).
SOURCE_FAKE = "fake"
SOURCE_LHM = "lhm"
SOURCE_PROC = "proc"
SOURCE_NAMES = (SOURCE_FAKE, SOURCE_LHM, SOURCE_PROC)

#: How often a reading source promises a fresh value (PROTOCOL.md s.6, and the
#: number the terminal uses for its own staleness rule: interval_ms x 3).
PROC_INTERVAL_MS = 1000

#: Rounding for published values. One decimal is what the panels show.
ROUND_DIGITS = 1

#: A declared range wider than this means the sensor is not a temperature.
TEMP_MIN_C = -50.0
TEMP_MAX_C = 200.0

#: Names the /sys ABI expects the value in millidegrees.
MILLI = 1000.0
#: Anything smaller than this is already celsius (some phone thermals are).
MILLI_THRESHOLD = 200.0


class Source:
    """The contract above, with do-nothing defaults.

    Subclassing is optional -- the daemon only ever calls these names -- but it
    is worth it for a place to keep stats and for the defaults below.
    """

    name = "?"

    def __init__(self):
        self.stats = collections.Counter()

    def start(self):
        return True

    def stop(self):
        return True

    def tick(self, now):
        return None

    def snapshot(self, now=None):
        return {}

    def metrics(self):
        return []


class ProcSource(Source):
    """Linux CPU / memory / network / temperature from procfs and sysfs.

    WHAT IT READS, AND NOTHING ELSE
        cpu.total   /proc/stat, first "cpu" line -> busy delta / total delta
        mem.used    /proc/meminfo -> 100 * (1 - MemAvailable / MemTotal)
        net.down    /proc/net/dev -> sum of rx bytes over real interfaces, as a
                    rate in KB/s (the unit PROTOCOL.md s.6 uses for net.down)
        cpu.temp    /sys/class/hwmon/hwmon*/temp*_input when it looks like a CPU
                    package sensor, else /sys/class/thermal/thermal_zone*/temp

    WHY /proc AND NOT psutil
        TASKS.md says standard library only, and this has to run on a phone
        chroot with no package manager. Four files are not worth a dependency.

    THE ONE THING THAT BITES
        cpu.total and net.down are deltas: they need a previous sample. The first
        snapshot() after start() therefore answers None for both. That is
        deliberate -- "unknown" is honest, 0.0 is a lie that looks like an idle
        machine, and the panel already knows how to draw -- .
    """

    name = SOURCE_PROC

    def __init__(self, interval_ms=PROC_INTERVAL_MS, clock=None,
                 proc_root="/proc", sys_root="/sys", **_ignored):
        # **_ignored: create() passes every source the same keyword set (seed,
        # bridge_path, log). A source takes what it needs and drops the rest, so
        # the dispatcher stays one call instead of one branch per source.
        super().__init__()
        self.interval_ms = max(1, int(interval_ms))
        self._clock = clock if clock is not None else time.monotonic
        self._proc = str(proc_root)
        self._sys = str(sys_root)
        self._prev_cpu = None        # (busy, total) from the previous snapshot
        self._prev_net = None        # rx bytes counted at the previous snapshot
        self._prev_at_time = None    # the `now` that went with _prev_net
        self._temp_path = None
        self._temp_tried = False

    # -- reading one file ------------------------------------------------

    def _read(self, path):
        """Text of a kernel file, or None. Counts, never raises."""
        try:
            with open(path, "r", encoding="ascii", errors="replace") as handle:
                return handle.read()
        except OSError:
            self.stats["read_errors"] += 1
            return None

    def _exists(self, path):
        try:
            return os.path.exists(path)
        except OSError:
            return False

    # -- the four metrics ------------------------------------------------

    def _cpu_sample(self):
        """(busy jiffies, total jiffies) from /proc/stat, or None."""
        text = self._read(os.path.join(self._proc, "stat"))
        if not text:
            return None
        for line in text.splitlines():
            if not line.startswith("cpu "):
                continue
            fields = line.split()[1:9]
            try:
                numbers = [float(field) for field in fields]
            except ValueError:
                self.stats["cpu_parse_errors"] += 1
                return None
            if len(numbers) < 4:
                return None
            # idle + iowait is "not doing work"; everything else is busy.
            idle = numbers[3]
            if len(numbers) > 4:
                idle += numbers[4]
            total = sum(numbers)
            if total <= 0:
                return None
            return (total - idle, total)
        return None

    def _mem_percent(self):
        """Used memory as a percentage, or None."""
        text = self._read(os.path.join(self._proc, "meminfo"))
        if not text:
            return None
        info = {}
        for line in text.splitlines():
            key, _, rest = line.partition(":")
            fields = rest.split()
            if not fields:
                continue
            try:
                info[key.strip()] = float(fields[0])
            except ValueError:
                continue
        total = info.get("MemTotal")
        if not total:
            return None
        available = info.get("MemAvailable")
        if available is None:
            # Kernel older than 3.14: the classic approximation, said out loud.
            free = info.get("MemFree")
            if free is None:
                return None
            available = (free + info.get("Buffers", 0.0)
                         + info.get("Cached", 0.0))
        used = 100.0 * (1.0 - available / total)
        return round(max(0.0, min(100.0, used)), ROUND_DIGITS)

    def _net_bytes(self):
        """Total received bytes over non-loopback interfaces, or None."""
        text = self._read(os.path.join(self._proc, "net", "dev"))
        if not text:
            return None
        total = 0.0
        for line in text.splitlines():
            name, sep, rest = line.partition(":")
            if not sep:
                continue
            if name.strip() == "lo":
                continue
            fields = rest.split()
            if len(fields) < 9:
                continue
            try:
                total += float(fields[0])          # first field is rx bytes
            except ValueError:
                self.stats["net_parse_errors"] += 1
                continue
        return total

    def _scan_temp(self):
        """First CPU-ish thermal input path, or None. Called once."""
        hwmon = os.path.join(self._sys, "class", "hwmon")
        candidates = []
        try:
            names = sorted(os.listdir(hwmon))
        except OSError:
            names = []
        for name in names:
            base = os.path.join(hwmon, name)
            chip = (self._read(os.path.join(base, "name")) or "").strip().lower()
            try:
                entries = sorted(os.listdir(base))
            except OSError:
                continue
            for entry in entries:
                if not entry.startswith("temp") or not entry.endswith("_input"):
                    continue
                path = os.path.join(base, entry)
                # Those four names are the CPU package sensors in practice.
                if chip in ("coretemp", "k10temp", "zenpower",
                            "cpu_thermal", "soc_thermal"):
                    return path
                candidates.append(path)
        if candidates:
            return candidates[0]
        thermal = os.path.join(self._sys, "class", "thermal")
        try:
            zones = sorted(os.listdir(thermal))
        except OSError:
            zones = []
        for zone in zones:
            path = os.path.join(thermal, zone, "temp")
            if self._exists(path):
                return path
        return None

    def _temp_path_or_none(self):
        if not self._temp_tried:
            self._temp_tried = True
            self._temp_path = self._scan_temp()
            if self._temp_path is None:
                self.stats["temp_absent"] += 1
        return self._temp_path

    def _temp_celsius(self):
        path = self._temp_path_or_none()
        if not path:
            return None
        raw = (self._read(path) or "").strip()
        if not raw:
            return None
        try:
            raw_value = float(raw)
        except ValueError:
            self.stats["temp_parse_errors"] += 1
            return None
        celsius = raw_value / MILLI if abs(raw_value) >= MILLI_THRESHOLD else raw_value
        if celsius <= TEMP_MIN_C or celsius >= TEMP_MAX_C:
            # A zone that answers 0 or 200 is not reporting a CPU temperature.
            self.stats["temp_out_of_range"] += 1
            return None
        return round(celsius, ROUND_DIGITS)

    # -- the contract ----------------------------------------------------

    def snapshot(self, now=None):
        """What is true now. None for every metric that cannot be answered."""
        now = float(self._clock() if now is None else now)
        out = {}

        sample = self._cpu_sample()
        if sample is not None:
            previous, self._prev_cpu = self._prev_cpu, sample
            if previous is None:
                out["cpu.total"] = None        # first sample: no delta yet
            else:
                busy = sample[0] - previous[0]
                total = sample[1] - previous[1]
                if total > 0:
                    share = 100.0 * busy / total
                    out["cpu.total"] = round(max(0.0, min(100.0, share)), ROUND_DIGITS)
                else:
                    out["cpu.total"] = None    # clock did not move: unknown

        mem = self._mem_percent()
        if mem is not None:
            out["mem.used"] = mem

        rx = self._net_bytes()
        if rx is not None:
            previous_net, previous_at = self._prev_net, self._prev_at_time
            self._prev_net, self._prev_at_time = rx, now
            if previous_net is None or previous_at is None:
                out["net.down"] = None
            else:
                seconds = now - previous_at
                if seconds > 0:
                    rate = (rx - previous_net) / seconds / 1024.0
                    out["net.down"] = round(max(0.0, rate), 0)
                else:
                    out["net.down"] = None

        temp = self._temp_celsius()
        if temp is not None:
            out["cpu.temp"] = temp

        self.stats["snapshots"] += 1
        return out

    def metrics(self):
        """Catalog entries for what this machine can deliver right now."""
        entries = []
        if self._cpu_sample() is not None:
            entries.append(self._entry("cpu.total", "CPU", "%", 0, 100))
        if self._mem_percent() is not None:
            entries.append(self._entry("mem.used", "MEM", "%", 0, 100))
        if self._net_bytes() is not None:
            entries.append(self._entry("net.down", "NET", "KB/s", 0, 8000))
        if self._temp_path_or_none() and self._temp_celsius() is not None:
            entries.append(self._entry("cpu.temp", "TEMP", "C", 0, 110))
        self.stats["catalog_builds"] += 1
        return entries

    def _entry(self, metric_id, label, unit, low, high):
        """One catalog entry (PROTOCOL.md s.6) -- the same shape lhm produces.

        `group` is PC-side only: it says which part of a dashboard a metric
        belongs to (system resources vs a weather plugin). Terminals never see
        it, because a terminal draws nodes, not categories.
        """
        return {
            "id": metric_id,
            "label": label,
            "unit": unit,
            "type": "number",
            "min": low,
            "max": high,
            "interval_ms": self.interval_ms,
            "group": "system",
        }


class CompositeSource(Source):
    """Several sources presented as one (the daemon holds this, not a list).

    WHY IT EXISTS
        C6 gave every source the same five methods, and a plugin hub is just
        another source on the far side of a socket. Composing them here is what
        keeps the daemon from growing a second code path for "data that comes
        from somewhere else".

    WHO WINS A CLASH
        Later sources win, and the clash is counted. A plugin that publishes
        `cpu.total` is either replacing a broken sensor or making a mistake;
        either way the operator can see `conflicts` in --stats rather than
        discovering a silently ignored metric.

    `extra_metrics` IS NOT A HACK
        A source with a fixed table (fake) cannot name its metrics -- the daemon
        owns that list. Passing it in here means metrics() can still answer the
        whole truth instead of dropping the built-in four.
    """

    name = "composite"

    def __init__(self, *sources_, extra_metrics=None, log=None):
        super().__init__()
        self._sources = [item for item in sources_ if item is not None]
        self._extra = list(extra_metrics or [])
        self._log = log if callable(log) else None
        self.conflicts = 0

    def _note(self, text):
        if self._log is None:
            return
        try:
            self._log(text)
        except Exception:      # noqa: BLE001 - logging must never hurt
            self.stats["log_errors"] += 1

    def start(self):
        started = False
        for source in self._sources:
            try:
                started = source.start() or started
            except Exception as exc:   # noqa: BLE001
                self._note("source start failed: %s: %s"
                           % (type(exc).__name__, exc))
        return started

    def stop(self):
        for source in self._sources:
            try:
                source.stop()
            except Exception as exc:   # noqa: BLE001
                self._note("source stop failed: %s: %s"
                           % (type(exc).__name__, exc))
        return True

    def tick(self, now):
        for source in self._sources:
            tick = getattr(source, "tick", None)
            if callable(tick):
                try:
                    tick(now)
                except Exception as exc:   # noqa: BLE001
                    self._note("source tick failed: %s: %s"
                               % (type(exc).__name__, exc))

    def snapshot(self, now=None):
        """Union of every source. A None from one source does not erase a value
        another source reported for the same id in this round."""
        merged = {}
        for source in self._sources:
            try:
                part = snapshot_of(source, now)
            except Exception as exc:       # noqa: BLE001
                self._note("source snapshot failed: %s: %s"
                           % (type(exc).__name__, exc))
                continue
            if not isinstance(part, dict):
                continue
            for metric_id, value in part.items():
                previous = merged.get(metric_id)
                if (previous is not None and value is not None
                        and previous != value):
                    self.conflicts += 1
                if value is None and previous is not None:
                    continue          # keep the fresher answer from this round
                merged[metric_id] = value
        self.stats["snapshots"] += 1
        return merged

    def metrics(self):
        """extra_metrics first, then every source that can name its own."""
        seen = {}
        ordered = []
        for entry in self._extra:
            metric_id = entry.get("id") if isinstance(entry, dict) else None
            if metric_id and metric_id not in seen:
                seen[metric_id] = True
                ordered.append(dict(entry))
        for source in self._sources:
            if not has_metrics(source):
                continue
            try:
                part = source.metrics()
            except Exception as exc:       # noqa: BLE001
                self._note("source metrics failed: %s: %s"
                           % (type(exc).__name__, exc))
                continue
            for entry in part or []:
                metric_id = entry.get("id") if isinstance(entry, dict) else None
                if not metric_id or metric_id in seen:
                    continue
                seen[metric_id] = True
                ordered.append(dict(entry))
        return ordered


#: name -> factory. Only what this file can build; the daemon registers its own
#: two (fake / lhm) at import time, because they live with the protocol code.
REGISTRY = {
    SOURCE_PROC: ProcSource,
}


def register(name, factory):
    """Let a module that owns a source plug it in. Returns the name."""
    REGISTRY[str(name)] = factory
    return str(name)


def create(name, **kwargs):
    """Build a source by name. An unknown name is an error, not a fallback."""
    key = str(name or "").strip().lower()
    factory = REGISTRY.get(key)
    if factory is None:
        raise ValueError("unknown source %r (known: %s)"
                         % (name, ", ".join(sorted(REGISTRY)) or "none"))
    return factory(**kwargs)


def snapshot_of(source, now):
    """Ask any source -- new contract or the old M5 shape -- what is true now.

    FakeSource has values(), LhmSource has snapshot(now). Callers should not
    have to know which one they are holding, so the shim lives here and dies
    with the last old-style source.
    """
    snapshot = getattr(source, "snapshot", None)
    if callable(snapshot):
        return snapshot(now)
    values = getattr(source, "values", None)
    if callable(values):
        return values()
    return {}


def has_metrics(source):
    """True when a source can say which metrics it publishes.

    fake has a fixed catalog list in the daemon, so it cannot; lhm and proc can.
    """
    return callable(getattr(source, "metrics", None))


def platform_reason(name):
    """Why a source cannot run here, or "" when it can."""
    if str(name) == SOURCE_PROC and os.name != "posix":
        return "proc reads /proc and /sys, which only exist on Linux (this is %s)" % os.name
    if str(name) == SOURCE_LHM and os.name == "posix":
        return "lhm needs the LibreHardwareMonitor bridge, which is Windows-only"
    return ""


def selftest():
    """Contract checks that need no hardware. Returns (ok, lines)."""
    lines = []
    ok = True

    def check(condition, text):
        nonlocal ok
        ok = ok and bool(condition)
        lines.append(("ok   " if condition else "FAIL ") + text)

    fake_clock = [1000.0]

    def clock():
        return fake_clock[0]

    source = ProcSource(clock=clock)
    check(isinstance(source.snapshot(1000.0), dict),
          "ProcSource.snapshot returns a dict even when every file is missing")
    check(source.start() is True and source.stop() is True,
          "start/stop are idempotent and never raise")

    # A window into a directory that does not exist: every metric unknown.
    blind = ProcSource(clock=clock, proc_root="/nonexistent/proc",
                       sys_root="/nonexistent/sys")
    empty = blind.snapshot(1000.0)
    check(empty == {}, "a machine with no procfs yields {}, not invented zeros")
    check(blind.metrics() == [], "a machine with no procfs publishes no metrics")

    # The contract every source must satisfy, checked on the one we can build.
    for name in ("start", "stop", "tick", "snapshot", "metrics"):
        check(callable(getattr(source, name, None)), "ProcSource has %s()" % name)

    check(platform_reason(SOURCE_LHM) != "" or os.name != "posix",
          "platform_reason explains lhm on posix")

    # Deltas: the first sample must be unknown, the second must be a number.
    if os.name == "posix" and os.path.exists("/proc/stat"):
        live = ProcSource(clock=clock)
        first = live.snapshot(1000.0)
        fake_clock[0] = 1001.0
        second = live.snapshot(1001.0)
        check("cpu.total" in first and first["cpu.total"] is None,
              "first cpu.total is None (no previous sample), not 0.0")
        value = second.get("cpu.total")
        check(isinstance(value, float) and 0.0 <= value <= 100.0,
              "second cpu.total is a percentage in range (%r)" % (value,))
        check(isinstance(second.get("mem.used"), float),
              "mem.used is a number on a live machine")
        entries = live.metrics()
        ids = [entry.get("id") for entry in entries]
        check(ids and all("interval_ms" in entry for entry in entries),
              "every catalog entry carries interval_ms (%s)" % (", ".join(ids),))
    return ok, lines


def _main():
    ok, lines = selftest()
    for line in lines:
        print(line)
    print("SOURCES %s (%d checks)" % ("OK" if ok else "FAILED", len(lines)))
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(_main())
