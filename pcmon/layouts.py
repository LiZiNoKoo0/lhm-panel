"""Applied layouts: the one piece of PC state that must survive a restart.

PROTOCOL 22.5 makes the distinction that everything here exists to protect:

  * a **draft** is what the editor is fiddling with. It is not sent anywhere
    and it is not saved. Close the tab and it is gone.
  * the **applied** layout is what the glass is allowed to show. It carries a
    `ver` that moves by one each time somebody presses 「应用」, and it is the
    *only* thing a daemon restart may resend.

Keeping drafts out of this file is the whole point. A daemon that resent its
editor buffer after a restart would push half-finished work onto a device
nobody is looking at, and the failure looks exactly like a layout bug.

Why a JSON file and not a database: this is a handful of nodes edited by one
person on one machine, and being able to read the file in a text editor is
worth more than any query speed. Writes go through a temporary file and a
rename so a power cut cannot leave half a layout on disk.
"""

import copy
import json
import os
import threading

#: Node fields the editor may change. Anything else in a node is either derived
#: (nothing, today) or belongs to the daemon's own layout builder.
EDITABLE = ("type", "x", "y", "w", "h", "fill", "color", "width", "text",
            "bind", "dec", "unit", "size", "align", "r", "focusable", "focus",
            "bind_w")

#: Bumped to 2 if the on-disk shape ever changes in a way an older daemon cannot
#: read. Nothing reads it yet; it exists so the first such change has a home.
FORMAT = 1


class LayoutError(ValueError):
    """A layout the daemon refuses to apply. The message is shown to the user."""


def _clean_node(raw):
    """One node, with only the fields the wire allows, validated.

    Rejecting unknown fields rather than dropping them is deliberate: a typo in
    a field name would otherwise produce a node that silently ignores what the
    user typed, and the glass would show the default. Better to refuse the whole
    apply and say which field was wrong.
    """
    if not isinstance(raw, dict):
        raise LayoutError("节点必须是对象")
    node = {}
    for key, value in raw.items():
        if key not in EDITABLE:
            raise LayoutError("节点里有未知字段 %r" % (key,))
        node[key] = value

    kind = node.get("type")
    if kind not in ("rect", "text", "num"):
        # `path`/`line`/`bar`/`dot` are refused for the same reason the firmware
        # refuses them: the daemon must not send a page a thin panel will reject
        # whole. See panel-core/src/ui.rs.
        raise LayoutError("不支持的节点类型 %r（只支持 rect/text/num）" % (kind,))

    for field in ("x", "y"):
        if not isinstance(node.get(field), int):
            raise LayoutError("节点缺少整数 %s" % field)
    if kind == "rect":
        for field in ("w", "h"):
            if not isinstance(node.get(field), int) or node[field] < 0:
                raise LayoutError("rect 需要非负整数 %s" % field)
    if kind in ("text", "num") and not isinstance(node.get("size", 16), int):
        raise LayoutError("字号必须是整数")
    if kind == "num":
        bind = node.get("bind")
        if not isinstance(bind, str) or not bind:
            raise LayoutError("num 节点必须有 bind")
        dec = node.get("dec", 0)
        if not isinstance(dec, int) or not 0 <= dec <= 3:
            raise LayoutError("dec 只能是 0..3")
    return node


def _clean_scene(raw):
    if not isinstance(raw, dict):
        raise LayoutError("页面必须是对象")
    bg = raw.get("bg", "#000000")
    if not isinstance(bg, str):
        raise LayoutError("bg 必须是 \"#RRGGBB\" 字符串")
    nodes = raw.get("nodes")
    if not isinstance(nodes, list):
        raise LayoutError("页面缺少 nodes 数组")
    return {
        "title": str(raw.get("title") or ""),
        "bg": bg,
        "nodes": [_clean_node(n) for n in nodes],
    }


class AppliedLayouts:
    """The applied layouts, shared by the HTTP threads and the daemon loop."""

    def __init__(self, path):
        self.path = path
        self._lock = threading.Lock()
        self._ver = 1
        self._scenes = {}
        self._load()

    # -- disk ----------------------------------------------------------

    def _load(self):
        try:
            with open(self.path, "r", encoding="utf-8") as fh:
                data = json.load(fh)
        except FileNotFoundError:
            return
        except (OSError, ValueError):
            # A corrupt file must not stop the daemon from starting: an empty
            # applied set means the built-in layouts are used, which is a working
            # panel. Say so instead of dying.
            print("[layouts] %s 读不出来，用内置布局" % self.path)
            return
        try:
            ver = int(data.get("ver") or 1)
            scenes = {}
            for key, scene in (data.get("scenes") or {}).items():
                scenes[int(key)] = _clean_scene(scene)
        except (LayoutError, TypeError, ValueError) as exc:
            print("[layouts] %s 内容无效（%s），用内置布局" % (self.path, exc))
            return
        self._ver = max(1, ver)
        self._scenes = scenes

    def _save_locked(self):
        payload = {
            "format": FORMAT,
            "ver": self._ver,
            "scenes": {str(k): v for k, v in sorted(self._scenes.items())},
        }
        tmp = self.path + ".tmp"
        directory = os.path.dirname(self.path)
        if directory:
            os.makedirs(directory, exist_ok=True)
        with open(tmp, "w", encoding="utf-8") as fh:
            json.dump(payload, fh, ensure_ascii=False, indent=2)
            fh.write("\n")
        os.replace(tmp, self.path)

    # -- read ----------------------------------------------------------

    def ver(self):
        with self._lock:
            return self._ver

    def snapshot(self):
        """(ver, {scene: scene}) -- a copy, safe to iterate while others edit."""
        with self._lock:
            return self._ver, copy.deepcopy(self._scenes)

    def scene(self, index):
        with self._lock:
            scene = self._scenes.get(int(index))
            return None if scene is None else copy.deepcopy(scene)

    def indices(self):
        with self._lock:
            return sorted(self._scenes)

    def has_any(self):
        with self._lock:
            return bool(self._scenes)

    def reload(self):
        """Re-read the file, keeping whatever this process already has on failure.

        Called by the daemon when the editor announces an apply. The editor and
        the daemon are separate processes sharing one file, so "who has the
        newest version" is answered by re-reading rather than by trusting the
        frame that announced it -- two browser tabs can apply in either order,
        and a hand-edited JSON must work the same way.

        A corrupt or missing file leaves the in-memory copy untouched: a broken
        apply must never take a working panel down. Returns True when the disk
        was actually read.
        """
        with self._lock:
            previous_ver, previous_scenes = self._ver, self._scenes
            self._scenes = {}
            self._load()
            if not self._scenes and previous_scenes:
                # `_load` bail-outs all leave an empty dict; treat that as "keep
                # what worked" rather than "the user deleted everything".
                self._ver, self._scenes = previous_ver, previous_scenes
                return False
            return True

    # -- write ---------------------------------------------------------

    def apply(self, scenes):
        """Replace the applied set and bump `ver`. Returns the new ver.

        All-or-nothing: every scene is validated before anything is written, so
        a bad node in scene 2 cannot leave scene 0 applied and scene 2 stale.
        """
        if not isinstance(scenes, dict) or not scenes:
            raise LayoutError("至少要有一个页面")
        cleaned = {}
        for key, scene in scenes.items():
            try:
                index = int(key)
            except (TypeError, ValueError):
                raise LayoutError("页面编号必须是整数：%r" % (key,))
            if index < 0:
                raise LayoutError("页面编号不能是负数")
            cleaned[index] = _clean_scene(scene)
        with self._lock:
            self._scenes = cleaned
            self._ver += 1
            self._save_locked()
            return self._ver

    def revert_to(self, scenes, ver):
        """Put back a previously applied set, without bumping `ver`.

        Used only by the daemon's own tests: `apply` is the only thing a user
        can reach, and it always moves ver forward (PROTOCOL 22.5).
        """
        with self._lock:
            self._scenes = {int(k): _clean_scene(v) for k, v in scenes.items()}
            self._ver = max(1, int(ver))
            self._save_locked()
