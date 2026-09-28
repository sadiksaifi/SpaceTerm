#!/usr/bin/env python3
"""Keep the release signing key in Keychain and commit only its public half."""

import base64
import importlib.util
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location("sparkle", Path(__file__).with_name("prepare-sparkle-macos.py"))
SPARKLE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SPARKLE)
ACCOUNT = "io.github.sadiksaifi.spaceterm"


def main():
    tool = SPARKLE.prepare() / "bin/generate_keys"
    public_file = ROOT / "packaging/macos/update-public-key.txt"
    public = subprocess.run([tool, "--account", ACCOUNT, "-p"], capture_output=True, text=True)
    if public.returncode:
        if public_file.exists():
            raise SystemExit("Restore the existing release key to Keychain before continuing")
        subprocess.run([tool, "--account", ACCOUNT], check=True, stdout=subprocess.DEVNULL)
        public = subprocess.run([tool, "--account", ACCOUNT, "-p"], capture_output=True, text=True, check=True)
    value = public.stdout.strip()
    if len(base64.b64decode(value, validate=True)) != 32:
        raise SystemExit("The signing tool returned an invalid public key")
    if public_file.exists() and public_file.read_text().strip() != value:
        raise SystemExit("The Keychain signing key does not match the release public key")
    public_file.write_text(value + "\n")
    print("Update signing key is in Keychain; public key is ready for the release bundle.")


if __name__ == "__main__":
    main()
