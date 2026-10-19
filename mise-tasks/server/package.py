#!/usr/bin/env python
# MISE description="Build and sign an independent headless server candidate without publishing"
"""Build and sign an independent headless server candidate without publishing."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.server_package import IDENTITIES, package

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--identity", choices=IDENTITIES, default="spaceterm-preflight")
    parser.add_argument("--release-tag")
    args = parser.parse_args()

    def run():
        package(args.identity, args.release_tag)

    main(run)
