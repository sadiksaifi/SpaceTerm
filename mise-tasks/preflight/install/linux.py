#!/usr/bin/env python
# MISE description="Build, verify, and install SpaceTerm Preflight beside a SpaceTerm release"
# MISE confirm="Replace the SpaceTerm Preflight installation in ~/.local/lib?"
"""Build, verify, and install SpaceTerm Preflight beside a SpaceTerm release."""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.package_linux import archive_path, install_preflight, package


def install():
    package()
    install_preflight(archive_path("preflight"))


if __name__ == "__main__":
    argparse.ArgumentParser(description=__doc__).parse_args()
    main(install)
