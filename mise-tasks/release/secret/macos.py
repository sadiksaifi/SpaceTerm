#!/usr/bin/env python
# MISE description="Set the GitHub Actions update signing secret from macOS Keychain"
# MISE confirm="Replace the UPDATE_SIGNING_KEY secret of sadiksaifi/SpaceTerm?"
"""Set the GitHub Actions update signing secret from macOS Keychain."""

import argparse
import os
import subprocess
import tempfile
from pathlib import Path

from spaceterm_tasks import TaskError, main, sparkle
from spaceterm_tasks.release import SIGNING_SECRET, public_key


def configure():
    generate_keys = sparkle.tool("generate_keys")
    keychain = subprocess.run(
        [generate_keys, "--account", sparkle.ACCOUNT, "-p"], capture_output=True, text=True
    )
    if keychain.returncode or keychain.stdout.strip() != public_key():
        raise TaskError("the Keychain signing key does not match the release public key")
    with tempfile.TemporaryDirectory(prefix="spaceterm-signing-") as temporary:
        path = Path(temporary) / "key"
        exported = subprocess.run(
            [generate_keys, "--account", sparkle.ACCOUNT, "-x", path], capture_output=True
        )
        if exported.returncode:
            raise TaskError("the signing key could not be exported from Keychain")
        os.chmod(path, 0o600)
        with path.open("rb") as key:
            stored = subprocess.run(
                ["gh", "secret", "set", SIGNING_SECRET, "--repo", "sadiksaifi/SpaceTerm"],
                stdin=key,
                capture_output=True,
            )
        if stored.returncode:
            raise TaskError("GitHub could not store the update signing secret")
    print("GitHub Actions update signing secret configured.")


if __name__ == "__main__":
    argparse.ArgumentParser(description=__doc__).parse_args()
    main(configure)
