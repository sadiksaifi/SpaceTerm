#!/usr/bin/env -S python -I
# MISE description="Request removal of the stopped owned disposable VM; execution unavailable"
# MISE confirm="Remove only the stopped disposable VM and its exact owned credentials?"
# USAGE arg "<receipt>" help="Independent ownership receipt outside every checkout"
"""Execution gate. Read no receipt, delete nothing, and return gate-not-ready."""

import sys
from pathlib import Path

if __name__ == "__main__":
    if not sys.flags.isolated:
        print("phase=remove classification=isolated-python-required", file=sys.stderr)
        raise SystemExit(2)
    # -I ignores PYTHONPATH and the current directory. Add only the trusted task library.
    sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "lib"))
    from spaceterm_tasks import main
    from spaceterm_tasks.server_vm import Phase, VmArgumentParser, run_task

    parser = VmArgumentParser(Phase.REMOVE, description=__doc__)
    parser.add_argument("receipt", type=Path)
    args = parser.parse_args()
    main(lambda: run_task(Phase.REMOVE, receipt=args.receipt))
