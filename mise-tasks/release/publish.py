#!/usr/bin/env python
# MISE description="Publish the assembled release assets as one GitHub release"
# USAGE arg "<tag>" help="Stable release tag at HEAD, for example v1.2.3"
# USAGE flag "--notes <file>" help="Release notes from release:notes" default="dist/release-notes.md"
"""Publish the assembled release assets in dist/release as one GitHub release.

A dry run verifies the assets and publishes nothing.
"""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.release import NOTES, publish

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    parser.add_argument("--notes", default=NOTES)
    args = parser.parse_args()
    main(lambda: publish(args.tag, args.notes))
