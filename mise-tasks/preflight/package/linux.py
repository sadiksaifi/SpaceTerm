#!/usr/bin/env python
# MISE description="Build and verify SpaceTerm Preflight, the optimized x86_64 Linux archive"
"""Build and verify SpaceTerm Preflight, the optimized x86_64 Linux archive."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.package_linux import package

if __name__ == "__main__":
    argparse.ArgumentParser(description=__doc__).parse_args()
    main(package)
