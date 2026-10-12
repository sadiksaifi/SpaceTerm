#!/usr/bin/env -S python -I
# MISE description="Test in-memory macOS credential ACL construction without Keychain changes"
"""Build and run the synthetic public-constructor tests without signing inputs."""

import os
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(ROOT / "mise-tasks" / "lib"))

from spaceterm_tasks import TaskError, main  # noqa: E402
from spaceterm_tasks.macos_signing import signing_environment  # noqa: E402


def run_task(*, sanitizers: bool = False) -> int:
    if sys.platform != "darwin":
        raise TaskError("native-acl-tests-unavailable")
    environment = signing_environment(os.environ)
    sources = ROOT / "tests" / "server_native" / "macos"
    with tempfile.TemporaryDirectory(prefix="spaceterm-native-acl-") as directory:
        binary = Path(directory) / "acl-tests"
        command = [
            "/usr/bin/xcrun",
            "--sdk",
            "macosx",
            "clang",
            "-std=c11",
            "-arch",
            "arm64",
            "-mmacosx-version-min=26.0",
            "-Wall",
            "-Wextra",
            "-Werror",
            str(sources / "keychain_policy.c"),
            str(sources / "acl_tests.c"),
            "-framework",
            "Security",
            "-framework",
            "CoreFoundation",
            "-o",
            str(binary),
        ]
        if sanitizers:
            command.extend(
                [
                    "-DST_POLICY_TEST_ALLOCATIONS",
                    "-g",
                    "-O1",
                    "-fsanitize=address,undefined",
                    "-fno-omit-frame-pointer",
                ]
            )
        try:
            compiled = subprocess.run(command, cwd=ROOT, env=environment, timeout=60)
            if compiled.returncode:
                return compiled.returncode
            return subprocess.run([str(binary)], cwd=ROOT, env=environment, timeout=30).returncode
        except subprocess.TimeoutExpired:
            raise TaskError("native-acl-tests-timeout") from None


def entry() -> int:
    arguments = sys.argv[1:]
    if arguments not in ([], ["--sanitizers"]):
        raise TaskError("native-acl-tests-invalid-arguments")
    return run_task(sanitizers=bool(arguments))


if __name__ == "__main__":
    main(entry)
