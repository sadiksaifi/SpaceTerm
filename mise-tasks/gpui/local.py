#!/usr/bin/env python
# MISE description="Build against a local SpaceTerm Zed checkout instead of the pinned tag"
# USAGE arg "[checkout]" help="Local SpaceTerm Zed checkout, relative to the repository" default="../zed"
"""Build against a local SpaceTerm Zed checkout instead of the pinned tag."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.overrides import GPUI, enable, refresh_metadata


def run(checkout):
    enable(GPUI, checkout)
    refresh_metadata()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("checkout", nargs="?", default="../zed")
    args = parser.parse_args()
    main(lambda: run(args.checkout))
