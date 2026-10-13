"""Create separately signed server assets; the existing Ed25519 trust remains authoritative."""

import base64
import binascii
import hashlib
import json
import os
import shutil
import tarfile
import tempfile
import time
from pathlib import Path

from spaceterm_tasks import TaskError, package_server, release
from spaceterm_tasks import server_artifacts as artifacts
from spaceterm_tasks.packaging import DIST, release_version


def asset_directory(platform, build="release"):
    artifacts.identifier(build)
    if platform not in artifacts.PLATFORMS:
        raise TaskError("unsupported server platform")
    return DIST / f"{build}-server-{platform}"


def platform_assets(version):
    build = "release"
    return [
        (
            asset_directory(platform),
            (
                artifacts.archive_name(version, platform, build),
                artifacts.feed_name(platform, build),
                artifacts.feed_name(platform, build) + ".sig",
            ),
        )
        for platform in artifacts.PLATFORMS
    ]


def manifest_bytes(archive):
    # This is called only after the complete bounded reader has accepted the tar structure.
    with tarfile.open(archive, "r:gz") as source:
        stream = source.extractfile("spaceterm-server/installation.json")
        if stream is None:
            raise TaskError("server installation manifest is missing")
        data = stream.read(artifacts.MAX_MANIFEST_BYTES + 1)
    if len(data) > artifacts.MAX_MANIFEST_BYTES:
        raise TaskError("server installation manifest exceeds its bound")
    return data


def sign_assets(seed, archive, platform, version, published_at, directory, *, build="release"):
    artifacts.archive_name(version, platform, build)
    manifest = package_server.verify_archive(archive)
    if (
        manifest["build_identity"] != build
        or manifest["platform"] != platform
        or manifest["version"] != version
        or type(published_at) is not int
        or not 0 < published_at <= int(time.time()) + 300
    ):
        raise TaskError("server release metadata does not match the packaged executable")
    public = release.derived_public_key(seed)
    destination = directory / artifacts.archive_name(version, platform, build)
    shutil.copyfile(archive, destination)
    feed = directory / artifacts.feed_name(platform, build)
    detached = directory / (feed.name + ".sig")
    with tempfile.TemporaryDirectory(prefix="spaceterm-server-signatures-") as temporary:
        raw = Path(temporary) / "signature"
        release.sign_ed25519(seed, destination, raw)
        release.verify_ed25519(public, destination, raw)
        document = {
            "schema_version": 2,
            "component": "server",
            "build_identity": build,
            "build_commit": manifest["build_commit"],
            "platform": platform,
            "version": version,
            "published_at": published_at,
            "protocol_majors": manifest["protocol_majors"],
            "storage": manifest["storage"],
            "installation": {
                "manifest_schema": 2,
                "registration_schema": 1,
                "journal_schema": 1,
            },
            "archive": {
                "name": destination.name,
                "size": destination.stat().st_size,
                "sha256": release.sha256(destination),
                "signature": base64.b64encode(raw.read_bytes()).decode(),
                "manifest_sha256": hashlib.sha256(manifest_bytes(destination)).hexdigest(),
            },
        }
        data = json.dumps(document, separators=(",", ":")).encode()
        if len(data) > artifacts.MAX_FEED_BYTES:
            raise TaskError("server release feed exceeds its bound")
        feed.write_bytes(data)
        release.sign_ed25519(seed, feed, raw)
        release.verify_ed25519(public, feed, raw)
        detached.write_bytes(base64.b64encode(raw.read_bytes()) + b"\n")
    verify_assets(
        directory, platform, version, public, build=build, build_commit=manifest["build_commit"]
    )
    return [destination, feed, detached]


def decode_signature(value):
    if not isinstance(value, str):
        raise TaskError("server release signature is invalid")
    try:
        raw = base64.b64decode(value, validate=True)
    except (ValueError, binascii.Error):
        raise TaskError("server release signature is invalid") from None
    if len(raw) != 64 or base64.b64encode(raw).decode() != value:
        raise TaskError("server release signature is invalid")
    return raw


def _bounded_file(path, maximum):
    with path.open("rb") as source:
        data = source.read(maximum + 1)
    if len(data) > maximum:
        raise TaskError("server release metadata exceeds its bound")
    return data


def verify_assets(directory, platform, version, public, *, build="release", build_commit=None):
    archive = directory / artifacts.archive_name(version, platform, build)
    feed = directory / artifacts.feed_name(platform, build)
    detached = directory / (feed.name + ".sig")
    if any(path.is_symlink() or not path.is_file() for path in (archive, feed, detached)):
        raise TaskError("complete server release assets are unavailable")
    feed_bytes = _bounded_file(feed, artifacts.MAX_FEED_BYTES)
    try:
        signature = _bounded_file(detached, 256).decode("ascii").strip(" \t\r\n\v\f")
    except UnicodeError:
        raise TaskError("server detached signature encoding is invalid") from None
    with tempfile.TemporaryDirectory(prefix="spaceterm-server-verification-") as temporary:
        stage = Path(temporary)
        raw = stage / "signature"
        raw.write_bytes(decode_signature(signature))
        authenticated_feed = stage / "feed.json"
        authenticated_feed.write_bytes(feed_bytes)
        # Authenticate the exact bounded bytes that will be interpreted, never
        # reopen a mutable input path after authenticating a different observation.
        release.verify_ed25519(public, authenticated_feed, raw)
        document = artifacts.strict_json(feed_bytes, artifacts.MAX_FEED_BYTES)
        fields = {
            "schema_version",
            "component",
            "build_identity",
            "build_commit",
            "platform",
            "version",
            "published_at",
            "protocol_majors",
            "storage",
            "installation",
            "archive",
        }
        if (
            not isinstance(document, dict)
            or set(document) != fields
            or type(document["schema_version"]) is not int
            or document["schema_version"] != 2
            or document["component"] != "server"
            or document["build_identity"] != build
            or document["platform"] != platform
            or document["version"] != version
            or (build != "release" and build_commit is None)
            or (build_commit is not None and document["build_commit"] != build_commit)
            or type(document["published_at"]) is not int
            or not 0 < document["published_at"] <= int(time.time()) + 300
            or document["installation"]
            != {
                "manifest_schema": 2,
                "registration_schema": 1,
                "journal_schema": 1,
            }
        ):
            raise TaskError("server release feed identity or compatibility does not match")
        if any(type(value) is not int for value in document["installation"].values()):
            raise TaskError("server release compatibility encoding is invalid")
        artifacts.validate_metadata(
            {
                **{
                    field: document[field]
                    for field in artifacts.METADATA_FIELDS
                    if field not in ("schema_version", "native_identifier")
                },
                "schema_version": 2,
                "native_identifier": artifacts.identifier(build),
            },
            platform,
            build,
            version,
        )
        record = document["archive"]
        if (
            not isinstance(record, dict)
            or set(record) != {"name", "size", "sha256", "signature", "manifest_sha256"}
            or record["name"] != archive.name
            or type(record["size"]) is not int
            or not 1 <= record["size"] <= artifacts.MAX_ARCHIVE_BYTES
            or not artifacts.valid_sha256(record["sha256"])
            or not artifacts.valid_sha256(record["manifest_sha256"])
        ):
            raise TaskError("server release archive does not match the feed")
        received = stage / "archive.tar.gz"
        digest = hashlib.sha256()
        size = 0
        with archive.open("rb") as source, received.open("xb") as target:
            while data := source.read(65536):
                size += len(data)
                if size > record["size"]:
                    raise TaskError("server release archive exceeds its bound")
                digest.update(data)
                target.write(data)
        if size != record["size"] or digest.hexdigest() != record["sha256"]:
            raise TaskError("server release archive does not match the feed")
        raw.write_bytes(decode_signature(record["signature"]))
        release.verify_ed25519(public, received, raw)
        manifest = package_server.verify_archive(received)
        if hashlib.sha256(manifest_bytes(received)).hexdigest() != record["manifest_sha256"] or any(
            manifest[field] != document[field]
            for field in (
                "component",
                "build_identity",
                "build_commit",
                "platform",
                "version",
                "protocol_majors",
                "storage",
            )
        ):
            raise TaskError("server release manifest does not match the feed")
    return manifest


def create_assets(tag, platform):
    return create_account_assets(platform, "release", release_version(tag))


def create_account_assets(platform, build="preflight", version=None):
    """Sign a local account artifact with the existing update key, without publishing it."""
    artifacts.identifier(build)
    archive = package_server.archive_path(platform, build)
    manifest = package_server.verify_archive(archive)
    if manifest["build_identity"] != build or (version and manifest["version"] != version):
        raise TaskError("server signing channel differs from the package")
    version = manifest["version"]
    secret = os.environ.get(release.SIGNING_SECRET)
    if not secret:
        raise TaskError("UPDATE_SIGNING_KEY must hold the existing release signing key")
    seed = release.signing_seed(secret)
    if release.derived_public_key(seed) != release.public_key():
        raise TaskError("server signing key differs from installed update trust")
    with tempfile.TemporaryDirectory(prefix="spaceterm-server-assets-", dir=DIST) as temporary:
        stage = Path(temporary)
        assets = sign_assets(seed, archive, platform, version, int(time.time()), stage, build=build)
        (stage / "SHA256SUMS").write_text(release.checksums(assets))
        destination = asset_directory(platform, build)
        shutil.rmtree(destination, ignore_errors=True)
        destination.mkdir()
        for path in [*assets, stage / "SHA256SUMS"]:
            shutil.copyfile(path, destination / path.name)
    print("Server archive, signed channel descriptor, and checksums verified.")
