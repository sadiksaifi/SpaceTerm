#!/usr/bin/env python
# MISE description="Build, verify, and install SpaceTerm Preflight beside a SpaceTerm release"
# MISE confirm="Replace /Applications/SpaceTerm Preflight.app?"
"""Build, verify, and install SpaceTerm Preflight beside a SpaceTerm release."""

import argparse
import shutil
import subprocess
from pathlib import Path

from spaceterm_tasks import TaskError, main
from spaceterm_tasks.package_macos import DIST, package

APPLICATION = Path("/Applications/SpaceTerm Preflight.app")


def install():
    package()
    shutil.rmtree(APPLICATION, ignore_errors=True)
    if subprocess.run(["ditto", str(DIST / APPLICATION.name), str(APPLICATION)]).returncode:
        raise TaskError("SpaceTerm Preflight could not be installed")
    print("Installed SpaceTerm Preflight in /Applications")


if __name__ == "__main__":
    argparse.ArgumentParser(description=__doc__).parse_args()
    main(install)
