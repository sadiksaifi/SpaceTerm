#!/usr/bin/env python
# MISE description="Restore the pinned Zed Git dependencies"
"""Restore the pinned Zed Git dependencies."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.overrides import GPUI, disable, refresh_metadata


def run():
    disable(GPUI)
    refresh_metadata()


if __name__ == "__main__":
    argparse.ArgumentParser(description=__doc__).parse_args()
    main(run)
