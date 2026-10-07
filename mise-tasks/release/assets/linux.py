#!/usr/bin/env python
# MISE description="Sign and verify the Linux GitHub Releases assets of a packaged release"
# USAGE arg "<tag>" help="Packaged stable release tag, for example v1.2.3"
"""Sign and verify the Linux GitHub Releases assets of a packaged release."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.release import create_linux_assets

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    args = parser.parse_args()
    main(lambda: create_linux_assets(args.tag))
