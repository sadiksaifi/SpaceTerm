#!/usr/bin/env python
# MISE description="Verify that macOS Keychain holds the update signing key installed apps trust"
"""Verify that macOS Keychain holds the update signing key installed apps trust."""

import argparse
import subprocess

from spaceterm_tasks import TaskError, main, sparkle
from spaceterm_tasks.release import public_key


def verify():
    keychain = subprocess.run(
        [sparkle.tool("generate_keys"), "--account", sparkle.ACCOUNT, "-p"],
        capture_output=True,
        text=True,
    )
    if keychain.returncode:
        raise TaskError("restore the release signing key to Keychain before continuing")
    if keychain.stdout.strip() != public_key():
        raise TaskError("the Keychain signing key does not match the release public key")
    print("The update signing key is in Keychain and matches the release public key.")


if __name__ == "__main__":
    argparse.ArgumentParser(description=__doc__).parse_args()
    main(verify)
