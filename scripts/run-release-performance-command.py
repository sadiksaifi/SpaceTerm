#!/usr/bin/env python3
"""Run one command and persist its elapsed monotonic duration."""

from __future__ import annotations

import os
import pathlib
import signal
import subprocess
import sys
import time


def main() -> None:
    if len(sys.argv) < 3:
        raise SystemExit("usage: run-release-performance-command.py ELAPSED_FILE COMMAND [ARG ...]")

    elapsed_path = pathlib.Path(sys.argv[1])
    child: subprocess.Popen[bytes] | None = None

    def forward_signal(signum: int, _frame: object) -> None:
        if child is not None and child.poll() is None:
            child.send_signal(signum)

    signal.signal(signal.SIGINT, forward_signal)
    signal.signal(signal.SIGTERM, forward_signal)
    started = time.monotonic_ns()
    try:
        child = subprocess.Popen(sys.argv[2:])
        returncode = child.wait()
    finally:
        if child is not None and child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
        elapsed_ns = time.monotonic_ns() - started
        temporary_path = elapsed_path.with_name(f"{elapsed_path.name}.tmp.{os.getpid()}")
        temporary_path.write_text(f"{elapsed_ns / 1_000_000_000:.6f}\n")
        temporary_path.replace(elapsed_path)

    raise SystemExit(returncode)


if __name__ == "__main__":
    main()
