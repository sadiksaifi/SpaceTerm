#!/usr/bin/env python
# MISE description="Dry-run the release steps this host can run, without release secrets"
# MISE depends=["setup:linux-toolchain"]
"""Dry-run the release steps this host can run, without Apple or update signing secrets.

The steps run in a disposable checkout of HEAD with a dry-run tag, a generated update key,
ad hoc code signatures and no notarization. Nothing is published. A Mac builds both Core
targets and the macOS desktop; Linux builds the Linux Core. A release workflow run with publish off exercises both platforms.
"""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.core_package import host_target
from spaceterm_tasks.release_dry_run import run

if __name__ == "__main__":
    argparse.ArgumentParser(description=__doc__).parse_args()
    main(lambda: run(host_target()))
