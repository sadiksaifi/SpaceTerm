#!/usr/bin/env python
# MISE description="Print the notes for the stable release tagged at HEAD"
"""Print the notes for the stable release tagged at HEAD."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.release import notes

if __name__ == "__main__":
    argparse.ArgumentParser(description=__doc__).parse_args()
    main(lambda: print(notes(), end=""))
