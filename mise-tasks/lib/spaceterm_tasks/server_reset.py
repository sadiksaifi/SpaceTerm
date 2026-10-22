"""Offline disposal of selected development state, never a runtime compatibility reader.

The archive preserves the original bytes. No credential store is opened. Stop,
signed installations, Settings, Commands and SSH trust are outside this scope.
"""

import fcntl
import hashlib
import json
import os
import pwd
import secrets
import stat
import sys
from contextlib import ExitStack
from pathlib import Path

from spaceterm_tasks import TaskError


def private_open(path, directory=False):
    """Walk without following symlinks and retain the selected directory or file."""
    fd = os.open("/", os.O_RDONLY | os.O_DIRECTORY)
    try:
        for index, part in enumerate(path.parts[1:]):
            last = index == len(path.parts) - 2
            flags = os.O_RDWR if last and not directory else os.O_RDONLY | os.O_DIRECTORY
            child = os.open(part, flags | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=fd)
            os.close(fd)
            fd = child
        metadata = os.fstat(fd)
        mode = 0o700 if directory else 0o600
        kind = stat.S_ISDIR if directory else stat.S_ISREG
        if (
            metadata.st_uid != os.geteuid()
            or stat.S_IMODE(metadata.st_mode) != mode
            or not kind(metadata.st_mode)
        ):
            raise TaskError("selected state has unsafe ownership, type or permissions")
        return fd
    except BaseException:
        os.close(fd)
        raise


def lock(stack, path):
    fd = private_open(path)
    stack.callback(os.close, fd)
    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)


def snapshot(path, entries, stack):
    directory = path.is_dir()
    fd = private_open(path, directory)
    stack.callback(os.close, fd)
    metadata = os.fstat(fd)
    data = b""
    if directory:
        for name in sorted(os.listdir(fd)):
            snapshot(path / name, entries, stack)
    else:
        if metadata.st_size > 12 * 1024 * 1024:
            raise TaskError("selected state exceeds the retained-record limit")
        data = os.read(fd, 12 * 1024 * 1024 + 1)
        if path.suffix == ".lock":
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        elif path.suffix == ".json":
            value = json.loads(data)
            if path.name == "operations.json" and value.get("pending") is not None:
                raise TaskError("resolve and retire the original client operation before reset")
            if started(value):
                raise TaskError("finish or reconcile the started Git write before reset")
    entries.append(
        (path, metadata, data if not directory else "\0".join(sorted(os.listdir(fd))).encode())
    )


def started(value):
    if isinstance(value, dict):
        return value.get("outcome") == "Pending" or any(started(v) for v in value.values())
    return isinstance(value, list) and any(started(v) for v in value)


def reset(identity, confirm=None):
    if identity not in ("spaceterm-development", "spaceterm-preflight") or os.geteuid() == 0:
        raise TaskError("select one nonroot Development or Preflight account")
    home = Path(pwd.getpwuid(os.geteuid()).pw_dir)
    data = home / ".local/share" / identity / "server"
    temporary = Path("/private/tmp" if sys.platform == "darwin" else "/tmp")
    runtime = temporary / f"{identity}-server-{os.geteuid()}"
    reset_at(data, runtime, confirm)


def reset_at(data, runtime, confirm=None):
    """Archive an exact offline scope after checking runtime, service and original operations."""
    try:
        with ExitStack() as stack:
            root = private_open(data, True)
            stack.callback(os.close, root)
            # An answering or stale socket requires runtime cleanup, never path-based signalling.
            if runtime.exists():
                runtime_fd = private_open(runtime, True)
                stack.callback(os.close, runtime_fd)
                lock(stack, runtime / "server.lock")
                if "s" in os.listdir(runtime_fd):
                    raise TaskError(
                        "finish server and process cleanup before reset; endpoint retained"
                    )
            for name in (
                "setup.lock",
                "service.lock",
                "service-intent.lock",
                "worktree.lock",
                "direct.lock",
            ):
                if name in os.listdir(root):
                    lock(stack, data / name)
            if "service.json" in os.listdir(root):
                fd = private_open(data / "service.json")
                stack.callback(os.close, fd)
                state = json.loads(os.read(fd, 65537))
                if state.get("run") is not None or state.get("removing"):
                    raise TaskError("service process cleanup remains unresolved; reset refused")
                if state.get("installed") and not state.get("stopped"):
                    raise TaskError("explicitly Stop the installed service before reset")
            selected = [
                data / name
                for name in ("identity.json", "direct.json", "clients")
                if name in os.listdir(root)
            ]
            if not selected:
                raise TaskError("no selected unreleased state exists")
            entries = []
            for path in selected:
                snapshot(path, entries, stack)
            digest = hashlib.sha256()
            for path, metadata, contents in entries:
                digest.update(str(path).encode() + b"\0")
                digest.update(str((metadata.st_dev, metadata.st_ino, metadata.st_mode)).encode())
                digest.update(contents)
            token = digest.hexdigest()
            print(f"Selected server state: {data}")
            for path in selected:
                print(f"Archive: {path}")
            print(
                "Loss from active use: server identity and organization, completed operation outcomes, "
                "direct server trust and paired-device grants, client identities, attachments, "
                "deferred releases and personal views in this scope."
            )
            print(
                "Kept: Settings, Commands, repository files, service Stop intent, installations, "
                "SSH trust, native client credentials and signing keys."
            )
            print(f"Review digest: {token}")
            if confirm is None:
                print(
                    "Preview only. Obtain explicit confirmation of this scope and loss before --confirm DIGEST."
                )
                return
            if confirm != token:
                raise TaskError(
                    "selected state changed or confirmation differs; review a fresh preview"
                )
            current_root = data.lstat()
            if (current_root.st_dev, current_root.st_ino) != (
                os.fstat(root).st_dev,
                os.fstat(root).st_ino,
            ):
                raise TaskError("selected directory changed; reset refused")
            for path, metadata, contents in entries:
                current = path.lstat()
                if (current.st_dev, current.st_ino, current.st_mode) != (
                    metadata.st_dev,
                    metadata.st_ino,
                    metadata.st_mode,
                ):
                    raise TaskError("selected entry changed; reset refused")
                if (
                    stat.S_ISDIR(metadata.st_mode)
                    and "\0".join(sorted(os.listdir(path))).encode() != contents
                ):
                    raise TaskError("selected directory entries changed; reset refused")
            archive = f"reset-{secrets.token_hex(12)}"
            os.mkdir(archive, 0o700, dir_fd=root)
            destination = os.open(
                archive, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=root
            )
            stack.callback(os.close, destination)
            for path in selected:
                os.rename(path.name, path.name, src_dir_fd=root, dst_dir_fd=destination)
                os.fsync(destination)
                os.fsync(root)
            print(
                f"Archived exact selected state at {data / archive}. Setup remains explicit; Stop is unchanged."
            )
    except (OSError, ValueError, TypeError):
        raise TaskError("state is unavailable, busy or unrecognized; reset refused") from None
