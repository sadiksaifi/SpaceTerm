#!/usr/bin/env -S python3 -I
# MISE description="Test systemd 249 and polkit management contracts without host mutations"
"""Portable policy/codec tests. No systemctl, pkexec, sudo, or policy mutation."""

import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(ROOT / "mise-tasks" / "lib"))

from spaceterm_tasks import TaskError, main  # noqa: E402
from spaceterm_tasks.macos_signing import signing_environment  # noqa: E402


def entry() -> int:
    if len(sys.argv) != 1 or sys.platform != "linux":
        raise TaskError("linux-management-tests-unavailable")
    command = [
        sys.executable,
        "-I",
        str(ROOT / "mise-tasks" / "server" / "native" / "macos" / "management_tests.py"),
        "--portable",
    ]
    try:
        return subprocess.run(
            command, cwd=ROOT, env=signing_environment(os.environ), timeout=120, check=False
        ).returncode
    except (OSError, subprocess.TimeoutExpired):
        raise TaskError("linux-management-tests-execution-unavailable") from None


if __name__ == "__main__":
    main(entry)
