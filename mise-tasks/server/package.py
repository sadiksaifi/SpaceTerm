#!/usr/bin/env python
# MISE description="Build and sign an independent headless server candidate without publishing"
# MISE depends=["setup:linux-toolchain"]
"""Build and sign an independent headless server candidate without publishing."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.server_package import IDENTITIES, TARGETS, package

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--identity", choices=IDENTITIES, default="spaceterm-preflight")
    parser.add_argument("--release-tag")
    parser.add_argument("--target", choices=TARGETS.values())
    parser.add_argument(
        "--catalog", help="Stage this verified native target for catalog collection"
    )
    args = parser.parse_args()

    def run():
        archive, signature = package(args.identity, args.release_tag, target=args.target)
        if args.release_tag:
            from spaceterm_tasks.release import create_server_assets

            create_server_assets(args.release_tag, archive, signature)
        if args.catalog:
            from spaceterm_tasks.server_catalog import stage_catalog

            stage_catalog(archive, signature, args.catalog, args.identity)

    main(run)
