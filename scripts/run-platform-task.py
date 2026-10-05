"""Runs the platform implementation of a generic mise task, named `<task>:<platform>`."""

from __future__ import annotations

import os
from pathlib import Path
import sys
import tomllib


PLATFORM_SEGMENTS = {"darwin": "macos", "linux": "linux"}


class TaskUnavailable(Exception):
    """The requested task has no implementation on the current platform."""


def platform_task(task: str, platform: str) -> str:
    segment = PLATFORM_SEGMENTS.get(platform)
    if segment is not None:
        implementation = f"{task}:{segment}"
        configuration = Path(__file__).resolve().parent.parent / ".mise.toml"
        tasks = tomllib.loads(configuration.read_text())["tasks"]
        if implementation in tasks:
            return implementation
    raise TaskUnavailable


def main(arguments: list[str]) -> int:
    if not arguments:
        print("usage: run-platform-task.py <task> [argument ...]", file=sys.stderr)
        return 2

    task, *task_arguments = arguments
    try:
        implementation = platform_task(task, sys.platform)
    except TaskUnavailable:
        print(f"error: task_not_available_on_platform: {task} is not available "
              f"on this platform ({sys.platform})", file=sys.stderr)
        return 2

    try:
        os.execvp("mise", ["mise", "run", implementation, *task_arguments])
    except OSError:
        print("error: task_dispatch_failed: unable to run mise", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
