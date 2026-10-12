#!/usr/bin/env python3
"""Build the configured-APT prerequisite, never install or elevate downloaded code."""

import argparse
import sys
from pathlib import Path

# -I keeps the input and extracted artifact directories out of Python's imports.
sys.path.insert(0, str(Path(__file__).resolve().parents[4] / "mise-tasks/lib"))

from spaceterm_tasks import TaskError, main
from spaceterm_tasks.package_server_linux import package_evidence_candidate, package_publisher


def build():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence-candidate", action="store_true")
    parser.add_argument("--release-tag")
    arguments = parser.parse_args()
    if arguments.evidence_candidate:
        if arguments.release_tag is not None:
            raise TaskError("native evidence candidates cannot use a release tag")
        package_evidence_candidate()
    else:
        package_publisher(arguments.release_tag)


if __name__ == "__main__":
    main(build)
