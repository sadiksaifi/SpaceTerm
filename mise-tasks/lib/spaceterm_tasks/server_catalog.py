"""Snapshot and authenticate the server packages a client can offer during setup.

Catalogs never execute foreign code. Matching-host entries receive full native checks;
foreign entries receive portable resource/machine checks and retain their signed bytes.
"""

import os
import re
import shutil
import stat
import tempfile
from pathlib import Path

from spaceterm_tasks import TaskError, release
from spaceterm_tasks.server_package import MAX_ARCHIVE, TARGETS, host_target, verified_copy

FILES = {"artifact.tar.gz": MAX_ARCHIVE, "artifact.sig": 64, "artifact.sha256": 65}
SUPPORTED = frozenset(TARGETS.values())


def _state(entry):
    return (
        entry.st_dev,
        entry.st_ino,
        entry.st_mode,
        entry.st_size,
        entry.st_nlink,
        entry.st_mtime_ns,
        entry.st_ctime_ns,
    )


def _directory(path, *, parent=None):
    return os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)


def _copy_file(parent, name, destination, maximum):
    descriptor = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent)
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or not 0 < before.st_size <= maximum:
            raise TaskError("the server catalog contains an invalid file")
        with destination.open("xb") as output:
            remaining = maximum + 1
            while remaining:
                block = os.read(descriptor, min(remaining, 1024 * 1024))
                if not block:
                    break
                output.write(block)
                remaining -= len(block)
        if (
            remaining == 0
            or destination.stat().st_size != before.st_size
            or _state(os.fstat(descriptor)) != _state(before)
            or _state(os.stat(name, dir_fd=parent, follow_symlinks=False)) != _state(before)
        ):
            raise TaskError("the server catalog changed while being read")
    finally:
        os.close(descriptor)


def _copy_pair_file(path, destination, maximum):
    path = Path(path)
    parent = _directory(path.parent)
    try:
        _copy_file(parent, path.name, destination, maximum)
    finally:
        os.close(parent)


def _snapshot(source, destination, *, require_all):
    descriptor = _directory(source)
    try:
        before = os.fstat(descriptor)
        targets = set(os.listdir(descriptor))
        if not targets or not targets <= SUPPORTED or host_target() not in targets:
            raise TaskError("the server catalog has missing or unsupported targets")
        if require_all and targets != SUPPORTED:
            raise TaskError("the server catalog must include every supported native target")
        for target in sorted(targets):
            nested = _directory(target, parent=descriptor)
            try:
                entry = os.fstat(nested)
                if set(os.listdir(nested)) != FILES.keys():
                    raise TaskError(
                        "the server catalog target must contain exactly its signed pair and digest"
                    )
                output = destination / target
                output.mkdir()
                for name, maximum in FILES.items():
                    _copy_file(nested, name, output / name, maximum)
                if _state(os.fstat(nested)) != _state(entry) or _state(
                    os.stat(target, dir_fd=descriptor, follow_symlinks=False)
                ) != _state(entry):
                    raise TaskError("the server catalog changed while being read")
            finally:
                os.close(nested)
        if _state(os.fstat(descriptor)) != _state(before) or _state(
            os.stat(source, follow_symlinks=False)
        ) != _state(before):
            raise TaskError("the server catalog changed while being read")
        return targets
    finally:
        os.close(descriptor)


def _verify_target(source, identity, target, temporary):
    raw = (source / "artifact.sha256").read_bytes()
    if not re.fullmatch(rb"[0-9a-f]{64}\n?", raw):
        raise TaskError("the server catalog digest is malformed")
    digest = raw.decode().strip()
    temporary.mkdir()
    tree, archive, signature = verified_copy(
        source / "artifact.tar.gz",
        source / "artifact.sig",
        temporary,
        None,
        identity=identity,
        target=target,
        expected_sha256=digest,
        native=target == host_target(),
    )
    from spaceterm_tasks.server_package import document

    return (
        {"build": document((tree / "build.json").read_bytes()), "sha256": digest},
        archive,
        signature,
    )


def _verified_catalog(source, identity, directory, *, require_all):
    snapshot = directory / "snapshot"
    snapshot.mkdir()
    targets = _snapshot(source, snapshot, require_all=require_all)
    return {
        target: _verify_target(snapshot / target, identity, target, directory / target)
        for target in sorted(targets)
    }


def stage_catalog(archive, signature, destination, identity, *, source=None, require_all=False):
    """Publish only a fully authenticated catalog. Explicit source catalogs must be complete."""
    destination = Path(destination)
    try:
        if destination.exists() or destination.is_symlink():
            raise TaskError("the server catalog destination already exists")
        destination.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            dir=destination.parent, prefix=".server-catalog-"
        ) as temporary:
            directory = Path(temporary)
            if source is not None:
                entries = _verified_catalog(source, identity, directory, require_all=True)
            else:
                if require_all:
                    raise TaskError("release packaging requires a complete signed server catalog")
                local = directory / "local"
                local.mkdir()
                _copy_pair_file(archive, local / "artifact.tar.gz", MAX_ARCHIVE)
                _copy_pair_file(signature, local / "artifact.sig", 64)
                (local / "artifact.sha256").write_text(
                    release.sha256(local / "artifact.tar.gz") + "\n"
                )
                target = host_target()
                entries = {target: _verify_target(local, identity, target, directory / "verified")}
            staged = directory / "catalog"
            staged.mkdir(mode=0o755)
            for target, (info, private, raw) in entries.items():
                output = staged / target
                output.mkdir(mode=0o755)
                shutil.copyfile(private, output / "artifact.tar.gz")
                shutil.copyfile(raw, output / "artifact.sig")
                (output / "artifact.sha256").write_text(info["sha256"] + "\n")
                for path in output.iterdir():
                    path.chmod(0o644)
            os.rename(staged, destination)
            return {target: info for target, (info, _, _) in entries.items()}
    except OSError:
        raise TaskError("the server catalog could not be read or staged safely") from None


def verify_catalog(source, identity, *, require_all=False):
    """Reauthenticate a packaged catalog with native checks only on matching-host entries."""
    try:
        with tempfile.TemporaryDirectory(prefix="spaceterm-catalog-verify-") as temporary:
            entries = _verified_catalog(source, identity, Path(temporary), require_all=require_all)
            return {target: info for target, (info, _, _) in entries.items()}
    except OSError:
        raise TaskError("the server catalog could not be read or verified safely") from None
