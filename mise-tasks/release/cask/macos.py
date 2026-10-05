#!/usr/bin/env python
# MISE description="Point the tapped Homebrew cask at a published release"
# USAGE arg "<tag>" help="Published stable release tag, for example v1.2.3"
"""Point the tapped Homebrew cask at a published release."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.release import update_cask

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    args = parser.parse_args()
    main(lambda: update_cask(args.tag))
