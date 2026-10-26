"""PC-Monitor, packaged as one program with its own window.

Runs three things in one process:

  * the daemon, on a worker thread (its accept loop never returns),
  * the dashboard's HTTP server, on another worker thread,
  * a native window showing that dashboard: an embedded WebView2 control
    (pywebview), so no Edge window and no Edge profile is involved.

Why a window and not "open the browser": `webbrowser.open` fails silently inside a
frozen exe often enough that the first report of this program was "I double
clicked it and nothing happened". The process was running fine -- both ports
listening -- but a program whose only evidence of life is a console scrolled past
is a program that looks broken.

Why an embedded WebView2 window rather than `msedge --app=<url>`: Edge in app
mode still runs the user's browser stack. With a fresh `--user-data-dir` it opens
its first-run machinery on top of the page -- on this machine the window that
showed up was `edge://sync-confirmation-dialog/` ("we are now syncing your
browsing data"), i.e. an Edge page, not the dashboard. That dialog is not
something a launcher can reliably suppress with command-line flags, so the
window is now ours: `webview` (pywebview) hosts a WebView2 control inside a
window this process owns. WebView2 runtime ships with every machine that has
Edge, but no Edge process, no Edge profile, no first-run flow participates.

Edge app mode is kept as a fallback (`--window edge`) for machines where the
WebView2 runtime or pywebview is missing.

Things that had to be got right when frozen by PyInstaller:

  * **The working directory.** `daemon.token` and `layouts/applied.json` are
    written relative to CWD, and under a one-file exe CWD is wherever the user
    launched from -- possibly `C:\\Windows\\System32`. The launcher moves to the
    exe's directory first.

  * **The token.** Under PyInstaller `fake_daemon.__file__` points into the
    temporary extraction directory, so the daemon would write `daemon.token`
    there and it would vanish with the process, leaving the dashboard reporting
    "no token". The launcher owns the token and hands the same string to both
    sides, so no file is involved in the handshake at all.

  * **The bridge.** `lhm_bridge.exe` is bundled; `pcmon/lhm.py` resolves it out of
    `sys._MEIPASS` when frozen.

  * **Static files.** `pcmon/webui.py` finds `webstatic/` beside itself, which
    works unchanged because the package keeps its shape inside the bundle.

Usage:
    PC-Monitor.exe                      # window, real sensors, 7802 + 7803
    PC-Monitor.exe --source fake        # invented numbers
    PC-Monitor.exe --window webview     # force the embedded WebView2 window
    PC-Monitor.exe --window edge        # force `msedge --app=` (fallback)
    PC-Monitor.exe --window browser     # default browser instead of a window
    PC-Monitor.exe --window none        # headless: servers only, for a server box
    PC-Monitor.exe --token 0123456789abcdef    # keep the token the board already has
    PC-Monitor.exe --window-selftest    # open the window, log what it rendered, close
    PC-Monitor.exe --browser            # alias for --window browser
    PC-Monitor.exe --no-window          # alias for --window none
"""

import io
import os
import sys
import threading
import time

#: A windowed build has no console, and `print` on `None` raises. Turning stdout
#: into a sink keeps every `print` in this file harmless there, so the same source
#: builds either way.
if sys.stdout is None:
    sys.stdout = io.StringIO()
if sys.stderr is None:
    sys.stderr = io.StringIO()


def _app_dir():
    """Where the exe lives, or the source tree when running unpacked."""
    if getattr(sys, "frozen", False):
        return os.path.dirname(sys.executable)
    return os.path.dirname(os.path.abspath(__file__))


LOG_NAME = "PC-Monitor.log"


def _log_path():
    return os.path.join(_app_dir(), LOG_NAME)


def _log(text):
    """Append one line to the log beside the exe, and echo it to the console.

    The log exists because the first build of this program died with a CRT
    fail-fast (0xC0000409): the process was gone before Python could print
    anything, the console window vanished with it, and all the user could report
    was "I double clicked it and nothing happened". A file that survives the
    crash is worth more than any amount of console output.
    """
    line = "%s %s" % (time.strftime("%Y-%m-%d %H:%M:%S"), text)
    try:
        with io.open(_log_path(), "a", encoding="utf-8") as fh:
            fh.write(line + "\n")
    except OSError:
        pass
    try:
        print(line)
    except Exception:
        pass


def _opt(argv, name, default=None):
    if name in argv:
        try:
            return argv[argv.index(name) + 1]
        except IndexError:
            raise SystemExit("launcher: %s needs a value" % name)
    return default


def _ensure_token(root, explicit=None):
    """Read the token beside the exe, or mint one and say where it went.

    The board has to be told the same string through its config page, so a fresh
    token is only useful if the user is told about it -- printing it is the point.
    An explicit token (--token) wins over the file, so a packaged build can keep
    the string the board already holds in flash; otherwise the board answers
    `bad token` and the glass stays on its self-test page.
    """
    path = os.path.join(root, "daemon.token")
    if explicit:
        try:
            io.open(path, "w", encoding="ascii").write(explicit)
        except OSError:
            pass
        return explicit
    token = ""
    if os.path.exists(path):
        try:
            token = io.open(path, encoding="ascii").read().strip()
        except OSError:
            token = ""
    if not token:
        import secrets

        token = secrets.token_hex(8)
        io.open(path, "w", encoding="ascii").write(token)
        print("launcher: new token written to %s" % path)
        print("launcher: type this into the board's config page: %s" % token)
    return token


def _have_webview():
    """True when an embedded WebView2 window is possible on this machine."""
    try:
        import webview  # noqa: F401
    except Exception:
        return False
    return True


PROBE_JS = (
    "JSON.stringify({"
    "title:document.title,"
    "kpis:document.querySelectorAll('#kpis > *').length,"
    "badge:(document.getElementById('badge')||{}).textContent,"
    "nodes:document.querySelectorAll('#node-body tr').length,"
    "rows:document.querySelectorAll('#table-body tr').length,"
    "chars:document.body.innerText.length"
    "})"
)


DOTNET_CONFIG = (
    '<?xml version="1.0" encoding="utf-8"?>\n'
    "<configuration>\n"
    '  <startup useLegacyV2RuntimeActivationPolicy="true">\n'
    '    <supportedRuntime version="v4.0" sku=".NETFramework,Version=v4.8" />\n'
    "  </startup>\n"
    "</configuration>\n"
)


def _write_dotnet_config():
    """Write `PC-Monitor.exe.config` so pythonnet gets .NET Framework 4, not 2.

    pythonnet hosts the CLR in *this* process, so the runtime version comes from the
    host executable's config file. `python.exe` ships one; a PyInstaller exe does
    not, and without it the CLR answers "Failed to resolve
    Python.Runtime.Loader.Initialize" -- because the assembly is netstandard2.0 and
    the loader picked an older runtime that cannot read it. This has to be on disk
    before `import clr`.
    """
    if not getattr(sys, "frozen", False):
        return None
    path = sys.executable + ".config"
    try:
        io.open(path, "w", encoding="utf-8").write(DOTNET_CONFIG)
    except OSError as exc:
        _log("dotnet: could not write %s (%s)" % (path, exc))
        return None
    return path


def _windows_desktop_version():
    """Installed `Microsoft.WindowsDesktop.App` version, newest first, or None.

    CoreCLR needs to be told to load the desktop framework: `System.Windows.Forms`
    lives there, not in `Microsoft.NETCore.App`, and pythonnet's default
    runtimeconfig only asks for the latter -- which is why an embedded window that
    loads `clr` just fine still dies on `AddReference('System.Windows.Forms')`.
    """
    root = os.environ.get("DOTNET_ROOT") or os.path.join(
        os.environ.get("ProgramFiles", r"C:\Program Files"), "dotnet")
    base = os.path.join(root, "shared", "Microsoft.WindowsDesktop.App")
    try:
        names = os.listdir(base)
    except OSError:
        return None

    def _key(text):
        parts = []
        for piece in text.split("."):
            digits = "".join(ch for ch in piece if ch.isdigit())
            parts.append(int(digits) if digits else 0)
        return parts

    names = [n for n in names if n[:1].isdigit()]
    if not names:
        return None
    names.sort(key=_key)
    return names[-1]


def _prepare_dotnet(root):
    """Pick a .NET runtime that pythonnet can load WinForms from, and say which.

    pythonnet defaults to .NET Framework on Windows, and that is the runtime to aim
    for: it is the one pywebview's backend is written against (it reaches into
    `System.Windows.Forms.FileDialogNative+IFileDialog`, a type CoreCLR's WinForms
    no longer exposes the same way). Inside a one-file bundle the default failed on
    this machine because the exe has no runtime config, so one is written first.

    Then CoreCLR, with that config, for machines where .NET Framework is absent.

    The check is `AddReference('System.Windows.Forms')` plus importing
    `Microsoft.Win32.SystemEvents`, because those are the exact two things
    `webview/platforms/winforms.py` needs at import time; anything weaker would call
    a doomed runtime a success. Under CoreCLR the second one needs its own
    AddReference -- `System.Windows.Forms` resolving does not make
    `Microsoft.Win32.SystemEvents` resolve, and pythonnet reports that as
    "cannot import name 'SystemEvents' from 'Microsoft.Win32' (unknown location)".
    """
    config_file = _write_dotnet_config()
    if config_file:
        _log("dotnet: wrote %s" % config_file)

    config = None
    version = _windows_desktop_version()
    if version:
        config = os.path.join(root, "tmp", "pywebview.runtimeconfig.json")
        body = (
            '{\n  "runtimeOptions": {\n    "tfm": "net8.0",\n    "frameworks": [\n'
            '      { "name": "Microsoft.NETCore.App", "version": "%s" },\n'
            '      { "name": "Microsoft.WindowsDesktop.App", "version": "%s" }\n'
            "    ]\n  }\n}\n" % (version, version)
        )
        try:
            if not os.path.isdir(os.path.dirname(config)):
                os.makedirs(os.path.dirname(config), exist_ok=True)
            io.open(config, "w", encoding="ascii").write(body)
        except OSError:
            config = None
    else:
        _log("dotnet: no Microsoft.WindowsDesktop.App found; CoreCLR lacks WinForms")

    for spec in ("netfx", "coreclr", "default"):
        os.environ["PYTHONNET_RUNTIME"] = spec
        if spec == "coreclr" and config:
            os.environ["PYTHONNET_CORECLR_RUNTIME_CONFIG"] = config
        try:
            import clr

            clr.AddReference("System.Windows.Forms")
            clr.AddReference("Microsoft.Win32.SystemEvents")
            from Microsoft.Win32 import SystemEvents  # noqa: F401
        except Exception as exc:
            _log("dotnet runtime %s: failed (%s: %s)"
                 % (spec, type(exc).__name__, exc))
            for name in [n for n in list(sys.modules)
                         if n in ("clr", "pythonnet", "clr_loader")
                         or n.startswith("pythonnet.")
                         or n.startswith("clr_loader.")]:
                del sys.modules[name]
            continue
        _log("dotnet runtime: %s (windowsdesktop %s)" % (spec, version))
        return True
    _log("dotnet runtime: none of coreclr/netfx/default could load WinForms")
    return False


def _run_webview(url, title, width, height, probe=False):
    """Show `url` in a window this process owns. Blocks until the window closes.

    The WebView2 control is a child of our own window, so there is no browser
    window to fight with and no Edge profile involved. `webview.start()` must run
    on the main thread and owns the message loop from there on; the daemon and
    the HTTP server keep working because they live on their own threads.

    With `probe`, the window reports what it actually rendered into the log and
    then closes. Without it there is no way to tell a working window from a white
    one from a script -- the failure mode this whole file exists to avoid.
    """
    import webview

    window = webview.create_window(title, url, width=width, height=height,
                                   min_size=(800, 600))

    if probe:
        def _probe():
            time.sleep(4)
            try:
                info = window.evaluate_js(PROBE_JS)
                _log("window probe: %s" % (info,))
            except Exception as exc:
                _log("window probe failed: %s: %s" % (type(exc).__name__, exc))
            try:
                window.destroy()
            except Exception:
                pass

        webview.start(_probe, debug=False)
        return 0

    webview.start(debug=False)
    return 0


def _find_browser():
    """The first Edge or Chrome we can find, or None."""
    roots = [
        os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)"),
        os.environ.get("ProgramFiles", r"C:\Program Files"),
        os.environ.get("LocalAppData", ""),
    ]
    names = [
        r"Microsoft\Edge\Application\msedge.exe",
        r"Google\Chrome\Application\chrome.exe",
    ]
    for root in roots:
        if not root:
            continue
        for name in names:
            candidate = os.path.join(root, name)
            if os.path.exists(candidate):
                return candidate
    return None


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)

    source = _opt(argv, "--source", "lhm")
    daemon_port = int(_opt(argv, "--daemon-port", 7802))
    web_port = int(_opt(argv, "--web-port", 7803))
    window_mode = _opt(argv, "--window", None)
    if window_mode is None:
        if "--no-window" in argv:
            window_mode = "none"
        elif "--browser" in argv:
            window_mode = "browser"
        else:
            window_mode = "auto"
    if window_mode not in ("auto", "webview", "edge", "browser", "none"):
        raise SystemExit("launcher: --window must be auto|webview|edge|browser|none")

    root = _app_dir()
    os.chdir(root)
    os.makedirs(os.path.join(root, "layouts"), exist_ok=True)

    # Import late so the CWD change above happens before anything resolves a
    # relative path.
    import fake_daemon
    from pcmon import webui

    # Decide the auth mode before minting anything: in hmac mode a token file is
    # a secret nobody reads.
        # hmac by default: the board speaks challenge-response since 2026-10-26 and
    # carries a 128-bit key in flash, so nothing here needs the v0.5 plain-token
    # path. --auth any exists only as an upgrade window for a board that has not
    # been reflashed yet, and it should be temporary by definition.
    auth_mode = _opt(argv, "--auth", "hmac")
    if auth_mode not in ("hmac", "static", "any"):
        raise SystemExit("launcher: --auth must be hmac|static|any")

    token = ""
    if auth_mode != "hmac":
        token = _ensure_token(root, explicit=_opt(argv, "--token", None))

    # v0.6: the dashboard is a device like any other. It is on this machine, so
    # it pairs itself instead of asking the operator for a code -- but it still
    # gets its own key, so revoking it is possible without touching the board.
    from pcmon.devices import DeviceTable

    devices_path = os.path.join(root, "devices.json")
    device_table = DeviceTable(devices_path)
    dash_key = device_table.key_for("webui")
    if not dash_key:
        dash_key = device_table.add("webui", note="bundled dashboard")
        _log("paired the bundled dashboard (client_id webui) into %s" % devices_path)

    def run_daemon():
        try:
            fake_daemon.main([
                "--source", source,
                "--port", str(daemon_port),
                "--token", token,
                "--auth", auth_mode,
                "--devices", devices_path,
            ])
        except SystemExit:
            pass
        except Exception as exc:  # pragma: no cover - surface, never swallow
            print("launcher: daemon stopped: %s: %s" % (type(exc).__name__, exc))

    threading.Thread(target=run_daemon, name="daemon", daemon=True).start()

    url = "http://127.0.0.1:%d/" % web_port
    print("PC-Monitor")
    print("  exe    : %s" % sys.executable)
    print("  workdir: %s" % root)
    print("  daemon : port %d, source=%s" % (daemon_port, source))
    print("  webui  : %s" % url)

    if window_mode == "none":
        # Server-only: the dashboard holds the foreground thread, which is what
        # keeps the process alive on a headless box.
        return webui.main([
            "--port", str(web_port),
            "--daemon", "127.0.0.1:%d" % daemon_port,
            "--token", dash_key,
        ])

    # Dashboard on its own thread so the window can own the main thread's sleep.
    link = webui.Link("127.0.0.1", daemon_port, dash_key, 160, 80, client_id="webui")
    link.start()
    httpd = webui.serve("127.0.0.1", web_port, link)
    threading.Thread(
        target=httpd.serve_forever,
        kwargs={"poll_interval": 0.5},
        name="webui-http",
        daemon=True,
    ).start()

    # Wait for the page to answer before opening the window, so the first thing
    # the user sees is the dashboard and not an error page.
    ready = False
    for _ in range(80):
        try:
            import urllib.request

            with urllib.request.urlopen(url, timeout=0.5) as resp:
                if resp.status == 200 and resp.read(1):
                    ready = True
                    break
        except Exception:
            pass
        time.sleep(0.1)
    _log("webui ready=%s after wait" % ready)
    if not ready:
        print("launcher: dashboard did not answer on %s" % url)

    if window_mode == "auto":
        window_mode = "webview" if _have_webview() else "edge"
    _log("window backend: %s" % window_mode)

    if window_mode == "webview":
        try:
            _prepare_dotnet(root)
            # Import the backend ourselves first: `webview.guilib` swallows everything
            # but ImportError while looking for a backend and then reports "you must
            # have pythonnet installed", which hides .NET load failures. Importing it
            # here puts the real exception in PC-Monitor.log instead.
            import webview.platforms.winforms  # noqa: F401

            code = _run_webview(url, "PC-Monitor", 1280, 880,
                                probe="--window-selftest" in argv)
        except Exception as exc:
            # A missing WebView2 runtime or a packaging slip must not leave the
            # user with nothing: fall back to the browser we know exists.
            _log("webview window failed: %s: %s" % (type(exc).__name__, exc))
            import traceback

            _log(traceback.format_exc())
            window_mode = "edge"
        else:
            _log("=== window closed")
            link.stop()
            httpd.shutdown()
            return code

    opened = False
    if window_mode == "browser":
        import webbrowser

        opened = webbrowser.open(url)
    else:
        exe = _find_browser()
        if exe:
            import subprocess

            # `--app=` gives a window with no address bar and no tabs: it is the
            # dashboard, framed. A separate profile keeps it from merging into
            # the user's everyday browser session (which would otherwise reuse a
            # running instance and ignore `--app`).
            subprocess.Popen([
                exe,
                "--app=%s" % url,
                "--window-size=1280,880",
                "--user-data-dir=%s" % os.path.join(root, "tmp", "browser-profile"),
                # A brand-new profile otherwise starts Edge's first-run flow and
                # quietly ignores the URL, which is why the first attempt at this
                # showed a blank window that looked like Edge's own empty page.
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-features=msEdgeIdentityFRE,EdgeCollectionsFRE",
                "--noerrdialogs",
            ])
            opened = True

    if not opened:
        print("launcher: no Edge/Chrome found; open %s yourself" % url)

    print("  stop   : close this window (Ctrl-C)")

    # Hold the process open. Without this the interpreter would reach the end of
    # `main` and start finalising while the daemon and HTTP threads still hold
    # stdout, which produces "Fatal Python error: _enter_buffered_busy" instead of
    # a clean exit -- and, worse, kills the servers the window is talking to.
    try:
        while True:
            time.sleep(0.5)
    except KeyboardInterrupt:
        pass
    finally:
        link.stop()
        httpd.shutdown()
    return 0


if __name__ == "__main__":
    # Everything runs behind a guard that writes the traceback to the log before
    # the process can die. A frozen exe that fails during startup otherwise
    # leaves nothing behind at all.
    try:
        _log("=== start: frozen=%s argv=%r ===" % (getattr(sys, "frozen", False), sys.argv[1:]))
        _code = main()
        _log("=== exit: code=%r ===" % (_code,))
    except SystemExit as exc:
        _log("=== SystemExit: %r ===" % (exc.code,))
        raise
    except BaseException as exc:  # noqa: BLE001 - everything belongs on disk
        import traceback

        _log("=== CRASH: %s: %s ===" % (type(exc).__name__, exc))
        _log(traceback.format_exc())
        raise
    raise SystemExit(_code)
