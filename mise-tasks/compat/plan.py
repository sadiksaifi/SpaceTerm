#!/usr/bin/env python
# MISE description="Plan the compatibility pairs from the listed releases"
"""Write the compatibility workflow's pair matrix as GitHub step outputs."""

import argparse
import os

from spaceterm_tasks import main
from spaceterm_tasks.compatibility import plan

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", default=os.environ.get("GITHUB_OUTPUT"), required=False)
    parser.add_argument(
        "--version",
        default=os.environ.get("RELEASE_VERSION"),
        help="The release version being planned, without v",
    )
    args = parser.parse_args()
    if not args.output:
        parser.error("--output or GITHUB_OUTPUT names the step output file")
    if not args.version:
        parser.error("--version or RELEASE_VERSION names the planned release")
    main(lambda: plan(args.output, args.version))
