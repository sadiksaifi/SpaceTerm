#!/usr/bin/env python
# MISE description="Prepare the private terminal font identities from the pinned Nerd Fonts archive"
# USAGE arg "<archive>" help="JetBrainsMono.tar.xz from Nerd Fonts 3.5.1"
# USAGE flag "--check" help="Verify the committed fonts without changing files"
"""Prepare the private terminal font identities from the pinned Nerd Fonts archive."""

import argparse
from pathlib import Path

from spaceterm_tasks import main
from spaceterm_tasks.fonts import TERMINAL, prepare
from spaceterm_tasks.venv import reexec_with


def run(archive, check):
    # fonttools lives in a private environment holding the pinned requirements.
    reexec_with(Path(__file__).with_name("requirements.txt"))
    prepare(TERMINAL, archive, check)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    main(lambda: run(args.archive, args.check))
