#!/usr/bin/env python
# MISE description="Build against a local SpaceTerm AccessKit checkout instead of the pinned tag"
# USAGE arg "<checkout>" help="Local SpaceTerm AccessKit checkout, relative to the repository"
"""Build against a local SpaceTerm AccessKit checkout instead of the pinned tag."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.overrides import ACCESSKIT, enable, refresh_metadata


def run(checkout):
    enable(ACCESSKIT, checkout)
    refresh_metadata()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("checkout")
    args = parser.parse_args()
    main(lambda: run(args.checkout))
