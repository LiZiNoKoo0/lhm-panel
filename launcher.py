"""PC-Monitor, packaged as one program with its own window.

Runs three things in one process:

  * the daemon, on a worker thread (its accept loop never returns),
  * the dashboard's HTTP server, on another worker thread,
  * a chromeless Edge/Chrome window showing that dashboard.

Why a window and not "open the browser": `webbrowser.open` fails silently inside a
frozen exe often enough that the first report of this program was "I double
clicked it and nothing happened". The process was running fine -- both ports
listening -- but a program whose only evidence of life is a console scrolled past
is a program that looks broken.

Why Edge in `--app=` mode rather than an embedded toolkit: pywebview's Windows
backend needs pythonnet plus the WebView2 assemblies to survive freezing, and it
did not -- it failed at import inside the bundle, and the window never appeared.
`msedge --app=<url>` needs nothing but Edge, which every Windows 10/11 machine
already has, and gives the same result the user asked for: a window titled
PC-Monitor with no address bar, no tabs, and no browser to understand.

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
    PC-Monitor.exe --browser            # default browser instead of the window
    PC-Monitor.exe --no-window          # headless: servers only, for a server box
"""

import io
import os
import sys
import threading
import time


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


def _ensure_token(root):
    """Read the token beside the exe, or mint one and say where it went.

    The board has to be told the same string through its config page, so a fresh
    token is only useful if the user is told about it -- printing it is the point.
    """
    path = os.path.join(root, "daemon.token")
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
    use_browser = "--browser" in argv
    want_window = "--no-window" not in argv and not use_browser

    root = _app_dir()
    os.chdir(root)
    os.makedirs(os.path.join(root, "layouts"), exist_ok=True)

    # Import late so the CWD change above happens before anything resolves a
    # relative path.
    import fake_daemon
    from pcmon import webui

    token = _ensure_token(root)

    def run_daemon():
        try:
            fake_daemon.main([
                "--source", source,
                "--port", str(daemon_port),
                "--token", token,
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

    if not want_window and not use_browser:
        # Server-only: the dashboard holds the foreground thread, which is what
        # keeps the process alive on a headless box.
        return webui.main([
            "--port", str(web_port),
            "--daemon", "127.0.0.1:%d" % daemon_port,
            "--token", token,
        ])

    # Dashboard on its own thread so the window can own the main thread's sleep.
    link = webui.Link("127.0.0.1", daemon_port, token, 160, 80, client_id="webui")
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

    opened = False
    if use_browser:
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
