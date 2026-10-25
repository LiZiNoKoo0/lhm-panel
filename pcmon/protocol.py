"""pcmon.protocol -- wire protocol codec + capability helpers for PC-Monitor v0.4.

Single source of truth: PROTOCOL.md (read it before changing anything here).

This module is PURE COMPUTATION. It deliberately does NOT:
  * open sockets, connect, listen, accept, or broadcast
  * read/write files, flash, recordings, or config
  * print, log, spawn threads, or keep global mutable state
  * own the "s" (sequence) counter -- the caller assigns it per client
  * judge metric semantics: labels, units, colors, focus targets are opaque data
  * raise out of decode() on bad input -- it returns None instead

The only thing that writes to stdout is the __main__ self-test, and only when
run as `python -m pcmon.protocol`.

Wire format: NDJSON -- one JSON object per line, terminated by "\n", UTF-8.
encode() always emits pure ASCII (non-ASCII characters are \\uXXXX escaped,
which is a valid subset of UTF-8), so a frame can never contain a raw newline.

Forward compatibility (PROTOCOL.md section 20): unknown "t", unknown node
"type", and unknown fields are ignored by callers, never fatal. kind() returns
None for an unknown "t" so a receiver can simply skip such a frame.

This source file is intentionally pure ASCII (TASKS.md coding rules: PowerShell
5.1 on this host mangles GBK/UTF-8 console output).
"""

import copy
import hashlib
import hmac as _hmac
import json
import os
import secrets
import sys
import threading
import time

# ---------------------------------------------------------------------------
# Constants (all magic numbers live here)
# ---------------------------------------------------------------------------

PROTO_VERSION = 1
DEFAULT_PORT = 7802
DEFAULT_DAEMON_HOST = "127.0.0.1"

#: Terminal is considered disconnected after this long without any message.
HEARTBEAT_TIMEOUT_S = 15.0

#: Reconnect backoff ladder for terminals, in seconds (capped by last value).
RECONNECT_BACKOFF_S = (1, 2, 4, 8, 10)

#: Default staleness factor: 3x the advertised interval_ms.
STALE_FACTOR = 3.0

#: Every message type of the protocol, keyed by direction.
KINDS_TERM_TO_PC = ("discover", "hello", "auth", "event", "stat", "ping")
KINDS_PC_TO_TERM = (
    "here",
    "challenge",
    "deny",
    "catalog",
    "scenes",
    "ui",
    "data",
    "cmd",
    "time",
    "debug",
    "pong",
)
KINDS = KINDS_TERM_TO_PC + KINDS_PC_TO_TERM

#: Node types that only make sense with absolute geometry (used by callers).
PRIMITIVE_TYPES = ("rect", "path", "text", "num")
WIDGET_TYPES = ("bar", "line", "dot")

#: Node types whose "size" must be one of hello["fonts"].
SIZED_TYPES = ("text", "num", "bar")

DEFAULT_PRIMITIVES = ["rect", "path", "text", "num"]
DEFAULT_FONTS = [12, 16]
DEFAULT_INPUT = ["key"]
DEFAULT_FEATURES = {"backlight": False, "rtc": False, "storage": False}
DEFAULT_LIMITS = {
    "max_nodes": 32,
    "max_line_points": 60,
    "max_text_len": 16,
    "max_json_bytes": 4096,
}
DEFAULT_BG = "#000000"

#: Roles a terminal can declare in hello.
ROLES = ("thin", "thick")

#: Character sets a terminal can declare in hello.
CHARSETS = ("ascii", "utf8")

#: Authentication (PROTOCOL.md s.2, v0.6).
#:
#: The key never goes on the wire: the terminal proves it holds K by answering a
#: nonce, and the daemon proves it holds K too (`srv`), so an impostor cannot
#: harvest a valid answer. One definition of the answer, used by both sides --
#: mac_for() below. Do not re-implement it anywhere else.
AUTH_ALG = "hmac-sha256/128"
AUTH_ALGS = (AUTH_ALG,)
NONCE_BYTES = 16        # -> 32 hex chars on the wire
MAC_BYTES = 16          # 128-bit truncated HMAC -> 32 hex chars
SRV_BYTES = 8           # 64-bit truncated HMAC -> 16 hex chars
AUTH_TTL_MS = 5000      # the answer must arrive within this
SRV_LABEL = b"srv"

# ---------------------------------------------------------------------------
# Small internal helpers
# ---------------------------------------------------------------------------


def _as_list(value):
    """Return value as a list; None/missing becomes []. Never raises."""
    if value is None:
        return []
    if isinstance(value, (list, tuple)):
        return list(value)
    return [value]


def _is_number(value):
    """True for int/float but not bool (bool is an int subclass)."""
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def _is_ascii_text(value):
    """True if value is a str that is entirely ASCII."""
    if not isinstance(value, str):
        return True
    try:
        value.encode("ascii")
    except UnicodeEncodeError:
        return False
    return True


def _sanitize(value):
    """Deep-copy JSON-ish data, turning non-finite floats into None.

    PROTOCOL.md section 9: null means "invalid value -> show --". NaN/inf are
    not legal JSON and would corrupt the wire format, so they become null.
    """
    if isinstance(value, float):
        if value != value or value == float("inf") or value == float("-inf"):
            return None
        return value
    if isinstance(value, dict):
        return {k: _sanitize(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_sanitize(v) for v in value]
    return value


def _nearest_font(fonts, size):
    """Closest allowed font pixel height; ties pick the smaller font."""
    best = fonts[0]
    best_delta = abs(best - size)
    for font in fonts[1:]:
        delta = abs(font - size)
        if delta < best_delta or (delta == best_delta and font < best):
            best = font
            best_delta = delta
    return best


def _copy_node(node):
    """Deep copy a node so fit_nodes never mutates the caller's layout."""
    try:
        return copy.deepcopy(node)
    except Exception:
        return dict(node)


# ---------------------------------------------------------------------------
# Codec
# ---------------------------------------------------------------------------


def encode(msg):
    """Encode one message dict into a single NDJSON line of bytes.

    Returns b'<json>\\n'. Output is pure ASCII with compact separators, so a
    frame is always exactly one line no matter what text it carries.
    Raises TypeError if msg is not a dict or holds unserializable objects
    (that is a programming error, not a wire-data problem).
    """
    if not isinstance(msg, dict):
        raise TypeError("encode() expects a dict, got " + type(msg).__name__)
    payload = json.dumps(
        _sanitize(msg),
        ensure_ascii=True,
        allow_nan=False,
        separators=(",", ":"),
    )
    return payload.encode("ascii") + b"\n"


def decode(line):
    """Decode one NDJSON line (bytes or str) into a dict.

    Returns None for anything illegal: non-bytes/str input, invalid UTF-8,
    blank lines, malformed JSON, or JSON that is not an object. Never raises.
    A well-formed object with an unknown "t" is returned as-is (forward
    compatibility); use kind() to find out whether you can handle it.
    """
    if isinstance(line, memoryview):
        line = line.tobytes()
    if isinstance(line, bytearray):
        line = bytes(line)
    if isinstance(line, bytes):
        try:
            text = line.decode("utf-8")
        except UnicodeDecodeError:
            return None
    elif isinstance(line, str):
        text = line
    else:
        return None

    text = text.strip()
    if not text:
        return None
    try:
        value = json.loads(text)
    except (ValueError, RecursionError):
        return None
    if not isinstance(value, dict):
        return None
    return value


def kind(msg):
    """Classify a message: 'hello' | 'catalog' | 'ui' | ... or None.

    Returns None when msg is not a dict, has no string "t", or declares a "t"
    this protocol version does not know -- callers just ignore those.
    """
    if not isinstance(msg, dict):
        return None
    name = msg.get("t")
    if not isinstance(name, str):
        return None
    if name not in KINDS:
        return None
    return name


def is_pc_to_term(msg):
    """True if msg is a PC -> terminal message (carries "s" seq)."""
    return kind(msg) in KINDS_PC_TO_TERM


def is_term_to_pc(msg):
    """True if msg is a terminal -> PC message."""
    return kind(msg) in KINDS_TERM_TO_PC


# ---------------------------------------------------------------------------
# Capability trimming (PROTOCOL.md section 5, "PC side must trim")
# ---------------------------------------------------------------------------


def fit_nodes(nodes, hello_msg):
    """Return a new node list trimmed to what this terminal can actually draw.

    Rules, in order (PROTOCOL.md section 5):
      1. drop nodes whose "type" is not in hello["primitives"] +
         hello["widgets"] (skipped entirely if hello declares neither key)
      2. drop "text" nodes containing non-ASCII characters when
         hello["charset"] == "ascii"
      3. force "size" to the nearest entry of hello["fonts"] when it is not
         already allowed
      4. keep at most hello["limits"]["max_nodes"] nodes

    Draw order is preserved; valid nodes are packed forward so the count limit
    is filled with usable nodes. The input list and its nodes are not mutated.
    """
    src = _as_list(nodes)
    hello_msg = hello_msg if isinstance(hello_msg, dict) else {}

    limits = hello_msg.get("limits")
    limits = limits if isinstance(limits, dict) else {}

    primitives = _as_list(hello_msg.get("primitives"))
    widgets = _as_list(hello_msg.get("widgets"))
    declares_types = ("primitives" in hello_msg) or ("widgets" in hello_msg)
    allowed = set()
    for name in primitives + widgets:
        if isinstance(name, str):
            allowed.add(name)

    fonts = [f for f in _as_list(hello_msg.get("fonts")) if _is_number(f)]

    charset = hello_msg.get("charset")
    ascii_only = isinstance(charset, str) and charset.lower() == "ascii"

    max_nodes = limits.get("max_nodes")
    if not _is_number(max_nodes) or max_nodes < 0:
        max_nodes = None
    else:
        max_nodes = int(max_nodes)

    out = []
    for node in src:
        if not isinstance(node, dict):
            continue
        node_type = node.get("type")
        if not isinstance(node_type, str):
            continue
        if declares_types and node_type not in allowed:
            continue
        if ascii_only and node_type == "text" and not _is_ascii_text(node.get("text")):
            continue

        trimmed = _copy_node(node)
        size = trimmed.get("size")
        if fonts and _is_number(size) and size not in fonts:
            trimmed["size"] = _nearest_font(fonts, size)
        out.append(trimmed)

        if max_nodes is not None and len(out) >= max_nodes:
            break

    if max_nodes == 0:
        return []
    return out


# ---------------------------------------------------------------------------
# Builders (plain dicts -- no seq, caller adds "s")
# ---------------------------------------------------------------------------


def hello(client_id, role, w, h, **kw):
    """Build a hello message. kw overrides any field, including nested ones.

    Accepted keyword shortcuts: proto, token, accept, charset, depth, display,
    limits, primitives, widgets, fonts, input, features, plus any extra field
    name (e.g. fw="1.0.0") which is copied to the top level.
    """
    proto = kw.pop("proto", PROTO_VERSION)
    # v0.6 puts no key in hello. `token` is still accepted so a terminal can
    # speak v0.5 static-token mode to a daemon started with --auth static; when
    # it is absent we advertise the algorithms we can do instead.
    token = kw.pop("token", None)
    algos = kw.pop("auth", None)
    accept = kw.pop("accept", None)
    charset = kw.pop("charset", "ascii")
    depth = kw.pop("depth", 16)
    display = kw.pop("display", None)
    limits = kw.pop("limits", None)
    primitives = kw.pop("primitives", None)
    widgets = kw.pop("widgets", None)
    fonts = kw.pop("fonts", None)
    input_kinds = kw.pop("input", None)
    features = kw.pop("features", None)

    if accept is None:
        accept = ["ui", "data"] if role == "thin" else ["data"]

    disp = {"w": int(w), "h": int(h), "depth": int(depth)}
    if isinstance(display, dict):
        disp.update(display)

    lim = dict(DEFAULT_LIMITS)
    if isinstance(limits, dict):
        lim.update(limits)

    feats = dict(DEFAULT_FEATURES)
    if isinstance(features, dict):
        feats.update(features)

    msg = {
        "t": "hello",
        "proto": proto,
        **({"token": token} if token else
           {"auth": _as_list(algos if algos is not None else [AUTH_ALG])}),
        "client_id": client_id,
        "role": role,
        "accept": _as_list(accept),
        "display": disp,
        "charset": charset,
        "limits": lim,
        "primitives": _as_list(primitives if primitives is not None else DEFAULT_PRIMITIVES),
        "widgets": _as_list(widgets),
        "fonts": _as_list(fonts if fonts is not None else DEFAULT_FONTS),
        "input": _as_list(input_kinds if input_kinds is not None else DEFAULT_INPUT),
        "features": feats,
    }
    for key, value in kw.items():
        msg[key] = value
    return msg


def catalog(ver, metrics):
    """Build a catalog message. Metric dicts are copied, never rewritten."""
    items = []
    for metric in _as_list(metrics):
        items.append(dict(metric) if isinstance(metric, dict) else metric)
    return {"t": "catalog", "ver": ver, "metrics": items}


def ui(scene, ver, nodes, bg=DEFAULT_BG):
    """Build a ui message (thin terminals). Draw order == node order."""
    items = []
    for node in _as_list(nodes):
        items.append(dict(node) if isinstance(node, dict) else node)
    return {"t": "ui", "scene": scene, "ver": ver, "bg": bg, "nodes": items}


def data(values, colors=None):
    """Build a data message. values: id -> value (None means invalid -> "--").

    colors is the optional per-metric color override map ("c").
    """
    out = {}
    if isinstance(values, dict):
        for key, value in values.items():
            out[key] = value
    msg = {"t": "data", "v": out}
    if colors is not None:
        overrides = {}
        if isinstance(colors, dict):
            for key, value in colors.items():
                overrides[key] = value
        msg["c"] = overrides
    return msg


def cmd(action, target="*", **kw):
    """Build a cmd message. target "*" broadcasts, else a client_id.

    Extra kwargs become top-level fields, e.g. cmd("show", scene=1) or
    cmd("brightness", target="esp32-a", value=60).
    """
    msg = {"t": "cmd", "target": target, "action": action}
    for key, value in kw.items():
        msg[key] = value
    return msg


# ---------------------------------------------------------------------------
# Authentication (PROTOCOL.md s.2). Pure computation: no I/O, no clock.
# ---------------------------------------------------------------------------


def mac_for(key, nonce, client_id):
    """The one and only definition of the answer to a challenge.

        mac = HMAC-SHA256(K, nonce_ascii || client_id)[0:16]   -> 32 hex chars

    `nonce` is the ASCII of the hex string as it appears on the wire, not the
    decoded bytes, so a firmware and a Python client cannot disagree about what
    was signed. `client_id` is inside the MAC, so an observed answer cannot be
    replayed under another device's name.
    """
    if not isinstance(key, str) or not key:
        raise ValueError("mac_for: empty key")
    if not isinstance(nonce, str) or not nonce:
        raise ValueError("mac_for: empty nonce")
    if not isinstance(client_id, str) or not client_id:
        raise ValueError("mac_for: empty client_id")
    message = (nonce + client_id).encode("ascii")
    digest = _hmac.new(key.encode("ascii"), message, hashlib.sha256).digest()
    return digest[:MAC_BYTES].hex()


def srv_for(key, nonce):
    """The daemon's half of the proof: HMAC-SHA256(K, "srv" || nonce)[0:8].

    A terminal that checks this cannot be tricked into answering a challenge
    minted by somebody who does not hold K.
    """
    if not isinstance(key, str) or not key:
        raise ValueError("srv_for: empty key")
    message = SRV_LABEL + nonce.encode("ascii")
    digest = _hmac.new(key.encode("ascii"), message, hashlib.sha256).digest()
    return digest[:SRV_BYTES].hex()


def verify_mac(key, nonce, client_id, mac):
    """Constant-time check of a terminal's answer."""
    if not isinstance(mac, str):
        return False
    try:
        expected = mac_for(key, nonce, client_id)
    except (ValueError, UnicodeError):
        return False
    return _hmac.compare_digest(expected, mac.strip().lower())


def verify_srv(key, nonce, srv):
    """Constant-time check of the daemon's half (used by terminals)."""
    if not isinstance(srv, str):
        return False
    try:
        expected = srv_for(key, nonce)
    except (ValueError, UnicodeError):
        return False
    return _hmac.compare_digest(expected, srv.strip().lower())


def new_nonce():
    """16 random bytes as 32 hex chars: one per connection, single use."""
    return secrets.token_bytes(NONCE_BYTES).hex()


def challenge(nonce, key=None, alg=AUTH_ALG, ttl_ms=AUTH_TTL_MS):
    """Build the challenge the daemon sends right after hello.

    Pass `key` to include the `srv` proof. Without it the message is still
    valid; the terminal just cannot tell a real daemon from an impostor.
    """
    msg = {"t": "challenge", "nonce": nonce, "alg": alg, "ttl_ms": int(ttl_ms)}
    if key:
        msg["srv"] = srv_for(key, nonce)
    return msg


def auth(mac):
    """Build the terminal's answer to a challenge."""
    return {"t": "auth", "mac": mac}


# ---------------------------------------------------------------------------
# Freshness
# ---------------------------------------------------------------------------


def is_stale(entry, now, factor=STALE_FACTOR):
    """True if entry has not been refreshed within factor * interval_ms.

    entry = {"value": .., "ts": .., "interval_ms": ..} where ts is in the same
    unit/clock as now (seconds). Rules:
      * interval_ms <= 0 or missing  -> no expectation, never stale (False)
      * ts missing / not a number    -> stale (True): we cannot prove freshness
      * otherwise stale iff (now - ts) > factor * interval_ms / 1000
        (exactly at the threshold counts as fresh)
    """
    if not isinstance(entry, dict):
        return True

    interval = entry.get("interval_ms")
    if not _is_number(interval) or interval <= 0:
        return False

    ts = entry.get("ts")
    if not _is_number(ts):
        return True
    if not _is_number(now):
        return True

    interval = float(interval)
    return (float(now) - float(ts)) > (float(factor) * interval / 1000.0)


# ---------------------------------------------------------------------------
# Self-test: python -m pcmon.protocol   ->  prints ALL OK or failing case names
# ---------------------------------------------------------------------------


def _check(condition, message):
    if not condition:
        raise AssertionError(message)


def _selftest_roundtrip():
    """Every message type survives encode -> decode unchanged."""
    samples = [
        {"t": "discover"},
        {"t": "here", "ip": "192.168.1.100", "port": DEFAULT_PORT},
        hello("esp32-a", "thin", 240, 320, token="a3f9c2", fw="1.0.0"),
        hello(
            "esp32-b",
            "thin",
            160,
            80,
            token="t",
            charset="ascii",
            depth=16,
            fonts=[12, 16],
            primitives=["rect", "path", "text", "num"],
            widgets=[],
            limits={"max_nodes": 24, "max_line_points": 60, "max_text_len": 16, "max_json_bytes": 2048},
            features={"backlight": True, "rtc": True, "storage": True},
        ),
        hello("phone", "thick", 1080, 2400, depth=32, charset="utf8", accept=["data"]),
        {"t": "deny", "reason": "bad token"},
        catalog(
            3,
            [
                {"id": "cpu.total", "label": "CPU", "unit": "%", "type": "number",
                 "min": 0, "max": 100, "interval_ms": 1000},
                {"id": "music.title", "label": "NOW", "unit": "", "type": "string",
                 "interval_ms": 2000},
            ],
        ),
        {"t": "scenes", "s": 2, "list": [{"i": 0, "title": "CPU"}, {"i": 1, "title": "GPU"}]},
        ui(
            0,
            7,
            [
                {"type": "text", "x": 8, "y": 6, "text": "CPU", "size": 12, "color": "#888888"},
                {"type": "rect", "x": 8, "y": 54, "w": 224, "h": 6, "fill": "#222222"},
                {"type": "rect", "x": 8, "y": 54, "w": 112, "h": 6, "fill": "#44DD88",
                 "bind_w": "cpu.total"},
                {"type": "num", "x": 8, "y": 22, "bind": "cpu.total", "dec": 0, "unit": "%",
                 "size": 28, "color": "#FFFFFF"},
                {"type": "path", "x": 8, "y": 66, "pts": [0, 0, 10, 5, 20, 0], "color": "#44DD88"},
            ],
            bg="#000000",
        ),
        data({"cpu.total": 23.4, "cpu.temp": 61.0, "gpu.temp": None}),
        data({"cpu.total": 23.4}, {"cpu.total": "#FF4444"}),
        cmd("show", target="*", scene=1),
        cmd("brightness", target="esp32-a", value=60),
        cmd("clear"),
        {"t": "time", "s": 9, "epoch": 1729500234, "tz": "+08:00"},
        {"t": "debug", "s": 10, "text": "waiting for ui"},
        {"t": "event", "k": "page", "want": "next"},
        {"t": "event", "k": "page", "want": 3},
        {"t": "event", "k": "page", "want": [2, 3, 4]},
        {"t": "event", "k": "key", "id": "focus", "v": 2},
        {"t": "stat", "rssi": -52, "rx": 1234, "drop": 0, "heap": 88000, "uptime": 3600},
        {"t": "ping"},
        {"t": "pong"},
        {"t": "future_type", "whatever": [1, 2, {"deep": True}]},
    ]
    for msg in samples:
        encoded = encode(msg)
        _check(isinstance(encoded, bytes), "encode() must return bytes")
        _check(encoded.endswith(b"\n"), "encode() must end with newline: " + repr(encoded))
        _check(encoded.count(b"\n") == 1, "encode() must produce one line: " + repr(encoded))
        encoded.decode("ascii")  # must be pure ASCII on the wire
        decoded = decode(encoded)
        _check(decoded == msg, "roundtrip mismatch for t=" + str(msg.get("t")))
        if msg.get("t") in KINDS:
            _check(kind(decoded) == msg.get("t"),
                   "kind() disagreement for t=" + str(msg.get("t")))
        else:
            _check(kind(decoded) is None, "unknown t must classify as None")
    _check(decode(encode({"t": "ping"}).decode("utf-8")) == {"t": "ping"},
           "decode() must accept str input")
    _check(decode(encode({"t": "ping"}).rstrip(b"\n")) == {"t": "ping"},
           "decode() must accept a line without its newline")


def _selftest_bad_input():
    """Corrupted / illegal input must yield None, never an exception."""
    bad = [
        b"",
        b"\n",
        b"   \n",
        b'{"t":"hello"',
        b'{"t":"hello",}',
        b'{"t":"hello"} trailing',
        b"[1,2,3]",
        b'"just a string"',
        b"42",
        b"null",
        b"true",
        b"not json at all",
        b"\xff\xfe\x00\x01",
        "{}".encode("utf-8") + b"\xc3",
    ]
    for item in bad:
        _check(decode(item) is None, "decode() must return None for " + repr(item))
    for item in (None, 5, 5.0, {}, [], object()):
        _check(decode(item) is None, "decode() must return None for non-text " + repr(item))


def _selftest_unknown_type():
    """Unknown "t" is not fatal: kind() says None, decode() does not crash."""
    _check(kind({"t": "brand_new"}) is None, "kind() must return None for unknown t")
    decoded = decode(b'{"t":"brand_new","x":1}')
    _check(decoded is None or kind(decoded) is None,
           "unknown t must classify as None (decoded=" + repr(decoded) + ")")
    _check(kind({"t": "data", "v": {}}) == "data", "kind() must know data")
    _check(kind({}) is None, "kind() must be None without t")
    _check(kind(None) is None, "kind(None) must be None")
    _check(kind({"t": 5}) is None, "kind() must be None for non-string t")
    _check(kind({"t": "DATA"}) is None, "kind() must be case sensitive")
    _check(kind("hello") is None, "kind() must be None for non-dict")
    _check(encode({"t": "brand_new"}).startswith(b'{"t":"brand_new"}'),
           "encode() must pass unknown types through untouched")


def _selftest_fit_max_nodes():
    """fit_nodes honours limits.max_nodes."""
    caps = hello("d", "thin", 240, 320, limits={"max_nodes": 2},
                 primitives=["rect", "text", "path", "num"], fonts=[12, 16])
    nodes = [
        {"type": "rect", "x": 0, "y": 0, "w": 4, "h": 4},
        {"type": "text", "x": 10, "y": 0, "text": "A", "size": 12},
        {"type": "rect", "x": 20, "y": 0, "w": 4, "h": 4},
        {"type": "text", "x": 30, "y": 0, "text": "B", "size": 16},
    ]
    out = fit_nodes(nodes, caps)
    _check(len(out) == 2, "max_nodes=2 must keep 2 nodes, got " + str(len(out)))
    _check(out == nodes[:2], "max_nodes must keep the first nodes in draw order")

    zero = hello("d", "thin", 240, 320, limits={"max_nodes": 0})
    _check(fit_nodes(nodes, zero) == [], "max_nodes=0 must keep nothing")

    roomy = hello("d", "thin", 240, 320, limits={"max_nodes": 99})
    _check(len(fit_nodes(nodes, roomy)) == 4, "roomy limits must keep all nodes")

    _check(len(fit_nodes(nodes, {})) == 4, "missing limits must not drop nodes")
    _check(fit_nodes(None, caps) == [], "None nodes must become []")
    _check(fit_nodes([None, "x", 5, {"no_type": 1}], caps) == [],
           "malformed nodes must be dropped")


def _selftest_fit_primitives():
    """Nodes using unlisted types are dropped."""
    caps = hello("d", "thin", 240, 320, primitives=["rect", "text"], widgets=[])
    nodes = [
        {"type": "rect", "x": 0, "y": 0, "w": 4, "h": 4},
        {"type": "path", "x": 0, "y": 0, "pts": [0, 0, 10, 10]},
        {"type": "text", "x": 10, "y": 0, "text": "A", "size": 16},
        {"type": "line", "x": 0, "y": 0, "w": 10, "h": 10, "bind": "cpu.total"},
        {"type": "num", "x": 0, "y": 0, "bind": "cpu.total"},
    ]
    out = fit_nodes(nodes, caps)
    kinds_out = [n["type"] for n in out]
    _check(kinds_out == ["rect", "text"],
           "only primitive-listed types may survive, got " + repr(kinds_out))

    with_widget = hello("d", "thin", 240, 320, primitives=["rect"], widgets=["bar"])
    mixed = fit_nodes(
        [{"type": "rect", "x": 0, "y": 0, "w": 4, "h": 4},
         {"type": "bar", "x": 0, "y": 0, "w": 10, "h": 4, "bind": "cpu.total"}],
        with_widget,
    )
    _check([n["type"] for n in mixed] == ["rect", "bar"],
           "widgets list must also be allowed")


def _selftest_fit_fonts():
    """size must snap to the nearest allowed font size."""
    caps = hello("d", "thin", 240, 320, fonts=[12, 16], primitives=["text", "num"])
    out = fit_nodes([{"type": "num", "x": 0, "y": 0, "bind": "cpu.total", "size": 28}], caps)
    _check(out and out[0]["size"] == 16, "size=28 must snap to 16, got " + repr(out))

    out = fit_nodes([{"type": "text", "x": 0, "y": 0, "text": "A", "size": 14}], caps)
    _check(out and out[0]["size"] == 12, "size=14 must snap to 12 (tie -> smaller)")

    out = fit_nodes([{"type": "text", "x": 0, "y": 0, "text": "A", "size": 12}], caps)
    _check(out and out[0]["size"] == 12, "allowed size must be left alone")

    node = {"type": "text", "x": 0, "y": 0, "text": "A", "size": 28}
    fit_nodes([node], caps)
    _check(node["size"] == 28, "fit_nodes must not mutate the caller's nodes")


def _selftest_fit_charset():
    """ASCII-only terminals never receive non-ASCII text."""
    nodes = [
        {"type": "text", "x": 0, "y": 0, "text": "CPU", "size": 12},
        {"type": "text", "x": 0, "y": 20, "text": "\u6e29\u5ea6", "size": 12},
    ]
    ascii_caps = hello("d", "thin", 240, 320, charset="ascii",
                       fonts=[12, 16], primitives=["text"])
    out = fit_nodes(nodes, ascii_caps)
    _check(len(out) == 1 and out[0]["text"] == "CPU",
           "non-ASCII text must be dropped on ascii terminals, got " + repr(out))

    utf8_caps = hello("d", "thin", 240, 320, charset="utf8",
                      fonts=[12, 16], primitives=["text"])
    _check(len(fit_nodes(nodes, utf8_caps)) == 2, "utf8 terminals keep non-ASCII text")


def _selftest_is_stale():
    """Staleness boundary behaviour."""
    entry = {"value": 1.0, "ts": 100.0, "interval_ms": 1000}
    _check(is_stale(entry, 100.0) is False, "fresh entry must not be stale")
    _check(is_stale(entry, 102.9) is False, "2.9s < 3s must not be stale")
    _check(is_stale(entry, 103.0) is False, "exactly 3s is still fresh (boundary)")
    _check(is_stale(entry, 103.001) is True, "just past 3s must be stale")

    slower = {"value": 1.0, "ts": 100.0, "interval_ms": 2000}
    _check(is_stale(slower, 105.999) is False, "2s interval -> 6s budget")
    _check(is_stale(slower, 106.001) is True, "2s interval -> stale after 6s")

    _check(is_stale(entry, 100.5, factor=1.0) is False, "factor=1.0: 0.5s is fresh")
    _check(is_stale(entry, 101.5, factor=1.0) is True, "factor=1.0: 1.5s is stale")

    _check(is_stale({"value": 1.0, "ts": 100.0}, 99999.0) is False,
           "no interval_ms means no expectation")
    _check(is_stale({"value": 1.0, "ts": 100.0, "interval_ms": 0}, 99999.0) is False,
           "interval_ms=0 means continuous")
    _check(is_stale({"value": 1.0, "interval_ms": 1000}, 1.0) is True,
           "missing ts cannot be proven fresh")
    _check(is_stale(None, 1.0) is True, "missing entry is stale")
    _check(is_stale({"value": None, "ts": 100.0, "interval_ms": 1000}, 100.5) is False,
           "null value is a data problem, not a timing problem")


def _selftest_encode_wire():
    """encode() output is one pure-ASCII line, whatever the payload."""
    msg = {"t": "debug", "s": 10, "text": "\u6e29\u5ea6 OK\nsecond line"}
    encoded = encode(msg)
    _check(encoded.isascii(), "encode() output must be pure ASCII")
    _check(encoded.count(b"\n") == 1, "embedded newline must be escaped")
    _check(decode(encoded) == msg, "escaped text must roundtrip")

    nonfinite = encode({"t": "data", "v": {"a": float("nan"), "b": float("inf"), "c": 1.5}})
    _check(decode(nonfinite) == {"t": "data", "v": {"a": None, "b": None, "c": 1.5}},
           "non-finite floats must become null on the wire")

    try:
        encode(["not", "a", "dict"])
    except TypeError:
        pass
    else:
        raise AssertionError("encode() must reject non-dict input")


def _selftest_builders():
    """Builders produce the shapes the protocol documents."""
    h = hello("esp32-a", "thin", 240, 320, token="hk", fw="1.0.0")
    _check(h["t"] == "hello" and h["client_id"] == "esp32-a", "hello builder basics")
    _check(h["display"] == {"w": 240, "h": 320, "depth": 16}, "hello display shape")
    _check(h["accept"] == ["ui", "data"], "thin defaults to ui+data")
    _check(h["fw"] == "1.0.0", "extra kwargs land on the top level")
    _check(hello("phone", "thick", 1080, 2400)["accept"] == ["data"],
           "thick defaults to data only")

    c = catalog(1, [{"id": "cpu.total", "label": "CPU"}])
    _check(c == {"t": "catalog", "ver": 1,
                 "metrics": [{"id": "cpu.total", "label": "CPU"}]}, "catalog shape")

    u = ui(0, 1, [{"type": "text", "x": 0, "y": 0, "text": "A", "size": 12}])
    _check(u["bg"] == DEFAULT_BG and u["scene"] == 0 and u["ver"] == 1, "ui shape")

    d = data({"cpu.total": 23.4})
    _check(d == {"t": "data", "v": {"cpu.total": 23.4}}, "data shape without colors")
    _check(data({"x": 1}, {"x": "#FF0000"})["c"] == {"x": "#FF0000"}, "data color override")

    _check(cmd("clear") == {"t": "cmd", "target": "*", "action": "clear"}, "cmd defaults")
    _check(cmd("show", scene=1)["action"] == "show", "cmd kwargs")


def _selftest_auth():
    """The answer to a challenge, and the daemon's half of it.

    The two pinned vectors are the whole point of this case: if anyone changes
    what is signed (decoding the nonce, adding a separator, signing only the
    nonce) the numbers below stop matching and the build fails, instead of a
    firmware and a daemon quietly disagreeing at runtime.
    """
    key = "0123456789abcdef"
    nonce = "ab" * 16

    mac = mac_for(key, nonce, "esp32-a")
    _check(mac == "a45fdad8c13e4ab02e140693b88e4005",
           "pinned mac vector changed: %r" % mac)
    _check(len(mac) == 32, "mac must be 32 hex chars")
    _check(verify_mac(key, nonce, "esp32-a", mac), "a correct mac must verify")
    _check(verify_mac(key, nonce, "esp32-a", mac.upper()), "mac compare is case-insensitive")
    _check(not verify_mac(key, nonce, "esp32-b", mac),
           "the same mac must NOT verify under another client_id")
    _check(not verify_mac(key, "cd" * 16, "esp32-a", mac),
           "the same mac must NOT verify against another nonce")
    _check(not verify_mac(key, nonce, "esp32-a", mac[:31]), "a truncated mac must not verify")
    _check(not verify_mac("", nonce, "esp32-a", mac), "an empty key must not verify")

    srv = srv_for(key, nonce)
    _check(srv == "4f8c924f99c82f32", "pinned srv vector changed: %r" % srv)
    _check(verify_srv(key, nonce, srv), "a correct srv must verify")
    _check(not verify_srv(key, nonce, "0" * 16), "a bogus srv must not verify")

    ch = challenge(nonce, key=key)
    _check(kind(ch) == "challenge", "challenge must classify as challenge")
    _check(ch["alg"] == AUTH_ALG, "challenge must name the algorithm")
    _check(ch["ttl_ms"] == AUTH_TTL_MS, "challenge must carry the ttl")
    _check(verify_srv(key, ch["nonce"], ch["srv"]), "challenge must carry a valid srv")
    _check(decode(encode(ch)) == ch, "challenge must survive the wire")
    _check("srv" not in challenge(nonce), "challenge without a key carries no srv")

    ans = auth(mac)
    _check(kind(ans) == "auth", "auth must classify as auth")
    _check(decode(encode(ans)) == ans, "auth must survive the wire")

    _check(len(new_nonce()) == 32, "a nonce is 32 hex chars")
    _check(new_nonce() != new_nonce(), "nonces must not repeat")


def _selftest_hello_has_no_key():
    """v0.6 hello advertises algorithms; it never carries the key."""
    h = hello("esp32-a", "thin", 160, 80)
    _check("token" not in h, "v0.6 hello must not carry token")
    _check(h.get("auth") == [AUTH_ALG], "hello must advertise the auth algorithm")
    legacy = hello("esp32-a", "thin", 160, 80, token="0123456789abcdef")
    _check(legacy.get("token") == "0123456789abcdef",
           "static-token mode must still be expressible")
    _check("auth" not in legacy, "a static-token hello must not advertise hmac")


def _selftest_cases():
    return [
        ("roundtrip", _selftest_roundtrip),
        ("bad_input", _selftest_bad_input),
        ("unknown_t", _selftest_unknown_type),
        ("fit_max_nodes", _selftest_fit_max_nodes),
        ("fit_primitives", _selftest_fit_primitives),
        ("fit_fonts", _selftest_fit_fonts),
        ("fit_charset", _selftest_fit_charset),
        ("is_stale", _selftest_is_stale),
        ("encode_wire", _selftest_encode_wire),
        ("builders", _selftest_builders),
        ("auth", _selftest_auth),
        ("hello_no_key", _selftest_hello_has_no_key),
    ]


# Self-test wall clock budget. The cases are pure functions, so 10 s is already
# absurdly generous; it exists only so that a future case which blocks (a stray
# open(), a socket, a read from stdin) can never leave the terminal hanging.
SELFTEST_TIMEOUT_S = 10.0


def _selftest_watchdog(timeout_s=SELFTEST_TIMEOUT_S):
    """Kill the process if the self-test overruns. Returns a cancel() callable.

    A daemon thread with a plain sleep, not threading.Timer: Timer spawns and
    joins threads, and the whole point here is to depend on as little as
    possible while something else is already misbehaving.

    os._exit is deliberate. A blocked read cannot be interrupted from another
    thread on Windows, so sys.exit() would leave the process sitting there --
    exactly the hang this is meant to break.
    """
    deadline = time.monotonic() + float(timeout_s)
    done = threading.Event()

    def _guard():
        while not done.is_set():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                sys.stderr.write("WATCHDOG: selftest exceeded %gs\n" % float(timeout_s))
                sys.stderr.flush()
                os._exit(2)
            done.wait(min(remaining, 0.1))

    thread = threading.Thread(target=_guard, name="selftest-watchdog")
    thread.daemon = True
    thread.start()
    return done.set


def _run_selftest(timeout_s=SELFTEST_TIMEOUT_S):
    """Run every case, isolate failures, never block forever.

    Contract: this returns in well under SELFTEST_TIMEOUT_S with either
    "ALL OK" or one FAIL line per broken case. Each case is announced before
    it runs, so a hang (should one ever come back) names its own culprit in the
    last line of output.
    """
    cases = _selftest_cases()
    total = len(cases)
    cancel = _selftest_watchdog(timeout_s)
    failures = []
    try:
        for index, (name, case) in enumerate(cases, 1):
            print("[%d/%d] %s ..." % (index, total, name), end=" ")
            sys.stdout.flush()
            started = time.monotonic()
            try:
                case()
            except Exception as exc:  # noqa: BLE001 - a failing case must not stop the rest
                failures.append((name, exc))
                print("FAIL (%s)" % type(exc).__name__)
            else:
                print("ok (%d ms)" % int((time.monotonic() - started) * 1000))
            sys.stdout.flush()
    finally:
        cancel()

    for name, exc in failures:
        print("FAIL " + name + ": " + type(exc).__name__ + ": " + str(exc))
    if failures:
        print("FAILED " + str(len(failures)) + "/" + str(total))
        return 1
    print("ALL OK")
    return 0


if __name__ == "__main__":
    sys.exit(_run_selftest())
