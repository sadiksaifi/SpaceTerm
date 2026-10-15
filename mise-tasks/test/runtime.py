#!/usr/bin/env python
# MISE description="Verify the headless runtime dependency graph and native PTY lifecycle"
"""Run the public runtime example without a graphical session."""

import os
import subprocess

from spaceterm_tasks import ROOT, TaskError, developer_environment, main


def verify():
    selection = ["--package", "spaceterm", "--no-default-features", "--locked"]
    graph = subprocess.run(
        [
            "cargo",
            "tree",
            *selection,
            "--edges",
            "normal,build",
            "--prefix",
            "none",
            "--format",
            "{p}",
        ],
        cwd=ROOT,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    ).stdout
    packages = {line.split()[0] for line in graph.splitlines() if line.split()}
    graphical = {
        name
        for name in packages
        if name.startswith(("gpui", "objc2-app-kit", "wayland-", "x11", "xcb", "wgpu", "accesskit"))
        or name in {"spaceterm-ui", "zbus", "font-kit", "fontconfig-sys", "yeslogic-fontconfig-sys"}
    }
    if graphical:
        raise TaskError(
            "headless runtime includes graphical dependencies: " + ", ".join(sorted(graphical))
        )
    environment = developer_environment(os.environ)
    for name in ("DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS", "XAUTHORITY"):
        environment.pop(name, None)
    subprocess.run(
        [
            "cargo",
            "run",
            *selection,
            "--example",
            "headless_terminal",
            "--",
            "--require-headless-environment",
        ],
        cwd=ROOT,
        env=environment,
        check=True,
        timeout=180,
    )


def run():
    try:
        verify()
    except subprocess.CalledProcessError as error:
        raise TaskError("headless runtime verification failed") from error
    except subprocess.TimeoutExpired as error:
        raise TaskError("headless runtime verification timed out") from error


if __name__ == "__main__":
    main(run)
