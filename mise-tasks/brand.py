#!/usr/bin/env python
# MISE description="Generate every brand asset in assets/brand from the mark and the icon document"
# USAGE flag "--check" help="Verify the committed brand files against their sources on any host"
"""Generate every brand asset in assets/brand from the mark and the icon document."""

import argparse
import platform
from pathlib import Path

from spaceterm_tasks import TaskError, main
from spaceterm_tasks.brand import check, generate
from spaceterm_tasks.venv import reexec_with


def run(checking):
    if checking:
        check()
        return
    if platform.system() != "Darwin":
        raise TaskError("generating brand files requires macOS with Xcode's Icon Composer")
    # fonttools outlines the social preview's text; the fonts tasks pin the same release.
    reexec_with(Path(__file__).parent / "fonts" / "requirements.txt")
    generate()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    main(lambda: run(args.check))
