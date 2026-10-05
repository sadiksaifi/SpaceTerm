#!/usr/bin/env python
# MISE description="Restore the pinned AccessKit Git dependencies"
"""Restore the pinned AccessKit Git dependencies."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.overrides import ACCESSKIT, disable, refresh_metadata


def run():
    disable(ACCESSKIT)
    refresh_metadata()


if __name__ == "__main__":
    argparse.ArgumentParser(description=__doc__).parse_args()
    main(run)
