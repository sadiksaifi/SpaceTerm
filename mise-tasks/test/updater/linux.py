#!/usr/bin/env python
# MISE description="Exercise the Linux updater against assets the release task signs"
"""Sign a fixture release with the release feed writer, then verify and install it with the
updater.

The updater's own tests cover rejected signatures, the staged swap, rollback after an
interrupted swap, and a read-only installation. This task adds the assets the release writes:
`spaceterm-desktop-linux-x86_64.json` with its raw signature, the archive with its raw
signature, and `update-trust.json` with a fresh current and backup key. The archive and feed are
signed with the backup key, so the updater must accept either key.
"""

import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, main, release_feed, update_signing
from spaceterm_tasks.package_linux import stage, write_archive

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
        assets = directory / "assets"
        assets.mkdir()
        archive = assets / release_feed.archive_name("desktop", "1.0.1", "linux-x86_64")
        write_archive(tree, archive)
        # Fresh keys: the fixture never touches the release signing key.
        current, backup = os.urandom(32), os.urandom(32)
        keys = tuple(update_signing.derived_public_key(seed) for seed in (current, backup))
        (assets / "update-trust.json").write_text(
            json.dumps({"current": keys[0], "backup": keys[1]})
        )
        update_signing.sign(backup, archive, archive.with_name(f"{archive.name}.sig"))
        release_feed.write(
            backup,
            keys,
            assets,
            product="desktop",
            platform="linux-x86_64",
            version="1.0.1",
            published_at=int(time.time()),
            protocols=release_feed.protocols(),
            archive=archive,
        )
        # Feed the desktop admission gate the URL the appcast writer publishes.
        (assets / "appcast-enclosure").write_text(
            release_feed.download_url("1.0.1", release_feed.dmg_name("1.0.1"))
        )
        executable.rename(assets / "executable")
        tests = subprocess.run(
            [
                "cargo",
                "test",
                "--package",
                "spaceterm",
                "--lib",
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
        if not verified.is_file() or verified.read_text() != "1.0.1":
            raise TaskError("the updater did not verify the release task's assets")
    print("The updater verified and installed the release task's signed assets.")


if __name__ == "__main__":
    main(run)
