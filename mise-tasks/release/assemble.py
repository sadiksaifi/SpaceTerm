#!/usr/bin/env python
# MISE description="Sign the feeds and Linux desktop archive and assemble the release assets"
# USAGE arg "<tag>" help="Annotated stable release tag at HEAD, for example v1.2.3"
# USAGE flag "--partial" help="Dry run only: omit the products this host did not build"
"""Sign the feeds and the Linux desktop archive and assemble the release assets.

Reads dist/release-core, dist/release-desktop-macos and dist/release-desktop-linux, and writes
every release asset with SHA256SUMS into dist/release.
"""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.release import assemble

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    parser.add_argument("--partial", action="store_true")
    args = parser.parse_args()
    main(lambda: assemble(args.tag, complete=not args.partial))
