#!/usr/bin/env python
# MISE description="Write the git-cliff notes of the stable release tagged at HEAD"
# USAGE flag "--output <file>" help="Notes file to write; prints the notes without it"
"""Write the git-cliff notes of the stable release tagged at HEAD."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.release import notes, write_notes


def run(output):
    if output:
        write_notes(output)
    else:
        print(notes(), end="")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output")
    args = parser.parse_args()
    main(lambda: run(args.output))
