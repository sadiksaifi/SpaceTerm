#!/usr/bin/env -S python -I
# MISE description="Run one explicit account-service lifecycle command"
# MISE confirm="Run this operation against the selected installed account server?"
# USAGE arg "<executable>" help="Explicit installed server executable"
# USAGE arg "<operation>" help="Fixed install, status, start, stop or uninstall action"
# USAGE flag "--build <build>" help="Expected compiled identity, not a registration selector" default="preflight"
# USAGE flag "--allow-mutation" help="Explicitly permit a service lifecycle mutation"
"""Owner-only lifecycle check. Normal validation never calls it."""

import argparse
import sys
from pathlib import Path

if __name__ == "__main__":
    if not sys.flags.isolated:
        print("isolated-python-required", file=sys.stderr)
        raise SystemExit(2)
    sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "lib"))
    from spaceterm_tasks import main
    from spaceterm_tasks.account_service_checks import run

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("executable")
    parser.add_argument("operation", choices=("install", "status", "start", "stop", "uninstall"))
    parser.add_argument(
        "--build", choices=("release", "preflight", "development"), default="preflight"
    )
    parser.add_argument("--allow-mutation", action="store_true")
    args = parser.parse_args()
    arguments = [
        "lifecycle",
        "--executable",
        args.executable,
        "--operation",
        args.operation,
        "--build",
        args.build,
    ]
    if args.allow_mutation:
        arguments.append("--allow-mutation")
    main(lambda: run(arguments))
