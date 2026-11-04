#!/usr/bin/env python
# MISE description="Tag HEAD of a disposable CI checkout for a release dry run"
"""Tag HEAD of a disposable CI checkout for a release dry run and print the tag.

The tag is newer than every stable tag. In GitHub Actions it is also written to the step's
`tag` output. It never runs outside CI: run release:dry-run on a workstation.
"""

import argparse
import os

from spaceterm_tasks import TaskError, main
from spaceterm_tasks.release import create_dry_run_tag


def tag():
    if os.environ.get("GITHUB_ACTIONS") != "true":
        raise TaskError("dry-run tags belong to disposable CI checkouts; run release:dry-run")
    created = create_dry_run_tag()
    output = os.environ.get("GITHUB_OUTPUT")
    if output:
        with open(output, "a") as step:
            step.write(f"tag={created}\n")
    print(created)


if __name__ == "__main__":
    argparse.ArgumentParser(description=__doc__).parse_args()
    main(tag)
