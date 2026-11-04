#!/usr/bin/env python
# MISE description="Package the x86_64 Linux desktop with both Core builds and the notes"
# MISE depends=["setup:linux-toolchain"]
# USAGE arg "<tag>" help="Annotated stable release tag at HEAD, for example v1.2.3"
# USAGE flag "--notes <file>" help="Release notes to embed" default="dist/release-notes.md"
# USAGE flag "--core <directory>" help="Both signed Core builds" default="dist/release-core"
"""Package the x86_64 Linux desktop with both Core builds and the release notes.

The archive lands in dist/release-desktop-linux. release:assemble signs it.
"""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.release import CORE, NOTES, desktop_linux

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    parser.add_argument("--notes", default=NOTES)
    parser.add_argument("--core", default=CORE)
    args = parser.parse_args()
    main(lambda: desktop_linux(args.tag, args.notes, args.core))
