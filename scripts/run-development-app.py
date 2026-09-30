from __future__ import annotations

import os
import sys


PROFILES = ("dev", "appearance")
TASKS_BY_PLATFORM = {
    "darwin": {
        "dev": "dev:macos",
        "appearance": "dev:appearance:macos",
    },
    "linux": {
        "dev": "dev:linux",
        "appearance": "dev:appearance:linux",
    },
}


def task_for_platform(profile: str, platform: str) -> str | None:
    if profile not in PROFILES:
        raise ValueError(f"unknown development profile: {profile}")

    tasks = TASKS_BY_PLATFORM.get(platform)
    return None if tasks is None else tasks[profile]


def main(arguments: list[str]) -> int:
    if len(arguments) != 1 or arguments[0] not in PROFILES:
        profiles = "|".join(PROFILES)
        print(f"usage: run-development-app.py <{profiles}>", file=sys.stderr)
        return 2

    task = task_for_platform(arguments[0], sys.platform)
    if task is None:
        print(
            f"error: SpaceTerm development applications are unsupported on {sys.platform}",
            file=sys.stderr,
        )
        return 2

    try:
        os.execvp("mise", ["mise", "run", task])
    except OSError as error:
        print(f"error: unable to run mise: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
