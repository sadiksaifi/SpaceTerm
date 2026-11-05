#!/usr/bin/env python
# MISE description="Update the release's Update Floor Core to the release's own Core on x86_64 Linux"
# USAGE arg "<tag>" help="The release tag at HEAD, for example v1.2.3"
# USAGE flag "--archive <file>" help="The release's signed x86_64 Linux Core archive"
# USAGE flag "--signature <file>" help="The archive's raw signature"
"""Update the Core of the release's Update Floor to the release's own signed Core.

The floor release's Core runs as this account's user service, so run this only where no other
SpaceTerm release Core runs and its state can be discarded, as on a CI runner.
"""

import argparse
from pathlib import Path

from spaceterm_tasks import main
from spaceterm_tasks.compatibility import update

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--signature", type=Path, required=True)
    args = parser.parse_args()
    main(lambda: update(args.tag, args.archive.resolve(), args.signature.resolve()))
