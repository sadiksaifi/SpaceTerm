#!/usr/bin/env python
# MISE description="Check the native updater and its Rust integration"
"""Check the native updater and its Rust integration."""

import argparse
import os
import subprocess

from spaceterm_tasks import ROOT, main, sparkle


def check():
    environment = {**os.environ, "SPACETERM_SPARKLE_DIR": str(sparkle.directory())}
    return subprocess.run(
        ["cargo", "check", "--all-targets", "--all-features", "--locked"], cwd=ROOT, env=environment
    ).returncode


if __name__ == "__main__":
    argparse.ArgumentParser(description=__doc__).parse_args()
    main(check)
