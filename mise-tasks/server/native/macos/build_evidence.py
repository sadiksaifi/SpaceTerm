#!/usr/bin/env -S python -I
# MISE description="Assemble native candidates without signing or installing them"
"""Build and sign are separate operations. No signing runs by default."""

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(ROOT / "mise-tasks/lib"))
sys.path.insert(0, str(ROOT / "packaging/macos/native-evidence"))


from assembly import (  # noqa: E402, F401  # ty: ignore[unresolved-import]
    DEFAULT_SIGNED,
    DEFAULT_UNSIGNED,
    build_candidate,
    sign_pair,
    task_main,
)
from spaceterm_tasks import TaskError  # noqa: E402
from spaceterm_tasks.server_native_macos import EvidenceArgumentParser  # noqa: E402


def entry():
    parser = EvidenceArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--sign", action="store_true")
    parser.add_argument("--candidate", type=Path)
    parser.add_argument("--update", type=Path)
    arguments = parser.parse_args()
    if arguments.sign:
        if arguments.candidate is None or arguments.update is None:
            raise TaskError("invalid-record")
        sign_pair(arguments.candidate, arguments.update, arguments.output or DEFAULT_SIGNED)
        print("phase=sign classification=preflight packages=2 native_passed=0")
    else:
        if arguments.candidate is not None or arguments.update is not None:
            raise TaskError("invalid-record")
        build_candidate(arguments.output or DEFAULT_UNSIGNED)
        print("phase=build classification=unsigned packages=1 native_passed=0")
    return 0


if __name__ == "__main__":
    task_main(entry)
