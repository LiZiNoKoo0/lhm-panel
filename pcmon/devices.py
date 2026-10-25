"""The device table: client_id -> key (PROTOCOL.md s.2.4).

This is the only place a terminal's key lives on the PC. One implementation,
imported by both the daemon and the `pair` tool -- the daemon must not have its
own copy of "what is a valid key", and the tool must not have its own idea of
where the table is.

Design notes that matter:

  * **No default key, ever.** A table with no entry means the device cannot
    connect. There is deliberately nothing to fall back to.
  * **Read on every lookup.** `pair --revoke` must take effect on a running
    daemon; caching would silently defeat revocation.
  * **Atomic writes.** A crash mid-save must not leave a truncated table that
    drops every device's key (`os.replace` on the same volume is atomic).
  * **Keys are opaque strings**, conventionally 32 hex chars (128 bits). The
    table does not care how they were produced, so a future scheme can change
    the generator without a storage migration.
"""

import json
import os
import threading
import time

#: Conventional key length in hex chars: 16 random bytes = 128 bits.
KEY_HEX_CHARS = 32

#: Legacy length: the v0.5 plain token was `secrets.token_hex(8)` = 16 chars
#: (64 bits). Accepted so a board can be adopted without a reflash; `pair --new`
#: mints 32, and the serial `KW` path replaces a legacy key with a new one.
LEGACY_KEY_HEX_CHARS = 16

DEFAULT_DEVICES_FILE = "devices.json"


def new_key():
    """Mint a key. 128 bits as 32 hex chars, the shape PROTOCOL.md s.2.1 signs."""
    import secrets

    return secrets.token_bytes(KEY_HEX_CHARS // 2).hex()


def looks_like_key(text):
    """Cheap shape check -- not a security decision, just a typo guard.

    Accepts the legacy 64-bit shape as well: an adopted v0.5 token is a real key
    that a board already holds, and refusing it here would force a reflash just
    to change the length of a secret.
    """
    if not isinstance(text, str) or len(text) not in (KEY_HEX_CHARS, LEGACY_KEY_HEX_CHARS):
        return False
    return all(c in "0123456789abcdef" for c in text.lower())


class DeviceTable:
    """client_id -> {key, added_at, last_seen, note}, persisted as JSON."""

    def __init__(self, path=None):
        self.path = path or DEFAULT_DEVICES_FILE
        self._lock = threading.Lock()

    # -- reading ----------------------------------------------------------

    def _load(self):
        try:
            with open(self.path, "r", encoding="utf-8") as handle:
                data = json.load(handle)
        except (OSError, ValueError):
            return {}
        devices = data.get("devices") if isinstance(data, dict) else None
        return devices if isinstance(devices, dict) else {}

    def entries(self):
        return self._load()

    def key_for(self, client_id):
        if not isinstance(client_id, str) or not client_id:
            return None
        entry = self._load().get(client_id)
        if isinstance(entry, dict):
            key = entry.get("key")
            return key if isinstance(key, str) and key else None
        return None

    def exists(self, client_id):
        return client_id in self._load()

    # -- writing ----------------------------------------------------------

    def _save(self, devices):
        directory = os.path.dirname(os.path.abspath(self.path))
        if directory and not os.path.isdir(directory):
            os.makedirs(directory, exist_ok=True)
        tmp = self.path + ".tmp"
        with open(tmp, "w", encoding="utf-8") as handle:
            json.dump({"version": 1, "devices": devices}, handle,
                      indent=2, sort_keys=True)
            handle.write("\n")
        os.replace(tmp, self.path)

    def add(self, client_id, key=None, note=""):
        """Store a device, minting a key when none is given. Returns the key."""
        key = key or new_key()
        with self._lock:
            devices = self._load()
            devices[client_id] = {
                "key": key,
                "added_at": int(time.time()),
                "last_seen": None,
                "note": note,
            }
            self._save(devices)
        return key

    def mark_seen(self, client_id, when=None):
        with self._lock:
            devices = self._load()
            entry = devices.get(client_id)
            if isinstance(entry, dict):
                entry["last_seen"] = int(when if when is not None else time.time())
                self._save(devices)

    def revoke(self, client_id):
        with self._lock:
            devices = self._load()
            if client_id not in devices:
                return False
            del devices[client_id]
            self._save(devices)
            return True

    def prune(self, days):
        """Drop devices unseen for `days`. Returns the ids that went away.

        Only devices that were seen at least once are candidates: a device
        paired and never used has no last_seen, and silently deleting it would
        turn "I paired it yesterday" into "why is it not connecting".
        """
        cutoff = time.time() - days * 86400.0
        removed = []
        with self._lock:
            devices = self._load()
            for client_id, entry in list(devices.items()):
                if not isinstance(entry, dict):
                    continue
                seen = entry.get("last_seen")
                if isinstance(seen, (int, float)) and seen < cutoff:
                    del devices[client_id]
                    removed.append(client_id)
            if removed:
                self._save(devices)
        return removed
