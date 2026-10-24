"""check_dashboard_style -- enforce the Dashboard Layout style guide on the page.

    python tools/check_dashboard_style.py [--dir pcmon/webstatic]

The style guide is a prompt, and a prompt is only as good as the check that
runs after it. This script reads the markup this project actually serves and
asserts the guide's three hard sections:

  1. [FORBIDDEN]  classes that must not appear at all
  2. [REQUIRED]   token chains every button / card / input must carry
  3. [COMPARE]    layout facts (dark sidebar w-64, 4-column KPI grid, ...)

It exits non-zero on the first failure with the file and the rule, so it can be
run by hand or wired into a loop later. Deliberately dumb: substring matching on
class attributes, no HTML parser, no dependencies.
"""

import argparse
import os
import re
import sys

FORBIDDEN_CLASSES = [
    "rounded-none", "rounded-3xl", "rounded-full",
    "shadow-2xl", "font-black", "font-extrabold",
    "text-5xl", "text-6xl", "text-7xl", "text-8xl",
]

#: Forbidden *patterns*, not just names (the guide lists both).
FORBIDDEN_PATTERNS = [
    (re.compile(r"rounded-(?:none|3xl|full)\b"), "rounded-none / rounded-3xl / rounded-full"),
    (re.compile(r"shadow-2xl\b"), "shadow-2xl"),
    (re.compile(r"text-[5-8]xl\b"), "text-5xl..text-8xl"),
    (re.compile(r"bg-clip-text|background-clip:\s*text"), "gradient text"),
    (re.compile(r"background:\s*linear-gradient\([^)]*#(?:6366f1|8b5cf6)", re.I), "purple/blue gradient"),
    (re.compile(r"border-left:\s*\d+px solid|border-l-\d"), "one-sided accent stripe"),
    (re.compile(r"backdrop-filter|backdrop-blur"), "glassmorphism as default"),
]

REQUIRED_CHAINS = [
    ("button", "rounded-lg", "font-medium", "text-sm", "transition-colors"),
    ("card", "bg-white", "rounded-xl", "shadow-sm", "border", "border-gray-100"),
    ("input", "bg-gray-50", "border", "border-gray-200", "rounded-lg", "text-sm",
     "focus:outline-none", "focus:ring-2", "focus:border-[#6366f1]", "transition-all"),
]

LAYOUT_FACTS = [
    ('sidebar', "w-64", "侧边导航必须是 w-64"),
    ('sidebar', "bg-gray-900", "侧边导航必须是深色 bg-gray-900"),
    ('kpi grid', "grid-cols-4", "KPI 必须是四列网格"),
    ('chart panel', "col-span-2", "主图表必须占 2/3 宽"),
    ('chart panel', "aspect-video", "主图表必须有 aspect-video"),
    # DEVIATION (2026-10-22, requested): the guide says the search box belongs in
    # the top toolbar. It now lives in the metrics card header instead, because
    # the dashboard has exactly one searchable list and a global-looking search
    # field that only filters one table is a lie. The toolbar keeps the
    # notification and user blocks; the search field must still exist and still
    # carry the full input token chain (checked above).
    ('toolbar', 'id="bell"', "顶部工具栏必须有通知入口"),
    ('search', 'type="search"', "页面上必须有搜索框（位置见上条说明）"),
]


def read(path):
    with open(path, "r", encoding="utf-8") as handle:
        return handle.read()


def strip_comments(text):
    """Drop /* ... */ so prose in a comment cannot trip a class-name check.

    The first run of this script failed on its own explanation of the rule it
    was enforcing -- the word `rounded-full` appeared in a comment saying not
    to use it. Rules apply to code, not to sentences about code.
    """
    return re.sub(r"/\*.*?\*/", "", text, flags=re.S)


def class_attributes(html):
    return re.findall(r'class="([^"]*)"', html)


def check(directory):
    problems = []
    html_path = os.path.join(directory, "index.html")
    css_path = os.path.join(directory, "tokens.css")
    js_path = os.path.join(directory, "app.js")
    for path in (html_path, css_path, js_path):
        if not os.path.isfile(path):
            problems.append("%s: missing" % path)
    if problems:
        return problems

    html = read(html_path)
    css = strip_comments(read(css_path))
    js = strip_comments(read(js_path))
    everything = "\n".join(
        class_attributes(html) + class_attributes(js) + [css])

    # 1. forbidden classes and patterns
    for name in FORBIDDEN_CLASSES:
        if re.search(r"(?<![\w-])" + re.escape(name) + r"(?![\w-])", everything):
            problems.append("forbidden class present: %s" % name)
    for pattern, label in FORBIDDEN_PATTERNS:
        if pattern.search(everything):
            problems.append("forbidden pattern present: %s" % label)

    # 2. required token chains: every element that declares one of the first
    #    tokens must carry the whole set. Cards are matched by the shadow-sm
    #    anchor, buttons by <button ...>, inputs by <input ...>.
    for tag in ("button", "input"):
        for match in re.finditer(r"<%s\b[^>]*class=\"([^\"]*)\"" % tag, html, re.S):
            classes = match.group(1).split()
            chain = next(c for c in REQUIRED_CHAINS if c[0] == tag)[1:]
            missing = [token for token in chain if token not in classes]
            if missing:
                problems.append("<%s> missing tokens: %s" % (tag, ", ".join(missing)))

    # A card is "a rounded panel that sits on the page", i.e. rounded-xl. Anchoring
    # on rounded-xl rather than on shadow-sm keeps ordinary chrome (the top bar is
    # bg-white + shadow-sm but not a card) out of the rule.
    card_chain = next(c for c in REQUIRED_CHAINS if c[0] == "card")[1:]
    for classes_text in class_attributes(html) + class_attributes(js):
        classes = classes_text.split()
        if "rounded-xl" in classes:
            missing = [token for token in card_chain if token not in classes]
            if missing:
                problems.append("card missing tokens: %s (in %r)"
                                % (", ".join(missing), classes_text[:60]))

    # 3. layout facts
    for _, needle, message in LAYOUT_FACTS:
        if needle not in html:
            problems.append("layout: %s (missing %r)" % (message, needle))

    # 4. the page must have loading and empty states (guide: [FORBIDDEN]
    #    "忽略加载状态和空状态")
    if "skeleton" not in css or "renderSkeletons" not in js:
        problems.append("no loading skeleton (<div class=\"skeleton\"> + renderSkeletons)")
    if 'id="table-empty"' not in html or 'id="preview-empty"' not in html:
        problems.append("no empty state for table/preview")

    # 5. reduced motion must exist (guide: 自检 & a11y)
    if "prefers-reduced-motion" not in css:
        problems.append("no prefers-reduced-motion fallback")

    # 6. every utility class used in markup must exist in tokens.css, otherwise
    #    it silently does nothing and the design drifts without anyone noticing
    defined = set()
    for raw in re.findall(r"\.([^\s,{}>+~]+)", css):
        name = raw.replace("\\", "")
        for pseudo in (":focus-visible", ":focus", ":hover", ":active"):
            if name.endswith(pseudo):
                name = name[: -len(pseudo)]
        defined.add(name)
    used = set()
    for classes_text in class_attributes(html) + class_attributes(js):
        for token in classes_text.split():
            used.add(token)
    # Component classes are defined in this same file but are not utilities, so
    # they are allowed to be absent from the utility list above.
    known_components = {"pill", "pill-live", "pill-lost", "pill-down", "pill-wait",
                        "nav-item", "kpi", "kpi-value", "data-table", "skeleton",
                        "track", "chart-canvas", "panel-canvas", "tabular",
                        "page", "active", "open"}
    unknown = sorted(t for t in used if t not in defined and t not in known_components)
    if unknown:
        problems.append("classes with no rule in tokens.css: %s" % ", ".join(unknown))

    return problems


def main(argv=None):
    here = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    parser = argparse.ArgumentParser(prog="check_dashboard_style")
    parser.add_argument("--dir", default=os.path.join(here, "pcmon", "webstatic"))
    args = parser.parse_args(argv)

    problems = check(args.dir)
    if problems:
        print("STYLE FAIL (%d)" % len(problems))
        for problem in problems:
            print("  - " + problem)
        return 1
    print("STYLE OK -- forbidden tokens absent, required chains intact, layout facts hold")
    return 0


if __name__ == "__main__":
    sys.exit(main())
