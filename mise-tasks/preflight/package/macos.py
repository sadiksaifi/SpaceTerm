#!/usr/bin/env python
# MISE description="Build and verify SpaceTerm Preflight, the optimized macOS app and disk image"
# USAGE flag "--ssh-config <path>" help="OpenSSH config containing trusted native builder aliases"
"""Build and verify SpaceTerm Preflight, the optimized macOS app and disk image."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.package_macos import package

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--ssh-config", help="OpenSSH config containing trusted native builder aliases"
    )
    args = parser.parse_args()
    main(lambda: package(ssh_config=args.ssh_config))
