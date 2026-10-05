#!/usr/bin/env python
# MISE description="Build and verify SpaceTerm Preflight, the optimized macOS app and disk image"
"""Build and verify SpaceTerm Preflight, the optimized macOS app and disk image."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.package_macos import package

if __name__ == "__main__":
    argparse.ArgumentParser(description=__doc__).parse_args()
    main(package)
