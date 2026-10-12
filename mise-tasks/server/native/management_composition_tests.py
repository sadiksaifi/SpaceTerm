#!/usr/bin/env -S python3 -I
# MISE description="Test the actual native-management facade, issuer, controller and admission callbacks"
"""Injected native peers only. This task does not authorize production or mutate native policy."""

import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "mise-tasks" / "lib"))

from spaceterm_tasks import TaskError, main  # noqa: E402
from spaceterm_tasks.macos_signing import signing_environment  # noqa: E402


def entry() -> int:
    if len(sys.argv) != 1:
        raise TaskError("management-composition-invalid-arguments")
    try:
        result = subprocess.run(
            [
                "cargo",
                "test",
                "--package",
                "spaceterm-host",
                "--features",
                "native-tests",
                "--locked",
                "--test",
                "native_management",
            ],
            cwd=ROOT,
            env=signing_environment(os.environ),
            capture_output=True,
            text=True,
            timeout=240,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        raise TaskError("management-composition-unavailable") from None
    if result.returncode:
        raise TaskError("management-composition-failed")
    counts = re.findall(
        r"test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;", result.stdout
    )
    if len(counts) != 1 or int(counts[0][0]) < 42 or any(int(value) for value in counts[0][1:]):
        raise TaskError("management-composition-incomplete")
    print(f"tests={counts[0][0]} scope=management-composition proof=injected-native-peers")
    return 0


if __name__ == "__main__":
    main(entry)
