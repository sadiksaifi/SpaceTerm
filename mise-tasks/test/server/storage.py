#!/usr/bin/env -S python -I
# MISE description="Record native SQLite sync, I/O-fault and owned-process-kill evidence without power operations"
"""Run with --source-commit <full candidate commit>; retain JSON stdout.

No installed service, credential storage, native authorization or power operation
is touched. VM abrupt-power-off and registered-store recovery remain separate
coordinator-run checks, not substitutes for the classifications emitted here.
"""

import sys
from pathlib import Path

if __name__ == "__main__":
    if not sys.flags.isolated:
        print("isolated-python-required", file=sys.stderr)
        raise SystemExit(2)
    if len(sys.argv) != 3 or sys.argv[1] != "--source-commit":
        print("unsupported-storage-test-arguments", file=sys.stderr)
        raise SystemExit(2)
    sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "lib"))
    from spaceterm_tasks import main
    from spaceterm_tasks.storage_acceptance import run

    main(lambda: run(sys.argv[2]))
