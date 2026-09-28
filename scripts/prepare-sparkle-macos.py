#!/usr/bin/env python3
"""Install the pinned Sparkle tools and framework after verifying their archive."""

import hashlib
import json
import shutil
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def prepare():
    if sys.platform != "darwin":
        raise SystemExit("Sparkle packaging requires macOS")
    pin = json.loads((ROOT / "packaging/macos/sparkle.json").read_text())
    destination = ROOT / "target/sparkle" / pin["version"]
    marker = destination / ".verified-sha256"
    if marker.is_file() and marker.read_text().strip() == pin["sha256"]:
        return destination
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=destination.parent) as temporary:
        temporary = Path(temporary)
        archive = temporary / "sparkle.tar.xz"
        url = f"https://github.com/sparkle-project/Sparkle/releases/download/{pin['version']}/Sparkle-{pin['version']}.tar.xz"
        with urllib.request.urlopen(url, timeout=120) as source, archive.open("wb") as target:
            shutil.copyfileobj(source, target)
        with archive.open("rb") as contents:
            if hashlib.file_digest(contents, "sha256").hexdigest() != pin["sha256"]:
                raise SystemExit("Sparkle archive checksum did not match")
        extracted = temporary / "extracted"
        extracted.mkdir()
        with tarfile.open(archive) as contents:
            contents.extractall(extracted, filter="data")
        (extracted / ".verified-sha256").write_text(pin["sha256"] + "\n")
        if destination.exists():
            shutil.rmtree(destination)
        extracted.rename(destination)
    return destination


if __name__ == "__main__":
    print(prepare())
