#!/usr/bin/env python3
"""Create the signed GitHub Releases assets from an already verified app and DMG."""

import argparse
import base64
import hashlib
import importlib.util
import os
import plistlib
import shutil
import subprocess
import tempfile
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPARKLE_NS = "http://www.andymatuschak.org/xml-namespaces/sparkle"
ACCOUNT = "io.github.sadiksaifi.spaceterm"


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def checked(arguments, **kwargs):
    result = subprocess.run(arguments, capture_output=True, check=False, **kwargs)
    if result.returncode:
        raise SystemExit("Release signing or verification failed")
    return result.stdout


def signing_options(tools):
    public = Path(__file__).with_name("update-public-key.txt").read_text().strip()
    secret = os.environ.get("SPARKLE_PRIVATE_KEY")
    if secret:
        try:
            seed = base64.b64decode(secret.strip(), validate=True)
        except ValueError:
            raise SystemExit("The update signing secret is invalid") from None
        if len(seed) != 32:
            raise SystemExit("The update signing secret must contain a 32-byte Ed25519 seed")
        # Derive only the public key through stdin. Never put the private key in argv or logs.
        derived = checked(["openssl", "pkey", "-inform", "DER", "-pubout", "-outform", "DER"],
                          input=bytes.fromhex("302e020100300506032b657004220420") + seed)
        actual = base64.b64encode(derived[-32:]).decode()
        options = ["--ed-key-file", "-"]
        stdin = secret.strip().encode()
    else:
        actual = checked([tools / "generate_keys", "--account", ACCOUNT, "-p"]).decode().strip()
        options = ["--account", ACCOUNT]
        stdin = None
    if actual != public:
        raise SystemExit("The signing key does not match the key trusted by installed applications")
    return options, stdin


def verify_feed(path, archive, version):
    root = ET.parse(path).getroot()
    items = root.findall("./channel/item")
    if len(items) != 1:
        raise ValueError("the release feed must contain exactly one complete update")
    item = items[0]
    if item.findtext(f"{{{SPARKLE_NS}}}version") != version or item.findtext(f"{{{SPARKLE_NS}}}shortVersionString") != version:
        raise ValueError("the feed version must match the Git tag")
    if item.find(f"{{{SPARKLE_NS}}}channel") is not None:
        raise ValueError("release channels are not supported")
    enclosure = item.find("enclosure")
    if enclosure is None:
        raise ValueError("the release archive is missing")
    expected = f"https://github.com/sadiksaifi/SpaceTerm/releases/download/v{version}/{archive.name}"
    if enclosure.get("url") != expected or enclosure.get("length") != str(archive.stat().st_size):
        raise ValueError("the release archive URL or size does not match")
    signature = enclosure.get(f"{{{SPARKLE_NS}}}edSignature", "")
    if len(base64.b64decode(signature, validate=True)) != 64:
        raise ValueError("the release archive signature is missing")
    return signature


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    args = parser.parse_args()
    version = load("version", ROOT / "packaging/resolve-release-version.py").resolve(ROOT, args.tag)["version"]
    plist = plistlib.loads((ROOT / "dist/SpaceTerm.app/Contents/Info.plist").read_bytes())
    if plist["CFBundleShortVersionString"] != version or plist["CFBundleVersion"] != version:
        raise SystemExit("Package the selected Git tag before preparing release assets")
    tools = load("sparkle", Path(__file__).with_name("prepare-sparkle.py")).prepare() / "bin"
    options, stdin = signing_options(tools)
    environment = {key: value for key, value in os.environ.items() if key != "SPARKLE_PRIVATE_KEY"}
    with tempfile.TemporaryDirectory(dir=ROOT / "dist") as temporary:
        directory = Path(temporary)
        archive = directory / f"SpaceTerm-{version}-darwin-arm64.dmg"
        shutil.copyfile(ROOT / "dist/SpaceTerm.dmg", archive)
        checked([tools / "generate_appcast", *options, "--maximum-deltas", "0", "--maximum-versions", "1",
                 "--download-url-prefix", f"https://github.com/sadiksaifi/SpaceTerm/releases/download/v{version}/",
                 "--link", f"https://github.com/sadiksaifi/SpaceTerm/releases/tag/v{version}", directory],
                input=stdin, env=environment)
        feed = directory / "appcast.xml"
        signature = verify_feed(feed, archive, version)
        checked([tools / "sign_update", *options, "--verify", archive, signature], input=stdin, env=environment)
        checked([tools / "sign_update", *options, "--verify", feed], input=stdin, env=environment)
        installer = directory / "install.sh"
        shutil.copyfile(Path(__file__).with_name("install-release.sh"), installer)
        checksums = []
        for path in (archive, feed, installer):
            with path.open("rb") as source:
                digest = hashlib.file_digest(source, "sha256").hexdigest()
            checksums.append(f"{digest}  {path.name}\n")
        (directory / "SHA256SUMS").write_text("".join(checksums))
        destination = ROOT / "dist/release"
        if destination.exists():
            shutil.rmtree(destination)
        destination.mkdir()
        for path in (archive, feed, installer, directory / "SHA256SUMS"):
            shutil.copyfile(path, destination / path.name)
    print("Release DMG, signed appcast, installer, and checksums verified.")


if __name__ == "__main__":
    main()
