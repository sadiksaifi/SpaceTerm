#!/usr/bin/env python
# MISE description="Run SpaceTerm Development from a signed macOS application bundle"
"""Run SpaceTerm Development from a signed macOS application bundle."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.development_macos import launch

if __name__ == "__main__":
    argparse.ArgumentParser(description=__doc__).parse_args()
    main(launch)
