#!/usr/bin/env python
# MISE description="Preview or explicitly archive selected unreleased local server state"
"""Preview or explicitly archive selected unreleased local server state."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.server_reset import reset

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--identity", choices=("spaceterm-development", "spaceterm-preflight"), required=True
    )
    parser.add_argument("--confirm", help="Exact digest from a reviewed preview")
    args = parser.parse_args()
    main(lambda: reset(args.identity, args.confirm))
