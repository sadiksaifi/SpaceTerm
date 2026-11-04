#!/usr/bin/env python
# MISE description="Package, sign, notarize and staple the macOS desktop and its appcast"
# USAGE arg "<tag>" help="Annotated stable release tag at HEAD, for example v1.2.3"
# USAGE flag "--notes <file>" help="Release notes to embed" default="dist/release-notes.md"
# USAGE flag "--core <directory>" help="Both signed Core builds" default="dist/release-core"
"""Package, sign, notarize and staple the macOS desktop and its appcast.

The app embeds both Core builds and the release notes. The disk image and its signed Sparkle
appcast land in dist/release-desktop-macos.
"""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.release import CORE, NOTES, desktop_macos

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    parser.add_argument("--notes", default=NOTES)
    parser.add_argument("--core", default=CORE)
    args = parser.parse_args()
    main(lambda: desktop_macos(args.tag, args.notes, args.core))
