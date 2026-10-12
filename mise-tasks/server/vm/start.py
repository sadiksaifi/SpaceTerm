#!/usr/bin/env -S python -I
# MISE description="Request startup of the exact disposable VM; execution unavailable"
# MISE confirm="Start only the owned disposable VM without changing penguin's OS?"
# USAGE arg "<receipt>" help="Independent ownership receipt outside every checkout"
"""Execution gate. Read no receipt, start no container, and return gate-not-ready."""

import sys
from pathlib import Path

if __name__ == "__main__":
    if not sys.flags.isolated:
        print("phase=start classification=isolated-python-required", file=sys.stderr)
        raise SystemExit(2)
    # -I ignores PYTHONPATH and the current directory. Add only the trusted task library.
    sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "lib"))
    from spaceterm_tasks import main
    from spaceterm_tasks.server_vm import Phase, VmArgumentParser, run_task

    parser = VmArgumentParser(Phase.START, description=__doc__)
    parser.add_argument("receipt", type=Path)
    args = parser.parse_args()
    main(lambda: run_task(Phase.START, receipt=args.receipt))
