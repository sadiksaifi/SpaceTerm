#!/usr/bin/env -S python -I
# MISE description="Verify the existing signed product/update pair without host changes"
"""Run trusted repository task code, never a module beside supplied artifacts."""

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(ROOT / "mise-tasks/lib"))
sys.path.insert(0, str(ROOT / "packaging/macos/native-evidence"))

from assembly import task_main  # noqa: E402  # ty: ignore[unresolved-import]
from sequence import prepare_pair  # noqa: E402  # ty: ignore[unresolved-import]
from spaceterm_tasks.server_native_macos import EvidenceArgumentParser, prepare_task  # noqa: E402


def entry():
    # Retain the earlier schema-1 inspection interface for existing callers. The
    # judged sequence no longer builds or elevates the removed evidence admin.
    parser = EvidenceArgumentParser(description=__doc__)
    parser.add_argument("--fixtures", type=Path)
    parser.add_argument("--output", type=Path)
    arguments = parser.parse_args()
    if arguments.fixtures is None:
        print("phase=prepare classification=native-unavailable")
        return 2
    if (arguments.fixtures / "baseline/candidate.json").is_file():
        options = {} if arguments.output is None else {"destination": arguments.output}
        prepare_pair(arguments.fixtures, **options)
        print("phase=prepare classification=preflight packages=2 native_passed=0")
        return 0
    if arguments.output is not None:
        from spaceterm_tasks import TaskError

        raise TaskError("invalid-record")
    return prepare_task()


if __name__ == "__main__":
    task_main(entry)
