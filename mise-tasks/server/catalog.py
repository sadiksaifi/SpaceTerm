#!/usr/bin/env python
# MISE description="Prepare both signed nonproduction server targets from the current source"
# USAGE arg "<destination>" help="New catalog directory to create"
# USAGE flag "--identity <identity>" help="Nonproduction application identity" default="spaceterm-development" {
# USAGE   choices "spaceterm-development" "spaceterm-preflight"
# USAGE }
"""Prepare both signed nonproduction server targets from the current source.

Both target packages are cross-built locally, including current uncommitted source.
No remote builder or production signing credentials are required.
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
