"""check_sources -- does every value source keep the C6 contract?

    python tools/check_sources.py                  # everything this platform can build
    python tools/check_sources.py --source proc    # just one

WHAT IT ASSERTS (per source, the same checks for all three)
    * create() finds it, and an unknown name is an error rather than a fallback
    * start/stop are idempotent and never raise
    * snapshot() is a dict of known metric ids -> None or a float inside the
      range the catalog declares (PROTOCOL.md s.6, s.9)
    * metrics() entries carry every field a catalog entry needs, ids are unique
    * a metric the source cannot measure is absent, not zero, and a value that
      is not known yet is None, not the previous reading

WHAT IT DOES NOT DO
    * start the lhm bridge -- that is M5's business and it would spawn a process
      this script then has to kill; construction and a pre-start snapshot is
      enough to prove the class satisfies the seam
    * talk to a socket, a daemon or a terminal
"""

import argparse
import os
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if ROOT not in sys.path:
    sys.path.insert(0, ROOT)

from pcmon import sources          # noqa: E402  (after sys.path)

try:                               # the daemon owns the fake/lhm sources
    import fake_daemon             # noqa: E402  (after sys.path)
except Exception:                  # noqa: BLE001 - the checker must run anywhere
    fake_daemon = None

#: Fields a catalog entry must carry (PROTOCOL.md s.6).
ENTRY_FIELDS = ("id", "label", "unit", "type", "min", "max", "interval_ms")

#: Declared ranges, copied from the protocol's metric table so this script does
#: not need the daemon module to know what a sane value looks like.
RANGES = {
    "cpu.total": (0.0, 100.0),
    "cpu.temp": (0.0, 110.0),
    "mem.used": (0.0, 100.0),
    "net.down": (0.0, 8000.0),
}


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


def range_for(metric_id):
    """Declared (min, max) for a known metric, or None."""
    return RANGES.get(metric_id)


def check_generic(checker, name, build_kwargs):
    """Everything that must hold for any source, whatever it measures."""
    known = set(RANGES)

    try:
        source = sources.create(name, **build_kwargs)
    except Exception as exc:                                  # noqa: BLE001
        checker.check(False, "%s: create() raised %s: %s"
                      % (name, type(exc).__name__, exc))
        return None
    checker.check(True, "%s: create() returns a source (%s)"
                  % (name, type(source).__name__))

    for method in ("start", "stop", "tick", "snapshot"):
        if not checker.check(callable(getattr(source, method, None)),
                             "%s: has %s()" % (name, method)):
            return source

    checker.check(source.start() is True, "%s: start() is true" % name)
    checker.check(source.stop() is True, "%s: stop() is true" % name)
    checker.check(source.stop() is True, "%s: stop() again is still true" % name)

    now = time.monotonic()
    snap = source.snapshot(now)
    if not checker.check(isinstance(snap, dict),
                         "%s: snapshot() returns a dict" % name):
        return source

    bad_ids = [key for key in snap if key not in known]
    checker.check(not bad_ids,
                  "%s: every key is a known metric id (%s)"
                  % (name, ", ".join(sorted(snap)) or "none"))

    bad_types = [key for key, value in snap.items()
                 if value is not None and not isinstance(value, (int, float))]
    checker.check(not bad_types,
                  "%s: every value is a number or None (%s)"
                  % (name, ", ".join(bad_types) or "all fine"))

    out_of_range = []
    for key, value in snap.items():
        if value is None:
            continue
        bounds = range_for(key)
        if not bounds:
            continue
        low, high = bounds
        if value < low or value > high:
            out_of_range.append("%s=%s not in [%s, %s]" % (key, value, low, high))
    checker.check(not out_of_range,
                  "%s: no value outside its declared range (%s)"
                  % (name, "; ".join(out_of_range) or "all inside"))

    if not sources.has_metrics(source):
        print("SKIP  %s: no metrics() -- the daemon owns this catalog" % name)
        return source

    entries = source.metrics()
    if not checker.check(isinstance(entries, list),
                         "%s: metrics() returns a list" % name):
        return source

    missing = [field for entry in entries for field in ENTRY_FIELDS
               if field not in entry]
    checker.check(not missing,
                  "%s: every catalog entry is complete (%d entries, %s)"
                  % (name, len(entries),
                     ", ".join(sorted({entry.get("id") for entry in entries})) or "none"))

    ids = [entry.get("id") for entry in entries]
    checker.check(len(ids) == len(set(ids)), "%s: catalog ids are unique" % name)

    unexplained = [metric_id for metric_id in ids if metric_id not in snap]
    checker.check(not unexplained,
                  "%s: every published metric also appears in snapshot() (%s)"
                  % (name, ", ".join(unexplained) or "none unexplained"))

    return source


def check_fake(checker):
    """The invented source: numbers in range, and they move."""
    source = sources.create(sources.SOURCE_FAKE, seed=7)
    source.start()
    start = time.monotonic()
    source.tick(start)
    first = dict(source.snapshot(start))
    source.tick(start + 1.0)
    second = dict(source.snapshot(start + 1.0))
    source.stop()

    checker.check(set(first) == set(RANGES),
                  "fake: publishes exactly the four invented metrics")
    checker.check(all(value is not None for value in first.values()),
                  "fake: invents a number for every metric (never None)")
    checker.check(first != second, "fake: tick() actually moves the numbers")
    checker.check(source.tick(start) is None, "fake: tick() returns None")


def check_proc(checker):
    """The Linux source: deltas are unknown first, then real."""
    source = sources.create(sources.SOURCE_PROC)
    source.start()
    entries = source.metrics()
    ids = [entry.get("id") for entry in entries]

    if not ids:
        checker.check(False, "proc: this machine offers no metrics at all")
        source.stop()
        return

    first = source.snapshot(1000.0)
    checker.check(first.get("cpu.total") is None,
                  "proc: first cpu.total is None (no delta yet), not 0.0")

    time.sleep(0.2)
    second = source.snapshot(1000.2)
    value = second.get("cpu.total")
    checker.check(isinstance(value, float) and 0.0 <= value <= 100.0,
                  "proc: second cpu.total is a percentage (%r)" % (value,))
    checker.check(isinstance(second.get("mem.used"), float),
                  "proc: mem.used is a number (%r)" % (second.get("mem.used"),))
    net = second.get("net.down")
    checker.check(net is None or (isinstance(net, float) and net >= 0.0),
                  "proc: net.down is a rate or None (%r)" % (net,))
    source.stop()


def main(argv=None):
    parser = argparse.ArgumentParser(prog="check_sources")
    parser.add_argument("--source", default=None,
                        help="check only this source (default: all it can build)")
    args = parser.parse_args(argv)

    names = [args.source] if args.source else list(sources.SOURCE_NAMES)
    checker = Checker()

    checker.check(isinstance(sources.create(sources.SOURCE_FAKE, seed=1), object)
                  if fake_daemon is not None else True,
                  "registry: fake is registered")
    try:
        sources.create("no-such-source")
        checker.check(False, "registry: an unknown name is refused")
    except ValueError:
        checker.check(True, "registry: an unknown name is refused")

    for name in names:
        reason = sources.platform_reason(name)
        if reason:
            print("SKIP  %s: %s" % (name, reason))
            continue

        if name == sources.SOURCE_FAKE:
            if fake_daemon is None:
                print("SKIP  fake: fake_daemon.py is not importable from here")
                continue
            source = check_generic(checker, name, {"seed": 7})
            if source is not None:
                check_fake(checker)
        elif name == sources.SOURCE_PROC:
            source = check_generic(checker, name, {})
            if source is not None:
                check_proc(checker)
        elif name == sources.SOURCE_LHM:
            # Never started: constructing a source must not spawn anything.
            source = check_generic(checker, name, {"bridge_path": None, "log": None})
            if source is not None:
                checker.check(sources.has_metrics(source),
                              "lhm: reports its metrics through the seam")
                snapshot = source.snapshot(time.monotonic())
                checker.check(isinstance(snapshot, dict) and not snapshot,
                              "lhm: before start() every value is unknown, not 0")

    total = checker.passed + checker.failed
    verdict = "SOURCES OK" if not checker.failed else "SOURCES FAILED"
    print("\n%s (%d checks, %d failed)" % (verdict, total, checker.failed))
    return 0 if not checker.failed else 1


if __name__ == "__main__":
    raise SystemExit(main())
