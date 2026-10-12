#!/usr/bin/env python
# MISE description="Package the separate macos headless server"
# USAGE arg "<tag>" help="Clean annotated stable tag at HEAD"
"""Package the separate macos headless server without installation."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.package_server_macos import package

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    args = parser.parse_args()
    main(lambda: package(args.tag))
