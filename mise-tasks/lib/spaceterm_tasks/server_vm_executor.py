"""Create-before-start ownership for one bounded disposable VM.

One private VM record retains the exact container and allocated files. Unknown
creation outcomes and failed sync stay closed. Removal never follows links,
searches by name, prunes containers, removes volumes, or recursively deletes data.
"""

import fcntl
import hashlib
import json
import os
import re
import secrets
import stat
from contextlib import contextmanager
from dataclasses import asdict

from spaceterm_tasks import TaskError, server_artifacts
from spaceterm_tasks import server_vm as vm
from spaceterm_tasks import server_vm_linux as native

STATE = "vm.json"
LOCK = "vm.lock"
FILES = {"os.qcow2", "seed.iso", "id_ed25519", "known_hosts"}


def _file_identity(directory, name):
    info = (directory / name).lstat()
    if (
        not stat.S_ISREG(info.st_mode)
        or info.st_uid != os.getuid()
        or info.st_nlink != 1
        or stat.S_IMODE(info.st_mode) != 0o600
    ):
        raise TaskError("unsafe-vm-file")
    result = {"device": info.st_dev, "inode": info.st_ino}
    if name != "os.qcow2":
        result["sha256"] = hashlib.sha256(
            native.private_file(directory / name, 4 * 1024 * 1024)
        ).hexdigest()
    return result


class VmExecutor:
    def __init__(self, directory, provider=None):
        self.directory = directory
        self.native = provider if provider is not None else native.NativeVm()

    @contextmanager
    def _locked(self):
        identity = vm.private_directory(self.directory)
        descriptor = os.open(self.directory, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        observed = os.fstat(descriptor)
        if (observed.st_dev, observed.st_ino) != identity:
            os.close(descriptor)
            raise TaskError("vm-directory-changed")
        lock = None
        try:
            lock = os.open(LOCK, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600, dir_fd=descriptor)
            info = os.fstat(lock)
            if (
                info.st_uid != os.getuid()
                or info.st_nlink != 1
                or not stat.S_ISREG(info.st_mode)
                or stat.S_IMODE(info.st_mode) != 0o600
            ):
                raise TaskError("unsafe-vm-lock")
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            if vm.private_directory(self.directory) != identity:
                raise TaskError("vm-directory-changed")
            yield descriptor, identity
        finally:
            if lock is not None:
                os.close(lock)
            os.close(descriptor)

    def _write(self, descriptor, document, initial=False):
        data = json.dumps(document, separators=(",", ":")).encode()
        if len(data) > 64 * 1024:
            raise TaskError("vm-record-limit")
        # A failed publication leaves this fence. Reopening never retries an
        # external action from a possibly visible tentative phase.
        fence = os.open(
            "vm.pending",
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
            0o600,
            dir_fd=descriptor,
        )
        os.close(fence)
        try:
            os.fsync(descriptor)
        except OSError:
            raise TaskError("vm-state-indeterminate") from None
        name = STATE if initial else "vm.next"
        fd = os.open(
            name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=descriptor
        )
        try:
            with os.fdopen(fd, "wb") as output:
                output.write(data)
                output.flush()
                os.fsync(output.fileno())
            if not initial:
                os.rename(name, STATE, src_dir_fd=descriptor, dst_dir_fd=descriptor)
            os.fsync(descriptor)
            os.unlink("vm.pending", dir_fd=descriptor)
        except OSError:
            raise TaskError("vm-state-indeterminate") from None

    def _load(self, identity):
        document = server_artifacts.strict_json(
            native.private_file(self.directory / STATE, 64 * 1024), 64 * 1024
        )
        if (
            not isinstance(document, dict)
            or set(document)
            != {
                "schema",
                "directory",
                "token",
                "runner_image",
                "resources",
                "ssh_port",
                "phase",
                "container",
                "files",
            }
            or type(document["schema"]) is not int
            or document["schema"] != 1
            or document["directory"] != list(identity)
            or document["phase"]
            not in ("prepared", "creating", "created", "running", "stopped", "removing")
            or not isinstance(document["files"], dict)
            or set(document["files"]) != FILES
            or not isinstance(document["resources"], dict)
        ):
            raise TaskError("vm-record-rejected")
        try:
            resources = vm.Resources(**document["resources"])
            resources.validate()
        except TypeError:
            raise TaskError("vm-record-rejected") from None
        if (
            not isinstance(document["token"], str)
            or not re.fullmatch(r"[0-9a-f]{64}", document["token"])
            or not isinstance(document["runner_image"], str)
            or not re.fullmatch(r"[a-z0-9./:_-]+@sha256:[0-9a-f]{64}", document["runner_image"])
            or type(document["ssh_port"]) is not int
            or not 1024 <= document["ssh_port"] <= 65535
            or (
                document["container"] is not None
                and (
                    not isinstance(document["container"], str)
                    or not re.fullmatch(r"[0-9a-f]{64}", document["container"])
                )
            )
        ):
            raise TaskError("vm-record-rejected")
        if set(os.listdir(self.directory)) - FILES - {STATE, LOCK}:
            raise TaskError("unexpected-vm-file")
        for name, expected in document["files"].items():
            try:
                actual = _file_identity(self.directory, name)
            except FileNotFoundError:
                if document["phase"] == "removing":
                    continue
                raise TaskError("vm-file-missing") from None
            if actual != expected:
                raise TaskError("vm-file-changed")
        return document, resources

    def prepare(self, request, *, allow_mutation=False):
        vm.require_mutation(allow_mutation)
        if request.directory != self.directory:
            raise TaskError("vm-directory-mismatch")
        request.validate()
        with self._locked() as (descriptor, identity):
            if set(os.listdir(self.directory)) != {LOCK}:
                raise TaskError("vm-directory-not-empty")
            image = self.directory / "image.qcow2"
            vm.copy_verified_image(request.image, image)
            token = secrets.token_hex(32)
            self.native.prepare(self.directory, image, request.resources, token)
            image.unlink()
            document = {
                "schema": 1,
                "directory": list(identity),
                "token": token,
                "runner_image": request.runner_image,
                "resources": asdict(request.resources),
                "ssh_port": request.ssh_port,
                "phase": "prepared",
                "container": None,
                "files": {name: _file_identity(self.directory, name) for name in sorted(FILES)},
            }
            self._write(descriptor, document, initial=True)

    def _inspect(self, document, resources):
        identifier = document["container"]
        if identifier is None:
            raise TaskError("vm-creation-unresolved")
        observation = self.native.inspect(identifier)
        if observation.get("Id") != identifier:
            raise TaskError("vm-ownership-mismatch")
        self.native.validate(
            observation,
            self.directory,
            document["runner_image"],
            resources,
            document["ssh_port"],
            document["token"],
        )
        running = observation.get("State", {}).get("Running")
        if type(running) is not bool:
            raise TaskError("vm-observation-rejected")
        return running

    def start(self, *, allow_mutation=False):
        vm.require_mutation(allow_mutation)
        with self._locked() as (descriptor, identity):
            document, resources = self._load(identity)
            if document["phase"] == "prepared":
                document["phase"] = "creating"
                self._write(descriptor, document)
                document["container"] = self.native.create(
                    self.directory,
                    document["runner_image"],
                    resources,
                    document["ssh_port"],
                    document["token"],
                )
                document["phase"] = "created"
                self._write(descriptor, document)
            if document["phase"] not in ("created", "stopped", "running"):
                raise TaskError("vm-creation-unresolved")
            if not self._inspect(document, resources):
                self.native.start(document["container"])
            if not self._inspect(document, resources):
                raise TaskError("vm-start-unresolved")
            document["phase"] = "running"
            self._write(descriptor, document)

    def stop(self, *, allow_mutation=False):
        vm.require_mutation(allow_mutation)
        with self._locked() as (descriptor, identity):
            document, resources = self._load(identity)
            if document["phase"] not in ("created", "running", "stopped"):
                raise TaskError("vm-creation-unresolved")
            if self._inspect(document, resources):
                self.native.stop(document["container"])
            if self._inspect(document, resources):
                raise TaskError("vm-stop-unresolved")
            document["phase"] = "stopped"
            self._write(descriptor, document)

    def guest(self, operation, *, allow_mutation=False):
        if operation not in ("observe", "status"):
            vm.require_mutation(allow_mutation)
        with self._locked() as (_, identity):
            document, resources = self._load(identity)
            if not self._inspect(document, resources):
                raise TaskError("vm-not-running")
            return native.guest_command(self.directory, document["ssh_port"], operation)

    def remove(self, *, allow_mutation=False):
        vm.require_mutation(allow_mutation)
        with self._locked() as (descriptor, identity):
            document, resources = self._load(identity)
            if document["phase"] not in ("prepared", "stopped", "removing"):
                raise TaskError("vm-not-stopped")
            if document["container"] is not None and self.native.exists(document["container"]):
                if self._inspect(document, resources):
                    raise TaskError("vm-not-stopped")
                self.native.remove(document["container"])
            if document["container"] is not None and self.native.exists(document["container"]):
                raise TaskError("vm-removal-unresolved")
            document["phase"] = "removing"
            self._write(descriptor, document)
            for name in sorted(FILES):
                if (self.directory / name).exists():
                    if _file_identity(self.directory, name) != document["files"][name]:
                        raise TaskError("vm-file-changed")
                    os.unlink(name, dir_fd=descriptor)
            os.unlink(STATE, dir_fd=descriptor)
            os.unlink(LOCK, dir_fd=descriptor)
            os.fsync(descriptor)
            if vm.private_directory(self.directory) != identity:
                raise TaskError("vm-directory-changed")
            self.directory.rmdir()
