#!/usr/bin/env python
# MISE description="Publish the checked-out release tag with its prepared assets"
# USAGE arg "<tag>" help="Stable release tag at HEAD, for example v1.2.3"
"""Publish the checked-out release tag with its prepared assets."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.release import publish

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    args = parser.parse_args()
    main(lambda: publish(args.tag))
