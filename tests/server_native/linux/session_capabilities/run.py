#!/usr/bin/env -S python -I
"""Exercise session-bound Linux bus, portal and agent adapters without GUI effects."""

import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]


def checked(command):
    environment = {
        key: os.environ[key]
        for key in (
            "PATH",
            "HOME",
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_JOBS",
            "CARGO_HOME",
            "RUSTUP_HOME",
            "RUSTUP_TOOLCHAIN",
            "TMPDIR",
        )
        if key in os.environ
    }
    result = subprocess.run(command, cwd=ROOT, env=environment, capture_output=True, timeout=120)
    if result.returncode or len(result.stdout) + len(result.stderr) > 65536:
        raise RuntimeError("session-capability-check-failed")
    return result.stdout


def run():
    if sys.platform != "linux":
        raise RuntimeError("session-capability-linux-unavailable")
    os.umask(0o077)
    rust = checked(
        [
            "cargo",
            "test",
            "--package",
            "spaceterm-host",
            "--locked",
            "--features",
            "native-tests",
            "--test",
            "session_capabilities",
        ]
    )
    result = re.search(rb"test result: ok\. ([1-9][0-9]*) passed; 0 failed; 0 ignored;", rust)
    if result is None:
        raise RuntimeError("session-capability-executed-count-unavailable")
    sources = ROOT / "tests/server_native/macos/capabilities"
    with tempfile.TemporaryDirectory(prefix="spaceterm-session-capabilities-") as temporary:
        binary = Path(temporary) / "tree-model"
        checked(
            [
                "cc",
                "-std=c11",
                "-Wall",
                "-Wextra",
                "-Werror",
                str(sources / "capability_tree.c"),
                str(sources / "tree_tests.c"),
                "-o",
                str(binary),
            ]
        )
        tree = checked([str(binary)])
        if not tree.endswith(b"executed=8 proof=synthetic native-effects=0\n"):
            raise RuntimeError("session-capability-executed-count-invalid")
    print(f"executed-rust={int(result.group(1))} executed-c=8 native-effects=0")


if __name__ == "__main__":
    try:
        run()
    except (RuntimeError, OSError, subprocess.TimeoutExpired):
        print("session-capability-check-unavailable", file=sys.stderr)
        raise SystemExit(2) from None
