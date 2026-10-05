#!/usr/bin/env python
# MISE description="Prepare the private UI font identities from the pinned Inter archive"
# USAGE arg "<archive>" help="Inter-4.1.zip from the official Inter 4.1 release"
# USAGE flag "--check" help="Verify the committed fonts without changing files"
"""Prepare the private UI font identities from the pinned Inter archive."""

import argparse
from pathlib import Path

from spaceterm_tasks import main
from spaceterm_tasks.fonts import UI, prepare
from spaceterm_tasks.venv import reexec_with


def run(archive, check):
    # fonttools lives in a private environment holding the pinned requirements.
    reexec_with(Path(__file__).with_name("requirements.txt"))
    prepare(UI, archive, check)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    main(lambda: run(args.archive, args.check))
