#!/usr/bin/env python
# MISE description="Run SpaceTerm Development on Wayland"
"""Run SpaceTerm Development on Wayland."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.development_linux import launch

if __name__ == "__main__":
    argparse.ArgumentParser(description=__doc__).parse_args()
    main(lambda: launch("wayland"))
