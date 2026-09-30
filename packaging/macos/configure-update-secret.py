#!/usr/bin/env python3
"""Provision this repository's Actions signing secret from the retained Keychain key."""

import importlib.util
import os
import subprocess
import tempfile
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("sparkle", Path(__file__).with_name("prepare-sparkle.py"))
SPARKLE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SPARKLE)


def main():
    tool = SPARKLE.prepare() / "bin/generate_keys"
    public = subprocess.run([tool, "--account", "io.github.sadiksaifi.spaceterm", "-p"],
                            check=True, capture_output=True, text=True).stdout.strip()
    expected = Path(__file__).with_name("update-public-key.txt").read_text().strip()
    if public != expected:
        raise SystemExit("The Keychain signing key does not match the release public key")
    with tempfile.TemporaryDirectory(prefix="spaceterm-signing-") as temporary:
        path = Path(temporary) / "key"
        subprocess.run([tool, "--account", "io.github.sadiksaifi.spaceterm", "-x", path], check=True, capture_output=True)
        os.chmod(path, 0o600)
        with path.open("rb") as key:
            subprocess.run(["gh", "secret", "set", "SPARKLE_PRIVATE_KEY", "--repo", "sadiksaifi/SpaceTerm"], stdin=key, check=True, capture_output=True)
    print("GitHub Actions update signing secret configured.")


if __name__ == "__main__":
    main()
