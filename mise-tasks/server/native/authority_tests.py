#!/usr/bin/env -S python -I
# MISE description="Run account observation and required custody-authority regressions"
"""Exercise host interfaces without privileged host mutation or native evidence credit."""

import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "mise-tasks" / "lib"))

from spaceterm_tasks import TaskError, main  # noqa: E402
from spaceterm_tasks.macos_signing import signing_environment  # noqa: E402


def run_suite(arguments: list[str], environment: dict[str, str], suites: int) -> int:
    try:
        result = subprocess.run(
            ["cargo", "test", "--package", "spaceterm-host", "--locked", *arguments],
            cwd=ROOT,
            env=environment,
            capture_output=True,
            text=True,
            timeout=240,
        )
    except (OSError, subprocess.TimeoutExpired):
        raise TaskError("native-authority-tests-unavailable") from None
    if result.returncode:
        # Never relay Cargo/native output. Missing targets remain a failed gate.
        raise TaskError("native-authority-tests-failed")
    counts = re.findall(
        r"test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;", result.stdout
    )
    if len(counts) != suites or any(
        int(passed) == 0 or int(failed) or int(ignored) for passed, failed, ignored in counts
    ):
        raise TaskError("native-authority-tests-incomplete")
    return sum(int(passed) for passed, _, _ in counts)


def entry() -> int:
    if sys.argv[1:]:
        raise TaskError("native-authority-invalid-arguments")
    environment = signing_environment(os.environ)
    if sys.platform == "darwin":
        environment["CARGO_BUILD_JOBS"] = "3"
    observers = run_suite(["--lib", "--", "observer_tests"], environment, 1)
    print(f"{observers} account observer regressions passed; no custody evidence credit")
    witnesses = run_suite(["--lib", "--", "linux_custody::mount_probe"], environment, 1)
    print(f"{witnesses} mount witness regressions passed; no privileged evidence credit")
    authority = run_suite(
        [
            "--test",
            "native_deployment",
            "--test",
            "linux_support_policy",
            "--test",
            "linux_custody_authority",
        ],
        environment,
        3,
    )
    print(f"{authority} host authority regressions passed; privileged evidence remains separate")
    return 0


if __name__ == "__main__":
    main(entry)
