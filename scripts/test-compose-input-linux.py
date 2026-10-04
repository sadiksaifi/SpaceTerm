#!/usr/bin/env python3
"""Exercise native Compose input on an isolated X11 or Wayland desktop."""

import argparse
import os
from pathlib import Path
import subprocess
import signal
import sys
import tempfile
import time


def session(backend):
    processes = []
    compositor_log = Path(os.environ["XDG_RUNTIME_DIR"]) / "compositor.log"
    compositor_output = compositor_log.open("w")
    try:
        address = os.environ["DBUS_SESSION_BUS_ADDRESS"]
        os.environ["DBUS_SYSTEM_BUS_ADDRESS"] = address
        subprocess.run([
            "gdbus", "call", "--session", "--dest", "org.freedesktop.DBus", "--object-path",
            "/org/freedesktop/DBus", "--method", "org.freedesktop.DBus.UpdateActivationEnvironment",
            str({"DBUS_SYSTEM_BUS_ADDRESS": address}),
        ], check=True, stdout=subprocess.DEVNULL, timeout=5)
        if backend == "x11":
            if Path("/tmp/.X96-lock").exists():
                raise RuntimeError("private display :96 is already in use")
            processes.append(subprocess.Popen([
                "Xvfb", ":96", "-screen", "0", "1920x1080x24", "-nolisten", "tcp", "-noreset",
            ], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
            for _ in range(100):
                if subprocess.run(["xdpyinfo"], stdout=subprocess.DEVNULL,
                                  stderr=subprocess.DEVNULL, timeout=2).returncode == 0:
                    break
                time.sleep(0.1)
            else:
                raise RuntimeError("private X11 display did not start")
        else:
            subprocess.run([
                "gsettings", "set", "org.gnome.shell", "welcome-dialog-last-shown-version", "'999'",
            ], check=True, timeout=5)
            subprocess.run([
                "gsettings", "set", "org.gnome.desktop.input-sources", "sources", "[('xkb', 'us+intl')]",
            ], check=True, timeout=5)
            processes.append(subprocess.Popen([
                "gnome-shell", "--headless", "--wayland", "--no-x11", "--virtual-monitor", "1920x1080",
                "--wayland-display", "spaceterm-compose-test",
            ], stdout=compositor_output, stderr=subprocess.STDOUT))
            socket = Path(os.environ["XDG_RUNTIME_DIR"]) / "spaceterm-compose-test"
            for _ in range(300):
                if socket.exists():
                    break
                if processes[0].poll() is not None:
                    raise RuntimeError("private Wayland compositor exited")
                time.sleep(0.1)
            else:
                raise RuntimeError("private Wayland display did not start")
        result = subprocess.run([
            "cargo", "test", "--package", "spaceterm", "--bin", "spaceterm", "--features", "native-tests",
            "--locked", "linux_compose_input_native", "--", "--ignored", "--test-threads=1",
        ]).returncode
        # The outer runner owns the deadline and the entire inherited process group.
        if result != 0 and backend == "wayland":
            compositor_output.flush()
            print(compositor_log.read_text()[-12000:], file=sys.stderr)
        return result
    finally:
        for process in reversed(processes):
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        compositor_output.close()


def run_private_session(command, environment):
    process = None
    interruption = {"spawning": False, "signal": None}

    def interrupted(signum, _frame):
        if interruption["signal"] is not None:
            return
        interruption["signal"] = signum
        if not interruption["spawning"]:
            raise SystemExit(128 + signum)

    handlers = {kind: signal.signal(kind, interrupted)
                for kind in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT)}
    try:
        interruption["spawning"] = True
        try:
            process = subprocess.Popen(command, env=environment, start_new_session=True)
        finally:
            interruption["spawning"] = False
        if interruption["signal"] is not None:
            raise SystemExit(128 + interruption["signal"])
        return process.wait(timeout=1300)
    except BaseException:
        if process is not None:
            # Keep the leader unreaped while displays remove their sockets and locks.
            # Then force resistant descendants to exit before reaping the direct child.
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            else:
                time.sleep(3)
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            process.wait(timeout=5)
        raise
    finally:
        for kind, handler in handlers.items():
            signal.signal(kind, handler)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("backend", choices=("x11", "wayland"))
    parser.add_argument("--session", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.session:
        return session(args.backend)
    with tempfile.TemporaryDirectory(prefix="spaceterm-compose-test-") as temporary:
        root = Path(temporary)
        environment = dict(os.environ)
        for name in ("DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DBUS_SESSION_BUS_ADDRESS", "ZED_HEADLESS"):
            environment.pop(name, None)
        environment["CARGO_HOME"] = os.environ.get("CARGO_HOME", str(Path.home() / ".cargo"))
        environment["RUSTUP_HOME"] = os.environ.get("RUSTUP_HOME", str(Path.home() / ".rustup"))
        for name, directory in (
            ("HOME", "home"), ("XDG_CONFIG_HOME", "config"), ("XDG_CACHE_HOME", "cache"),
            ("XDG_DATA_HOME", "data"), ("XDG_STATE_HOME", "state"), ("XDG_RUNTIME_DIR", "runtime"),
        ):
            path = root / directory
            path.mkdir(mode=0o700)
            environment[name] = str(path)
        environment["DBUS_SYSTEM_BUS_ADDRESS"] = f"unix:path={root}/no-system-bus"
        environment["SPACETERM_COMPOSE_BACKEND"] = args.backend
        environment["DISPLAY" if args.backend == "x11" else "WAYLAND_DISPLAY"] = (
            ":96" if args.backend == "x11" else "spaceterm-compose-test"
        )
        return run_private_session([
            "dbus-run-session", "--", sys.executable, str(Path(__file__).resolve()), args.backend, "--session",
        ], environment)


if __name__ == "__main__":
    sys.exit(main())
