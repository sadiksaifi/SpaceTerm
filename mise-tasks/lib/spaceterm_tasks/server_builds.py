"""Prepare source-matched nonproduction server catalogs with native macOS builds."""

import fcntl
import hashlib
import os
import shutil
import tempfile
import tomllib
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError
from spaceterm_tasks.packaging import checked
from spaceterm_tasks.release import sha256
from spaceterm_tasks.server_catalog import stage_catalog
from spaceterm_tasks.server_package import TARGETS, host_target, package

NONPRODUCTION = ("spaceterm-development", "spaceterm-preflight")


def _git(root, *arguments):
    return checked(
        ["git", "--no-optional-locks", "-C", str(root), *arguments],
        "the server catalog source Git metadata is unavailable",
    )


def _source(root):
    """List current nonignored source, recursively including initialized gitlinks."""
    files = {}
    repositories = {}

    def visit(directory):
        relative = directory.relative_to(root)
        repositories[relative] = _git(directory, "rev-parse", "HEAD").strip().decode()
        paths = _git(directory, "ls-files", "--cached", "--others", "--exclude-standard", "-z")
        for raw in paths.split(b"\0"):
            if not raw:
                continue
            path = directory / os.fsdecode(raw)
            if path.is_dir() and not path.is_symlink():
                if not (path / ".git").exists():
                    raise TaskError(
                        "initialize server catalog submodules: mise run setup:submodules"
                    )
                visit(path)
            elif path.exists() or path.is_symlink():
                if not path.is_file() and not path.is_symlink():
                    raise TaskError("server catalog source must contain regular files or symlinks")
                files[path.relative_to(root)] = path
        # Do not silently omit a missing, uninitialized gitlink.
        for line in _git(directory, "ls-files", "--stage", "-z").split(b"\0"):
            if line.startswith(b"160000 "):
                nested = directory / os.fsdecode(line.split(b"\t", 1)[1])
                if nested.relative_to(root) not in repositories:
                    raise TaskError(
                        "initialize server catalog submodules: mise run setup:submodules"
                    )

    visit(root)
    digest = hashlib.sha256()
    for relative, commit in sorted(repositories.items()):
        digest.update(relative.as_posix().encode() + b"\0" + commit.encode() + b"\0")
    for relative, path in sorted(files.items()):
        metadata = path.lstat()
        digest.update(relative.as_posix().encode() + b"\0")
        if path.is_symlink():
            link = path.readlink()
            if link.is_absolute() or not (path.parent / link).resolve().is_relative_to(root):
                raise TaskError("server catalog source symlinks must stay inside the project")
            digest.update(b"link\0" + hashlib.sha256(os.fsencode(link)).digest())
        else:
            digest.update(b"executable\0" if metadata.st_mode & 0o111 else b"file\0")
            with path.open("rb") as source:
                digest.update(hashlib.file_digest(source, "sha256").digest())
        digest.update(b"\0")
    return files, repositories, digest.hexdigest()


def _portable_paths(root):
    for path in (root / "Cargo.toml", root / ".cargo/config.toml"):
        if not path.exists():
            continue
        configuration = tomllib.loads(path.read_text())
        for patches in configuration.get("patch", {}).values():
            for dependency in patches.values():
                if isinstance(dependency, dict) and "path" in dependency:
                    selected = Path(dependency["path"])
                    resolved = (root / selected).resolve()
                    if selected.is_absolute() or not resolved.is_relative_to(root):
                        raise TaskError(
                            "server catalogs require project-relative Cargo patches; remove local overrides"
                        )


def prepare(destination, identity):
    """Build Linux locally and collect macOS only from its native host."""
    if identity not in NONPRODUCTION:
        raise TaskError("automatic server catalogs accept only nonproduction identities")
    destination = Path(destination)
    if destination.exists() or destination.is_symlink():
        raise TaskError("the server catalog destination already exists")
    local = host_target()
    source_catalog = os.environ.get("SPACETERM_SERVER_ARTIFACTS")
    if local != TARGETS["darwin"] and not source_catalog:
        raise TaskError(
            "Linux catalogs need a Mac-built catalog in SPACETERM_SERVER_ARTIFACTS from the same source snapshot"
        )
    cache = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")) / "server"
    cache.mkdir(parents=True, exist_ok=True)
    with (cache / ".catalog.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        _portable_paths(ROOT)
        digest = _source(ROOT)[2]
        overrides = {
            "CARGO_TARGET_DIR": str(cache / identity),
            "SPACETERM_SERVER_SOURCE_STATE": digest,
        }
        previous = {name: os.environ.get(name) for name in overrides}
        os.environ.update(overrides)
        try:
            with tempfile.TemporaryDirectory(dir=cache, prefix=".catalog-") as temporary:
                collected = Path(temporary)
                targets = [local, *(target for target in TARGETS.values() if target != local)]
                for target in targets:
                    if target == TARGETS["darwin"] and target != local:
                        from spaceterm_tasks.server_catalog import collect_target

                        collect_target(source_catalog, collected / target, identity, target, digest)
                    else:
                        archive, signature = package(identity, target=target)
                        if _source(ROOT)[2] != digest:
                            raise TaskError(
                                "source changed during server catalog preparation; retry the command"
                            )
                        entry = collected / target
                        entry.mkdir()
                        shutil.copyfile(archive, entry / "artifact.tar.gz")
                        shutil.copyfile(signature, entry / "artifact.sig")
                        (entry / "artifact.sha256").write_text(sha256(archive) + "\n")
                    if _source(ROOT)[2] != digest:
                        raise TaskError(
                            "source changed during server catalog preparation; retry the command"
                        )
                stage_catalog(None, None, destination, identity, source=collected, require_all=True)
        finally:
            for name, value in previous.items():
                if value is None:
                    os.environ.pop(name, None)
                else:
                    os.environ[name] = value
    print(f"Prepared {identity} server catalog from source SHA256 {digest}")
    return destination
