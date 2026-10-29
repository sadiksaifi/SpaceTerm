#!/usr/bin/env python
# MISE description="Prepare both signed nonproduction server targets from the current source"
# USAGE arg "<destination>" help="New catalog directory to create"
# USAGE flag "--identity <identity>" help="Nonproduction application identity" default="spaceterm-development" {
# USAGE   choices "spaceterm-development" "spaceterm-preflight"
# USAGE }
"""Prepare both signed nonproduction server targets from the current source.

macOS builds natively on a Mac, including current uncommitted source. Linux cross-builds
locally. Linux hosts need SPACETERM_SERVER_ARTIFACTS with a Mac-built target from the same
source snapshot. No production signing credentials are required.
"""

import argparse
from pathlib import Path

from spaceterm_tasks import main
from spaceterm_tasks.server_builds import NONPRODUCTION, prepare

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path)
    parser.add_argument("--identity", choices=NONPRODUCTION, default="spaceterm-development")
    args = parser.parse_args()
    main(lambda: prepare(args.destination, args.identity) and 0)
