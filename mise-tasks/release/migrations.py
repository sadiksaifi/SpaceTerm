#!/usr/bin/env python
# MISE description="Check the release's Data Generations, migration fixtures and Update Floors"
# USAGE arg "<tag>" help="Stable release tag, for example v1.2.3"
"""Check the release's Data Generations, migration fixtures and Update Floors.

No root's oldest or current generation decreases against the previous release, every
generation from oldest to current has a fixture, and each product's floor is this release or
a published one, which the GitHub CLI confirms.
"""

import argparse

from spaceterm_tasks import main, release_floor
from spaceterm_tasks.packaging import release_version


def run(tag):
    floors = release_floor.check(release_version(tag))
    for product, floor in floors.items():
        print(f"The {product} Update Floor of {tag} is {floor}.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    args = parser.parse_args()
    main(lambda: run(args.tag))
