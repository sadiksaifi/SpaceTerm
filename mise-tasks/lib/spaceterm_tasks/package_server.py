"""Construct and verify headless archives without installing or executing them as root."""

import gzip
import hashlib
import json
import os
import selectors
import shutil
import stat
import subprocess
import tarfile
import time

from spaceterm_tasks import ROOT, TaskError
from spaceterm_tasks import server_artifacts as artifacts
from spaceterm_tasks.cargo import build_executable
from spaceterm_tasks.macos_signing import signing_environment
from spaceterm_tasks.packaging import (
    DIST,
    NOTICES,
    SHELL_INTEGRATION,
    TERMINFO,
    checked,
    packaged_environment,
    release_version,
)


def executable_metadata(executable, platform, build, version=None):
    """Bound both output streams and time. Never relay executable diagnostics."""
    with subprocess.Popen(
        [executable, "--distribution-metadata"],
        env={},
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    ) as process:
        assert process.stdout is not None and process.stderr is not None
        selector = selectors.DefaultSelector()
        output = bytearray()
        deadline = time.monotonic() + 5
        try:
            for stream in (process.stdout, process.stderr):
                os.set_blocking(stream.fileno(), False)
                selector.register(stream, selectors.EVENT_READ)
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TaskError("server executable metadata query timed out")
                for key, _ in selector.select(remaining):
                    data = os.read(key.fd, 4097)
                    if not data:
                        selector.unregister(key.fileobj)
                    elif key.fileobj is process.stderr:
                        raise TaskError("server executable metadata query failed")
                    else:
                        output.extend(data)
                        if len(output) > 4096:
                            raise TaskError("server executable metadata query exceeded its bound")
            try:
                status = process.wait(timeout=max(0.001, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                raise TaskError("server executable metadata query timed out") from None
            if status:
                raise TaskError("server executable metadata query failed")
        finally:
            selector.close()
            if process.poll() is None:
                process.kill()
            process.wait()
    metadata = artifacts.strict_json(bytes(output), 4096)
    return artifacts.validate_metadata(metadata, platform, build, version)


def verify_executable_metadata(root, metadata):
    """Query the copied executable, not the mutable build-output source."""
    executable = (
        root / artifacts.entry_points(metadata["platform"], metadata["build_identity"])["server"]
    )
    actual = executable_metadata(
        executable, metadata["platform"], metadata["build_identity"], metadata["version"]
    )
    if actual != metadata:
        raise TaskError("packaged executable compatibility differs from its installation manifest")


def build(release_tag=None):
    environment = signing_environment(packaged_environment(release_tag))
    if release_tag:
        release_version(release_tag)
        status = checked(
            ["git", "--no-optional-locks", "status", "--porcelain", "--untracked-files=normal"],
            "Git could not report the checkout state",
            cwd=ROOT,
            env=environment,
        )
        if status.strip():
            raise TaskError("a release package requires a clean checkout")
    environment["CARGO_BUILD_JOBS"] = "3"
    if os.uname().sysname == "Darwin":
        environment["MACOSX_DEPLOYMENT_TARGET"] = "26.0"
    return {
        binary: build_executable(
            "--release", "--locked", "-p", package, binary=binary, env=environment
        )
        for binary, package in (
            ("spaceterm-server", "spaceterm-server"),
            ("spaceterm-server-launcher", "spaceterm-host"),
            ("spaceterm-server-manager", "spaceterm-host"),
        )
    }


def archive_path(platform, build_identity="preflight"):
    artifacts.identifier(build_identity)
    if platform not in artifacts.PLATFORMS:
        raise TaskError("unsupported server platform")
    return DIST / f"spaceterm-server-{build_identity}-{platform}.tar.gz"


def copy_file(source, destination, mode=0o644):
    if source.is_symlink() or not source.is_file():
        raise TaskError("server package input is not a regular file")
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)
    destination.chmod(mode)


def copy_resources(destination, *, additional_notices=b""):
    if not isinstance(additional_notices, bytes) or len(additional_notices) > 1024 * 1024:
        raise TaskError("credential-tool notices exceed their bound")
    notices = destination / NOTICES.name
    notices_bytes = NOTICES.read_bytes()
    if additional_notices:
        if notices.is_symlink() or (notices.exists() and not notices.is_file()):
            raise TaskError("server notices input is not a regular file")
        notices.parent.mkdir(parents=True, exist_ok=True)
        notices.unlink(missing_ok=True)
        notices_bytes += b"\n\nBundled credential-tool notices\n" + additional_notices
        notices.write_bytes(notices_bytes)
        notices.chmod(0o644)
    else:
        copy_file(NOTICES, notices)
    for source in SHELL_INTEGRATION.rglob("*"):
        if source.is_symlink():
            raise TaskError("server shell integration contains a link")
        if source.is_file():
            copy_file(
                source, destination / "shell-integration" / source.relative_to(SHELL_INTEGRATION)
            )
    (destination / "terminfo").mkdir(parents=True)
    checked(
        ["tic", "-x", "-o", destination / "terminfo", TERMINFO],
        "server terminfo compilation failed",
        env=signing_environment(os.environ),
    )
    for path in (destination / "terminfo").rglob("*"):
        if path.is_symlink() or (not path.is_dir() and not path.is_file()):
            raise TaskError("compiled server terminfo contains a link or special file")
        path.chmod(0o755 if path.is_dir() else 0o644)
    return hashlib.sha256(notices_bytes).hexdigest()


def validate_manifest(document):
    if not isinstance(document, dict) or set(document) != artifacts.MANIFEST_FIELDS:
        raise TaskError("unsupported server installation manifest")
    artifacts.validate_metadata({key: document[key] for key in artifacts.METADATA_FIELDS})
    if document["entry_points"] != artifacts.entry_points(
        document["platform"], document["build_identity"]
    ):
        raise TaskError("server installation entry points do not match")
    files = document["files"]
    dependencies = document["dependencies"]
    if (
        not isinstance(files, list)
        or not 1 <= len(files) <= 4096
        or not isinstance(dependencies, list)
        or len(dependencies) > 128
    ):
        raise TaskError("server installation list exceeds its bound")
    names = []
    total = 0
    for file in files:
        if not isinstance(file, dict) or set(file) != {"name", "mode", "size", "sha256"}:
            raise TaskError("invalid server installation file")
        artifacts.validate_headless_name(file["name"])
        mode = file["mode"]
        size = file["size"]
        if (
            file["name"] == "installation.json"
            or type(mode) is not int
            or mode & ~0o777
            or mode & 0o022
            or not mode & 0o400
            or type(size) is not int
            or not 0 <= size <= artifacts.MAX_EXPANDED_BYTES
            or not isinstance(file["sha256"], str)
            or not artifacts.valid_sha256(file["sha256"])
        ):
            raise TaskError("invalid server installation file metadata")
        names.append(file["name"])
        total += size
    if total > artifacts.MAX_EXPANDED_BYTES or names != sorted(set(names)):
        raise TaskError("server installation files are duplicate, unordered, or excessive")
    known = set(names)
    for name in names:
        parent = name
        while "/" in parent:
            parent = parent.rsplit("/", 1)[0]
            if parent in known:
                raise TaskError("server installation file is also a directory")
    indexed = {file["name"]: file for file in files}
    for name in document["entry_points"].values():
        if name is not None and (
            name not in indexed or not indexed[name]["mode"] & 0o111 or indexed[name]["size"] == 0
        ):
            raise TaskError("server installation entry point is missing")
    if document["platform"] == "linux-x86_64" and indexed["bin/spaceterm-server"]["mode"] != 0o501:
        raise TaskError("Linux server main ELF must be unreadable to the final account")
    dependency_names = set()
    for dependency in dependencies:
        if not isinstance(dependency, dict) or set(dependency) != {
            "name",
            "version",
            "source_identity",
            "license",
            "notice_file",
            "corresponding_source",
        }:
            raise TaskError("invalid bundled dependency record")
        if any(
            not isinstance(v, str)
            or not v
            or len(v.encode()) > 1024
            or any(ord(c) < 32 or 127 <= ord(c) <= 159 for c in v)
            for v in dependency.values()
        ):
            raise TaskError("invalid bundled dependency metadata")
        if dependency["name"] in dependency_names or any(
            dependency[field] not in known for field in ("notice_file", "corresponding_source")
        ):
            raise TaskError("bundled dependency source or license material is missing")
        dependency_names.add(dependency["name"])
    return document


def write_manifest(root, metadata, dependencies=()):
    artifacts.validate_metadata(metadata)
    files = []
    for path in sorted(root.rglob("*")):
        name = path.relative_to(root).as_posix()
        artifacts.validate_headless_name(name)
        info = path.lstat()
        if stat.S_ISDIR(info.st_mode):
            continue
        if not stat.S_ISREG(info.st_mode) or name == "installation.json":
            raise TaskError(
                "server installation input contains a special or preexisting manifest file"
            )
        files.append(
            {
                "name": name,
                "mode": stat.S_IMODE(info.st_mode),
                "size": info.st_size,
                "sha256": artifacts.digest(path),
            }
        )
    manifest = validate_manifest(
        {
            **metadata,
            "entry_points": artifacts.entry_points(
                metadata["platform"], metadata["build_identity"]
            ),
            "files": files,
            "dependencies": list(dependencies),
        }
    )
    data = json.dumps(manifest, sort_keys=True, separators=(",", ":")).encode()
    if len(data) > artifacts.MAX_MANIFEST_BYTES:
        raise TaskError("server installation manifest exceeds its bound")
    (root / "installation.json").write_bytes(data)
    (root / "installation.json").chmod(0o444)
    return manifest


def write_archive(root, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    # GNU long names avoid PAX metadata overrides. All entries have deterministic metadata.
    with (
        destination.open("xb") as output,
        gzip.GzipFile(filename="", mode="wb", fileobj=output, mtime=0) as compressed,
        tarfile.open(fileobj=compressed, mode="w|", format=tarfile.GNU_FORMAT) as archive,
    ):
        for path in [root, *sorted(root.rglob("*"))]:
            info = path.lstat()
            if not stat.S_ISDIR(info.st_mode) and not stat.S_ISREG(info.st_mode):
                raise TaskError("server archive input contains a special file")
            name = "spaceterm-server" + (
                "/" + path.relative_to(root).as_posix() if path != root else ""
            )
            member = tarfile.TarInfo(name)
            member.uid = member.gid = member.mtime = 0
            member.mode = 0o755 if path.is_dir() else stat.S_IMODE(info.st_mode)
            if path.is_dir():
                member.type = tarfile.DIRTYPE
                archive.addfile(member)
            else:
                member.size = info.st_size
                with path.open("rb") as source:
                    archive.addfile(member, source)
    if destination.stat().st_size > artifacts.MAX_ARCHIVE_BYTES:
        raise TaskError("server archive exceeds its bound")


def verify_archive(path):
    """Check every byte without extraction, code execution, or native effects."""
    if (
        path.is_symlink()
        or not path.is_file()
        or not 0 < path.stat().st_size <= artifacts.MAX_ARCHIVE_BYTES
    ):
        raise TaskError("server archive exceeds its bound or is unavailable")
    files = {}
    directories = set()
    seen = set()
    manifest_bytes = None
    count = 0
    entry_count = 0
    total = 0
    long_name = None
    try:
        with gzip.open(path, "rb") as source:

            def read(size):
                nonlocal total
                if total + size > artifacts.MAX_EXPANDED_BYTES:
                    raise TaskError("server expanded archive exceeds its bound")
                data = source.read(size)
                total += len(data)
                if len(data) != size:
                    raise TaskError("server archive is truncated")
                return data

            while True:
                header = read(512)
                if header == bytes(512):
                    if long_name is not None or read(512) != bytes(512):
                        raise TaskError("server archive end marker is invalid")
                    while data := source.read(8192):
                        total += len(data)
                        if total > artifacts.MAX_EXPANDED_BYTES or any(data):
                            raise TaskError("server archive has trailing data or exceeds its bound")
                    break
                count += 1
                if count > 8194:
                    raise TaskError("server archive entry count exceeds its bound")
                member = tarfile.TarInfo.frombuf(header, "utf-8", "strict")
                if not 0 <= member.size <= artifacts.MAX_EXPANDED_BYTES:
                    raise TaskError("server archive entry exceeds its bound")
                if member.type == tarfile.GNUTYPE_LONGNAME:
                    if long_name is not None or not 1 <= member.size <= 1043:
                        raise TaskError("server archive long name exceeds its bound")
                    name = read(member.size)
                    if not name.endswith(b"\0"):
                        raise TaskError("server archive long name is invalid")
                    long_name = name[:-1].decode("utf-8")
                else:
                    entry_count += 1
                    if entry_count > 4096:
                        raise TaskError("server archive entry count exceeds its bound")
                    name = long_name if long_name is not None else member.name
                    long_name = None
                    if member.isdir():
                        name = name.removesuffix("/")
                    if (
                        not artifacts.relative_name(name)
                        or name in seen
                        or member.uid != 0
                        or member.gid != 0
                        or member.mode & ~0o777
                        or member.mode & 0o022
                    ):
                        raise TaskError("server archive contains an unsafe or duplicate entry")
                    seen.add(name)
                    if member.isdir():
                        if member.size:
                            raise TaskError("server archive directory has data")
                        directories.add(name)
                    elif member.isfile():
                        if not name.startswith("spaceterm-server/"):
                            raise TaskError("server archive root is invalid")
                        relative = name.removeprefix("spaceterm-server/")
                        artifacts.validate_headless_name(relative)
                        if relative == "installation.json":
                            if member.size > artifacts.MAX_MANIFEST_BYTES or member.mode & 0o111:
                                raise TaskError("server installation manifest exceeds its bound")
                            manifest_bytes = read(member.size)
                        else:
                            digest = hashlib.sha256()
                            remaining = member.size
                            while remaining:
                                data = read(min(remaining, 65536))
                                digest.update(data)
                                remaining -= len(data)
                            files[relative] = {
                                "name": relative,
                                "mode": member.mode,
                                "size": member.size,
                                "sha256": digest.hexdigest(),
                            }
                    else:
                        raise TaskError("server archive contains a link or special file")
                padding = read((-member.size) % 512)
                if any(padding):
                    raise TaskError("server archive padding is invalid")
    except (tarfile.TarError, EOFError, ValueError, UnicodeError):
        raise TaskError("invalid server archive") from None
    if manifest_bytes is None:
        raise TaskError("server installation manifest is missing")
    manifest = validate_manifest(
        artifacts.strict_json(manifest_bytes, artifacts.MAX_MANIFEST_BYTES)
    )
    if list(files.values()) != manifest["files"]:
        # Archive ordering is immaterial. Manifest ordering remains strict.
        if sorted(files.values(), key=lambda f: f["name"]) != manifest["files"]:
            raise TaskError("server archive files differ from the installation manifest")
    for directory in directories:
        if directory != "spaceterm-server":
            prefix = directory.removeprefix("spaceterm-server/") + "/"
            if not directory.startswith("spaceterm-server/") or not any(
                name.startswith(prefix) for name in files
            ):
                raise TaskError("server archive contains an unexpected directory")
    return manifest
