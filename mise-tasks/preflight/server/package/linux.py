#!/usr/bin/env python
# MISE description="Package the separate linux headless server"
"""Package the separate linux headless server without installation."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.package_server_linux import package

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    args = parser.parse_args()
    main(package)
