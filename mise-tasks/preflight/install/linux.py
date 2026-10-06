#!/usr/bin/env python
# MISE description="Build, verify, and install SpaceTerm Preflight beside a SpaceTerm release"
# MISE confirm="Replace the SpaceTerm Preflight installation in ~/.local/lib?"
"""Build, verify, and install SpaceTerm Preflight beside a SpaceTerm release."""

import argparse
import subprocess

from spaceterm_tasks import TaskError, main
from spaceterm_tasks.package_linux import archive_path, package
from spaceterm_tasks.release import INSTALLER


def install():
    package()
    archive = archive_path("preflight")
    if subprocess.run(["sh", str(INSTALLER), "--archive", str(archive)]).returncode:
        raise TaskError("SpaceTerm Preflight could not be installed")


if __name__ == "__main__":
    argparse.ArgumentParser(description=__doc__).parse_args()
    main(install)
