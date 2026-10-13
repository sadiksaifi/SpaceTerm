#!/usr/bin/env python
# MISE description="Package the separate macos headless server"
"""Package the separate macos headless server without installation."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.package_server_macos import package
from spaceterm_tasks.release_server import create_account_assets

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    args = parser.parse_args()

    def build_and_sign():
        package()
        create_account_assets("darwin-arm64")

    main(build_and_sign)
