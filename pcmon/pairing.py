"""The 6-digit pairing code, as rules rather than as an HTTP handler.

PROTOCOL.md s.2.2. A browser cannot hold a pre-shared key, and it cannot prove
"the human is sitting at the PC" over the network -- so the proof is a short
code that only appears on the operator's own screen (CLI or desktop), and the
key is *derived* from it on both sides instead of being sent.

That derivation is the whole design:

    K = HKDF-SHA256(secret = code,
                    salt   = client_id || client_nonce || daemon_nonce,
                    info   = "pcmon-pair-v1", len = 16)

Both nonces are in the salt, so a code overheard by somebody who was not in the
exchange is not enough: they also need the two nonces, and the client_nonce
never leaves the client's memory. WebCrypto has HKDF, which is why the phone can
compute the same value without extra libraries.

What this module does NOT do: no sockets, no HTTP, no clock of its own (callers
pass `now`), so every rule below is a host test.

Rules carried here, one per P-number in the spec:
  P-1  6 digits, 60 s, single use
  P-2  only sha256(code) is stored -- never the code
  P-3  3 wrong codes from one source in 10 min locks that source for 10 min
  P-4  one pending request at a time; a new one replaces the old
  P-5  the caller derives the same key it will store, and must prove it works
"""

import hashlib
import hmac
import secrets
import threading
import time

CODE_DIGITS = 6
CODE_TTL_S = 60.0
MAX_TRIES = 3
TRY_WINDOW_S = 600.0
LOCKOUT_S = 600.0
KEY_HEX_CHARS = 32
INFO = b"pcmon-pair-v1"


def new_code():
    """6 digits, uniformly. `randbelow` avoids the modulo bias of `% 10**6`."""
    return "%06d" % secrets.randbelow(10 ** CODE_DIGITS)


def _hkdf_sha256(secret, salt, info, length):
    """HKDF (RFC 5869) with HMAC-SHA256. Ten lines, no dependency."""
    prk = hmac.new(salt, secret, hashlib.sha256).digest()
    out = b""
    block = b""
    counter = 1
    while len(out) < length:
        block = hmac.new(prk, block + info + bytes([counter]), hashlib.sha256).digest()
        out += block
        counter += 1
    return out[:length]


def derive_key(code, client_id, client_nonce, daemon_nonce):
    """The key both sides end up holding. Hex, 32 chars (128 bits)."""
    salt = ("%s|%s|%s" % (client_id, client_nonce, daemon_nonce)).encode("utf-8")
    raw = _hkdf_sha256(code.encode("utf-8"), salt, INFO, KEY_HEX_CHARS // 2)
    return raw.hex()


class Pending:
    """One outstanding pairing request. Holds a hash of the code, never the code."""

    __slots__ = ("pair_id", "client_id", "daemon_nonce", "code_hash", "created", "expires")

    def __init__(self, pair_id, client_id, daemon_nonce, code, now, ttl):
        self.pair_id = pair_id
        self.client_id = client_id
        self.daemon_nonce = daemon_nonce
        self.code_hash = hashlib.sha256(code.encode("ascii")).hexdigest()
        self.created = now
        self.expires = now + ttl

    def matches(self, code):
        given = hashlib.sha256(code.encode("ascii")).hexdigest()
        return hmac.compare_digest(self.code_hash, given)


class PairingCodes:
    """The rules. Thread-safe: picoserve-style servers call handlers concurrently."""

    def __init__(self, ttl=CODE_TTL_S, max_tries=MAX_TRIES,
                 try_window=TRY_WINDOW_S, lockout=LOCKOUT_S):
        self.ttl = float(ttl)
        self.max_tries = int(max_tries)
        self.try_window = float(try_window)
        self.lockout = float(lockout)
        self._lock = threading.Lock()
        self._pending = None      # P-4: at most one
        self._tries = {}          # source -> [monotonic-ish timestamps of failures]
        self._locked = {}         # source -> deadline

    # -- the operator's side ------------------------------------------------

    def begin(self, client_id, source="?", now=None):
        """Mint a code for `client_id`.

        Returns `(pair_id, code, daemon_nonce, expires_in_s)`. The daemon nonce
        goes back to the caller because it belongs in the browser's salt: the
        browser needs it to derive the same key, and it is useless without the
        code.

        The code is returned to the caller so it can be *printed where the
        operator is looking*. It is not stored.
        """
        now = time.monotonic() if now is None else now
        code = new_code()
        pair_id = secrets.token_hex(8)
        daemon_nonce = secrets.token_hex(16)
        with self._lock:
            self._pending = Pending(pair_id, client_id, daemon_nonce, code, now, self.ttl)
        return pair_id, code, daemon_nonce, self.ttl

    # -- the browser's side -------------------------------------------------

    def claim(self, pair_id, code, client_nonce, source="?", now=None):
        """Check a code and derive the key. Returns (client_id, key) or None.

        Fails closed in every direction: wrong code, wrong pair id, expired,
        reused, locked source, missing nonce.
        """
        now = time.monotonic() if now is None else now
        if not isinstance(code, str) or not isinstance(client_nonce, str) or not client_nonce:
            return None

        with self._lock:
            if self._is_locked(source, now):
                return None
            pending = self._pending
            if pending is None or pending.pair_id != pair_id:
                self._note_try(source, now)
                return None
            if now > pending.expires:
                # P-1: expired codes are dead, and they do not come back.
                self._pending = None
                self._note_try(source, now)
                return None
            if not pending.matches(code):
                self._note_try(source, now)
                return None

            # P-1: single use. Consume before deriving, so a crash cannot leave
            # a code that still works.
            self._pending = None
            key = derive_key(code, pending.client_id, client_nonce,
                             pending.daemon_nonce)
            return pending.client_id, key

    # -- housekeeping -------------------------------------------------------

    def pending(self):
        """What is waiting, for the operator's convenience (`pair --list`)."""
        with self._lock:
            pending = self._pending
            if pending is None:
                return None
            return {
                "pair_id": pending.pair_id,
                "client_id": pending.client_id,
                "expires_in_s": max(0.0, pending.expires - time.monotonic()),
            }

    def _is_locked(self, source, now):
        deadline = self._locked.get(source)
        if deadline is None:
            return False
        if now >= deadline:
            del self._locked[source]
            self._tries.pop(source, None)
            return False
        return True

    def _note_try(self, source, now):
        """P-3: count failures per source inside a sliding window."""
        tries = self._tries.setdefault(source, [])
        tries.append(now)
        cutoff = now - self.try_window
        while tries and tries[0] < cutoff:
            tries.pop(0)
        if len(tries) >= self.max_tries:
            self._locked[source] = now + self.lockout
            tries.clear()
