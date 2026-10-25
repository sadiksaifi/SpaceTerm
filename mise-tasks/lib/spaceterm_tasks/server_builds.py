"""Prepare both nonproduction server targets from one exact current-source snapshot.

Native peers are ordinary SSH builders, selected by packaging/server-builders.json.
They need strict trusted SSH access, rsync, mise, and the project's native build
prerequisites. Only target/server-builds is written on their existing checkout.
"""

import argparse
import fcntl
import hashlib
import json
import os
import re
import select
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import tomllib
from contextlib import suppress
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, main
from spaceterm_tasks.packaging import checked
from spaceterm_tasks.processes import OWN_GROUP, stop_group, terminating_signals_raise
from spaceterm_tasks.server_catalog import stage_catalog, verify_catalog
from spaceterm_tasks.server_package import TARGETS, host_target, package

NONPRODUCTION = ("spaceterm-development", "spaceterm-preflight")


def _git(root, *arguments):
    return checked(
        ["git", "--no-optional-locks", "-C", str(root), *arguments],
        "the native builder source Git metadata is unavailable",
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
                        "initialize native builder submodules: mise run setup:submodules"
                    )
                visit(path)
            elif path.exists() or path.is_symlink():
                if not path.is_file() and not path.is_symlink():
                    raise TaskError("native builder source must contain regular files or symlinks")
                files[path.relative_to(root)] = path
        # Do not silently omit a missing, uninitialized gitlink.
        for line in _git(directory, "ls-files", "--stage", "-z").split(b"\0"):
            if line.startswith(b"160000 "):
                nested = directory / os.fsdecode(line.split(b"\t", 1)[1])
                if nested.relative_to(root) not in repositories:
                    raise TaskError(
                        "initialize native builder submodules: mise run setup:submodules"
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
                raise TaskError("native builder source symlinks must stay inside the project")
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
                            "native builders require project-relative Cargo patches; remove local overrides"
                        )


def snapshot(root, parent):
    """Freeze source bytes and detached Git identity, without config, remotes or credentials."""
    root = Path(root).resolve()
    parent = Path(parent)
    _portable_paths(root)
    files, repositories, digest = _source(root)
    parent.mkdir(parents=True, exist_ok=True)
    destination = parent / digest
    if destination.is_dir():
        # The content address alone is never evidence that cached bytes stayed intact.
        if _source(destination)[2] != digest:
            raise TaskError(
                "the cached native builder source changed; remove that snapshot and retry"
            )
        if _source(root)[2] != digest:
            raise TaskError("source changed during native builder snapshot; retry the command")
        return destination, digest
    with tempfile.TemporaryDirectory(dir=parent, prefix=".source-") as temporary:
        staging = Path(temporary) / "source"
        staging.mkdir()
        for relative, path in files.items():
            output = staging / relative
            output.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, output, follow_symlinks=False)
        for relative, commit in repositories.items():
            source = root / relative
            metadata = staging / relative / ".git"
            metadata.mkdir()
            (metadata / "refs").mkdir()
            git_directory = Path(_git(source, "rev-parse", "--absolute-git-dir").strip().decode())
            common = Path(
                _git(source, "rev-parse", "--path-format=absolute", "--git-common-dir")
                .strip()
                .decode()
            )
            if (common / "objects/info/alternates").exists():
                raise TaskError(
                    "native builders require self-contained Git objects, without alternates"
                )
            shutil.copytree(common / "objects", metadata / "objects", copy_function=os.link)
            shutil.copyfile(git_directory / "index", metadata / "index")
            (metadata / "HEAD").write_text(commit + "\n")
            (metadata / "config").write_text("[core]\nrepositoryformatversion = 0\nbare = false\n")
        if _source(staging)[2] != digest or _source(root)[2] != digest:
            raise TaskError("source changed during native builder snapshot; retry the command")
        try:
            staging.rename(destination)
        except FileExistsError:
            pass
    return destination, digest


def _ssh(configuration):
    return [
        "ssh",
        *(["-F", str(Path(configuration).resolve())] if configuration else []),
        "-o",
        "BatchMode=yes",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "ConnectTimeout=10",
    ]


def _remote_run(command, failure):
    """Closing SSH stdin cancels only this runner and its owned Cargo process group."""
    with terminating_signals_raise():
        process = subprocess.Popen(command, stdin=subprocess.PIPE, start_new_session=OWN_GROUP)
        try:
            assert process.stdin is not None
            while process.poll() is None:
                try:
                    process.stdin.write(b".")
                    process.stdin.flush()
                except BrokenPipeError:
                    break
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    pass
            status = process.wait()
        except BaseException:
            if process.stdin:
                with suppress(BrokenPipeError):
                    process.stdin.close()
            stop_group(process)
            raise
        finally:
            if process.stdin:
                with suppress(BrokenPipeError):
                    process.stdin.close()
    if status:
        raise TaskError(failure)


def _sync_source(source, destination, transport, failure):
    """Transfer source and sanitized Git metadata, excluding later ignored tool output."""
    files, repositories, _ = _source(source)
    selected = set(files)
    for relative in repositories:
        metadata = source / relative / ".git"
        selected.add(relative / ".git/refs")
        selected.update(path.relative_to(source) for path in metadata.rglob("*") if path.is_file())
    with tempfile.NamedTemporaryFile() as manifest:
        manifest.write(b"\0".join(os.fsencode(path) for path in sorted(selected)) + b"\0")
        manifest.flush()
        checked(
            [
                "rsync",
                "-a",
                "--from0",
                "--files-from",
                manifest.name,
                *transport,
                str(source) + "/",
                destination,
            ],
            failure,
        )


def _peer(source, digest, identity, target, output, ssh_config):
    configuration = json.loads((source / "packaging/server-builders.json").read_text())
    peer = configuration[target]
    # These are local adapter choices, never values supplied by an SSH server.
    if not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9_.@-]*", peer):
        raise TaskError("packaging/server-builders.json needs a valid SSH builder alias")
    ssh = _ssh(ssh_config)
    failure = (
        f"native builder {peer} is unavailable; configure trusted SSH access to the alias "
        "in packaging/server-builders.json (or use --ssh-config), install rsync and mise, "
        "and prepare its native project build tools"
    )
    remote = checked(
        [
            *ssh,
            peer,
            'command -v rsync >/dev/null && test -d "$HOME/Projects/SpaceTerm" && printf %s "$HOME/Projects/SpaceTerm/target/server-builds"',
        ],
        failure,
        text=True,
    )
    if not remote.startswith("/") or "\n" in remote:
        raise TaskError(failure)
    remote_source = remote + "/sources/" + digest
    checked([*ssh, peer, "mkdir -p " + shlex.quote(remote_source)], failure)
    # A content-addressed tree is only source. Cargo and Zig caches remain separate.
    _sync_source(
        source, peer + ":" + shlex.quote(remote_source) + "/", ("-e", shlex.join(ssh)), failure
    )
    runner = (
        'set -eu; export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH" PYTHONDONTWRITEBYTECODE=1; '
        f"cd {shlex.quote(remote_source)}; "
        "mise trust --yes .mise.toml; "
        f"exec mise exec -- python -m spaceterm_tasks.server_builds --native {shlex.quote(identity)} "
        f"--digest {digest} --cache {shlex.quote(remote)} --watch-stdin"
    )
    _remote_run([*ssh, peer, runner], failure)
    output.parent.mkdir(parents=True, exist_ok=True)
    checked(
        [
            "rsync",
            "-a",
            "-e",
            shlex.join(ssh),
            peer
            + ":"
            + shlex.quote(remote + "/catalogs/" + digest + "/" + identity + "/" + target)
            + "/",
            str(output) + "/",
        ],
        failure,
    )


def native(identity, digest, cache):
    """Native producer authenticates its own package before any other host consumes it."""
    if identity not in NONPRODUCTION:
        raise TaskError("automatic native builders accept only nonproduction identities")
    cache = Path(cache).resolve()
    cache.mkdir(parents=True, exist_ok=True)
    with (cache / ".lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if _source(ROOT)[2] != digest:
            raise TaskError("native builder source does not match the requested content hash")
        target = host_target()
        catalog = cache / "catalogs" / digest / identity
        if catalog.exists():
            entries = verify_catalog(catalog, identity)
            commit = _git(ROOT, "rev-parse", "HEAD").strip().decode()
            if entries[target]["build"]["commit"] != commit:
                raise TaskError("the cached server catalog differs from its source commit")
            return catalog / target
        # Build scripts may embed CARGO_MANIFEST_DIR. Keep that path stable across
        # snapshots while the host lock excludes another producer from replacing it.
        stable = cache / "build-source" / identity
        with tempfile.TemporaryDirectory(dir=cache, prefix=".build-source-") as temporary:
            prepared = Path(temporary) / "source"
            prepared.mkdir()
            _sync_source(
                ROOT,
                str(prepared) + "/",
                (),
                "the native builder could not prepare its stable source directory",
            )
            stable.parent.mkdir(parents=True, exist_ok=True)
            # Only this task's disposable source copy is replaced, under its host lock.
            # Cargo, Zig, sysroots and authenticated catalog outputs are retained.
            if stable.exists():
                shutil.rmtree(stable)
            prepared.rename(stable)
        _run_local(stable, identity, digest, cache, build=True)
        return catalog / target


def _build(identity, digest, cache):
    """Called only by the native producer while it holds the host build lock."""
    if _source(ROOT)[2] != digest:
        raise TaskError("native builder source does not match the requested content hash")
    os.environ["CARGO_TARGET_DIR"] = str(cache / "cargo" / identity)
    if sys.platform == "linux":
        from spaceterm_tasks.linux_toolchain import setup

        os.environ["SPACETERM_LINUX_PACKAGE_SYSROOT"] = str(cache / "linux-sysroots")
        setup()
    archive, signature = package(identity)
    if _source(ROOT)[2] != digest:
        raise TaskError("native builder source changed during the build; retry the command")
    stage_catalog(archive, signature, cache / "catalogs" / digest / identity, identity)
    commit = _git(ROOT, "rev-parse", "HEAD").strip().decode()
    print(f"Native {host_target()} server: commit {commit}, source SHA256 {digest}")


def _run_local(source, identity, digest, cache, *, build=False):
    checked(
        ["mise", "trust", "--yes", str(source / ".mise.toml")],
        "mise could not trust the local source snapshot",
    )
    command = [
        "mise",
        "exec",
        "--",
        "python",
        "-m",
        "spaceterm_tasks.server_builds",
        "--build" if build else "--native",
        identity,
        "--digest",
        digest,
        "--cache",
        str(cache),
    ]
    with terminating_signals_raise():
        process = subprocess.Popen(
            command,
            cwd=source,
            env={
                **os.environ,
                "PYTHONPATH": str(source / "mise-tasks/lib"),
                "PYTHONDONTWRITEBYTECODE": "1",
            },
            start_new_session=OWN_GROUP,
        )
        try:
            if process.wait():
                raise TaskError("the local native server builder failed")
        except BaseException:
            stop_group(process, timeout=15)
            raise


def prepare(destination, identity, *, ssh_config=None):
    """Prepare every supported native target automatically; never fall back to a partial catalog."""
    if identity not in NONPRODUCTION:
        raise TaskError("automatic native builders accept only nonproduction identities")
    if any(shutil.which(tool) is None for tool in ("mise", "ssh", "rsync")):
        raise TaskError(
            "automatic server catalogs require mise, OpenSSH and rsync on the build host"
        )
    cache = (ROOT / "target/server-builds").resolve()
    source, digest = snapshot(ROOT, cache / "sources")
    destination = Path(destination)
    if destination.exists() or destination.is_symlink():
        raise TaskError("the server catalog destination already exists")
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(
        dir=destination.parent, prefix=".native-catalog-"
    ) as temporary:
        collected = Path(temporary) / "collected"
        collected.mkdir()
        _run_local(source, identity, digest, cache)
        local = host_target()
        shutil.copytree(cache / "catalogs" / digest / identity / local, collected / local)
        for target in TARGETS.values():
            if target != local:
                _peer(source, digest, identity, target, collected / target, ssh_config)
        if _source(ROOT)[2] != digest:
            raise TaskError("source changed during catalog preparation; retry the command")
        stage_catalog(None, None, destination, identity, source=collected, require_all=True)
    print(f"Prepared {identity} server catalog from source SHA256 {digest}")
    return destination


def _watch_stdin():
    """A broken SSH connection cannot leave the exact native build running indefinitely."""
    while True:
        ready, _, _ = select.select([sys.stdin], [], [], 15)
        if not ready or os.read(sys.stdin.fileno(), 1) != b".":
            os.kill(os.getpid(), signal.SIGTERM)
            return


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--native", choices=NONPRODUCTION)
    mode.add_argument("--build", choices=NONPRODUCTION)
    parser.add_argument("--digest", required=True)
    parser.add_argument("--cache", type=Path, required=True)
    parser.add_argument("--watch-stdin", action="store_true")
    args = parser.parse_args()
    if args.watch_stdin:
        threading.Thread(target=_watch_stdin, daemon=True).start()

    def run():
        if args.build:
            _build(args.build, args.digest, args.cache)
        else:
            native(args.native, args.digest, args.cache)

    main(run)
