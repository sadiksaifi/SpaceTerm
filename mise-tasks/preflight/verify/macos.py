#!/usr/bin/env python
# MISE description="Verify the SpaceTerm Preflight app and disk image in dist"
# USAGE flag "--app <app>" help="Application bundle; defaults to dist/SpaceTerm Preflight.app"
# USAGE flag "--dmg <dmg>" help="Disk image; defaults to dist/SpaceTerm Preflight.dmg"
"""Verify the SpaceTerm Preflight app and disk image in dist."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.package_macos import verify

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app")
    parser.add_argument("--dmg")
    args = parser.parse_args()
    main(lambda: verify(args.app, args.dmg))
