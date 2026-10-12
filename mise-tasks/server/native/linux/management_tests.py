#!/usr/bin/env -S python3 -I
# MISE description="Test systemd 249 and polkit management contracts without host mutations"
"""Portable policy/codec tests. No systemctl, pkexec, sudo, or policy mutation."""

import os
import subprocess
import sys
import tempfile
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
    environment = signing_environment(os.environ)
    try:
        result = subprocess.run(command, cwd=ROOT, env=environment, timeout=120, check=False)
        if result.returncode:
            return result.returncode
        with tempfile.TemporaryDirectory(prefix="spaceterm-polkit-") as directory:
            for sanitizers in (False, True):
                binary = Path(directory) / ("polkit-asan" if sanitizers else "polkit")
                compiler = ["cc", "-std=c11", "-Wall", "-Wextra", "-Werror"]
                if sanitizers:
                    compiler += [
                        "-O1",
                        "-g",
                        "-fsanitize=address,undefined",
                        "-fno-omit-frame-pointer",
                    ]
                compiler += [
                    "-DST_MANAGEMENT_TESTING",
                    str(ROOT / "crates/spaceterm-host/native/linux/management/polkit.c"),
                    str(ROOT / "crates/spaceterm-host/native/macos/management.c"),
                    str(ROOT / "tests/server_native/linux/management/polkit_tests.c"),
                    "-ldl",
                    "-o",
                    str(binary),
                ]
                for invocation in (compiler, [str(binary)]):
                    result = subprocess.run(
                        invocation, cwd=ROOT, env=environment, timeout=60, check=False
                    )
                    if result.returncode:
                        return result.returncode
        return 0
    except (OSError, subprocess.TimeoutExpired):
        raise TaskError("linux-management-tests-execution-unavailable") from None


if __name__ == "__main__":
    main(entry)
