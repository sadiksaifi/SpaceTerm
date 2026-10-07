#!/usr/bin/env python
# MISE description="Verify the SpaceTerm Preflight Linux archive in dist"
# USAGE flag "--archive <archive>" help="Archive; defaults to dist/spaceterm-preflight-linux-x86_64.tar.gz"
"""Verify the SpaceTerm Preflight Linux archive in dist."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.package_linux import verify

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive")
    args = parser.parse_args()
    main(lambda: verify(args.archive))
