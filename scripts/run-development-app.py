from __future__ import annotations

import os
import sys


MACOS_TASK_BY_PROFILE = {
    "dev": "dev:macos",
    "appearance": "dev:appearance:macos",
}


def task_for_platform(profile: str, platform: str) -> str | None:
    if profile not in MACOS_TASK_BY_PROFILE:
        raise ValueError(f"unknown development profile: {profile}")

    if platform == "darwin":
        return MACOS_TASK_BY_PROFILE[profile]

    return None


def main(arguments: list[str]) -> int:
    if len(arguments) != 1 or arguments[0] not in MACOS_TASK_BY_PROFILE:
        profiles = "|".join(MACOS_TASK_BY_PROFILE)
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
