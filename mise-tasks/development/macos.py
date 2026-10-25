#!/usr/bin/env python
# MISE description="Run SpaceTerm Development from a signed macOS application bundle"
# USAGE flag "--ssh-config <path>" help="OpenSSH config containing trusted native builder aliases"
"""Run SpaceTerm Development from a signed macOS application bundle."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.development_macos import launch

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--ssh-config", help="OpenSSH config containing trusted native builder aliases"
    )
    args = parser.parse_args()
    main(lambda: launch(ssh_config=args.ssh_config))
