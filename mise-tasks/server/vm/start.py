#!/usr/bin/env -S python -I
# MISE description="Start the owned disposable Jammy VM"
# MISE confirm="Start only the owned disposable VM without changing the host OS?"
# USAGE arg "<directory>" help="Exact owned VM directory outside checkouts"
# USAGE flag "--allow-mutation" help="Explicitly permit this VM operation"
"""Operate only an explicitly selected owned disposable VM."""

import sys
from pathlib import Path

if __name__ == "__main__":
    if not sys.flags.isolated:
        print("isolated-python-required", file=sys.stderr)
        raise SystemExit(2)
    sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "lib"))
    from spaceterm_tasks import main
    from spaceterm_tasks.server_vm import run_task

    main(lambda: run_task("start", sys.argv[1:]))
