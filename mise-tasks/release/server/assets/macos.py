#!/usr/bin/env python
# MISE description="Sign and verify complete macos server release assets"
# USAGE arg "<tag>" help="Packaged annotated stable release tag"
"""Sign the separate server feed and archive, using existing update trust."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.release_server import create_assets

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    args = parser.parse_args()
    main(lambda: create_assets(args.tag, "darwin-arm64"))
