#!/usr/bin/env python
# MISE description="Build signed Preflight candidates for native server administration"
"""Build native administration fixtures without modifying the working tree."""

import argparse
from pathlib import Path

from spaceterm_tasks import main
from spaceterm_tasks.admin_fixtures import prepare
from spaceterm_tasks.server_package import TARGETS

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path)
    parser.add_argument("--target", choices=("all", *TARGETS.values()), default="all")
    parser.add_argument(
        "--resume", action="store_true", help="Authenticate and reuse completed fixtures"
    )
    args = parser.parse_args()
    main(lambda: prepare(args.destination, args.target, resume=args.resume))
