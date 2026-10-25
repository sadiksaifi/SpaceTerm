#!/usr/bin/env python
# MISE description="Prepare both signed nonproduction server targets from the current source"
# USAGE arg "<destination>" help="New catalog directory to create"
# USAGE flag "--identity <identity>" help="Nonproduction application identity" default="spaceterm-development" {
# USAGE   choices "spaceterm-development" "spaceterm-preflight"
# USAGE }
# USAGE flag "--ssh-config <path>" help="OpenSSH config containing trusted native builder aliases"
"""Prepare both signed nonproduction server targets from the current source.

Configure macos-builder and linux-builder as trusted native SSH builder aliases.
Each peer needs ~/Projects/SpaceTerm, rsync, mise and the native build prerequisites.
The current source, including uncommitted files, is built in target/server-builds;
existing source, services and other build caches are preserved.
"""

import argparse
from pathlib import Path

from spaceterm_tasks import main
from spaceterm_tasks.server_builds import NONPRODUCTION, prepare

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path)
    parser.add_argument("--identity", choices=NONPRODUCTION, default="spaceterm-development")
    parser.add_argument(
        "--ssh-config", help="OpenSSH config containing trusted native builder aliases"
    )
    args = parser.parse_args()
    main(lambda: prepare(args.destination, args.identity, ssh_config=args.ssh_config) and 0)
