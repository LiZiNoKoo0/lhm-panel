"""M4 acceptance: is a replayed tape the same tape?

Compares the frames one client actually received from `replay.py` against the
frames in the recording file.

Two earlier attempts at this were wrong, and both are worth remembering:

  1. Parsing the replay's *log* instead of the wire. The log interleaves every
     client that ever said hello, so a terminal reconnecting mid-tape looked
     like a frame-order mismatch that did not exist.
  2. Byte sizes as the witness. Two different frames can be the same length;
     only the content proves anything.

So: canonical JSON, field order ignored, `s` (the sequence counter) excluded
because it is per-connection and renumbering it is legal -- PROTOCOL section 4
only says it increases.

Timing is deliberately not compared. `--speed` exists to change timing, and M4's
contract is "the画面 is the same", not "the pacing is the same".
"""

import json
import sys

RECORDING = "recordings/session1.ndjson"
CAPTURED = "_captured.ndjson"


def load(path):
    frames = []
    with open(path, encoding="utf-8", errors="replace") as fh:
        for raw in fh:
            raw = raw.strip()
            if not raw or raw.startswith("["):
                continue
            try:
                msg = json.loads(raw)
            except ValueError:
                continue
            if isinstance(msg, dict) and msg.get("t"):
                frames.append(msg)
    return frames


def canon(msg):
    """Message without the fields that are allowed to differ on a replay."""
    return {k: v for k, v in msg.items() if k != "s"}


def main():
    recorded = load(RECORDING)
    captured = load(CAPTURED)
    print("recorded : %d frames" % len(recorded))
    print("captured : %d frames" % len(captured))

    n = min(len(recorded), len(captured))
    if n == 0:
        print("FAIL: nothing to compare")
        return 1

    mismatches = []
    for index in range(n):
        if canon(recorded[index]) != canon(captured[index]):
            mismatches.append(index)

    print("compared : first %d frames" % n)
    if not mismatches:
        print("PARITY OK -- every compared frame is identical")
        kinds = [f["t"] for f in captured[:n]]
        print("kinds    : %s" % ", ".join("%s×%d" % (k, kinds.count(k)) for k in dict.fromkeys(kinds)))
        return 0

    print("PARITY FAIL -- %d of %d frames differ" % (len(mismatches), n))
    for index in mismatches[:8]:
        print("  frame %d" % index)
        print("    recorded: %s" % json.dumps(canon(recorded[index]), ensure_ascii=False)[:160])
        print("    captured: %s" % json.dumps(canon(captured[index]), ensure_ascii=False)[:160])
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
