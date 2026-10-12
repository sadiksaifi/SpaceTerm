"""Executable VM contract tests with real private files and intentional boundaries.

No Docker, SSH, QEMU, download, signing or credential command runs here. Synthetic
native facts do not establish Ubuntu, KVM, cryptography, or boot evidence. Tests
assert the missing first vertical behavior, not the not-ready scaffold itself.
"""

import errno
import fcntl
import hashlib
import io
import json
import multiprocessing
import os
import socket
import stat
import tempfile
import threading
import time
import unittest
from collections.abc import Callable, Iterator
from contextlib import redirect_stderr, redirect_stdout
from dataclasses import replace
from multiprocessing.connection import Connection
from pathlib import Path
from typing import TYPE_CHECKING
from unittest.mock import patch

from spaceterm_tasks import server_vm as vm_module
from spaceterm_tasks import server_vm_executor as executor_module
from spaceterm_tasks.server_vm import (
    MANIFEST_NAME,
    MANIFEST_SCHEMA,
    Classification,
    ContainerObservation,
    DiskEvidence,
    GuestImage,
    OwnedPath,
    Ownership,
    Phase,
    PreparedVm,
    PrepareRequest,
    Resources,
    RunnerIdentity,
    SeedEvidence,
)
from spaceterm_tasks.server_vm_executor import (
    JOURNAL_NAME,
    JOURNAL_SCHEMA,
    ExecutionReceipt,
    ExecutionResult,
    VmExecutor,
)

from tests.test_server_vm import (
    CONTAINER_ID,
    LOCAL_RUNNER_IMAGE,
    SIGNER,
    TOKEN,
    ControlledSignatures,
    bounded_configuration,
    bounded_policy,
    manifest_artifact_record,
    manifest_path_record,
    prepared_artifacts,
)

if TYPE_CHECKING:
    from multiprocessing.connection import PipeConnection


def write_private(directory_fd: int, name: str, data: bytes) -> None:
    fd = os.open(
        name,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
        0o600,
        dir_fd=directory_fd,
    )
    try:
        remaining = memoryview(data)
        while remaining:
            remaining = remaining[os.write(fd, remaining) :]
    finally:
        os.close(fd)


def state_document(fd: int, state: Path) -> dict | None:
    """Observe a real fsynced journal inode, including atomic replacement files."""
    observation = os.fstat(fd)
    if not stat.S_ISREG(observation.st_mode) or observation.st_size > 64 * 1024:
        return None
    for candidate in state.iterdir():
        current = candidate.lstat()
        if (observation.st_dev, observation.st_ino) == (current.st_dev, current.st_ino):
            try:
                document = json.loads(candidate.read_bytes())
            except (ValueError, UnicodeError):
                return None
            return document if isinstance(document, dict) else None
    return None


class ControlledVmNative:
    """Fixed native operations, never a generic command runner or policy mock."""

    def __init__(self):
        self.calls: list[tuple[str, str | None]] = []
        self.callbacks: dict[str, Callable[[], None]] = {}
        self.errors: dict[str, BaseException] = {}
        self.image_reads: list[bytes] = []
        self.create_result: str | None = CONTAINER_ID
        self.created_vm: PreparedVm | None = None
        self.observation: ContainerObservation | None = None
        self.os_virtual_size = 24 * 1024**3
        self.encrypted_virtual_size = 8 * 1024**3
        self.os_disk_bytes = b"controlled standalone OS disk fixture"
        self.encrypted_disk_bytes = b"controlled standalone data disk fixture"

    def boundary(self, operation: str, container_id: str | None = None) -> None:
        self.calls.append((operation, container_id))
        if operation in self.callbacks:
            self.callbacks[operation]()
        if operation in self.errors:
            raise self.errors[operation]

    def create_credentials(self, directory_fd: int) -> None:
        self.boundary("create-credentials")
        os.mkdir("credentials", 0o700, dir_fd=directory_fd)
        fd = os.open(
            "credentials",
            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
            dir_fd=directory_fd,
        )
        try:
            write_private(fd, "id_ed25519", b"controlled disposable private key fixture")
            write_private(fd, "id_ed25519.pub", b"controlled disposable public key fixture")
        finally:
            os.close(fd)

    def create_os_disk(self, directory_fd: int, image_fd: int, virtual_size_bytes: int) -> None:
        self.boundary("create-os-disk")
        os.lseek(image_fd, 0, os.SEEK_SET)
        data = bytearray()
        while chunk := os.read(image_fd, 1024 * 1024):
            data.extend(chunk)
        self.image_reads.append(bytes(data))
        self.os_virtual_size = virtual_size_bytes
        write_private(directory_fd, "os.qcow2", self.os_disk_bytes)

    def create_encrypted_disk(self, directory_fd: int, virtual_size_bytes: int) -> None:
        self.boundary("create-encrypted-disk")
        self.encrypted_virtual_size = virtual_size_bytes
        write_private(directory_fd, "encrypted.qcow2", self.encrypted_disk_bytes)

    def inspect_disk(self, disk_fd: int, retained: OwnedPath) -> DiskEvidence:
        self.boundary("inspect-disk")
        size = (
            self.os_virtual_size
            if retained.path.name == "os.qcow2"
            else self.encrypted_virtual_size
        )
        return DiskEvidence(
            retained, "qcow2", size, False, False, os.fstat(disk_fd).st_blocks * 512
        )

    def create_seed(self, directory_fd: int, token: str) -> None:
        self.boundary("create-seed")
        write_private(directory_fd, "seed.iso", b"controlled CIDATA NoCloud seed fixture")

    def inspect_seed(self, seed_fd: int, retained: OwnedPath) -> SeedEvidence:
        self.boundary("inspect-seed")
        os.lseek(seed_fd, 0, os.SEEK_SET)
        data = os.read(seed_fd, 4 * 1024**2 + 1)
        return SeedEvidence(retained, len(data), hashlib.sha256(data).hexdigest(), "CIDATA")

    def create_container(self, vm: PreparedVm, runner: RunnerIdentity) -> str | None:
        self.created_vm = vm
        self.boundary("create-container")
        return self.create_result

    def inspect(self, container_id: str) -> ContainerObservation | None:
        self.boundary("inspect", container_id)
        return self.observation

    def start_container(self, container_id: str) -> None:
        self.boundary("start-container", container_id)

    def stop_container(self, container_id: str, timeout_seconds: int) -> None:
        self.boundary("stop-container", container_id)

    def remove_container(self, container_id: str) -> None:
        self.boundary("remove-container", container_id)


class FailingCredentialAllocation(ControlledVmNative):
    """Allocate the fixed files through the real descriptor, then lose the callback."""

    def __init__(self, failure: BaseException, after_allocation: Callable[[], None]):
        super().__init__()
        self.failure = failure
        self.after_allocation = after_allocation

    def create_credentials(self, directory_fd: int) -> None:
        super().create_credentials(directory_fd)
        self.after_allocation()
        raise self.failure


class ObservedCredentialAllocation(ControlledVmNative):
    def __init__(self, after_allocation: Callable[[], None]):
        super().__init__()
        self.after_allocation = after_allocation

    def create_credentials(self, directory_fd: int) -> None:
        super().create_credentials(directory_fd)
        self.after_allocation()


def invoke_inside_handled_interruption(
    operation: Callable[[], ExecutionResult],
    handled: BaseException,
) -> tuple[ExecutionResult | None, BaseException | None]:
    """Invoke normally from a caller's handler, without unwinding the operation."""
    try:
        raise handled
    except (KeyboardInterrupt, SystemExit):
        try:
            return operation(), None
        except BaseException as error:
            return None, error


class OwnedDescriptorCloseFault:
    """Close an owned generation once, then report one controlled release error."""

    def __init__(
        self,
        failure: BaseException | None = None,
        *,
        track_later_opens: bool = False,
    ):
        self.failure = (
            failure
            if failure is not None
            else OSError(errno.EIO, "controlled error after owned descriptor release")
        )
        self.track_later_opens = track_later_opens
        self.real_open, self.real_close = os.open, os.close
        self.real_fstat, self.real_flock = os.fstat, fcntl.flock
        self.active: dict[int, tuple[int, tuple[int, int], str, int]] = {}
        self.claims: dict[tuple[int, int], int] = {}
        self.serial = 0
        self.pending: set[int] = set()
        self.attempted: list[int] = []
        self.unowned_closes: list[int] = []
        self.target: int | None = None
        self.triggered = False

    def open(self, path, flags: int, mode: int = 0o777, *, dir_fd: int | None = None) -> int:
        fd = self.real_open(path, flags, mode, dir_fd=dir_fd)
        observation = self.real_fstat(fd)
        self.serial += 1
        self.active[fd] = (
            self.serial,
            (observation.st_dev, observation.st_ino),
            Path(path).name,
            flags,
        )
        if self.track_later_opens and self.target is not None:
            self.pending.add(self.serial)
        return fd

    def flock(self, fd: int, operation: int) -> None:
        self.real_flock(fd, operation)
        if operation & fcntl.LOCK_EX:
            serial, identity, _, _ = self.active[fd]
            self.claims[identity] = serial

    def arm(self, mechanism: str, directory: OwnedPath) -> None:
        self.pending = {record[0] for record in self.active.values()}
        if mechanism == "journal-callback":
            candidates = [
                serial
                for serial, _, name, flags in self.active.values()
                if name == JOURNAL_NAME and flags & os.O_RDWR
            ]
            if len(candidates) != 1:
                raise AssertionError("close fixture needs one held writable journal")
            self.target = candidates[0]
        else:
            self.target = self.claims[(directory.device, directory.inode)]

    def close(self, fd: int) -> None:
        record = self.active.pop(fd, None)
        if record is None:
            # Never retry a released number: another owner could already reuse it.
            self.unowned_closes.append(fd)
            raise OSError(errno.EBADF, "controlled repeated descriptor release")
        serial = record[0]
        if self.target is not None:
            self.attempted.append(serial)
        self.real_close(fd)
        if serial == self.target and not self.triggered:
            self.triggered = True
            raise self.failure

    def locks_available(self, roots: tuple[OwnedPath, ...]) -> list[bool]:
        results = []
        for root in roots:
            fd = self.real_open(
                root.path,
                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
            )
            try:
                try:
                    self.real_flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except OSError as error:
                    if error.errno not in (errno.EACCES, errno.EAGAIN):
                        raise
                    results.append(False)
                else:
                    results.append(True)
            finally:
                self.real_close(fd)
        return results

    def release_remaining(self) -> set[int]:
        unreleased = {record[0] for record in self.active.values()}
        for fd, (_, identity, _, _) in tuple(self.active.items()):
            try:
                observation = self.real_fstat(fd)
            except OSError as error:
                if error.errno != errno.EBADF:
                    raise
            else:
                if (observation.st_dev, observation.st_ino) == identity:
                    self.real_close(fd)
            finally:
                del self.active[fd]
        return unreleased


class FiniteDirectoryEntries(Iterator[os.DirEntry[str]]):
    """A finite OS edge with consumption and context-release observations."""

    def __init__(
        self,
        entries: tuple[os.DirEntry[str], ...],
        interruption: BaseException | None = None,
    ):
        self.entries = iter(entries)
        self.interruption = interruption
        self.consumed = 0
        self.closed = False

    def __iter__(self):
        return self

    def __next__(self) -> os.DirEntry[str]:
        if self.closed:
            raise StopIteration
        if self.interruption is not None and self.consumed:
            raise self.interruption
        entry = next(self.entries)
        self.consumed += 1
        return entry

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        self.close()

    def close(self) -> None:
        self.closed = True


class ControlledNamespaceOs:
    """Private module OS view; all entries are real, bounded, owned fixture files."""

    def __init__(self, interruption: BaseException | None = None):
        self.interruption = interruption
        self.target: OwnedPath | None = None
        self.entries: tuple[os.DirEntry[str], ...] = ()
        self.scans: list[FiniteDirectoryEntries] = []
        self.foreign_names = ("foreign-first", "foreign-tail-0", "foreign-tail-1", "foreign-tail-2")

    def __getattr__(self, name: str):
        return getattr(os, name)

    def arm(self, directory: Path, allowed: tuple[str, ...], *, foreign_first: bool) -> None:
        for name in self.foreign_names:
            path = directory / name
            path.touch(mode=0o600)
            path.write_bytes(b"owned foreign-entry fixture")
        self.target = OwnedPath.capture(directory)
        with os.scandir(directory) as entries:
            actual = {entry.name: entry for entry in entries}
        order = (
            (self.foreign_names[0], *allowed, *self.foreign_names[1:])
            if foreign_first
            else (*allowed, *self.foreign_names)
        )
        self.entries = tuple(actual[name] for name in order)

    def controlled_entries(self, path) -> FiniteDirectoryEntries | None:
        if self.target is None or type(path) is not int:
            return None
        observation = os.fstat(path)
        if (observation.st_dev, observation.st_ino) != (self.target.device, self.target.inode):
            return None
        entries = FiniteDirectoryEntries(self.entries, self.interruption)
        self.scans.append(entries)
        return entries

    def scandir(self, path):
        entries = self.controlled_entries(path)
        return entries if entries is not None else os.scandir(path)

    def listdir(self, path):
        entries = self.controlled_entries(path)
        if entries is None:
            return os.listdir(path)
        # Model listdir's eager allocation with only nine or fewer real names.
        with entries:
            return [entry.name for entry in entries]


_PROCESS_WAIT_SECONDS = 5.0


def start_in_process(
    checkout: Path,
    runner: RunnerIdentity,
    receipt: ExecutionReceipt,
    container_id: str,
    messages: "Connection | PipeConnection",
    release_pending,
    release_create,
    *,
    pause_pending: bool,
    pause_create: bool,
) -> None:
    """Independent executor and native adapter; only real journal writes are paused.

    A removed write hook or a rejected competing invocation needs no rendezvous.
    Every wait is finite. Separate processes rule out process-local lock repairs.
    """
    try:
        if receipt.prepared is None:
            raise AssertionError("worker requires successful public preparation")
        vm = receipt.prepared
        native = ControlledVmNative()
        native.create_result = container_id
        native.observation = ContainerObservation(
            container_id,
            receipt.token,
            vm.request.directory,
            False,
            bounded_configuration(vm.request.directory, runner, LOCAL_RUNNER_IMAGE),
            bounded_policy(),
        )

        def allocated() -> None:
            messages.send(("allocated", container_id))
            if pause_create and not release_create.wait(4 * _PROCESS_WAIT_SECONDS):
                raise TimeoutError("bounded create pause expired")

        native.callbacks["create-container"] = allocated
        native.callbacks["start-container"] = lambda: messages.send(("native-start", container_id))
        executor = VmExecutor(
            checkout,
            ControlledSignatures(),
            native,
            runner,
            runner_image_pin=LOCAL_RUNNER_IMAGE,
        )
        real_write = os.write
        paused = False

        def pause_pending_write(fd: int, data) -> int:
            nonlocal paused
            if pause_pending and not paused and len(data) <= 64 * 1024:
                try:
                    document = json.loads(bytes(data))
                except (ValueError, UnicodeError):
                    document = None
                if (
                    isinstance(document, dict)
                    and document.get("schema") == JOURNAL_SCHEMA
                    and document.get("token") == receipt.token
                    and document.get("stage") == "create-pending"
                    and state_document(fd, receipt.state_directory.path) is not None
                ):
                    # This is the filesystem edge after admission, before any byte
                    # of the pending record is written, not a two-party barrier.
                    paused = True
                    messages.send(("pending-write", None))
                    if not release_pending.wait(4 * _PROCESS_WAIT_SECONDS):
                        raise TimeoutError("bounded journal pause expired")
            return real_write(fd, data)

        messages.send(("invoked", None))
        with patch("os.write", side_effect=pause_pending_write):
            result = executor.start(receipt)
        messages.send(
            (
                "result",
                result.classification,
                result.receipt.container_id if result.receipt is not None else None,
            )
        )
    except BaseException as error:
        messages.send(("error", type(error).__name__))
    finally:
        messages.close()


class VmExecutorTests(unittest.TestCase):
    def setUp(self):
        self.new_fixture()

    def new_fixture(self, *, temporary_directory: str | None = None):
        temporary = tempfile.TemporaryDirectory(
            prefix="spaceterm-vm-executor-test-",
            dir=temporary_directory,
        )
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.checkout = self.root / "checkout"
        self.directory = self.root / "vm"
        self.state = self.root / "independent-state"
        for path in (self.checkout, self.directory, self.state):
            path.mkdir(mode=0o700)
        self.downloads = self.root / "downloads-only"
        self.downloads.mkdir(mode=0o700)
        paths = []
        for parent, name, data in (
            ("image-download", "ubuntu.img", b"controlled exact guest image fixture"),
            ("checksum-download", "SHA256SUMS", b""),
            ("signature-download", "SHA256SUMS.gpg", b"controlled signature fixture"),
        ):
            directory = self.downloads / parent
            directory.mkdir(mode=0o700)
            path = directory / name
            path.touch(mode=0o600)
            path.write_bytes(data)
            paths.append(path)
        image, checksums, signature = paths
        self.image_bytes = image.read_bytes()
        self.digest = hashlib.sha256(self.image_bytes).hexdigest()
        checksums.write_text(f"{self.digest} *{image.name}\n", encoding="ascii")
        self.request = PrepareRequest(
            self.directory,
            GuestImage(image, checksums, signature, self.digest, SIGNER, len(self.image_bytes)),
            LOCAL_RUNNER_IMAGE,
        )
        self.state_identity = OwnedPath.capture(self.state)
        self.signatures = ControlledSignatures()
        self.native = ControlledVmNative()
        self.runner = RunnerIdentity(os.getuid() or 1000, os.getgid() or 1000, 107)
        self.executor = VmExecutor(
            self.checkout,
            self.signatures,
            self.native,
            self.runner,
            runner_image_pin=LOCAL_RUNNER_IMAGE,
        )

    def journal(self) -> dict:
        return json.loads((self.state / JOURNAL_NAME).read_bytes())

    def prepared_receipt(self) -> ExecutionReceipt:
        """Independent pre-start receipt fixture, not a call to the RED prepare stub."""
        credentials = self.directory / "credentials"
        credentials.mkdir(mode=0o700)
        files = []
        for name in ("id_ed25519", "id_ed25519.pub"):
            path = credentials / name
            path.touch(mode=0o600)
            path.write_bytes(b"controlled disposable key fixture")
            files.append(OwnedPath.capture(path))
        artifacts = prepared_artifacts(self.directory, self.request.resources)
        manifest = self.directory / MANIFEST_NAME
        manifest.touch(mode=0o600)
        owner = Ownership(
            TOKEN,
            OwnedPath.capture(self.checkout),
            OwnedPath.capture(self.directory),
            OwnedPath.capture(manifest),
            OwnedPath.capture(credentials),
            tuple(files),
            None,
        )
        # Same independent version-1 fixture format as the planner suite.
        document = {
            "schema": MANIFEST_SCHEMA,
            "token": owner.token,
            "checkout": manifest_path_record(owner.checkout),
            "directory": manifest_path_record(owner.directory),
            "manifest": manifest_path_record(owner.manifest),
            "credential_directory": manifest_path_record(owner.credential_directory),
            "credential_files": [manifest_path_record(path) for path in owner.credential_files],
            "container_id": None,
            "runner_image": self.request.runner_image,
            "guest_sha256": self.request.image.sha256,
            "guest_size_bytes": self.request.image.size_bytes,
            "guest_signer_fingerprint": self.request.image.signer_fingerprint,
            "artifacts": manifest_artifact_record(artifacts),
            "resources": {
                "cpus": 2,
                "memory_mib": 4096,
                "disk_gib": 24,
                "encrypted_disk_gib": 8,
                "boot_timeout_seconds": 180,
            },
            "ssh": {"address": "127.0.0.1", "port": 25022},
        }
        manifest.write_text(json.dumps(document), encoding="utf-8")
        vm = PreparedVm(self.request, owner, artifacts)
        created_paths = (
            owner.credential_directory,
            *owner.credential_files,
            artifacts.os_disk.file,
            artifacts.encrypted_disk.file,
            artifacts.seed.file,
            OwnedPath.capture(manifest),
        )
        journal = self.state / JOURNAL_NAME
        journal.touch(mode=0o600)
        journal.write_text(
            json.dumps(
                {
                    "schema": JOURNAL_SCHEMA,
                    "stage": "prepared",
                    "token": TOKEN,
                    "container_id": None,
                    "directory": {
                        **manifest_path_record(owner.directory),
                        "ancestors": [list(pair) for pair in owner.directory.ancestors],
                        "content_sha256": owner.directory.content_sha256,
                    },
                    "checkout": {
                        **manifest_path_record(owner.checkout),
                        "ancestors": [list(pair) for pair in owner.checkout.ancestors],
                        "content_sha256": owner.checkout.content_sha256,
                    },
                    "created_paths": [
                        {
                            **manifest_path_record(path),
                            "ancestors": [list(pair) for pair in path.ancestors],
                            "content_sha256": path.content_sha256,
                        }
                        for path in created_paths
                    ],
                }
            ),
            encoding="ascii",
        )
        configuration = bounded_configuration(self.directory, self.runner, LOCAL_RUNNER_IMAGE)
        self.native.observation = ContainerObservation(
            CONTAINER_ID,
            TOKEN,
            self.directory,
            False,
            configuration,
            bounded_policy(),
        )
        return ExecutionReceipt(
            self.state_identity,
            OwnedPath.capture(journal),
            TOKEN,
            created_paths,
            vm,
            directory=owner.directory,
            checkout=owner.checkout,
        )

    def large_prepared_receipt(self) -> ExecutionReceipt:
        self.native.os_disk_bytes = b"O" * (128 * 1024)
        self.native.encrypted_disk_bytes = b"D" * (128 * 1024)
        prepared = self.executor.prepare(self.request, state_directory=self.state_identity)
        self.assertEqual(prepared.classification, Classification.PREPARED)
        if prepared.receipt is None or prepared.receipt.prepared is None:
            self.fail("large disks must survive successful public preparation")
        receipt = prepared.receipt
        self.native.observation = ContainerObservation(
            CONTAINER_ID,
            receipt.token,
            self.directory,
            False,
            bounded_configuration(self.directory, self.runner, LOCAL_RUNNER_IMAGE),
            bounded_policy(),
        )
        return receipt

    def overwrite_allocated_byte(self, path: Path) -> None:
        fd = os.open(path, os.O_RDWR | os.O_NOFOLLOW | os.O_CLOEXEC)
        try:
            before = os.fstat(fd)
            offset = 64 * 1024 + 17
            self.assertGreater(before.st_size, offset)
            self.assertGreater(before.st_blocks, 0)
            original = os.pread(fd, 1, offset)
            self.assertEqual(os.pwrite(fd, b"!" if original != b"!" else b"?", offset), 1)
            os.fsync(fd)
            after = os.fstat(fd)
            attributes = ("st_dev", "st_ino", "st_size", "st_blocks", "st_mode", "st_nlink")
            self.assertEqual(
                tuple(getattr(after, name) for name in attributes),
                tuple(getattr(before, name) for name in attributes),
            )
        finally:
            os.close(fd)

    def assert_failed_credential_allocation_retains_scope(self, failure: BaseException) -> None:
        # Preserve leaf inodes while changing both admitted ancestries. A later
        # pathname observation cannot recover the original namespace authority.
        parents = (self.root / "vm-parent", self.root / "checkout-parent")
        for parent in parents:
            parent.mkdir(mode=0o700)
        self.directory.rmdir()
        self.directory = parents[0] / "vm"
        self.directory.mkdir(mode=0o700)
        self.checkout.rename(parents[1] / "checkout")
        self.checkout = parents[1] / "checkout"
        self.request = replace(self.request, directory=self.directory)
        roots = {
            "directory": OwnedPath.capture(self.directory),
            "checkout": OwnedPath.capture(self.checkout),
        }
        pending_at_allocation: list[dict] = []
        synchronizations: list[tuple[str, dict]] = []
        real_fsync = os.fsync

        def replace_ancestries() -> None:
            for parent, leaf in zip(parents, ("vm", "checkout"), strict=True):
                retained = parent.with_name("retained-" + parent.name)
                parent.rename(retained)
                parent.mkdir(mode=0o700)
                (retained / leaf).rename(parent / leaf)

        self.native = FailingCredentialAllocation(failure, replace_ancestries)
        self.executor = VmExecutor(
            self.checkout,
            self.signatures,
            self.native,
            self.runner,
            runner_image_pin=LOCAL_RUNNER_IMAGE,
        )

        def capture_initial_pending() -> None:
            pending_at_allocation.append(self.journal())

        def observe_initial_sync(fd: int) -> None:
            real_fsync(fd)
            observation = os.fstat(fd)
            if (observation.st_dev, observation.st_ino) == (
                self.state_identity.device,
                self.state_identity.inode,
            ):
                kind, document = "directory", self.journal()
            else:
                kind, document = "journal", state_document(fd, self.state)
            if document is not None and document.get("stage") == "credentials-pending":
                synchronizations.append((kind, document))

        self.native.callbacks["create-credentials"] = capture_initial_pending
        result = None
        with patch("os.fsync", side_effect=observe_initial_sync):
            if isinstance(failure, Exception):
                result = self.executor.prepare(self.request, state_directory=self.state_identity)
            else:
                with self.assertRaises(type(failure)) as caught:
                    self.executor.prepare(self.request, state_directory=self.state_identity)
                self.assertIs(caught.exception, failure)

        document = self.journal()
        self.assertEqual(self.native.calls, [("create-credentials", None)])
        self.assertEqual(document["stage"], "credentials-pending")
        self.assertIsNone(document["container_id"])
        self.assertEqual(document["created_paths"], [])
        self.assertEqual(pending_at_allocation, [document])
        self.assertEqual(synchronizations, [("journal", document), ("directory", document)])
        for leaf in ("id_ed25519", "id_ed25519.pub"):
            self.assertTrue((self.directory / "credentials" / leaf).is_file())
        for owned in roots.values():
            current = OwnedPath.capture(owned.path)
            self.assertEqual((current.device, current.inode), (owned.device, owned.inode))
            self.assertNotEqual(current.ancestors, owned.ancestors)

        if result is not None:
            self.assertEqual(result.classification, Classification.CLEANUP_INCOMPLETE)
            if result.receipt is None:
                self.fail("allocation failure must return the original cleanup scope")
            self.assertIsNone(result.receipt.prepared)
            self.assertIsNone(result.receipt.container_id)
            self.assertEqual(result.receipt.state_directory, self.state_identity)
            self.assertIsNotNone(result.receipt.image_evidence)
            self.assertEqual(result.receipt.token, document["token"])
            for name, owned in roots.items():
                with self.subTest(authority="partial-receipt", root=name):
                    self.assertEqual(getattr(result.receipt, name, None), owned)

        for name, owned in roots.items():
            with self.subTest(authority="synchronized-pending-journal", root=name):
                scope = document.get(name)
                if not isinstance(scope, dict):
                    self.fail("initial pending journal lost the independently admitted " + name)
                expected = {
                    **manifest_path_record(owned),
                    "ancestors": [list(pair) for pair in owned.ancestors],
                }
                for key, value in expected.items():
                    self.assertEqual(scope.get(key), value)

    def assert_failed_final_prepared_sync_cannot_start(self, kind: str) -> None:
        real_fsync = os.fsync
        failed_documents: list[dict] = []

        def fail_final_prepared_sync(fd: int) -> None:
            observation = os.fstat(fd)
            is_state = (observation.st_dev, observation.st_ino) == (
                self.state_identity.device,
                self.state_identity.inode,
            )
            document = self.journal() if is_state else state_document(fd, self.state)
            if (
                (is_state if kind == "directory" else not is_state)
                and document is not None
                and document.get("stage") == "prepared"
                and document.get("schema") == JOURNAL_SCHEMA
            ):
                failed_documents.append(document)
                raise OSError("controlled final prepared synchronization failure")
            real_fsync(fd)

        with patch("os.fsync", side_effect=fail_final_prepared_sync):
            failed = self.executor.prepare(self.request, state_directory=self.state_identity)
        self.assertEqual(len(failed_documents), 1)
        self.assertEqual(failed.classification, Classification.CLEANUP_INCOMPLETE)
        if failed.receipt is None:
            self.fail("uncertain preparation must retain cleanup evidence")
        receipt = failed.receipt
        self.assertIsNone(receipt.prepared)
        self.assertEqual(receipt.directory, OwnedPath.capture(self.directory))
        self.assertEqual(receipt.checkout, OwnedPath.capture(self.checkout))
        self.assertEqual(receipt.state_directory, self.state_identity)
        self.assertEqual(receipt.token, failed_documents[0]["token"])
        self.assertIsNone(receipt.container_id)
        self.assertIsNotNone(receipt.image_evidence)
        self.assertEqual(len(receipt.created_paths), 7)
        for owned in (receipt.journal, *receipt.created_paths):
            current = OwnedPath.capture(owned.path)
            self.assertEqual(current, owned)
            self.assertEqual(current.content_sha256, owned.content_sha256)
        self.assertEqual(self.journal(), failed_documents[0])
        self.assertNotIn("create-container", [name for name, _ in self.native.calls])

        # Remove the fault and supply valid exact-ID native facts. Repeating a
        # synchronization in start is not recovery of failed preparation.
        self.native.observation = ContainerObservation(
            CONTAINER_ID,
            receipt.token,
            self.directory,
            False,
            bounded_configuration(self.directory, self.runner, LOCAL_RUNNER_IMAGE),
            bounded_policy(),
        )
        calls_before_start = list(self.native.calls)
        started = self.executor.start(receipt)
        with self.subTest(authority="failed-preparation-receipt"):
            self.assertEqual(started.classification, Classification.INVALID_STATE)
        with self.subTest(effect="native-allocation"):
            self.assertEqual(self.native.calls, calls_before_start)
        with self.subTest(evidence="cleanup-journal"):
            self.assertEqual(self.journal(), failed_documents[0])

    def competing_starts(self, *, pause_racing_create: bool) -> tuple[dict[str, list[tuple]], dict]:
        prepared = self.executor.prepare(self.request, state_directory=self.state_identity)
        self.assertEqual(prepared.classification, Classification.PREPARED)
        if prepared.receipt is None:
            self.fail("concurrent starts need a successful public preparation receipt")
        context = multiprocessing.get_context("spawn")
        release_pending, release_create = context.Event(), context.Event()
        messages: dict[str, Connection | PipeConnection] = {}
        senders: list[Connection | PipeConnection] = []
        processes = {}
        events: dict[str, list[tuple]] = {"paused": [], "racing": []}
        exit_codes = {}
        for name, container_id in (("paused", "a" * 64), ("racing", "b" * 64)):
            receive, send = context.Pipe(duplex=False)
            messages[name] = receive
            senders.append(send)
            processes[name] = context.Process(
                target=start_in_process,
                args=(
                    self.checkout,
                    self.runner,
                    prepared.receipt,
                    container_id,
                    send,
                    release_pending,
                    release_create,
                ),
                kwargs={
                    "pause_pending": name == "paused",
                    "pause_create": name == "racing" and pause_racing_create,
                },
                daemon=True,
            )

        def wait_for(name: str, kinds: set[str]) -> None:
            if any(event[0] in kinds for event in events[name]):
                return
            deadline = time.monotonic() + _PROCESS_WAIT_SECONDS
            while time.monotonic() < deadline:
                if messages[name].poll(min(0.05, max(0, deadline - time.monotonic()))):
                    try:
                        event = messages[name].recv()
                    except EOFError:
                        return
                    events[name].append(event)
                    if event[0] in kinds:
                        return
                elif not processes[name].is_alive():
                    return

        try:
            processes["paused"].start()
            senders[0].close()
            wait_for("paused", {"pending-write", "result", "error"})
            processes["racing"].start()
            senders[1].close()
            # Under serialization the racing invocation blocks or rejects. The
            # timeout releases the owner instead of requiring an impossible
            # two-party barrier inside its future descriptor-held claim.
            racing_boundary = (
                {"allocated", "result", "error"} if pause_racing_create else {"result", "error"}
            )
            wait_for("racing", racing_boundary)
            release_pending.set()
            wait_for("paused", {"result", "error"})
            release_create.set()
            wait_for("racing", {"result", "error"})
        finally:
            release_pending.set()
            release_create.set()
            for name, process in processes.items():
                if process.pid is not None:
                    process.join(_PROCESS_WAIT_SECONDS)
                    if process.is_alive():
                        process.terminate()
                        process.join(2)
                    if process.is_alive():
                        process.kill()
                        process.join(2)
                    exit_codes[name] = process.exitcode
            for name, connection in messages.items():
                while connection.poll():
                    try:
                        events[name].append(connection.recv())
                    except EOFError:
                        break
                connection.close()
            for sender in senders:
                sender.close()
            for process in processes.values():
                if process.pid is not None and not process.is_alive():
                    process.close()
        self.assertEqual(exit_codes, {"paused": 0, "racing": 0})
        for name, observed in events.items():
            self.assertEqual([event for event in observed if event[0] == "error"], [], name)
            self.assertEqual(len([event for event in observed if event[0] == "result"]), 1, name)
        return events, self.journal()

    def assert_one_durably_started_container(
        self, events: dict[str, list[tuple]], document: dict
    ) -> None:
        allocations = [
            event[1]
            for observed in events.values()
            for event in observed
            if event[0] == "allocated"
        ]
        self.assertEqual(len(allocations), 1, "one prepared namespace must allocate at most once")
        self.assertEqual(
            document["container_id"],
            allocations[0],
            "a stale invocation erased the durable exact ID",
        )
        self.assertEqual(document["stage"], "started")
        starts = [
            event[1]
            for observed in events.values()
            for event in observed
            if event[0] == "native-start"
        ]
        self.assertEqual(starts, allocations)
        winners = [
            event[2]
            for observed in events.values()
            for event in observed
            if event[0] == "result" and event[1] == Classification.STARTED
        ]
        self.assertEqual(winners, allocations)

    def test_competing_start_cannot_erase_a_durable_exact_id(self):
        events, document = self.competing_starts(pause_racing_create=False)
        self.assert_one_durably_started_container(events, document)

    def test_competing_start_cannot_allocate_twice_from_one_prepared_receipt(self):
        events, document = self.competing_starts(pause_racing_create=True)
        self.assert_one_durably_started_container(events, document)

    def test_partial_credential_allocation_retains_original_roots_in_receipt_and_synced_journal(
        self,
    ):
        self.assert_failed_credential_allocation_retains_scope(
            OSError("controlled credential allocation failure")
        )

    def test_interrupted_credential_allocation_retains_original_roots_in_synced_journal(self):
        for failure in (KeyboardInterrupt(), SystemExit(7)):
            with self.subTest(exception=type(failure).__name__):
                self.new_fixture()
                self.assert_failed_credential_allocation_retains_scope(failure)

    def test_failed_final_prepared_journal_file_sync_receipt_cannot_start(self):
        self.assert_failed_final_prepared_sync_cannot_start("journal")

    def test_failed_final_prepared_directory_sync_receipt_cannot_start(self):
        self.assert_failed_final_prepared_sync_cannot_start("directory")

    def test_start_rejects_large_prepared_disk_byte_changes_before_container_creation(self):
        for name in ("os_disk", "encrypted_disk"):
            with self.subTest(changed_disk=name):
                self.new_fixture()
                receipt = self.large_prepared_receipt()
                if receipt.prepared is None or receipt.prepared.artifacts is None:
                    self.fail("preparation must retain both large disks")
                disk = getattr(receipt.prepared.artifacts, name)
                original_digest = hashlib.sha256(disk.file.path.read_bytes()).hexdigest()
                self.overwrite_allocated_byte(disk.file.path)
                self.assertNotEqual(
                    hashlib.sha256(disk.file.path.read_bytes()).hexdigest(), original_digest
                )
                fd = os.open(disk.file.path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
                try:
                    self.assertEqual(self.native.inspect_disk(fd, disk.file), disk)
                finally:
                    os.close(fd)
                self.native.calls.clear()
                result = self.executor.start(receipt)
                with self.subTest(admission="changed-prepared-bytes"):
                    self.assertEqual(result.classification, Classification.OWNERSHIP_MISMATCH)
                with self.subTest(effect="native-allocation"):
                    self.assertNotIn(
                        "create-container", [operation for operation, _ in self.native.calls]
                    )
                    self.assertNotIn(
                        "start-container", [operation for operation, _ in self.native.calls]
                    )
                with self.subTest(evidence="retained-digest"):
                    self.assertEqual(disk.file.content_sha256, original_digest)

    def test_large_prepared_disk_pins_apply_until_final_admission_but_allow_runtime_writes(self):
        for boundary in ("inspect", "start-container"):
            with self.subTest(mutation_boundary=boundary):
                self.new_fixture()
                receipt = self.large_prepared_receipt()
                if receipt.prepared is None or receipt.prepared.artifacts is None:
                    self.fail("preparation must retain both large disks")
                artifacts = receipt.prepared.artifacts
                disks = (artifacts.os_disk, artifacts.encrypted_disk)
                original_digests = tuple(
                    hashlib.sha256(disk.file.path.read_bytes()).hexdigest() for disk in disks
                )

                def mutate_disks() -> None:
                    for disk in disks:
                        self.overwrite_allocated_byte(disk.file.path)

                self.native.callbacks[boundary] = mutate_disks
                result = self.executor.start(receipt)
                expected = (
                    Classification.STARTED
                    if boundary == "start-container"
                    else Classification.OWNERSHIP_MISMATCH
                )
                self.assertEqual(result.classification, expected)
                starts = [call for call in self.native.calls if call[0] == "start-container"]
                self.assertEqual(
                    starts,
                    [("start-container", CONTAINER_ID)] if boundary == "start-container" else [],
                )
                if result.receipt is None:
                    self.fail("post-create admission must retain its exact ID")
                self.assertEqual(result.receipt.container_id, CONTAINER_ID)
                self.assertEqual(self.journal()["container_id"], CONTAINER_ID)
                for disk, digest in zip(disks, original_digests, strict=True):
                    self.assertNotEqual(
                        hashlib.sha256(disk.file.path.read_bytes()).hexdigest(), digest
                    )
                    with self.subTest(retained_disk=disk.file.path.name):
                        self.assertEqual(disk.file.content_sha256, digest)

    def test_prepare_serializes_one_vm_namespace_even_with_independent_journals(self):
        competing_state = self.root / "competing-state"
        competing_state.mkdir(mode=0o700)
        competing_identity = OwnedPath.capture(competing_state)
        competing_native = ControlledVmNative()
        competing_executor = VmExecutor(
            self.checkout,
            ControlledSignatures(),
            competing_native,
            self.runner,
            runner_image_pin=LOCAL_RUNNER_IMAGE,
        )
        competing_results = []

        def compete_before_first_allocation() -> None:
            competing_results.append(
                competing_executor.prepare(
                    self.request,
                    state_directory=competing_identity,
                )
            )

        self.native.callbacks["create-credentials"] = compete_before_first_allocation
        prepared = self.executor.prepare(self.request, state_directory=self.state_identity)
        self.assertEqual(prepared.classification, Classification.PREPARED)
        self.assertEqual(len(competing_results), 1)
        self.assertEqual(competing_results[0].classification, Classification.OWNERSHIP_MISMATCH)
        self.assertIsNone(competing_results[0].receipt)
        self.assertEqual(competing_native.calls, [])
        self.assertEqual(list(competing_state.iterdir()), [])
        self.assertEqual(OwnedPath.capture(competing_state), competing_identity)
        self.assertEqual(self.journal()["stage"], "prepared")

    def test_rejected_image_admission_releases_namespaces_without_creating_lock_files(self):
        self.signatures.fingerprint = "2" * 40
        rejected = self.executor.prepare(self.request, state_directory=self.state_identity)
        self.assertEqual(rejected.classification, Classification.UNVERIFIED_IMAGE)
        self.assertIsNone(rejected.receipt)
        self.assertEqual(self.native.calls, [])
        self.assertEqual(list(self.directory.iterdir()), [])
        self.assertEqual(list(self.state.iterdir()), [])
        self.signatures.fingerprint = SIGNER
        prepared = self.executor.prepare(self.request, state_directory=self.state_identity)
        self.assertEqual(prepared.classification, Classification.PREPARED)
        self.assertEqual(set(path.name for path in self.state.iterdir()), {JOURNAL_NAME})

    def test_both_prepared_disk_digests_stream_all_chunks_with_bounded_reads(self):
        self.native.os_disk_bytes = b"O" * (2 * 1024 * 1024 + 19)
        self.native.encrypted_disk_bytes = b"D" * (2 * 1024 * 1024 + 31)
        fixtures = {
            "os.qcow2": self.native.os_disk_bytes,
            "encrypted.qcow2": self.native.encrypted_disk_bytes,
        }
        names_by_size = {len(data): name for name, data in fixtures.items()}
        reads: dict[str, list[int]] = {name: [] for name in fixtures}
        real_read = os.read

        def observe_read(fd: int, maximum: int) -> bytes:
            name = names_by_size.get(os.fstat(fd).st_size)
            if name is not None:
                reads[name].append(maximum)
            return real_read(fd, maximum)

        with patch("os.read", side_effect=observe_read):
            prepared = self.executor.prepare(self.request, state_directory=self.state_identity)
        self.assertEqual(prepared.classification, Classification.PREPARED)
        if prepared.receipt is None or prepared.receipt.prepared is None:
            self.fail("streamed disk pins must survive successful public preparation")
        vm = prepared.receipt.prepared
        if vm.artifacts is None:
            self.fail("preparation must retain both streamed disk digests")
        for disk in (vm.artifacts.os_disk, vm.artifacts.encrypted_disk):
            name = disk.file.path.name
            self.assertEqual(disk.file.content_sha256, hashlib.sha256(fixtures[name]).hexdigest())
            self.assertGreaterEqual(len(reads[name]), 3)
            self.assertTrue(all(0 < size <= 1024 * 1024 for size in reads[name]))

        # A tail byte beyond the first two chunks is still preparation authority.
        path = vm.artifacts.os_disk.file.path
        fd = os.open(path, os.O_RDWR | os.O_NOFOLLOW | os.O_CLOEXEC)
        try:
            self.assertEqual(os.pwrite(fd, b"!", os.fstat(fd).st_size - 1), 1)
            os.fsync(fd)
        finally:
            os.close(fd)
        self.native.calls.clear()
        result = self.executor.start(prepared.receipt)
        self.assertEqual(result.classification, Classification.OWNERSHIP_MISMATCH)
        self.assertEqual(self.native.calls, [])

    def test_start_rejects_large_disk_changes_from_each_precreate_observer(self):
        for boundary in ("signature", "inspect-disk", "inspect-seed"):
            with self.subTest(observer=boundary):
                self.new_fixture()
                receipt = self.large_prepared_receipt()
                if receipt.prepared is None or receipt.prepared.artifacts is None:
                    self.fail("observer admission requires retained large disks")
                artifacts = receipt.prepared.artifacts

                def mutate_disks() -> None:
                    for disk in (artifacts.os_disk, artifacts.encrypted_disk):
                        self.overwrite_allocated_byte(disk.file.path)

                if boundary == "signature":
                    self.signatures.on_read = mutate_disks
                else:
                    self.native.callbacks[boundary] = mutate_disks
                self.native.calls.clear()
                result = self.executor.start(receipt)
                self.assertEqual(result.classification, Classification.OWNERSHIP_MISMATCH)
                self.assertNotIn("create-container", [name for name, _ in self.native.calls])
                self.assertNotIn("start-container", [name for name, _ in self.native.calls])
                self.assertEqual(self.journal()["stage"], "prepared")
                self.assertIsNone(self.journal()["container_id"])

    def test_prepare_rejects_unadmitted_inputs_before_any_native_effect(self):
        for changed, classification in (
            (replace(self.request, runner_image="qemu:owned"), Classification.UNVERIFIED_IMAGE),
            (
                replace(self.request, runner_image="sha256:" + "e" * 64),
                Classification.UNVERIFIED_IMAGE,
            ),
            (
                replace(
                    self.request,
                    image=replace(self.request.image, size_bytes=len(self.image_bytes) + 1),
                ),
                Classification.UNVERIFIED_IMAGE,
            ),
            (
                replace(self.request, image=replace(self.request.image, sha256="f" * 64)),
                Classification.UNVERIFIED_IMAGE,
            ),
            (
                replace(
                    self.request, image=replace(self.request.image, signer_fingerprint="2" * 40)
                ),
                Classification.UNVERIFIED_IMAGE,
            ),
            (replace(self.request, resources=Resources(cpus=3)), Classification.INVALID_RESOURCES),
            (replace(self.request, directory=self.checkout), Classification.UNSAFE_PATH),
        ):
            with self.subTest(classification=classification.value):
                result = self.executor.prepare(changed, state_directory=self.state_identity)
                self.assertEqual(result.classification, classification)
                self.assertIsNone(result.receipt)
                self.assertEqual(self.native.calls, [])
                self.assertEqual(list(self.directory.iterdir()), [])
                self.assertEqual(list(self.state.iterdir()), [])

    def test_prepare_authenticates_held_bytes_and_journals_real_artifacts_outside_the_bind(self):
        result = self.executor.prepare(self.request, state_directory=self.state_identity)
        self.assertEqual(result.phase, Phase.PREPARE)
        self.assertEqual(result.classification, Classification.PREPARED)
        if result.receipt is None or result.receipt.prepared is None:
            self.fail("successful preparation must retain independently owned real artifacts")
        receipt = result.receipt
        vm = receipt.prepared
        assert vm is not None
        self.assertIsNotNone(vm.artifacts)
        self.assertRegex(receipt.token, r"\A[0-9a-f]{64}\Z")
        self.assertIsNone(receipt.container_id)
        self.assertEqual(receipt.state_directory, self.state_identity)
        self.assertFalse(receipt.journal.path.is_relative_to(self.directory))
        self.assertFalse(receipt.journal.path.is_relative_to(self.checkout))
        self.assertLessEqual(receipt.journal.path.stat().st_size, 64 * 1024)
        self.assertEqual(stat.S_IMODE(receipt.journal.path.stat().st_mode), 0o600)
        self.assertEqual(self.journal()["schema"], JOURNAL_SCHEMA)
        self.assertEqual(self.journal()["stage"], "prepared")
        self.assertEqual(self.journal()["token"], receipt.token)
        self.assertIsNone(self.journal()["container_id"])
        self.assertEqual(self.native.image_reads, [self.image_bytes])
        self.assertEqual(
            self.signatures.reads,
            [
                (
                    self.request.image.checksums.read_bytes(),
                    self.request.image.signature.read_bytes(),
                )
            ],
        )
        for retained in receipt.created_paths:
            self.assertEqual(OwnedPath.capture(retained.path), retained)
        self.assertNotIn("create-container", [name for name, _ in self.native.calls])

    def test_prepare_revalidates_checkout_exclusion_after_native_callbacks(self):
        self.native.callbacks["create-credentials"] = lambda: (self.root / ".git").touch(mode=0o600)
        result = self.executor.prepare(self.request, state_directory=self.state_identity)
        self.assertIn(
            result.classification,
            (
                Classification.UNSAFE_PATH,
                Classification.OWNERSHIP_MISMATCH,
                Classification.CLEANUP_INCOMPLETE,
            ),
        )
        self.assertEqual(self.native.calls, [("create-credentials", None)])
        self.assertFalse((self.directory / "os.qcow2").exists())
        self.assertFalse((self.directory / "seed.iso").exists())

    def test_partial_prepare_keeps_an_explicit_incomplete_receipt_without_inventing_prepared_state(
        self,
    ):
        foreign = self.directory / "foreign-object"

        def insert_foreign_object() -> None:
            foreign.write_bytes(b"foreign sentinel")

        self.native.callbacks["create-encrypted-disk"] = insert_foreign_object
        self.native.errors["create-encrypted-disk"] = OSError("sensitive native failure")
        result = self.executor.prepare(self.request, state_directory=self.state_identity)
        self.assertEqual(result.classification, Classification.CLEANUP_INCOMPLETE)
        if result.receipt is None:
            self.fail("partial preparation must retain only independently recorded ownership")
        self.assertIsNone(result.receipt.prepared)
        self.assertIsNone(result.receipt.container_id)
        self.assertNotIn(foreign, [owned.path for owned in result.receipt.created_paths])
        self.assertEqual(foreign.read_bytes(), b"foreign sentinel")
        self.assertNotIn("create-seed", [name for name, _ in self.native.calls])
        self.assertNotIn("create-container", [name for name, _ in self.native.calls])

    def test_start_persists_and_syncs_the_exact_created_id_before_inspection_and_start(self):
        receipt = self.prepared_receipt()
        journal = self.state / JOURNAL_NAME
        synchronizations: list[tuple[str, str | None]] = []
        real_fsync = os.fsync

        def observe_fsync(fd: int) -> None:
            real_fsync(fd)
            observation = os.fstat(fd)
            if (observation.st_dev, observation.st_ino) == (
                self.state_identity.device,
                self.state_identity.inode,
            ):
                synchronizations.append(("directory", self.journal()["container_id"]))
            else:
                document = state_document(fd, self.state)
                if (
                    document is not None
                    and document.get("schema") == JOURNAL_SCHEMA
                    and document.get("token") == TOKEN
                    and "container_id" in document
                ):
                    synchronizations.append(("journal", document["container_id"]))

        def assert_pending_create() -> None:
            self.assertEqual(self.journal()["stage"], "create-pending")
            self.assertIsNone(self.journal()["container_id"])
            self.assertEqual(synchronizations[-2:], [("journal", None), ("directory", None)])

        def assert_durable_created_id() -> None:
            document = self.journal()
            self.assertEqual(document["schema"], JOURNAL_SCHEMA)
            self.assertEqual(document["stage"], "created")
            self.assertEqual(document["token"], TOKEN)
            self.assertEqual(document["container_id"], CONTAINER_ID)
            self.assertLessEqual(journal.stat().st_size, 64 * 1024)
            self.assertEqual(stat.S_IMODE(journal.stat().st_mode), 0o600)
            self.assertEqual(
                [entry for entry in synchronizations if entry[1] == CONTAINER_ID][-2:],
                [("journal", CONTAINER_ID), ("directory", CONTAINER_ID)],
            )

        self.native.callbacks["create-container"] = assert_pending_create
        self.native.callbacks["inspect"] = assert_durable_created_id
        self.native.callbacks["start-container"] = assert_durable_created_id
        with patch("os.fsync", side_effect=observe_fsync):
            result = self.executor.start(receipt)
        self.assertEqual(result.classification, Classification.STARTED)
        self.assertEqual(result.phase, Phase.START)
        container_calls = [
            call
            for call in self.native.calls
            if call[0]
            in (
                "create-container",
                "inspect",
                "start-container",
                "stop-container",
                "remove-container",
            )
        ]
        self.assertEqual(
            container_calls,
            [
                ("create-container", None),
                ("inspect", CONTAINER_ID),
                ("start-container", CONTAINER_ID),
            ],
        )
        if result.receipt is None or result.receipt.prepared is None:
            self.fail("started execution must retain its exact container ID")
        self.assertEqual(result.receipt.container_id, CONTAINER_ID)
        self.assertEqual(result.receipt.prepared.ownership.container_id, CONTAINER_ID)
        self.assertIsNone(receipt.container_id)
        self.assertEqual(self.journal()["container_id"], CONTAINER_ID)
        self.assertEqual(self.journal()["stage"], "started")

    def test_failed_created_id_journal_sync_never_starts_the_object(self):
        receipt = self.prepared_receipt()
        real_fsync = os.fsync

        def fail_created_record_sync(fd: int) -> None:
            document = state_document(fd, self.state)
            if document is not None and document.get("container_id") == CONTAINER_ID:
                raise OSError("sensitive journal failure")
            real_fsync(fd)

        with patch("os.fsync", side_effect=fail_created_record_sync):
            result = self.executor.start(receipt)
        self.assertEqual(result.classification, Classification.CLEANUP_INCOMPLETE)
        self.assertIn(("create-container", None), self.native.calls)
        self.assertNotIn("start-container", [name for name, _ in self.native.calls])
        if result.receipt is None:
            self.fail("a known returned ID must remain retained even when journaling fails")
        self.assertEqual(result.receipt.container_id, CONTAINER_ID)
        self.assertNotIn("sensitive journal failure", repr(result))

    def test_start_requires_independent_receipt_and_guest_pins_before_create(self):
        receipt = self.prepared_receipt()
        if receipt.prepared is None:
            self.fail("fixture must retain prepared VM authority")
        bad_request = replace(
            receipt.prepared.request,
            image=replace(receipt.prepared.request.image, size_bytes=len(self.image_bytes) + 1),
        )
        for changed, classification in (
            (replace(receipt, token="f" * 64), Classification.OWNERSHIP_MISMATCH),
            (
                replace(receipt, prepared=replace(receipt.prepared, request=bad_request)),
                Classification.UNVERIFIED_IMAGE,
            ),
        ):
            with self.subTest(classification=classification.value):
                result = self.executor.start(changed)
                self.assertEqual(result.classification, classification)
                self.assertEqual(self.native.calls, [])
                self.assertIsNone(self.journal()["container_id"])
        unpinned = VmExecutor(self.checkout, self.signatures, self.native, self.runner)
        self.assertEqual(unpinned.start(receipt).classification, Classification.UNVERIFIED_IMAGE)
        original_document = (self.state / JOURNAL_NAME).read_bytes()
        self.state.rename(self.root / "retained-state")
        self.state.mkdir(mode=0o700)
        replacement = self.state / JOURNAL_NAME
        replacement.touch(mode=0o600)
        replacement.write_bytes(original_document)
        foreign = self.state / "foreign-object"
        foreign.write_bytes(b"foreign state sentinel")
        result = self.executor.start(receipt)
        self.assertEqual(result.classification, Classification.OWNERSHIP_MISMATCH)
        self.assertEqual(self.native.calls, [])
        self.assertEqual(foreign.read_bytes(), b"foreign state sentinel")

    def test_unknown_or_malformed_create_result_stays_uncertain_without_adopting_ownership(self):
        for returned_id in (None, "owned-name", "c" * 12, "C" * 64, CONTAINER_ID + "\n"):
            with self.subTest(returned_id=returned_id):
                self.new_fixture()
                receipt = self.prepared_receipt()
                self.native.create_result = returned_id
                result = self.executor.start(receipt)
                self.assertEqual(result.classification, Classification.CREATION_UNCERTAIN)
                container_calls = [
                    call
                    for call in self.native.calls
                    if call[0]
                    in (
                        "create-container",
                        "inspect",
                        "start-container",
                        "stop-container",
                        "remove-container",
                    )
                ]
                self.assertEqual(container_calls, [("create-container", None)])
                if result.receipt is None:
                    self.fail("uncertain create must retain pending intent, not an adopted object")
                self.assertIsNone(result.receipt.container_id)
                self.assertIsNone(self.journal()["container_id"])
                self.assertEqual(self.journal()["stage"], "create-pending")
                if result.receipt.prepared is not None:
                    self.assertIsNone(result.receipt.prepared.ownership.container_id)
                self.assertTrue((self.directory / "seed.iso").is_file())
                calls_after_uncertain_create = list(self.native.calls)
                pending_document = self.journal()
                for retry_receipt in (receipt, result.receipt):
                    with self.subTest(
                        retry="original" if retry_receipt is receipt else "uncertain"
                    ):
                        retry = self.executor.start(retry_receipt)
                        self.assertEqual(retry.classification, Classification.OWNERSHIP_MISMATCH)
                        self.assertEqual(self.native.calls, calls_after_uncertain_create)
                        self.assertEqual(self.journal(), pending_document)

    def test_start_revalidates_retained_authority_after_create_and_inspection_callbacks(self):
        for boundary in ("create-container", "inspect"):
            for mutation in (
                "key",
                "disk",
                "seed",
                "download-permissions",
                "download-ancestor-permissions",
                "checkout-marker",
            ):
                with self.subTest(boundary=boundary, mutation=mutation):
                    self.new_fixture()
                    receipt = self.prepared_receipt()
                    if receipt.prepared is None or receipt.prepared.artifacts is None:
                        self.fail("fixture must retain keys, disks and seed")
                    vm = receipt.prepared
                    artifacts = vm.artifacts
                    assert artifacts is not None

                    def substitute() -> None:
                        if mutation == "key":
                            vm.ownership.credential_files[0].path.write_bytes(
                                b"changed key fixture"
                            )
                        elif mutation == "disk":
                            artifacts.os_disk.file.path.write_bytes(b"changed disk fixture")
                        elif mutation == "seed":
                            seed = self.directory / "seed.iso"
                            original = seed.read_bytes()
                            seed.rename(self.directory / "retained-seed.iso")
                            seed.touch(mode=0o600)
                            seed.write_bytes(original)
                        elif mutation == "download-permissions":
                            parent = self.request.image.path.parent
                            parent.chmod(0o777)
                            parent.chmod(0o700)
                        elif mutation == "download-ancestor-permissions":
                            self.downloads.chmod(0o777)
                            self.downloads.chmod(0o700)
                        else:
                            # Leaf identities stay unchanged; final checkout exclusion must rerun.
                            (self.downloads / ".git").touch(mode=0o600)

                    self.native.callbacks[boundary] = substitute
                    result = self.executor.start(receipt)
                    self.assertIn(
                        result.classification,
                        (
                            Classification.OWNERSHIP_MISMATCH,
                            Classification.UNVERIFIED_IMAGE,
                            Classification.UNSAFE_PATH,
                            Classification.CLEANUP_INCOMPLETE,
                        ),
                    )
                    self.assertNotIn("start-container", [name for name, _ in self.native.calls])
                    if result.receipt is None:
                        self.fail("a returned exact create ID must remain independently retained")
                    self.assertEqual(result.receipt.container_id, CONTAINER_ID)
                    self.assertEqual(self.journal()["container_id"], CONTAINER_ID)
                    for operation, container_id in self.native.calls:
                        if operation in ("stop-container", "remove-container"):
                            self.assertEqual(container_id, CONTAINER_ID)

    def test_created_container_must_match_exact_owner_image_and_complete_runner_policy(self):
        for mutation in ("container-id", "owner", "mount", "image-id", "healthcheck", "running"):
            with self.subTest(mutation=mutation):
                self.new_fixture()
                receipt = self.prepared_receipt()
                observation = self.native.observation
                if observation is None or observation.policy is None:
                    self.fail("fixture must supply complete native inspection facts")
                if mutation == "container-id":
                    observation = replace(observation, container_id="f" * 64)
                elif mutation == "owner":
                    observation = replace(observation, owner_token="f" * 64)
                elif mutation == "mount":
                    observation = replace(observation, mounted_directory=self.checkout)
                elif mutation == "image-id":
                    observation = replace(
                        observation,
                        policy=replace(
                            observation.policy,
                            image_id="sha256:" + "e" * 64,
                        ),
                    )
                elif mutation == "healthcheck":
                    observation = replace(
                        observation,
                        policy=replace(
                            observation.policy,
                            healthcheck=("CMD-SHELL", "foreign-command"),
                        ),
                    )
                else:
                    observation = replace(observation, running=True)
                self.native.observation = observation
                result = self.executor.start(receipt)
                self.assertIn(
                    result.classification,
                    (
                        Classification.OWNERSHIP_MISMATCH,
                        Classification.INVALID_STATE,
                    ),
                )
                self.assertNotIn("start-container", [name for name, _ in self.native.calls])
                self.assertEqual(self.journal()["container_id"], CONTAINER_ID)

    def test_native_create_errors_are_uncertain_and_never_expose_sensitive_output(self):
        receipt = self.prepared_receipt()
        sensitive = f"native stderr {self.root} credential=fixture-secret " + "x" * (128 * 1024)
        self.native.errors["create-container"] = RuntimeError(sensitive)
        output = io.StringIO()
        with redirect_stdout(output), redirect_stderr(output):
            result = self.executor.start(receipt)
        self.assertEqual(result.classification, Classification.CREATION_UNCERTAIN)
        self.assertEqual(output.getvalue(), "")
        self.assertLess(len(repr(result)), 256)
        for value in (str(self.root), TOKEN, "fixture-secret", "native stderr"):
            self.assertNotIn(value, repr(result))
        self.assertNotIn("start-container", [name for name, _ in self.native.calls])

    def test_prepare_syncs_each_intent_before_native_allocation(self):
        real_fsync = os.fsync
        synchronizations: list[tuple[str, str]] = []

        def observe_fsync(fd: int) -> None:
            real_fsync(fd)
            observation = os.fstat(fd)
            if (observation.st_dev, observation.st_ino) == (
                self.state_identity.device,
                self.state_identity.inode,
            ):
                synchronizations.append(("directory", self.journal()["stage"]))
            else:
                document = state_document(fd, self.state)
                if document is not None and document.get("schema") == JOURNAL_SCHEMA:
                    synchronizations.append(("journal", document["stage"]))

        def require_intent(stage: str) -> None:
            self.assertEqual(self.journal()["stage"], stage)
            self.assertEqual(synchronizations[-2:], [("journal", stage), ("directory", stage)])

        for operation, stage in (
            ("create-credentials", "credentials-pending"),
            ("create-os-disk", "os-disk-pending"),
            ("create-encrypted-disk", "data-disk-pending"),
            ("create-seed", "seed-pending"),
        ):
            self.native.callbacks[operation] = lambda stage=stage: require_intent(stage)
        with patch("os.fsync", side_effect=observe_fsync):
            result = self.executor.prepare(self.request, state_directory=self.state_identity)
        self.assertEqual(result.classification, Classification.PREPARED)
        self.assertEqual(self.journal()["stage"], "prepared")

    def test_prepared_receipt_starts_without_adopting_manifest_authority(self):
        prepared = self.executor.prepare(self.request, state_directory=self.state_identity)
        self.assertEqual(prepared.classification, Classification.PREPARED)
        if prepared.receipt is None or prepared.receipt.prepared is None:
            self.fail("prepare must return independent authority for start")
        receipt = prepared.receipt
        self.native.observation = ContainerObservation(
            CONTAINER_ID,
            receipt.token,
            self.directory,
            False,
            bounded_configuration(self.directory, self.runner, LOCAL_RUNNER_IMAGE),
            bounded_policy(),
        )
        result = self.executor.start(receipt)
        self.assertEqual(result.classification, Classification.STARTED)
        if result.receipt is None or result.receipt.prepared is None:
            self.fail("start must retain the returned exact ID and original prepared authority")
        self.assertEqual(result.receipt.prepared.ownership.container_id, CONTAINER_ID)
        self.assertEqual(result.receipt.image_evidence, receipt.image_evidence)
        self.assertEqual(self.journal()["stage"], "started")
        self.assertEqual(self.journal()["container_id"], CONTAINER_ID)
        self.assertEqual(self.native.image_reads, [self.image_bytes])
        self.assertIsNone(receipt.container_id)
        self.assertNotIn("stop-container", [name for name, _ in self.native.calls])
        self.assertNotIn("remove-container", [name for name, _ in self.native.calls])

    def test_failed_created_id_directory_sync_never_starts_the_object(self):
        receipt = self.prepared_receipt()
        real_fsync = os.fsync

        def fail_created_directory_sync(fd: int) -> None:
            observation = os.fstat(fd)
            if (observation.st_dev, observation.st_ino) == (
                self.state_identity.device,
                self.state_identity.inode,
            ) and self.journal()["container_id"] == CONTAINER_ID:
                raise OSError("sensitive directory synchronization failure")
            real_fsync(fd)

        with patch("os.fsync", side_effect=fail_created_directory_sync):
            result = self.executor.start(receipt)
        self.assertEqual(result.classification, Classification.CLEANUP_INCOMPLETE)
        if result.receipt is None:
            self.fail("a known returned ID must survive uncertain directory durability")
        self.assertEqual(result.receipt.container_id, CONTAINER_ID)
        self.assertNotIn("start-container", [name for name, _ in self.native.calls])
        self.assertNotIn("inspect", [name for name, _ in self.native.calls])

    def test_start_rejects_state_and_bind_replacement_after_native_callbacks(self):
        for boundary in ("create-container", "inspect"):
            for target in ("state", "bind"):
                with self.subTest(boundary=boundary, target=target):
                    self.new_fixture()
                    receipt = self.prepared_receipt()
                    path = self.state if target == "state" else self.directory
                    saved = path.with_name("retained-" + path.name)
                    foreign = path / "foreign-object"

                    def replace_root() -> None:
                        path.rename(saved)
                        path.mkdir(mode=0o700)
                        foreign.write_bytes(b"foreign sentinel")

                    self.native.callbacks[boundary] = replace_root
                    result = self.executor.start(receipt)
                    self.assertIn(
                        result.classification,
                        (
                            Classification.OWNERSHIP_MISMATCH,
                            Classification.CLEANUP_INCOMPLETE,
                        ),
                    )
                    if result.receipt is None:
                        self.fail(
                            "exact created ID must remain retained despite lost path authority"
                        )
                    self.assertEqual(result.receipt.container_id, CONTAINER_ID)
                    self.assertEqual(foreign.read_bytes(), b"foreign sentinel")
                    self.assertNotIn("start-container", [name for name, _ in self.native.calls])
                    self.assertNotIn("remove-container", [name for name, _ in self.native.calls])

    def test_failed_or_interrupted_start_never_claims_cancelled_native_effects(self):
        for failure in (
            RuntimeError("sensitive start failure"),
            KeyboardInterrupt(),
            SystemExit(9),
        ):
            with self.subTest(exception=type(failure).__name__):
                self.new_fixture()
                receipt = self.prepared_receipt()
                self.native.errors["start-container"] = failure
                if isinstance(failure, Exception):
                    result = self.executor.start(receipt)
                    self.assertEqual(result.classification, Classification.CLEANUP_INCOMPLETE)
                    if result.receipt is None:
                        self.fail("failed start must retain its exact possibly running container")
                    self.assertEqual(result.receipt.container_id, CONTAINER_ID)
                    self.assertNotIn("sensitive start failure", repr(result))
                else:
                    with self.assertRaises(type(failure)) as caught:
                        self.executor.start(receipt)
                    self.assertIs(caught.exception, failure)
                self.assertIn(("start-container", CONTAINER_ID), self.native.calls)
                self.assertEqual(self.journal()["stage"], "created")
                self.assertEqual(self.journal()["container_id"], CONTAINER_ID)
                self.assertNotIn("stop-container", [name for name, _ in self.native.calls])
                self.assertNotIn("remove-container", [name for name, _ in self.native.calls])

    def test_start_preserves_keyboard_interrupt_and_system_exit_from_native_boundaries(self):
        for failure in (KeyboardInterrupt(), SystemExit(7)):
            with self.subTest(exception=type(failure).__name__):
                self.new_fixture()
                receipt = self.prepared_receipt()
                self.native.errors["create-container"] = failure
                with self.assertRaises(type(failure)) as caught:
                    self.executor.start(receipt)
                self.assertIs(caught.exception, failure)
                self.assertNotIn("start-container", [name for name, _ in self.native.calls])
                self.assertEqual(self.journal()["stage"], "create-pending")
                self.assertIsNone(self.journal()["container_id"])
                calls_after_interrupt = list(self.native.calls)
                retry = self.executor.start(receipt)
                self.assertEqual(retry.classification, Classification.OWNERSHIP_MISMATCH)
                self.assertEqual(self.native.calls, calls_after_interrupt)

    def public_prepared_receipt(self) -> ExecutionReceipt:
        prepared = self.executor.prepare(self.request, state_directory=self.state_identity)
        self.assertEqual(prepared.classification, Classification.PREPARED)
        if prepared.receipt is None or prepared.receipt.prepared is None:
            self.fail("regression fixture requires successful public preparation")
        receipt = prepared.receipt
        self.native.observation = ContainerObservation(
            CONTAINER_ID,
            receipt.token,
            self.directory,
            False,
            bounded_configuration(self.directory, self.runner, LOCAL_RUNNER_IMAGE),
            bounded_policy(),
        )
        self.native.calls.clear()
        return receipt

    def assert_interrupt_survives_close_failure(
        self,
        mechanism: str,
        boundary: str,
        failure: BaseException,
    ) -> None:
        receipt = self.public_prepared_receipt()
        if receipt.directory is None or receipt.checkout is None:
            self.fail("preparation must retain both original roots")
        directory = receipt.directory
        assert directory is not None
        fault = OwnedDescriptorCloseFault()
        pending_documents: list[dict] = []
        calls_at_interrupt: list[tuple[str, str | None]] = []

        def arm_release_error() -> None:
            pending_documents.append(self.journal())
            calls_at_interrupt.extend(self.native.calls)
            fault.arm(mechanism, directory)

        self.native.callbacks[boundary] = arm_release_error
        self.native.errors[boundary] = failure
        caught = None
        # Install before registration: ExitStack saves the callback object itself.
        with (
            patch("os.open", side_effect=fault.open),
            patch("os.close", side_effect=fault.close),
            patch("fcntl.flock", side_effect=fault.flock),
        ):
            try:
                self.executor.start(receipt)
            except BaseException as error:
                caught = error
        try:
            locks_available = fault.locks_available((receipt.state_directory, directory))
        finally:
            # The broken path-close loop can skip ancestors. Close only still-owned
            # generations before any assertion, never retry the faulted descriptor.
            unreleased = fault.release_remaining()

        self.assertTrue(fault.triggered, "fixture must report its one-shot close error")
        self.assertEqual(len(pending_documents), 1)
        self.assertEqual(self.native.calls, calls_at_interrupt)
        self.assertEqual(self.journal(), pending_documents[0])
        self.assertEqual(
            self.journal()["stage"],
            "create-pending" if boundary == "create-container" else "created",
        )
        self.assertEqual(
            self.journal()["container_id"], None if boundary == "create-container" else CONTAINER_ID
        )
        self.assertEqual(locks_available, [True, True])
        self.assertEqual(OwnedPath.capture(self.directory), receipt.directory)
        self.assertEqual(OwnedPath.capture(self.checkout), receipt.checkout)
        retry = self.executor.start(receipt)
        self.assertEqual(retry.classification, Classification.OWNERSHIP_MISMATCH)
        self.assertEqual(self.native.calls, calls_at_interrupt)
        self.assertEqual(self.journal(), pending_documents[0])
        with self.subTest(contract="first-interruption-object"):
            self.assertIs(caught, failure)
        with self.subTest(contract="every-owned-release-attempted-once"):
            self.assertEqual(set(fault.attempted), fault.pending)
            self.assertEqual(len(fault.attempted), len(fault.pending))
            self.assertEqual(unreleased, set())
            self.assertEqual(fault.unowned_closes, [])

    def test_native_interrupt_survives_journal_callback_close_failure(self):
        for boundary in ("create-container", "start-container"):
            for failure in (KeyboardInterrupt(), SystemExit(17)):
                with self.subTest(boundary=boundary, exception=type(failure).__name__):
                    self.new_fixture()
                    self.assert_interrupt_survives_close_failure(
                        "journal-callback", boundary, failure
                    )

    def test_native_interrupt_survives_path_close_loop_failure(self):
        for boundary in ("create-container", "start-container"):
            for failure in (KeyboardInterrupt(), SystemExit(17)):
                with self.subTest(boundary=boundary, exception=type(failure).__name__):
                    self.new_fixture()
                    self.assert_interrupt_survives_close_failure("path-loop", boundary, failure)

    def test_ordinary_native_and_close_failures_retain_original_cleanup_scope(self):
        for mechanism in ("journal-callback", "path-loop"):
            for boundary in ("create-container", "start-container"):
                with self.subTest(mechanism=mechanism, boundary=boundary):
                    self.new_fixture()
                    receipt = self.public_prepared_receipt()
                    original_directory = receipt.directory
                    if original_directory is None:
                        self.fail("ordinary cleanup failure needs the original VM root")
                    fault = OwnedDescriptorCloseFault()
                    pending_documents: list[dict] = []
                    calls_at_failure: list[tuple[str, str | None]] = []

                    def arm_release_error() -> None:
                        pending_documents.append(self.journal())
                        calls_at_failure.extend(self.native.calls)
                        fault.arm(mechanism, original_directory)

                    self.native.callbacks[boundary] = arm_release_error
                    self.native.errors[boundary] = RuntimeError(
                        "controlled sensitive native failure"
                    )
                    with (
                        patch("os.open", side_effect=fault.open),
                        patch("os.close", side_effect=fault.close),
                        patch("fcntl.flock", side_effect=fault.flock),
                    ):
                        result = self.executor.start(receipt)
                    try:
                        locks_available = fault.locks_available(
                            (receipt.state_directory, original_directory)
                        )
                    finally:
                        unreleased = fault.release_remaining()

                    self.assertTrue(fault.triggered)
                    self.assertEqual(result.classification, Classification.CLEANUP_INCOMPLETE)
                    if result.receipt is None:
                        self.fail("release uncertainty must retain original cleanup authority")
                    self.assertEqual(result.receipt.directory, receipt.directory)
                    self.assertEqual(result.receipt.checkout, receipt.checkout)
                    self.assertEqual(result.receipt.state_directory, receipt.state_directory)
                    expected_id = None if boundary == "create-container" else CONTAINER_ID
                    self.assertEqual(result.receipt.container_id, expected_id)
                    self.assertEqual(len(pending_documents), 1)
                    self.assertEqual(self.journal(), pending_documents[0])
                    self.assertEqual(self.journal()["container_id"], expected_id)
                    self.assertEqual(self.native.calls, calls_at_failure)
                    self.assertEqual(locks_available, [True, True])
                    self.assertEqual(set(fault.attempted), fault.pending)
                    self.assertEqual(len(fault.attempted), len(fault.pending))
                    self.assertEqual(unreleased, set())
                    self.assertEqual(fault.unowned_closes, [])
                    self.assertNotIn("controlled sensitive native failure", repr(result))
                    self.assertNotIn(str(self.root), repr(result))
                    self.assertEqual(
                        self.executor.start(receipt).classification,
                        Classification.OWNERSHIP_MISMATCH,
                    )
                    self.assertEqual(self.native.calls, calls_at_failure)
                    self.assertEqual(self.journal(), pending_documents[0])

    def assert_successful_body_reports_owned_close_failure(
        self,
        phase: Phase,
        mechanism: str,
        handled: BaseException | None,
        failure: BaseException,
    ) -> None:
        original_receipt = self.public_prepared_receipt() if phase == Phase.START else None
        original_directory = OwnedPath.capture(self.directory)
        original_checkout = OwnedPath.capture(self.checkout)
        fault = OwnedDescriptorCloseFault(failure, track_later_opens=True)
        armed_stages: list[str] = []
        release_documents: list[dict] = []
        release_journals: list[OwnedPath] = []
        calls_at_release: list[tuple[str, str | None]] = []

        def arm_release_failure() -> None:
            armed_stages.append(self.journal()["stage"])
            fault.arm(mechanism, original_directory)

        def close_owned_generation(fd: int) -> None:
            record = fault.active.get(fd)
            if record is not None and record[0] == fault.target and not fault.triggered:
                # Observe the completed body before releasing the exact generation.
                # The wrapper neither fabricates a receipt nor changes admission.
                release_documents.append(self.journal())
                release_journals.append(OwnedPath.capture(self.state / JOURNAL_NAME))
                calls_at_release.extend(self.native.calls)
            fault.close(fd)

        def invoke() -> ExecutionResult:
            if original_receipt is not None:
                return self.executor.start(original_receipt)
            return self.executor.prepare(self.request, state_directory=self.state_identity)

        self.native.callbacks["inspect-seed"] = arm_release_failure
        result, caught = None, None
        # Every descriptor and the writable journal are registered under the real
        # public operation. Only their OS release is faulted, after actual close.
        with (
            patch("os.open", side_effect=fault.open),
            patch("os.close", side_effect=close_owned_generation),
            patch("fcntl.flock", side_effect=fault.flock),
        ):
            if handled is not None:
                result, caught = invoke_inside_handled_interruption(invoke, handled)
            else:
                try:
                    result = invoke()
                except BaseException as error:
                    caught = error
        try:
            locks_available = fault.locks_available((self.state_identity, original_directory))
        finally:
            unreleased = fault.release_remaining()

        self.assertTrue(fault.triggered, "the exact owned generation must close before its fault")
        self.assertEqual(locks_available, [True, True])
        self.assertEqual(set(fault.attempted), fault.pending)
        self.assertEqual(len(fault.attempted), len(fault.pending))
        assert fault.target is not None
        self.assertEqual(fault.attempted.count(fault.target), 1)
        self.assertEqual(unreleased, set())
        self.assertEqual(fault.unowned_closes, [])
        self.assertEqual(armed_stages, ["prepared" if phase == Phase.START else "seed-created"])
        self.assertEqual(len(release_documents), 1)
        self.assertEqual(len(release_journals), 1)
        document, journal = release_documents[0], release_journals[0]
        expected_id = CONTAINER_ID if phase == Phase.START else None
        self.assertEqual(document["stage"], "started" if phase == Phase.START else "prepared")
        self.assertEqual(document["container_id"], expected_id)
        self.assertEqual(self.journal(), document)
        self.assertEqual(OwnedPath.capture(journal.path), journal)
        self.assertEqual(OwnedPath.capture(journal.path).content_sha256, journal.content_sha256)
        self.assertEqual(OwnedPath.capture(self.directory), original_directory)
        self.assertEqual(OwnedPath.capture(self.checkout), original_checkout)
        self.assertEqual(self.native.calls, calls_at_release)
        self.assertEqual(
            self.native.calls,
            [
                ("inspect-disk", None),
                ("inspect-disk", None),
                ("inspect-seed", None),
                ("create-container", None),
                ("inspect", CONTAINER_ID),
                ("start-container", CONTAINER_ID),
            ]
            if phase == Phase.START
            else [
                ("create-credentials", None),
                ("create-os-disk", None),
                ("inspect-disk", None),
                ("create-encrypted-disk", None),
                ("inspect-disk", None),
                ("create-seed", None),
                ("inspect-seed", None),
            ],
        )

        if isinstance(failure, Exception):
            self.assertIsNone(caught)
            if result is None:
                self.fail("ordinary release errors must return a bounded failure result")
            self.assertEqual(result.phase, phase)
            self.assertEqual(result.classification, Classification.CLEANUP_INCOMPLETE)
            if result.receipt is None:
                self.fail("normal-exit release uncertainty must retain original cleanup scope")
            receipt = result.receipt
            self.assertEqual(receipt.directory, original_directory)
            assert receipt.directory is not None
            self.assertEqual(receipt.directory.ancestors, original_directory.ancestors)
            self.assertEqual(receipt.checkout, original_checkout)
            assert receipt.checkout is not None
            self.assertEqual(receipt.checkout.ancestors, original_checkout.ancestors)
            self.assertEqual(receipt.state_directory, self.state_identity)
            self.assertEqual(receipt.journal, journal)
            self.assertEqual(receipt.journal.content_sha256, journal.content_sha256)
            self.assertEqual(receipt.token, document["token"])
            self.assertEqual(receipt.container_id, expected_id)
            self.assertEqual(len(receipt.created_paths), 7)
            self.assertIsNotNone(receipt.image_evidence)
            self.assertNotIn(str(self.root), repr(result))
            self.assertNotIn(str(failure), repr(result))
            if phase == Phase.PREPARE:
                self.assertIsNone(receipt.prepared, "failed descriptor teardown cannot grant start")
            else:
                if receipt.prepared is None:
                    self.fail("started release failure must retain the possibly running exact ID")
                self.assertEqual(receipt.prepared.ownership.container_id, CONTAINER_ID)
            self.assertEqual(
                self.executor.start(receipt).classification, Classification.INVALID_STATE
            )
        else:
            self.assertIs(
                caught,
                failure,
                "a handled caller interruption cannot replace a fresh release interruption",
            )
            self.assertIsNone(result)

        if original_receipt is not None:
            self.assertEqual(
                self.executor.start(original_receipt).classification,
                Classification.OWNERSHIP_MISMATCH,
            )
            self.assertIsNone(original_receipt.container_id)
            self.assertEqual(document["token"], original_receipt.token)
            self.assertEqual(journal, original_receipt.journal)
        else:
            rejected = self.executor.prepare(self.request, state_directory=self.state_identity)
            self.assertEqual(rejected.classification, Classification.UNSAFE_PATH)
            self.assertIsNone(rejected.receipt)
        self.assertEqual(self.native.calls, calls_at_release)
        self.assertEqual(self.journal(), document)

    def test_prepare_reports_owned_close_error_inside_a_handled_interruption(self):
        for mechanism in ("journal-callback", "path-loop"):
            for handled in (KeyboardInterrupt(), SystemExit(37)):
                with self.subTest(mechanism=mechanism, caller=type(handled).__name__):
                    self.new_fixture()
                    self.assert_successful_body_reports_owned_close_failure(
                        Phase.PREPARE,
                        mechanism,
                        handled,
                        OSError(errno.EIO, "controlled normal-exit descriptor close failure"),
                    )

    def test_start_reports_owned_close_error_inside_a_handled_interruption(self):
        for mechanism in ("journal-callback", "path-loop"):
            for handled in (KeyboardInterrupt(), SystemExit(37)):
                with self.subTest(mechanism=mechanism, caller=type(handled).__name__):
                    self.new_fixture()
                    self.assert_successful_body_reports_owned_close_failure(
                        Phase.START,
                        mechanism,
                        handled,
                        OSError(errno.EIO, "controlled normal-exit descriptor close failure"),
                    )

    def test_prepare_propagates_fresh_close_interruption_inside_a_handled_interruption(self):
        for mechanism in ("journal-callback", "path-loop"):
            for handled in (KeyboardInterrupt(), SystemExit(37)):
                for failure in (KeyboardInterrupt(), SystemExit(41)):
                    with self.subTest(
                        mechanism=mechanism,
                        caller=type(handled).__name__,
                        release=type(failure).__name__,
                    ):
                        self.new_fixture()
                        self.assert_successful_body_reports_owned_close_failure(
                            Phase.PREPARE,
                            mechanism,
                            handled,
                            failure,
                        )

    def test_start_propagates_fresh_close_interruption_inside_a_handled_interruption(self):
        for mechanism in ("journal-callback", "path-loop"):
            for handled in (KeyboardInterrupt(), SystemExit(37)):
                for failure in (KeyboardInterrupt(), SystemExit(41)):
                    with self.subTest(
                        mechanism=mechanism,
                        caller=type(handled).__name__,
                        release=type(failure).__name__,
                    ):
                        self.new_fixture()
                        self.assert_successful_body_reports_owned_close_failure(
                            Phase.START,
                            mechanism,
                            handled,
                            failure,
                        )

    def test_successful_prepare_and_start_report_owned_close_errors_without_handled_interruptions(
        self,
    ):
        for phase in (Phase.PREPARE, Phase.START):
            for mechanism in ("journal-callback", "path-loop"):
                with self.subTest(phase=phase.value, mechanism=mechanism):
                    self.new_fixture()
                    self.assert_successful_body_reports_owned_close_failure(
                        phase,
                        mechanism,
                        None,
                        OSError(errno.EIO, "controlled normal-exit descriptor close failure"),
                    )

    def test_public_prepare_and_start_succeed_inside_handled_interruptions_without_release_faults(
        self,
    ):
        for handled in (KeyboardInterrupt(), SystemExit(37)):
            with self.subTest(caller=type(handled).__name__):
                self.new_fixture()
                prepared, caught = invoke_inside_handled_interruption(
                    lambda: self.executor.prepare(
                        self.request, state_directory=self.state_identity
                    ),
                    handled,
                )
                self.assertIsNone(caught)
                if (
                    prepared is None
                    or prepared.receipt is None
                    or prepared.receipt.prepared is None
                ):
                    self.fail("normally handled caller interruption must permit public preparation")
                self.assertEqual(prepared.classification, Classification.PREPARED)
                receipt = prepared.receipt
                self.native.observation = ContainerObservation(
                    CONTAINER_ID,
                    receipt.token,
                    self.directory,
                    False,
                    bounded_configuration(self.directory, self.runner, LOCAL_RUNNER_IMAGE),
                    bounded_policy(),
                )
                started, caught = invoke_inside_handled_interruption(
                    lambda: self.executor.start(receipt),
                    handled,
                )
                self.assertIsNone(caught)
                if started is None or started.receipt is None or started.receipt.prepared is None:
                    self.fail("normally handled caller interruption must permit public start")
                self.assertEqual(started.classification, Classification.STARTED)
                self.assertEqual(started.receipt.container_id, CONTAINER_ID)
                self.assertEqual(started.receipt.prepared.ownership.container_id, CONTAINER_ID)
                self.assertEqual(started.receipt.directory, receipt.directory)
                self.assertEqual(started.receipt.checkout, receipt.checkout)
                self.assertEqual(self.journal()["stage"], "started")
                self.assertEqual(self.journal()["container_id"], CONTAINER_ID)
                if receipt.directory is None:
                    self.fail("public preparation must retain the original VM root")
                releases = OwnedDescriptorCloseFault()
                self.assertEqual(
                    releases.locks_available((receipt.state_directory, receipt.directory)),
                    [True, True],
                )

    def start_with_late_qmp_socket(self, receipt: ExecutionReceipt, mutation: str = "socket"):
        native_started, after_refresh, socket_ready, release_socket = (
            threading.Event() for _ in range(4)
        )
        worker_errors: list[BaseException] = []
        sockets: list[os.stat_result] = []
        runtime_windows: list[os.stat_result] = []
        real_refresh = executor_module._Execution.refresh_directory

        def bind_after_refresh() -> None:
            try:
                if not after_refresh.wait(4 * _PROCESS_WAIT_SECONDS):
                    raise TimeoutError("bounded late-socket rendezvous expired")
                with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as qmp:
                    qmp_path = self.directory / "qmp.sock"
                    qmp.bind(str(qmp_path))
                    qmp_path.chmod(0o600)
                    sockets.append(qmp_path.lstat())
                    if mutation == "foreign-entry":
                        (self.directory / "foreign-runtime-entry").write_bytes(
                            b"owned foreign fixture"
                        )
                    elif mutation == "replaced-root":
                        self.directory.rename(self.root / "retained-runtime-vm")
                        self.directory.mkdir(mode=0o700)
                        (self.directory / "foreign-root-entry").write_bytes(
                            b"owned replacement fixture"
                        )
                    elif mutation == "nonprivate-socket":
                        qmp_path.chmod(0o660)
                    elif mutation == "socket-type":
                        qmp_path.rename(self.root / "retained-qmp.sock")
                        qmp_path.touch(mode=0o600)
                        qmp_path.write_bytes(b"owned non-socket replacement fixture")
                    elif mutation == "nonprivate-root":
                        self.directory.chmod(0o755)
                    socket_ready.set()
                    if not release_socket.wait(4 * _PROCESS_WAIT_SECONDS):
                        raise TimeoutError("bounded owned-socket lease expired")
            except BaseException as error:
                worker_errors.append(error)
            finally:
                socket_ready.set()

        def refresh_then_allow_socket(execution, *, state: bool = False) -> None:
            real_refresh(execution, state=state)
            if not state and native_started.is_set() and not runtime_windows:
                runtime_windows.append(execution.directory.observation)
                after_refresh.set()
                if not socket_ready.wait(_PROCESS_WAIT_SECONDS):
                    raise TimeoutError("bounded socket creation expired")

        self.native.callbacks["start-container"] = native_started.set
        worker = threading.Thread(target=bind_after_refresh, daemon=True)
        worker.start()
        try:
            with patch.object(
                executor_module._Execution, "refresh_directory", refresh_then_allow_socket
            ):
                result = self.executor.start(receipt)
        finally:
            after_refresh.set()
            release_socket.set()
            worker.join(2 * _PROCESS_WAIT_SECONDS)
        self.assertFalse(worker.is_alive(), "owned socket worker must finish its finite lease")
        self.assertEqual(worker_errors, [])
        self.assertEqual(
            len(runtime_windows), 1, "socket must follow the completed runtime refresh"
        )
        self.assertEqual(len(sockets), 1)
        self.assertTrue(stat.S_ISSOCK(sockets[0].st_mode))
        self.assertEqual(sockets[0].st_uid, os.geteuid())
        self.assertEqual(stat.S_IMODE(sockets[0].st_mode), 0o600)
        return result, runtime_windows[0]

    def test_start_accepts_private_qmp_socket_created_after_runtime_directory_refresh(self):
        # Keep the real AF_UNIX address below macOS's limit in a new owned root.
        self.new_fixture(temporary_directory="/tmp")
        receipt = self.public_prepared_receipt()
        result, refreshed = self.start_with_late_qmp_socket(receipt)
        current = self.directory.stat()
        self.assertNotEqual(
            (current.st_size, current.st_mtime_ns, current.st_ctime_ns),
            (refreshed.st_size, refreshed.st_mtime_ns, refreshed.st_ctime_ns),
        )
        self.assertEqual(result.classification, Classification.STARTED)
        if result.receipt is None or result.receipt.prepared is None:
            self.fail("successful native start must retain its exact started ID")
        self.assertEqual(result.receipt.container_id, CONTAINER_ID)
        self.assertEqual(result.receipt.prepared.ownership.container_id, CONTAINER_ID)
        self.assertEqual(result.receipt.directory, receipt.directory)
        self.assertEqual(result.receipt.checkout, receipt.checkout)
        self.assertEqual(self.journal()["stage"], "started")
        self.assertEqual(self.journal()["container_id"], CONTAINER_ID)
        self.assertEqual(
            [
                call
                for call in self.native.calls
                if call[0]
                in (
                    "create-container",
                    "inspect",
                    "start-container",
                    "stop-container",
                    "remove-container",
                )
            ],
            [
                ("create-container", None),
                ("inspect", CONTAINER_ID),
                ("start-container", CONTAINER_ID),
            ],
        )

    def test_late_runtime_socket_allowance_rejects_foreign_entries_and_replaced_roots(self):
        for mutation in ("foreign-entry", "replaced-root"):
            with self.subTest(mutation=mutation):
                self.new_fixture(temporary_directory="/tmp")
                receipt = self.public_prepared_receipt()
                result, _ = self.start_with_late_qmp_socket(receipt, mutation)
                self.assertEqual(result.classification, Classification.CLEANUP_INCOMPLETE)
                if result.receipt is None:
                    self.fail("runtime rejection must retain original exact-ID cleanup scope")
                self.assertEqual(result.receipt.directory, receipt.directory)
                self.assertEqual(result.receipt.checkout, receipt.checkout)
                self.assertEqual(result.receipt.container_id, CONTAINER_ID)
                self.assertEqual(self.journal()["stage"], "created")
                self.assertEqual(self.journal()["container_id"], CONTAINER_ID)
                if mutation == "replaced-root":
                    self.assertNotEqual(OwnedPath.capture(self.directory), receipt.directory)
                    self.assertTrue((self.root / "retained-runtime-vm" / "qmp.sock").exists())
                    self.assertEqual(
                        (self.directory / "foreign-root-entry").read_bytes(),
                        b"owned replacement fixture",
                    )
                else:
                    self.assertEqual(
                        (self.directory / "foreign-runtime-entry").read_bytes(),
                        b"owned foreign fixture",
                    )
                self.assertNotIn("stop-container", [name for name, _ in self.native.calls])
                self.assertNotIn("remove-container", [name for name, _ in self.native.calls])

    def test_late_runtime_socket_allowance_preserves_type_and_protection_checks(self):
        for mutation in ("nonprivate-socket", "socket-type", "nonprivate-root"):
            with self.subTest(mutation=mutation):
                self.new_fixture(temporary_directory="/tmp")
                receipt = self.public_prepared_receipt()
                result, _ = self.start_with_late_qmp_socket(receipt, mutation)
                self.assertEqual(result.classification, Classification.CLEANUP_INCOMPLETE)
                if result.receipt is None:
                    self.fail("runtime protection rejection must retain the original exact ID")
                self.assertEqual(result.receipt.directory, receipt.directory)
                self.assertEqual(result.receipt.checkout, receipt.checkout)
                self.assertEqual(result.receipt.container_id, CONTAINER_ID)
                self.assertEqual(self.journal()["stage"], "created")
                self.assertEqual(self.journal()["container_id"], CONTAINER_ID)
                self.assertNotIn("stop-container", [name for name, _ in self.native.calls])
                self.assertNotIn("remove-container", [name for name, _ in self.native.calls])
                if mutation == "nonprivate-socket":
                    self.assertEqual(
                        stat.S_IMODE((self.directory / "qmp.sock").lstat().st_mode), 0o660
                    )
                elif mutation == "socket-type":
                    self.assertEqual(
                        (self.directory / "qmp.sock").read_bytes(),
                        b"owned non-socket replacement fixture",
                    )
                else:
                    self.assertEqual(stat.S_IMODE(self.directory.lstat().st_mode), 0o755)

    def test_prestart_rejects_qmp_socket_foreign_entries_and_nonprivate_vm_roots(self):
        for mutation in ("qmp-socket", "foreign-entry", "nonprivate-root"):
            with self.subTest(mutation=mutation):
                self.new_fixture(temporary_directory="/tmp")
                receipt = self.public_prepared_receipt()
                with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as qmp:
                    if mutation == "qmp-socket":
                        qmp_path = self.directory / "qmp.sock"
                        qmp.bind(str(qmp_path))
                        qmp_path.chmod(0o600)
                    elif mutation == "foreign-entry":
                        (self.directory / "foreign-prestart-entry").write_bytes(
                            b"owned foreign fixture"
                        )
                    else:
                        self.directory.chmod(0o755)
                    result = self.executor.start(receipt)
                self.assertEqual(result.classification, Classification.OWNERSHIP_MISMATCH)
                self.assertEqual(self.native.calls, [])
                self.assertEqual(self.journal()["stage"], "prepared")
                self.assertIsNone(self.journal()["container_id"])

    def test_initial_journal_refresh_failure_retains_original_scope_without_adopting_replacement(
        self,
    ):
        for replace_journal in (False, True):
            with self.subTest(replaced_journal=replace_journal):
                self.new_fixture()
                parents = (self.root / "vm-parent", self.root / "checkout-parent")
                for parent in parents:
                    parent.mkdir(mode=0o700)
                self.directory.rmdir()
                self.directory = parents[0] / "vm"
                self.directory.mkdir(mode=0o700)
                self.checkout.rename(parents[1] / "checkout")
                self.checkout = parents[1] / "checkout"
                self.request = replace(self.request, directory=self.directory)
                original_roots = (
                    OwnedPath.capture(self.directory),
                    OwnedPath.capture(self.checkout),
                )
                self.executor = VmExecutor(
                    self.checkout,
                    self.signatures,
                    self.native,
                    self.runner,
                    runner_image_pin=LOCAL_RUNNER_IMAGE,
                )
                proven_journals: list[OwnedPath] = []
                initial_tokens: list[str] = []
                faulted: list[tuple[int, int]] = []
                armed = False
                real_fstat = os.fstat
                real_refresh = executor_module._Execution.refresh_directory
                journal_path = self.state / JOURNAL_NAME
                saved_journal = self.state / "retained-journal.json"
                replacement_identity = None

                def fail_state_fstat_after_capture(fd: int):
                    nonlocal replacement_identity
                    observation = real_fstat(fd)
                    identity = (observation.st_dev, observation.st_ino)
                    if (
                        armed
                        and not faulted
                        and identity
                        == (
                            self.state_identity.device,
                            self.state_identity.inode,
                        )
                    ):
                        faulted.append(identity)
                        for parent, leaf in zip(parents, ("vm", "checkout"), strict=True):
                            retained = parent.with_name("retained-" + parent.name)
                            parent.rename(retained)
                            parent.mkdir(mode=0o700)
                            (retained / leaf).rename(parent / leaf)
                        if replace_journal:
                            journal_path.rename(saved_journal)
                            journal_path.touch(mode=0o600)
                            journal_path.write_bytes(b"unproven replacement journal fixture")
                            replacement_identity = OwnedPath.capture(journal_path)
                        raise OSError(errno.EIO, "controlled post-capture state stat failure")
                    return observation

                def arm_after_journal_identity_capture(execution, *, state: bool = False) -> None:
                    nonlocal armed
                    if state and not armed:
                        # The constructor has proved and retained the journal inode
                        # before entering this post-creation directory observation.
                        proven_journals.append(OwnedPath.capture(journal_path))
                        initial_tokens.append(execution.token)
                        armed = True
                    real_refresh(execution, state=state)

                with (
                    patch("os.fstat", side_effect=fail_state_fstat_after_capture),
                    patch.object(
                        executor_module._Execution,
                        "refresh_directory",
                        arm_after_journal_identity_capture,
                    ),
                ):
                    result = self.executor.prepare(
                        self.request, state_directory=self.state_identity
                    )
                self.assertEqual(len(proven_journals), 1)
                self.assertEqual(faulted, [(self.state_identity.device, self.state_identity.inode)])
                self.assertEqual(self.native.calls, [])
                self.assertEqual(list(self.directory.iterdir()), [])
                self.assertEqual(
                    (saved_journal if replace_journal else journal_path).read_bytes(), b""
                )
                for original in original_roots:
                    current = OwnedPath.capture(original.path)
                    self.assertEqual(
                        (current.device, current.inode), (original.device, original.inode)
                    )
                    self.assertNotEqual(current.ancestors, original.ancestors)
                self.assertEqual(result.classification, Classification.CLEANUP_INCOMPLETE)
                if result.receipt is None:
                    self.fail("initial journal allocation must retain cleanup-only authority")
                receipt = result.receipt
                self.assertIsNone(receipt.prepared)
                self.assertIsNone(receipt.container_id)
                self.assertEqual(receipt.created_paths, ())
                self.assertEqual(receipt.directory, original_roots[0])
                self.assertEqual(receipt.checkout, original_roots[1])
                self.assertEqual(receipt.state_directory, self.state_identity)
                self.assertEqual(receipt.journal, proven_journals[0])
                self.assertEqual(receipt.token, initial_tokens[0])
                self.assertRegex(receipt.token, r"\A[0-9a-f]{64}\Z")
                self.assertIsNotNone(receipt.image_evidence)
                if replace_journal:
                    self.assertNotEqual(receipt.journal, replacement_identity)
                    self.assertEqual(
                        journal_path.read_bytes(), b"unproven replacement journal fixture"
                    )
                before_start = list(self.native.calls)
                self.assertEqual(
                    self.executor.start(receipt).classification, Classification.INVALID_STATE
                )
                self.assertEqual(self.native.calls, before_start)

    def test_initial_journal_refresh_interrupt_preserves_proven_inode_and_releases_descriptors(
        self,
    ):
        for mechanism in ("journal-callback", "path-loop"):
            for failure in (KeyboardInterrupt(), SystemExit(29)):
                with self.subTest(mechanism=mechanism, exception=type(failure).__name__):
                    self.new_fixture()
                    original_directory = OwnedPath.capture(self.directory)
                    original_checkout = OwnedPath.capture(self.checkout)
                    proven_journals: list[OwnedPath] = []
                    fault = OwnedDescriptorCloseFault()
                    real_refresh = executor_module._Execution.refresh_directory

                    def interrupt_after_capture(execution, *, state: bool = False) -> None:
                        if state:
                            proven_journals.append(OwnedPath.capture(self.state / JOURNAL_NAME))
                            fault.arm(mechanism, original_directory)
                            raise failure
                        real_refresh(execution, state=state)

                    caught = None
                    with (
                        patch("os.open", side_effect=fault.open),
                        patch("os.close", side_effect=fault.close),
                        patch("fcntl.flock", side_effect=fault.flock),
                        patch.object(
                            executor_module._Execution, "refresh_directory", interrupt_after_capture
                        ),
                    ):
                        try:
                            self.executor.prepare(self.request, state_directory=self.state_identity)
                        except BaseException as error:
                            caught = error
                    try:
                        locks_available = fault.locks_available(
                            (self.state_identity, original_directory)
                        )
                    finally:
                        unreleased = fault.release_remaining()

                    self.assertIs(caught, failure)
                    self.assertTrue(fault.triggered)
                    self.assertEqual(len(proven_journals), 1)
                    self.assertEqual(
                        OwnedPath.capture(self.state / JOURNAL_NAME), proven_journals[0]
                    )
                    self.assertEqual((self.state / JOURNAL_NAME).read_bytes(), b"")
                    self.assertEqual(OwnedPath.capture(self.directory), original_directory)
                    self.assertEqual(OwnedPath.capture(self.checkout), original_checkout)
                    self.assertEqual(self.native.calls, [])
                    self.assertEqual(list(self.directory.iterdir()), [])
                    self.assertEqual(locks_available, [True, True])
                    self.assertEqual(set(fault.attempted), fault.pending)
                    self.assertEqual(len(fault.attempted), len(fault.pending))
                    self.assertEqual(unreleased, set())
                    self.assertEqual(fault.unowned_closes, [])
                    rejected = self.executor.prepare(
                        self.request, state_directory=self.state_identity
                    )
                    self.assertEqual(rejected.classification, Classification.UNSAFE_PATH)
                    self.assertIsNone(rejected.receipt)
                    self.assertEqual(self.native.calls, [])
                    self.assertEqual((self.state / JOURNAL_NAME).read_bytes(), b"")

    def assert_initial_journal_stat_failure_scope(self, stage: str) -> None:
        original_directory = OwnedPath.capture(self.directory)
        original_checkout = OwnedPath.capture(self.checkout)
        releases = OwnedDescriptorCloseFault()
        journal_path = self.state / JOURNAL_NAME
        saved_journal = self.state / "unproved-allocated-journal.json"
        allocated_journals: list[OwnedPath] = []
        replacement_journals: list[OwnedPath] = []
        path_observations: list[os.stat_result] = []
        held_observations: list[os.stat_result] = []
        comparisons: list[bool] = []
        failed_stats: list[int] = []
        journal_fd = None
        path_fd = None
        capturing = False
        stamp_attributes = (
            "st_dev",
            "st_ino",
            "st_mode",
            "st_uid",
            "st_gid",
            "st_nlink",
            "st_size",
            "st_mtime_ns",
            "st_ctime_ns",
        )

        def observe_journal_open(path, flags: int, mode: int = 0o777, *, dir_fd=None) -> int:
            nonlocal journal_fd, path_fd, capturing
            fd = releases.open(path, flags, mode, dir_fd=dir_fd)
            if Path(path).name != JOURNAL_NAME:
                return fd
            if flags & os.O_CREAT and journal_fd is None:
                journal_fd = fd
                capturing = True
                try:
                    # Independent real capture is an expectation, not authority
                    # supplied to the executor or a replacement for its own proof.
                    allocated_journals.append(OwnedPath.capture(journal_path))
                    if stage == "replaced":
                        journal_path.rename(saved_journal)
                        journal_path.touch(mode=0o600)
                        journal_path.write_bytes(b"unproved replacement journal fixture")
                        replacement_journals.append(OwnedPath.capture(journal_path))
                finally:
                    capturing = False
            elif not capturing and flags & os.O_ACCMODE == os.O_RDONLY:
                path_fd = fd
            return fd

        def fail_journal_stat(fd: int):
            observation = releases.real_fstat(fd)
            if fd == path_fd and not capturing:
                path_observations.append(observation)
            if fd == journal_fd:
                held_observations.append(observation)
                if stage == "identity" and len(held_observations) == 1:
                    failed_stats.append(fd)
                    raise OSError(errno.EIO, "controlled unproved journal identity stat failure")
                if len(held_observations) == 1 and path_observations:
                    comparisons.append(
                        tuple(getattr(path_observations[-1], name) for name in stamp_attributes)
                        == tuple(getattr(observation, name) for name in stamp_attributes)
                    )
                elif stage == "protection" and comparisons == [True] and not failed_stats:
                    # The first held stat returned the exact path descriptor stamp.
                    # The next held stat is the protection check, before a record.
                    failed_stats.append(fd)
                    raise OSError(errno.EIO, "controlled proven journal protection stat failure")
            return observation

        with (
            patch("os.open", side_effect=observe_journal_open),
            patch("os.fstat", side_effect=fail_journal_stat),
            patch("os.close", side_effect=releases.close),
            patch("fcntl.flock", side_effect=releases.flock),
        ):
            result = self.executor.prepare(self.request, state_directory=self.state_identity)
        try:
            locks_available = releases.locks_available((self.state_identity, original_directory))
        finally:
            unreleased = releases.release_remaining()

        self.assertEqual(len(allocated_journals), 1)
        self.assertEqual(locks_available, [True, True])
        self.assertEqual(unreleased, set())
        self.assertEqual(releases.unowned_closes, [])
        self.assertEqual(
            set(releases.claims),
            {
                (self.state_identity.device, self.state_identity.inode),
                (original_directory.device, original_directory.inode),
            },
        )
        self.assertEqual(self.native.calls, [])
        self.assertEqual(list(self.directory.iterdir()), [])
        self.assertEqual(
            self.signatures.reads,
            [
                (
                    self.request.image.checksums.read_bytes(),
                    self.request.image.signature.read_bytes(),
                )
            ],
        )
        allocated_path = saved_journal if stage == "replaced" else journal_path
        allocated_now = OwnedPath.capture(allocated_path)
        self.assertEqual(
            (allocated_now.device, allocated_now.inode),
            (allocated_journals[0].device, allocated_journals[0].inode),
        )
        self.assertEqual(allocated_path.read_bytes(), b"")
        for original in (original_directory, original_checkout, self.state_identity):
            self.assertEqual(OwnedPath.capture(original.path), original)
            self.assertEqual(OwnedPath.capture(original.path).ancestors, original.ancestors)
        self.assertEqual(result.phase, Phase.PREPARE)

        if stage == "protection":
            self.assertEqual(comparisons, [True])
            self.assertEqual(len(held_observations), 2)
            self.assertEqual(failed_stats, [journal_fd])
            self.assertEqual(result.classification, Classification.CLEANUP_INCOMPLETE)
            if result.receipt is None:
                self.fail(
                    "proved journal allocation must retain cleanup-only scope before protection stat"
                )
            receipt = result.receipt
            self.assertEqual(receipt.journal, allocated_journals[0])
            self.assertEqual(receipt.journal.ancestors, allocated_journals[0].ancestors)
            self.assertIsNone(receipt.journal.content_sha256)
            self.assertEqual(receipt.directory, original_directory)
            assert receipt.directory is not None
            self.assertEqual(receipt.directory.ancestors, original_directory.ancestors)
            self.assertEqual(receipt.checkout, original_checkout)
            assert receipt.checkout is not None
            self.assertEqual(receipt.checkout.ancestors, original_checkout.ancestors)
            self.assertEqual(receipt.state_directory, self.state_identity)
            self.assertEqual(receipt.created_paths, ())
            self.assertIsNone(receipt.prepared)
            self.assertIsNone(receipt.container_id)
            self.assertIsNotNone(receipt.image_evidence)
            self.assertRegex(receipt.token, r"\A[0-9a-f]{64}\Z")
            self.assertEqual(
                self.executor.start(receipt).classification, Classification.INVALID_STATE
            )
            self.assertEqual(self.native.calls, [])
            self.assertEqual(journal_path.read_bytes(), b"")
        else:
            self.assertEqual(len(held_observations), 1)
            self.assertEqual(comparisons, [] if stage == "identity" else [False])
            self.assertEqual(failed_stats, [journal_fd] if stage == "identity" else [])
            self.assertEqual(
                result.classification,
                Classification.NATIVE_UNAVAILABLE
                if stage == "identity"
                else Classification.OWNERSHIP_MISMATCH,
            )
            self.assertIsNone(
                result.receipt, "unproved allocated or replacement inodes grant no scope"
            )
            if stage == "replaced":
                self.assertEqual(len(replacement_journals), 1)
                self.assertNotEqual(replacement_journals[0], allocated_journals[0])
                self.assertEqual(OwnedPath.capture(journal_path), replacement_journals[0])
                self.assertEqual(journal_path.read_bytes(), b"unproved replacement journal fixture")

        rejected = self.executor.prepare(self.request, state_directory=self.state_identity)
        self.assertEqual(rejected.classification, Classification.UNSAFE_PATH)
        self.assertIsNone(rejected.receipt)
        self.assertEqual(self.native.calls, [])
        self.assertEqual(allocated_path.read_bytes(), b"")

    def test_initial_journal_protection_stat_failure_retains_proven_cleanup_scope(self):
        self.assert_initial_journal_stat_failure_scope("protection")

    def test_initial_journal_unproved_or_replaced_inode_grants_no_cleanup_receipt(self):
        for stage in ("identity", "replaced"):
            with self.subTest(failure_before_proof=stage):
                self.new_fixture()
                self.assert_initial_journal_stat_failure_scope(stage)

    def assert_bounded_namespace_rejection(
        self,
        observed: ControlledNamespaceOs,
        allowed: tuple[str, ...],
        *,
        foreign_first: bool,
    ) -> None:
        self.assertIsNotNone(observed.target)
        self.assertEqual(len(observed.scans), 1)
        for scan in observed.scans:
            with self.subTest(contract="bounded-consumption"):
                self.assertLessEqual(scan.consumed, len(allowed) + 1)
                self.assertEqual(scan.consumed, 1 if foreign_first else len(allowed) + 1)
            with self.subTest(contract="directory-iterator-release"):
                self.assertTrue(scan.closed)

    def test_start_namespace_checks_stop_at_first_foreign_entry_and_close_iterator(self):
        for namespace in ("vm", "credentials", "state"):
            for foreign_first in (False, True):
                with self.subTest(namespace=namespace, foreign_first=foreign_first):
                    self.new_fixture()
                    receipt = self.public_prepared_receipt()
                    directory, allowed = {
                        "vm": (
                            self.directory,
                            (
                                "credentials",
                                "os.qcow2",
                                "encrypted.qcow2",
                                "seed.iso",
                                MANIFEST_NAME,
                            ),
                        ),
                        "credentials": (
                            self.directory / "credentials",
                            ("id_ed25519", "id_ed25519.pub"),
                        ),
                        "state": (self.state, (JOURNAL_NAME,)),
                    }[namespace]
                    observed = ControlledNamespaceOs()
                    observed.arm(directory, allowed, foreign_first=foreign_first)
                    with (
                        patch.object(executor_module, "os", observed),
                        patch.object(vm_module, "os", observed),
                    ):
                        result = self.executor.start(receipt)
                    self.assertEqual(result.classification, Classification.OWNERSHIP_MISMATCH)
                    self.assertEqual(self.native.calls, [])
                    self.assertEqual(self.journal()["stage"], "prepared")
                    self.assertIsNone(self.journal()["container_id"])
                    for name in observed.foreign_names:
                        self.assertEqual(
                            (directory / name).read_bytes(), b"owned foreign-entry fixture"
                        )
                    self.assert_bounded_namespace_rejection(
                        observed, allowed, foreign_first=foreign_first
                    )

    def test_interrupted_namespace_checks_close_iterator_without_native_effects(self):
        for namespace in ("vm", "credentials", "state"):
            for failure in (KeyboardInterrupt(), SystemExit(23)):
                with self.subTest(namespace=namespace, exception=type(failure).__name__):
                    self.new_fixture()
                    receipt = self.public_prepared_receipt()
                    directory, allowed = {
                        "vm": (
                            self.directory,
                            (
                                "credentials",
                                "os.qcow2",
                                "encrypted.qcow2",
                                "seed.iso",
                                MANIFEST_NAME,
                            ),
                        ),
                        "credentials": (
                            self.directory / "credentials",
                            ("id_ed25519", "id_ed25519.pub"),
                        ),
                        "state": (self.state, (JOURNAL_NAME,)),
                    }[namespace]
                    observed = ControlledNamespaceOs(failure)
                    observed.arm(directory, allowed, foreign_first=False)
                    before = self.journal()
                    with (
                        patch.object(executor_module, "os", observed),
                        patch.object(vm_module, "os", observed),
                        self.assertRaises(type(failure)) as caught,
                    ):
                        self.executor.start(receipt)
                    self.assertIs(caught.exception, failure)
                    self.assertEqual(self.native.calls, [])
                    self.assertEqual(self.journal(), before)
                    self.assertEqual(len(observed.scans), 1)
                    self.assertEqual(observed.scans[0].consumed, 1)
                    self.assertTrue(observed.scans[0].closed)
                    for owned in (receipt.state_directory, receipt.directory):
                        if owned is None:
                            self.fail("interrupted admission must retain its namespace roots")
                        fd = os.open(
                            owned.path,
                            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                        )
                        try:
                            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                        finally:
                            os.close(fd)

    def test_prepare_created_namespace_checks_stop_at_first_foreign_entry_and_close_iterator(self):
        for namespace in ("vm", "credentials"):
            for foreign_first in (False, True):
                with self.subTest(namespace=namespace, foreign_first=foreign_first):
                    self.new_fixture()
                    directory, allowed = (
                        (self.directory, ("credentials",))
                        if namespace == "vm"
                        else (self.directory / "credentials", ("id_ed25519", "id_ed25519.pub"))
                    )
                    observed = ControlledNamespaceOs()
                    self.native = ObservedCredentialAllocation(
                        lambda: observed.arm(directory, allowed, foreign_first=foreign_first),
                    )
                    self.executor = VmExecutor(
                        self.checkout,
                        self.signatures,
                        self.native,
                        self.runner,
                        runner_image_pin=LOCAL_RUNNER_IMAGE,
                    )
                    with (
                        patch.object(executor_module, "os", observed),
                        patch.object(vm_module, "os", observed),
                    ):
                        result = self.executor.prepare(
                            self.request, state_directory=self.state_identity
                        )
                    self.assertEqual(result.classification, Classification.CLEANUP_INCOMPLETE)
                    self.assertEqual(self.native.calls, [("create-credentials", None)])
                    self.assertEqual(self.journal()["stage"], "credentials-pending")
                    self.assertIsNone(self.journal()["container_id"])
                    if result.receipt is None:
                        self.fail("namespace rejection after allocation must retain cleanup scope")
                    self.assertIsNone(result.receipt.prepared)
                    self.assertIsNone(result.receipt.container_id)
                    self.assertFalse((self.directory / "os.qcow2").exists())
                    self.assertFalse((self.directory / "seed.iso").exists())
                    for name in observed.foreign_names:
                        self.assertNotIn(
                            directory / name, [owned.path for owned in result.receipt.created_paths]
                        )
                        self.assertEqual(
                            (directory / name).read_bytes(), b"owned foreign-entry fixture"
                        )
                    self.assert_bounded_namespace_rejection(
                        observed, allowed, foreign_first=foreign_first
                    )

    def started_receipt(self):
        prepared = self.executor.prepare(self.request, state_directory=self.state_identity)
        self.assertEqual(prepared.classification, Classification.PREPARED)
        assert prepared.receipt is not None
        self.native.observation = ContainerObservation(
            CONTAINER_ID,
            prepared.receipt.token,
            self.directory,
            False,
            bounded_configuration(self.directory, self.runner, LOCAL_RUNNER_IMAGE),
            bounded_policy(),
        )
        started = self.executor.start(prepared.receipt)
        self.assertEqual(started.classification, Classification.STARTED)
        assert started.receipt is not None
        self.native.observation = replace(self.native.observation, running=True)
        return started.receipt

    def stop_native_guest(self):
        assert self.native.observation is not None
        self.native.observation = replace(self.native.observation, running=False)

    def test_stop_observes_exact_id_absence_before_recording_stopped(self):
        receipt = self.started_receipt()
        self.native.callbacks["stop-container"] = self.stop_native_guest
        stopped = self.executor.stop(receipt)
        self.assertEqual(stopped.classification, Classification.STOPPED)
        self.assertEqual(self.journal()["stage"], "stopped")
        self.assertEqual(
            self.native.calls[-3:],
            [
                ("inspect", CONTAINER_ID),
                ("stop-container", CONTAINER_ID),
                ("inspect", CONTAINER_ID),
            ],
        )
        self.assertIsNotNone(stopped.receipt)

    def test_stop_does_not_credit_command_exit_if_guest_is_still_running(self):
        receipt = self.started_receipt()
        stopped = self.executor.stop(receipt)
        self.assertEqual(stopped.classification, Classification.CLEANUP_INCOMPLETE)
        self.assertEqual(self.journal()["stage"], "stop-pending")
        self.assertIsNotNone(stopped.receipt)

    def test_stop_rejects_changed_policy_and_binding_before_effect(self):
        for changed in ("token", "configuration", "directory", "disk"):
            with self.subTest(changed=changed):
                self.new_fixture()
                receipt = self.started_receipt()
                assert self.native.observation is not None
                if changed == "token":
                    self.native.observation = replace(self.native.observation, owner_token="b" * 64)
                elif changed == "configuration":
                    config = self.native.observation.configuration
                    assert config is not None
                    self.native.observation = replace(
                        self.native.observation, configuration=replace(config, privileged=True)
                    )
                elif changed == "directory":
                    self.directory.rename(self.root / "original-vm")
                    self.directory.mkdir(mode=0o700)
                else:
                    disk = self.directory / "os.qcow2"
                    disk.rename(self.directory / "foreign-original")
                    disk.touch(mode=0o600)
                start = len(self.native.calls)
                stopped = self.executor.stop(receipt)
                self.assertEqual(stopped.classification, Classification.OWNERSHIP_MISMATCH)
                self.assertNotIn("stop-container", [name for name, _ in self.native.calls[start:]])

    def test_stop_accepts_guest_disk_writes_without_rehashing_downloads(self):
        receipt = self.started_receipt()
        with (self.directory / "os.qcow2").open("ab") as file:
            file.write(b"ordinary guest disk write")
        self.request.image.path.unlink()
        self.native.callbacks["stop-container"] = self.stop_native_guest
        stopped = self.executor.stop(receipt)
        self.assertEqual(stopped.classification, Classification.STOPPED)

    def test_cleanup_requires_stopped_exact_container_and_removes_only_retained_entries(self):
        receipt = self.started_receipt()
        self.native.callbacks["stop-container"] = self.stop_native_guest
        stopped = self.executor.stop(receipt)
        self.assertEqual(stopped.classification, Classification.STOPPED)
        assert stopped.receipt is not None
        self.native.callbacks["remove-container"] = lambda: setattr(
            self.native, "observation", None
        )
        removed = self.executor.remove(stopped.receipt)
        self.assertEqual(removed.classification, Classification.REMOVED)
        self.assertFalse(self.directory.exists())
        self.assertEqual(self.journal()["stage"], "removed")
        self.assertIsNotNone(removed.receipt)
        self.assertTrue(self.request.image.path.exists())
        self.assertTrue(self.checkout.exists())

    def test_cleanup_refuses_running_guest_foreign_entries_and_replacements(self):
        for changed in ("running", "foreign-entry", "key"):
            with self.subTest(changed=changed):
                self.new_fixture()
                receipt = self.started_receipt()
                if changed != "running":
                    self.native.callbacks["stop-container"] = self.stop_native_guest
                    stopped = self.executor.stop(receipt)
                    assert stopped.receipt is not None
                    receipt = stopped.receipt
                if changed == "foreign-entry":
                    (self.directory / "unowned").touch(mode=0o600)
                elif changed == "key":
                    key = self.directory / "credentials/id_ed25519"
                    key.unlink()
                    key.touch(mode=0o600)
                    key.write_bytes(b"replacement fixture")
                start = len(self.native.calls)
                removed = self.executor.remove(receipt)
                self.assertIn(
                    removed.classification,
                    (
                        Classification.INVALID_STATE,
                        Classification.OWNERSHIP_MISMATCH,
                    ),
                )
                self.assertNotIn(
                    "remove-container", [name for name, _ in self.native.calls[start:]]
                )
                self.assertTrue(self.directory.exists())

    def test_stop_interruption_retains_durable_intent_and_never_removes_guest(self):
        receipt = self.started_receipt()
        self.native.errors["stop-container"] = KeyboardInterrupt()
        with self.assertRaises(KeyboardInterrupt):
            self.executor.stop(receipt)
        self.assertEqual(self.journal()["stage"], "stop-pending")
        self.assertNotIn("remove-container", [name for name, _ in self.native.calls])
        self.assertTrue(self.directory.exists())

    def test_remove_checks_absence_and_preserves_files_when_native_result_is_lost(self):
        receipt = self.started_receipt()
        self.native.callbacks["stop-container"] = self.stop_native_guest
        stopped = self.executor.stop(receipt)
        assert stopped.receipt is not None
        removed = self.executor.remove(stopped.receipt)
        self.assertEqual(removed.classification, Classification.CLEANUP_INCOMPLETE)
        self.assertEqual(self.journal()["stage"], "remove-pending")
        self.assertTrue((self.directory / "credentials/id_ed25519").exists())

    def test_stop_sync_uncertainty_prevents_native_effect_and_preserves_intent(self):
        receipt = self.started_receipt()

        def fail_sync(_fd):
            raise OSError("private sync diagnostic")

        before = len(self.native.calls)
        with patch("os.fsync", side_effect=fail_sync):
            stopped = self.executor.stop(receipt)
        self.assertEqual(stopped.classification, Classification.CLEANUP_INCOMPLETE)
        self.assertNotIn("stop-container", [name for name, _ in self.native.calls[before:]])
        self.assertEqual(self.journal()["stage"], "stop-pending")
        self.assertIsNotNone(stopped.receipt)

    def test_unsynced_final_stopped_receipt_never_authorizes_removal(self):
        for kind in ("journal", "directory"):
            with self.subTest(kind=kind):
                self.new_fixture()
                receipt = self.started_receipt()
                self.native.callbacks["stop-container"] = self.stop_native_guest
                real_fsync = os.fsync
                failed = []

                def fail_final_stop(fd):
                    observed = os.fstat(fd)
                    journal = self.journal()
                    target = (
                        stat.S_ISREG(observed.st_mode) and state_document(fd, self.state) == journal
                        if kind == "journal"
                        else (observed.st_dev, observed.st_ino)
                        == (self.state_identity.device, self.state_identity.inode)
                    )
                    if journal["stage"] == "stopped" and target:
                        failed.append(kind)
                        raise OSError("controlled final stopped synchronization failure")
                    real_fsync(fd)

                with patch("os.fsync", side_effect=fail_final_stop):
                    stopped = self.executor.stop(receipt)
                self.assertEqual(failed, [kind])
                self.assertEqual(stopped.classification, Classification.CLEANUP_INCOMPLETE)
                self.assertEqual(self.journal()["stage"], "stopped")
                assert stopped.receipt is not None
                self.native.callbacks["remove-container"] = lambda: setattr(
                    self.native, "observation", None
                )
                before = len(self.native.calls)
                removed = self.executor.remove(stopped.receipt)
                self.assertEqual(removed.classification, Classification.INVALID_STATE)
                self.assertEqual(self.native.calls[before:], [])
                self.assertTrue((self.directory / "os.qcow2").exists())
                self.assertTrue((self.directory / "credentials/id_ed25519").exists())
                self.assertEqual(self.journal()["stage"], "stopped")

    def test_final_cleanup_lookup_race_preserves_replacement_for_review(self):
        receipt = self.started_receipt()
        self.native.callbacks["stop-container"] = self.stop_native_guest
        stopped = self.executor.stop(receipt)
        assert stopped.receipt is not None
        self.native.callbacks["remove-container"] = lambda: setattr(
            self.native, "observation", None
        )
        credentials = (self.directory / "credentials").stat()
        real_stat = os.stat
        replaced = []
        unrelated = b"unrelated nonsecret cleanup race control"
        outside = os.open(self.root, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
        self.addCleanup(os.close, outside)

        def replace_after_lookup(name, *args, **kwargs):
            observed = real_stat(name, *args, **kwargs)
            parent = kwargs.get("dir_fd")
            if name == "id_ed25519" and parent is not None and not replaced:
                stamp = os.fstat(parent)
                if (stamp.st_dev, stamp.st_ino) == (credentials.st_dev, credentials.st_ino):
                    os.rename(name, "retained-original", src_dir_fd=parent, dst_dir_fd=outside)
                    write_private(parent, name, unrelated)
                    replaced.append(True)
            return observed

        with patch("os.stat", side_effect=replace_after_lookup):
            removed = self.executor.remove(stopped.receipt)
        self.assertEqual(replaced, [True])
        self.assertEqual(removed.classification, Classification.CLEANUP_INCOMPLETE)
        assert removed.receipt is not None and removed.receipt.cleanup_directory is not None
        self.assertEqual(
            self.journal()["cleanup_directory"]["inode"], removed.receipt.cleanup_directory.inode
        )
        surviving = [
            path
            for path in self.root.rglob("*")
            if path.is_file() and path.read_bytes() == unrelated
        ]
        self.assertEqual(len(surviving), 1)
        self.assertTrue((self.root / "retained-original").exists())
        self.assertNotEqual(self.journal()["stage"], "removed")

    def test_atomic_detachment_never_overwrites_an_existing_cleanup_entry(self):
        receipt = self.started_receipt()
        self.native.callbacks["stop-container"] = self.stop_native_guest
        stopped = self.executor.stop(receipt)
        assert stopped.receipt is not None
        self.native.callbacks["remove-container"] = lambda: setattr(
            self.native, "observation", None
        )
        original = executor_module._rename_noreplace
        injected = []
        control = b"unrelated no-overwrite control"

        def collide(source_fd, source, destination_fd, destination):
            if not injected:
                write_private(destination_fd, destination, control)
                injected.append(True)
            original(source_fd, source, destination_fd, destination)

        with patch.object(executor_module, "_rename_noreplace", side_effect=collide):
            removed = self.executor.remove(stopped.receipt)
        self.assertEqual(injected, [True])
        self.assertEqual(removed.classification, Classification.CLEANUP_INCOMPLETE)
        assert removed.receipt is not None and removed.receipt.cleanup_directory is not None
        cleanup = removed.receipt.cleanup_directory
        self.assertEqual(self.journal()["cleanup_directory"]["inode"], cleanup.inode)
        self.assertEqual((cleanup.path / "entry-1").read_bytes(), control)
        self.assertTrue((self.directory / "credentials/id_ed25519").exists())
        self.assertEqual(self.journal()["stage"], "detach-pending")

    def test_replaced_directory_at_detachment_is_preserved_with_its_contents(self):
        for target in ("credentials", "vm"):
            with self.subTest(target=target):
                self.new_fixture()
                receipt = self.started_receipt()
                self.native.callbacks["stop-container"] = self.stop_native_guest
                stopped = self.executor.stop(receipt)
                assert stopped.receipt is not None
                self.native.callbacks["remove-container"] = lambda: setattr(
                    self.native, "observation", None
                )
                original = executor_module._rename_noreplace
                injected = []
                outside = os.open(self.root, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
                self.addCleanup(os.close, outside)

                def substitute(source_fd, source, destination_fd, destination):
                    if destination == target and not injected:
                        os.rename(
                            source,
                            "retained-empty-directory",
                            src_dir_fd=source_fd,
                            dst_dir_fd=outside,
                        )
                        os.mkdir(source, 0o700, dir_fd=source_fd)
                        fd = os.open(
                            source,
                            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                            dir_fd=source_fd,
                        )
                        try:
                            write_private(fd, "unrelated", b"unrelated directory control")
                        finally:
                            os.close(fd)
                        injected.append(True)
                    original(source_fd, source, destination_fd, destination)

                with patch.object(executor_module, "_rename_noreplace", side_effect=substitute):
                    removed = self.executor.remove(stopped.receipt)
                self.assertEqual(injected, [True])
                self.assertEqual(removed.classification, Classification.CLEANUP_INCOMPLETE)
                assert removed.receipt is not None and removed.receipt.cleanup_directory is not None
                cleanup = removed.receipt.cleanup_directory
                self.assertEqual(
                    (cleanup.path / target / "unrelated").read_bytes(),
                    b"unrelated directory control",
                )
                self.assertTrue((self.root / "retained-empty-directory").is_dir())
                self.assertEqual(self.journal()["stage"], "detach-pending")

    def test_unsynced_detachment_keeps_original_inode_for_review(self):
        receipt = self.started_receipt()
        self.native.callbacks["stop-container"] = self.stop_native_guest
        stopped = self.executor.stop(receipt)
        assert stopped.receipt is not None
        key = (self.directory / "credentials/id_ed25519").stat()
        self.native.callbacks["remove-container"] = lambda: setattr(
            self.native, "observation", None
        )
        real_fsync = os.fsync
        failed = []

        def fail_detachment_sync(fd):
            if self.journal()["stage"] == "detach-pending" and stat.S_ISDIR(os.fstat(fd).st_mode):
                names = os.listdir(fd)
                if "entry-1" in names:
                    failed.append(True)
                    raise OSError("controlled detachment synchronization failure")
            real_fsync(fd)

        with patch("os.fsync", side_effect=fail_detachment_sync):
            removed = self.executor.remove(stopped.receipt)
        self.assertEqual(failed, [True])
        self.assertEqual(removed.classification, Classification.CLEANUP_INCOMPLETE)
        assert removed.receipt is not None and removed.receipt.cleanup_directory is not None
        cleanup = removed.receipt.cleanup_directory
        observed = (cleanup.path / "entry-1").stat()
        self.assertEqual((observed.st_dev, observed.st_ino), (key.st_dev, key.st_ino))
        self.assertTrue((self.directory / "os.qcow2").exists())
        self.assertEqual(
            self.executor.remove(removed.receipt).classification, Classification.INVALID_STATE
        )
        self.assertEqual(self.journal()["stage"], "detach-pending")

    def test_remove_native_inspection_failure_never_becomes_absence(self):
        receipt = self.started_receipt()
        self.native.callbacks["stop-container"] = self.stop_native_guest
        stopped = self.executor.stop(receipt)
        assert stopped.receipt is not None

        def fail_inspection():
            self.native.errors["inspect"] = OSError("private inspection diagnostic")

        self.native.callbacks["remove-container"] = fail_inspection
        removed = self.executor.remove(stopped.receipt)
        self.assertEqual(removed.classification, Classification.CLEANUP_INCOMPLETE)
        self.assertTrue((self.directory / "os.qcow2").exists())
        self.assertEqual(self.journal()["stage"], "remove-pending")

    def test_remove_revalidates_after_native_removal_before_unlinking_files(self):
        receipt = self.started_receipt()
        self.native.callbacks["stop-container"] = self.stop_native_guest
        stopped = self.executor.stop(receipt)
        assert stopped.receipt is not None

        def replace_key():
            self.native.observation = None
            key = self.directory / "credentials/id_ed25519"
            key.rename(self.root / "retained-original-key")
            key.touch(mode=0o600)
            key.write_bytes(b"replacement control data")

        self.native.callbacks["remove-container"] = replace_key
        removed = self.executor.remove(stopped.receipt)
        self.assertEqual(removed.classification, Classification.CLEANUP_INCOMPLETE)
        self.assertTrue((self.directory / "credentials/id_ed25519").exists())
        self.assertTrue((self.directory / "os.qcow2").exists())

    def test_stopped_receipt_cannot_be_restarted_or_removed_twice(self):
        receipt = self.started_receipt()
        self.native.callbacks["stop-container"] = self.stop_native_guest
        stopped = self.executor.stop(receipt)
        assert stopped.receipt is not None
        self.assertEqual(
            self.executor.start(stopped.receipt).classification, Classification.INVALID_STATE
        )
        self.native.callbacks["remove-container"] = lambda: setattr(
            self.native, "observation", None
        )
        removed = self.executor.remove(stopped.receipt)
        self.assertEqual(removed.classification, Classification.REMOVED)
        assert removed.receipt is not None
        before = len(self.native.calls)
        self.assertEqual(
            self.executor.remove(removed.receipt).classification, Classification.INVALID_STATE
        )
        self.assertEqual(len(self.native.calls), before)

    def test_remove_never_adopts_an_unretained_runtime_socket_as_cleanup_authority(self):
        self.new_fixture(temporary_directory="/tmp")
        receipt = self.started_receipt()
        self.native.callbacks["stop-container"] = self.stop_native_guest
        stopped = self.executor.stop(receipt)
        assert stopped.receipt is not None
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as control:
            control.bind(str(self.directory / "qmp.sock"))
            (self.directory / "qmp.sock").chmod(0o700)
            before = len(self.native.calls)
            removed = self.executor.remove(stopped.receipt)
            self.assertEqual(removed.classification, Classification.CLEANUP_INCOMPLETE)
            self.assertNotIn("remove-container", [name for name, _ in self.native.calls[before:]])
            self.assertTrue((self.directory / "qmp.sock").exists())
            self.assertTrue((self.directory / "credentials/id_ed25519").exists())


if __name__ == "__main__":
    unittest.main()
