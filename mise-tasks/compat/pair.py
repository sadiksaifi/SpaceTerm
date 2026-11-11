#!/usr/bin/env python
# MISE description="Pair this commit's CLI or Core with a published release on x86_64 Linux"
"""Pair this commit's CLI or Core with a published release's other side.

The release's Core runs in the foreground in this account, so run this only where no other
SpaceTerm release Core runs and its state can be discarded, as on a CI runner.
"""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.compatibility import SIDES, pair

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("release", help="A published release tag, such as v0.11.3")
    parser.add_argument(
        "--candidate", choices=SIDES, required=True, help="The side this commit plays"
    )
    parser.add_argument("--release-tag", help="The exact planned release tag already at HEAD")
    args = parser.parse_args()
    main(lambda: pair(args.release, args.candidate, release_tag=args.release_tag))
