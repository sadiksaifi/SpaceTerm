"""Create signed GitHub Releases assets and update the Homebrew cask."""

import base64
import hashlib
import json
import os
import plistlib
import shutil
import subprocess
import tempfile
import time
import xml.etree.ElementTree as ET
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, package_linux, sparkle
from spaceterm_tasks.macos_bundle import identity
from spaceterm_tasks.packaging import DIST, STABLE_TAG, release_version

SPARKLE_NS = "http://www.andymatuschak.org/xml-namespaces/sparkle"
RELEASES = "https://github.com/sadiksaifi/SpaceTerm/releases"
RELEASE_ASSETS = DIST / "release"
LINUX_RELEASE_ASSETS = DIST / "release-linux"
LINUX_PLATFORM = "linux-x86_64"
INSTALLER = ROOT / "packaging" / "install.sh"
CASK = "sadiksaifi/tap/spaceterm"
SIGNING_SECRET = "UPDATE_SIGNING_KEY"
ED25519_PKCS8_PREFIX = bytes.fromhex("302e020100300506032b657004220420")
ED25519_SPKI_PREFIX = bytes.fromhex("302a300506032b6570032100")
# The Linux updater refuses a longer feed.
MAX_FEED_BYTES = 4096


def archive_name(version):
    return f"SpaceTerm-{version}-darwin-arm64.dmg"


def linux_archive_name(version):
    return f"SpaceTerm-{version}-{LINUX_PLATFORM}.tar.gz"


def linux_feed_name():
    return f"latest-{LINUX_PLATFORM}.json"


def public_key():
    """The update trust root that installed applications carry in their Info.plist."""
    return identity("spaceterm")["SUPublicEDKey"]


def checked(arguments, **kwargs):
    result = subprocess.run(arguments, capture_output=True, check=False, **kwargs)
    if result.returncode:
        raise TaskError("release signing or verification failed")
    return result.stdout


def signing_seed(secret):
    """The 32-byte Ed25519 seed of the base64 update signing secret."""
    try:
        seed = base64.b64decode(secret.strip(), validate=True)
    except ValueError:
        raise TaskError("the update signing secret is invalid") from None
    if len(seed) != 32:
        raise TaskError("the update signing secret must contain a 32-byte Ed25519 seed")
    return seed


def derived_public_key(seed):
    # Derive only the public key through stdin. Never put the private key in argv or logs.
    derived = checked(
        ["openssl", "pkey", "-inform", "DER", "-pubout", "-outform", "DER"],
        input=ED25519_PKCS8_PREFIX + seed,
    )
    return base64.b64encode(derived[-32:]).decode()


def signing_options():
    secret = os.environ.get(SIGNING_SECRET)
    if secret:
        actual = derived_public_key(signing_seed(secret))
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
    environment = {key: value for key, value in os.environ.items() if key != SIGNING_SECRET}
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
        (directory / "SHA256SUMS").write_text(checksums((archive, feed, installer)))
        shutil.rmtree(RELEASE_ASSETS, ignore_errors=True)
        RELEASE_ASSETS.mkdir()
        for path in (archive, feed, installer, directory / "SHA256SUMS"):
            shutil.copyfile(path, RELEASE_ASSETS / path.name)
    print("Release DMG, signed appcast, installer, and checksums verified.")


def sha256(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def checksums(paths):
    return "".join(f"{sha256(path)}  {path.name}\n" for path in paths)


def sign_ed25519(seed, message, signature):
    """Sign the file `message` into `signature` with a key file only this process can read."""
    with tempfile.TemporaryDirectory(prefix="spaceterm-signing-") as temporary:
        key = Path(temporary) / "key.der"
        descriptor = os.open(key, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "wb") as output:
            output.write(ED25519_PKCS8_PREFIX + seed)
        checked(
            [
                "openssl",
                "pkeyutl",
                "-sign",
                "-rawin",
                "-keyform",
                "DER",
                "-inkey",
                key,
                "-in",
                message,
                "-out",
                signature,
            ]
        )


def verify_ed25519(public, message, signature):
    """Verify a raw Ed25519 signature with the base64 public key installed releases trust."""
    with tempfile.TemporaryDirectory(prefix="spaceterm-verify-") as temporary:
        key = Path(temporary) / "key.der"
        key.write_bytes(ED25519_SPKI_PREFIX + base64.b64decode(public))
        checked(
            [
                "openssl",
                "pkeyutl",
                "-verify",
                "-pubin",
                "-rawin",
                "-keyform",
                "DER",
                "-inkey",
                key,
                "-in",
                message,
                "-sigfile",
                signature,
            ]
        )


def sign_linux_release(seed, archive, version, published_at, directory):
    """Write the archive, its signed feed, and the feed's detached signature into `directory`.

    The feed names the archive with its size, SHA-256, and signature. The detached signature
    covers the exact feed bytes, which the updater verifies before reading any field.
    """
    public = derived_public_key(seed)
    release_archive = directory / linux_archive_name(version)
    shutil.copyfile(archive, release_archive)
    feed = directory / linux_feed_name()
    feed_signature = directory / f"{feed.name}.sig"
    with tempfile.TemporaryDirectory(prefix="spaceterm-signatures-") as temporary:
        archive_signature = Path(temporary) / "archive"
        sign_ed25519(seed, release_archive, archive_signature)
        verify_ed25519(public, release_archive, archive_signature)
        document = {
            "version": version,
            "published_at": published_at,
            "archive": {
                "name": release_archive.name,
                "size": release_archive.stat().st_size,
                "sha256": sha256(release_archive),
                "signature": base64.b64encode(archive_signature.read_bytes()).decode(),
            },
        }
        feed.write_bytes(json.dumps(document, separators=(",", ":")).encode())
        if feed.stat().st_size > MAX_FEED_BYTES:
            raise TaskError("the Linux release feed is larger than installed updaters accept")
        raw = Path(temporary) / "feed"
        sign_ed25519(seed, feed, raw)
        verify_ed25519(public, feed, raw)
        feed_signature.write_text(base64.b64encode(raw.read_bytes()).decode() + "\n")
    return [release_archive, feed, feed_signature]


def create_linux_assets(tag):
    """Sign the packaged Linux release into dist/release-linux: archive, feed, and checksums."""
    version = release_version(tag)
    archive = package_linux.archive_path("spaceterm")
    if not archive.is_file():
        raise TaskError("package the selected Git tag before preparing release assets")
    # Verification runs the packaged executable, which reports the version it was built for.
    package_linux.verify(archive, tag)
    secret = os.environ.get(SIGNING_SECRET)
    if not secret:
        raise TaskError(f"{SIGNING_SECRET} must hold the update signing key")
    seed = signing_seed(secret)
    if derived_public_key(seed) != public_key():
        raise TaskError("the signing key does not match the key trusted by installed applications")
    with tempfile.TemporaryDirectory(dir=DIST) as temporary:
        directory = Path(temporary)
        assets = sign_linux_release(seed, archive, version, int(time.time()), directory)
        (directory / "SHA256SUMS").write_text(checksums(assets))
        shutil.rmtree(LINUX_RELEASE_ASSETS, ignore_errors=True)
        LINUX_RELEASE_ASSETS.mkdir()
        for path in (*assets, directory / "SHA256SUMS"):
            shutil.copyfile(path, LINUX_RELEASE_ASSETS / path.name)
    print("Linux release archive, signed feed, and checksums verified.")


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
    platforms = [
        (RELEASE_ASSETS, (archive_name(version), "appcast.xml", "install.sh")),
        (
            LINUX_RELEASE_ASSETS,
            (linux_archive_name(version), linux_feed_name(), f"{linux_feed_name()}.sig"),
        ),
    ]
    assets = [directory / name for directory, names in platforms for name in names]
    if not all(path.is_file() for path in assets):
        raise TaskError("prepare the complete release assets before publishing")
    # Each package job lists its own assets. The release publishes one list of every asset.
    for directory, names in platforms:
        if published_checksums(directory) != checksums(directory / name for name in names):
            raise TaskError("the release assets differ from their checksums")
    with tempfile.TemporaryDirectory() as temporary:
        combined = Path(temporary) / "SHA256SUMS"
        combined.write_text(checksums(assets))
        assets.append(combined)
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


def published_checksums(directory):
    try:
        return (directory / "SHA256SUMS").read_text()
    except OSError:
        raise TaskError("the release checksums are missing") from None


def archive_digest(version, listing):
    archive = archive_name(version)
    digests = [
        line.split("  ", 1)[0] for line in listing.splitlines() if line.endswith(f"  {archive}")
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
    cask = json.loads(checked(["brew", "info", "--cask", "--json=v2", CASK]))["casks"][0]
    if not supersedes(version, cask["version"]):
        print(f"The cask already installs a release newer than SpaceTerm {tag}.")
        return
    digest = archive_digest(version, (RELEASE_ASSETS / "SHA256SUMS").read_text())
    # brew bump-cask-pr refuses a cask that already matches, so a rerun stops here.
    if (cask["version"], cask["sha256"]) == (version, digest):
        print(f"The cask already installs SpaceTerm {tag}.")
        return
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
