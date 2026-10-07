#!/usr/bin/env python
# MISE description="Exercise the Linux updater against assets the release task signs"
"""Sign a fixture release with the release task, then verify and install it with the updater.

The updater's own tests cover rejected signatures, the staged swap, rollback after an
interrupted swap, and a read-only installation. This task adds the assets the release task
produces, so the signer and the verifier cannot drift apart.
"""

import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, main
from spaceterm_tasks.package_linux import stage, write_archive
from spaceterm_tasks.release import derived_public_key, sign_linux_release

FILTERS = ("platform::linux_updates", "updates::release_feed")


def run():
    if sys.platform != "linux":
        raise TaskError("the Linux updater runs only on Linux")
    with tempfile.TemporaryDirectory(prefix="spaceterm-updater-test-") as temporary:
        directory = Path(temporary)
        executable = directory / "executable"
        executable.write_bytes(b"#!/bin/sh\nexit 0\n")
        tree = directory / "tree" / "spaceterm"
        stage("spaceterm", executable, tree)
        archive = directory / "archive.tar.gz"
        write_archive(tree, archive)
        assets = directory / "assets"
        assets.mkdir()
        # A fresh key: the fixture never touches the release signing key.
        seed = os.urandom(32)
        sign_linux_release(seed, archive, "0.1.1", int(time.time()), assets)
        (assets / "public-key").write_text(derived_public_key(seed))
        executable.rename(assets / "executable")
        tests = subprocess.run(
            [
                "cargo",
                "test",
                "--package",
                "spaceterm",
                "--bin",
                "spaceterm",
                "--all-features",
                "--locked",
                "--",
                *FILTERS,
            ],
            cwd=ROOT,
            env={**os.environ, "SPACETERM_RELEASE_FIXTURE": str(assets)},
        )
        if tests.returncode:
            raise TaskError("the Linux updater tests failed")
        verified = assets / "verified"
        if not verified.is_file() or verified.read_text() != "0.1.1":
            raise TaskError("the updater did not verify the release task's assets")
    print("The updater verified and installed the release task's signed assets.")


if __name__ == "__main__":
    main(run)
