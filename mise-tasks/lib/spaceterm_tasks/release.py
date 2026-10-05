"""Create signed GitHub Releases assets and update the Homebrew cask; see ADR 0009."""

import base64
import hashlib
import json
import os
import plistlib
import shutil
import subprocess
import tempfile
import xml.etree.ElementTree as ET
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, sparkle
from spaceterm_tasks.macos_bundle import identity
from spaceterm_tasks.package_macos import DIST, STABLE_TAG, release_version

SPARKLE_NS = "http://www.andymatuschak.org/xml-namespaces/sparkle"
RELEASES = "https://github.com/sadiksaifi/SpaceTerm/releases"
RELEASE_ASSETS = DIST / "release"
INSTALLER = ROOT / "packaging" / "macos" / "install-release.sh"
CASK = "sadiksaifi/tap/spaceterm"
ED25519_PKCS8_PREFIX = bytes.fromhex("302e020100300506032b657004220420")


def archive_name(version):
    return f"SpaceTerm-{version}-darwin-arm64.dmg"


def public_key():
    """The update trust root that installed applications carry in their Info.plist."""
    return identity("spaceterm")["SUPublicEDKey"]


def checked(arguments, **kwargs):
    result = subprocess.run(arguments, capture_output=True, check=False, **kwargs)
    if result.returncode:
        raise TaskError("release signing or verification failed")
    return result.stdout


def signing_options():
    secret = os.environ.get("SPARKLE_PRIVATE_KEY")
    if secret:
        try:
            seed = base64.b64decode(secret.strip(), validate=True)
        except ValueError:
            raise TaskError("the update signing secret is invalid") from None
        if len(seed) != 32:
            raise TaskError("the update signing secret must contain a 32-byte Ed25519 seed")
        # Derive only the public key through stdin. Never put the private key in argv or logs.
        derived = checked(
            ["openssl", "pkey", "-inform", "DER", "-pubout", "-outform", "DER"],
            input=ED25519_PKCS8_PREFIX + seed,
        )
        actual = base64.b64encode(derived[-32:]).decode()
        options = ["--ed-key-file", "-"]
        stdin = secret.strip().encode()
    else:
        actual = (
            checked([sparkle.tool("generate_keys"), "--account", sparkle.ACCOUNT, "-p"])
            .decode()
            .strip()
        )
        options = ["--account", sparkle.ACCOUNT]
        stdin = None
    if actual != public_key():
        raise TaskError("the signing key does not match the key trusted by installed applications")
    return options, stdin


def verify_feed(path, archive, version):
    root = ET.parse(path).getroot()
    items = root.findall("./channel/item")
    if len(items) != 1:
        raise ValueError("the release feed must contain exactly one complete update")
    item = items[0]
    if (
        item.findtext(f"{{{SPARKLE_NS}}}version") != version
        or item.findtext(f"{{{SPARKLE_NS}}}shortVersionString") != version
    ):
        raise ValueError("the feed version must match the Git tag")
    if item.find(f"{{{SPARKLE_NS}}}channel") is not None:
        raise ValueError("release channels are not supported")
    enclosure = item.find("enclosure")
    if enclosure is None:
        raise ValueError("the release archive is missing")
    expected = f"{RELEASES}/download/v{version}/{archive.name}"
    if enclosure.get("url") != expected or enclosure.get("length") != str(archive.stat().st_size):
        raise ValueError("the release archive URL or size does not match")
    signature = enclosure.get(f"{{{SPARKLE_NS}}}edSignature", "")
    if len(base64.b64decode(signature, validate=True)) != 64:
        raise ValueError("the release archive signature is missing")
    return signature


def create_assets(tag):
    """Sign the packaged release into dist/release: DMG, appcast, installer, and checksums."""
    version = release_version(tag)
    plist = plistlib.loads((DIST / "SpaceTerm.app/Contents/Info.plist").read_bytes())
    if plist["CFBundleShortVersionString"] != version or plist["CFBundleVersion"] != version:
        raise TaskError("package the selected Git tag before preparing release assets")
    options, stdin = signing_options()
    environment = {key: value for key, value in os.environ.items() if key != "SPARKLE_PRIVATE_KEY"}
    with tempfile.TemporaryDirectory(dir=DIST) as temporary:
        directory = Path(temporary)
        archive = directory / archive_name(version)
        shutil.copyfile(DIST / "SpaceTerm.dmg", archive)
        checked(
            [
                sparkle.tool("generate_appcast"),
                *options,
                "--maximum-deltas",
                "0",
                "--maximum-versions",
                "1",
                "--download-url-prefix",
                f"{RELEASES}/download/v{version}/",
                "--link",
                f"{RELEASES}/tag/v{version}",
                directory,
            ],
            input=stdin,
            env=environment,
        )
        feed = directory / "appcast.xml"
        try:
            signature = verify_feed(feed, archive, version)
        except ValueError as error:
            raise TaskError(str(error)) from None
        checked(
            [sparkle.tool("sign_update"), *options, "--verify", archive, signature],
            input=stdin,
            env=environment,
        )
        checked(
            [sparkle.tool("sign_update"), *options, "--verify", feed], input=stdin, env=environment
        )
        installer = directory / "install.sh"
        shutil.copyfile(INSTALLER, installer)
        checksums = []
        for path in (archive, feed, installer):
            with path.open("rb") as source:
                digest = hashlib.file_digest(source, "sha256").hexdigest()
            checksums.append(f"{digest}  {path.name}\n")
        (directory / "SHA256SUMS").write_text("".join(checksums))
        shutil.rmtree(RELEASE_ASSETS, ignore_errors=True)
        RELEASE_ASSETS.mkdir()
        for path in (archive, feed, installer, directory / "SHA256SUMS"):
            shutil.copyfile(path, RELEASE_ASSETS / path.name)
    print("Release DMG, signed appcast, installer, and checksums verified.")


def notes(root=ROOT):
    """Markdown notes for the release tagged at HEAD, from its Conventional Commits."""
    tags = subprocess.run(
        ["git", "tag", "--points-at", "HEAD"], cwd=root, capture_output=True, text=True
    ).stdout.split()
    if not any(STABLE_TAG.fullmatch(tag) for tag in tags):
        raise TaskError("release notes require a stable release tag at HEAD")
    result = subprocess.run(
        ["git-cliff", "--config", "cliff.toml", "--current", "--offline", "--strip", "all"],
        cwd=root,
        capture_output=True,
        text=True,
    )
    if result.returncode or not result.stdout.strip():
        raise TaskError("release notes require a stable release tag at HEAD")
    return result.stdout


def publish(tag):
    """Publish the release with its complete assets; a published release is never replaced."""
    version = release_version(tag)
    head = checked(["git", "rev-parse", "HEAD"], cwd=ROOT).strip()
    tagged = subprocess.run(
        ["git", "rev-parse", "--verify", "--quiet", f"refs/tags/{tag}^{{commit}}"],
        cwd=ROOT,
        capture_output=True,
    )
    if tagged.returncode or tagged.stdout.strip() != head:
        raise TaskError("check out the release tag before publishing")
    assets = [
        RELEASE_ASSETS / name
        for name in (archive_name(version), "appcast.xml", "install.sh", "SHA256SUMS")
    ]
    if not all(path.is_file() for path in assets):
        raise TaskError("prepare the complete release assets before publishing")
    with tempfile.TemporaryDirectory() as temporary:
        release_notes = Path(temporary) / "release-notes.md"
        release_notes.write_text(notes())
        # gh uploads the assets to a draft and publishes it only after every upload succeeds, so
        # the latest/download URLs never point at a partial release.
        if subprocess.run(
            [
                "gh",
                "release",
                "create",
                tag,
                *assets,
                "--verify-tag",
                "--latest",
                "--title",
                f"SpaceTerm {tag}",
                "--notes-file",
                release_notes,
            ],
            cwd=ROOT,
        ).returncode:
            raise TaskError("GitHub could not publish the release")
    print(f"Published SpaceTerm {tag} with complete update assets.")


def archive_digest(version, checksums):
    archive = archive_name(version)
    digests = [
        line.split("  ", 1)[0] for line in checksums.splitlines() if line.endswith(f"  {archive}")
    ]
    if (
        len(digests) != 1
        or len(digests[0]) != 64
        or not all(c in "0123456789abcdef" for c in digests[0])
    ):
        raise TaskError(f"the release checksums must list {archive} once")
    return digests[0]


def supersedes(version, installed):
    """Whether a release is at least as new as the version the cask installs."""

    def parse(value):
        return tuple(int(part) for part in value.split("."))

    return parse(version) >= parse(installed)


def update_cask(tag):
    """Point the tapped cask at a release; an older release's job never rolls the tap back."""
    version = release_version(tag)
    info = json.loads(checked(["brew", "info", "--cask", "--json=v2", CASK]))
    installed = info["casks"][0]["version"]
    if not supersedes(version, installed):
        print(f"The cask already installs a release newer than SpaceTerm {tag}.")
        return
    digest = archive_digest(version, (RELEASE_ASSETS / "SHA256SUMS").read_text())
    if subprocess.run(
        [
            "brew",
            "bump-cask-pr",
            "--write-only",
            "--no-audit",
            "--no-style",
            "--version",
            version,
            "--sha256",
            digest,
            CASK,
        ]
    ).returncode:
        raise TaskError("Homebrew could not update the cask")
    print(f"Updated the SpaceTerm cask to {tag}.")
