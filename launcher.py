"""PC-Monitor, packaged as one program.

Runs the daemon and the dashboard in one process so a user has one thing to
double-click. The daemon keeps its own accept loop on a worker thread; the
dashboard owns the main thread because `ThreadingHTTPServer.serve_forever` is
what keeps the process alive.

Two things this has to get right when frozen by PyInstaller:

  * **The working directory.** `daemon.token` and `layouts/applied.json` are
    written relative to CWD. Inside a one-file exe CWD is wherever the user
    happened to launch from -- possibly `C:\\Windows\\System32`. So the launcher
    moves to the directory holding the exe first, and the files land beside it.

  * **The bundled static files.** `pcmon/webui.py` finds `webstatic/` next to
    itself, which works unchanged under PyInstaller because the package keeps its
    directory shape inside the bundle. Nothing to special-case, but worth stating
    because it is the usual way a frozen GUI ships with a blank page.

Usage:
    PC-Monitor.exe                      # real sensors, 7802 + 7803
    PC-Monitor.exe --source fake        # invented numbers
    PC-Monitor.exe --no-browser         # do not open the dashboard
"""

import io
import os
import sys
import threading


def _app_dir():
    """Where the exe lives, or the source tree when running unpacked."""
    if getattr(sys, "frozen", False):
        return os.path.dirname(sys.executable)
    return os.path.dirname(os.path.abspath(__file__))


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)

    source = "lhm"
    daemon_port = 7802
    web_port = 7803
    open_browser = "--no-browser" not in argv
    if "--source" in argv:
        try:
            source = argv[argv.index("--source") + 1]
        except IndexError:
            print("launcher: --source needs a value (fake or lhm)")
            return 2
    if "--daemon-port" in argv:
        daemon_port = int(argv[argv.index("--daemon-port") + 1])
    if "--web-port" in argv:
        web_port = int(argv[argv.index("--web-port") + 1])

    root = _app_dir()
    os.chdir(root)
    os.makedirs(os.path.join(root, "layouts"), exist_ok=True)

    # Import late: keeps `--help`-ish failures readable and lets the CWD change
    # above happen before anything reads a relative path.
    import fake_daemon
    from pcmon import webui

    # The token is owned here, not by either side.
    #
    # Under PyInstaller `fake_daemon.__file__` points into the temporary
    # extraction directory, so the daemon writes `daemon.token` there and it
    # vanishes with the process -- the dashboard then reports "no token" and
    # the whole exe looks broken. Resolving the path once, beside the exe, and
    # handing the same string to both sides removes the file from the
    # handshake entirely.
    token_path = os.path.join(root, "daemon.token")
    token = ""
    if os.path.exists(token_path):
        token = io.open(token_path, encoding="ascii").read().strip()
    if not token:
        import secrets
        token = secrets.token_hex(8)
        io.open(token_path, "w", encoding="ascii").write(token)
        print("launcher: new token written to %s" % token_path)
        print("launcher: type that token into the board's config page" )

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

    worker = threading.Thread(target=run_daemon, name="daemon", daemon=True)
    worker.start()

    print("PC-Monitor")
    print("  exe    : %s" % sys.executable)
    print("  workdir: %s" % root)
    print("  daemon : port %d, source=%s" % (daemon_port, source))
    print("  webui  : http://127.0.0.1:%d/" % web_port)
    print("  stop   : close this window")

    web_args = [
        "--port", str(web_port),
        "--daemon", "127.0.0.1:%d" % daemon_port,
        "--token", token,
    ]
    if open_browser:
        web_args.append("--open")
    return webui.main(web_args)


if __name__ == "__main__":
    raise SystemExit(main())
