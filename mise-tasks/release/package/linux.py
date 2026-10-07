#!/usr/bin/env python
# MISE description="Package an annotated release tag at HEAD as SpaceTerm for x86_64 Linux"
# USAGE arg "<tag>" help="Annotated stable release tag at HEAD, for example v1.2.3"
"""Package an annotated release tag at HEAD as SpaceTerm for x86_64 Linux."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.package_linux import package

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    args = parser.parse_args()
    main(lambda: package(args.tag))
