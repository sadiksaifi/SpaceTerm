"""Dispatch required renderer checks without platform commands in the shared gate."""

from __future__ import annotations

import subprocess
import sys


def main() -> int:
    tasks = {
        "darwin": "validate:gpui:macos",
        "linux": "validate:gpui:linux",
        "win32": "validate:gpui:windows",
    }
    task = tasks.get(sys.platform)
    if task is None:
        print(f"error: GPUI validation is unsupported on {sys.platform}", file=sys.stderr)
        return 2
    try:
        return subprocess.run(["mise", "run", task], check=False).returncode
    except OSError as error:
        print(f"error: unable to run mise: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
