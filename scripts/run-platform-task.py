"""Runs the platform implementation of a generic mise task, named `<task>:<platform>`."""

from __future__ import annotations

import os
import sys


PLATFORM_SEGMENTS = {"darwin": "macos"}


def platform_task(task: str, platform: str) -> str | None:
    segment = PLATFORM_SEGMENTS.get(platform)
    return None if segment is None else f"{task}:{segment}"


def main(arguments: list[str]) -> int:
    if not arguments:
        print("usage: run-platform-task.py <task> [argument ...]", file=sys.stderr)
        return 2

    task, *task_arguments = arguments
    implementation = platform_task(task, sys.platform)
    if implementation is None:
        print(f"error: {task} is unsupported on {sys.platform}", file=sys.stderr)
        return 2

    try:
        os.execvp("mise", ["mise", "run", implementation, *task_arguments])
    except OSError as error:
        print(f"error: unable to run mise: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
