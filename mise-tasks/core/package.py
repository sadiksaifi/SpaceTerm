#!/usr/bin/env python
# MISE description="Build and sign a nonproduction SpaceTerm Core candidate without publishing"
# MISE depends=["setup:linux-toolchain"]
"""Build and sign a nonproduction SpaceTerm Core candidate without publishing.

Release Core builds come from release:core.
"""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.core_builds import NONPRODUCTION
from spaceterm_tasks.core_package import TARGETS, package

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--identity", choices=NONPRODUCTION, default="spaceterm-preflight")
    parser.add_argument("--target", choices=TARGETS.values())
    parser.add_argument(
        "--catalog", help="Stage this verified native target for catalog collection"
    )
    args = parser.parse_args()

    def run():
        archive, signature = package(args.identity, target=args.target)
        if args.catalog:
            from spaceterm_tasks.core_catalog import stage_catalog

            stage_catalog(archive, signature, args.catalog, args.identity)

    main(run)
