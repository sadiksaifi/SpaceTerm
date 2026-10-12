"""Admitted descriptor-based prepare and exact-ID create-before-start execution.

Stop/remove require the retained started receipt, exact native policy, observed
shutdown and descriptor-relative cleanup. No method accepts commands, Docker
flags, guest code or cleanup paths. STARTED means the admitted native start
returned, not guest/KVM readiness.
The nonexecuting planner's proposals are never consumed as execution authority.
"""

import ctypes
import errno
import fcntl
import hashlib
import json
import os
import secrets
import stat
import sys
from contextlib import ExitStack
from dataclasses import dataclass, field, replace
from pathlib import Path
from typing import Protocol

from spaceterm_tasks import ROOT
from spaceterm_tasks import server_vm as _vm
from spaceterm_tasks.server_vm import (
    Classification,
    ContainerInspector,
    ContainerObservation,
    DiskEvidence,
    ImageEvidence,
    OwnedPath,
    Ownership,
    Phase,
    PreparedVm,
    PrepareRequest,
    RunnerIdentity,
    SeedEvidence,
    SignatureVerifier,
    VmArtifacts,
    VmPlanner,
)

JOURNAL_NAME = "journal.json"
JOURNAL_SCHEMA = 1
_VM_NAMES = frozenset(("credentials", "os.qcow2", "encrypted.qcow2", "seed.iso", _vm.MANIFEST_NAME))
_CREDENTIAL_NAMES = frozenset(("id_ed25519", "id_ed25519.pub"))
_HOST_CREDENTIAL_NAMES = frozenset(("host_ed25519", "host_ed25519.pub"))
_STATE_NAMES = frozenset((JOURNAL_NAME,))
_RUNTIME_NAMES = frozenset(("qmp.sock",))


@dataclass(frozen=True, slots=True)
class ExecutionReceipt:
    """Independent retained authority outside every checkout and the VM bind.

    The bounded journal records intent, token, original roots, created paths and
    exact container ID. Each committed record requires file and directory fsync.
    A failed sync remains uncertain; this increment does not resume or retry it.
    The "created" record is intent to inspect and start that exact object, never a
    name or label lookup. Failed preparation exposes cleanup evidence only. If
    initial journal validation fails, journal retains only the proven allocated
    inode, without claiming that an initial record exists or was synchronized.

    directory and checkout retain the initially admitted identities and ancestry,
    even when the first credential allocation fails before any created path is
    admitted. Missing roots in an older receipt do not grant execution authority.
    created_paths retains partial preparation without inventing a PreparedVm.
    image_evidence preserves authenticated bytes and private directory observations
    across prepare/start. Observations do not prove atomic bytes or mutation history.
    No manifest, path-only recursive deletion, or returned proposal grants authority.
    """

    state_directory: OwnedPath = field(repr=False)
    journal: OwnedPath = field(repr=False)
    token: str = field(repr=False)
    created_paths: tuple[OwnedPath, ...] = field(repr=False)
    prepared: PreparedVm | None = field(repr=False)
    container_id: str | None = field(default=None, repr=False)
    image_evidence: ImageEvidence | None = field(default=None, repr=False)
    directory: OwnedPath | None = field(default=None, repr=False)
    checkout: OwnedPath | None = field(default=None, repr=False)
    cleanup_directory: OwnedPath | None = field(default=None, repr=False)


@dataclass(frozen=True, slots=True)
class ExecutionResult:
    """Public diagnostics expose classifications only, not native messages or paths."""

    phase: Phase
    classification: Classification
    receipt: ExecutionReceipt | None = field(default=None, repr=False)


class VmNative(ContainerInspector, Protocol):
    """inspect returns None only for positively observed exact-ID absence.

    Inspection failures raise instead; unavailable evidence is never absence.
    Intentional operations on held files or independently retained exact IDs.

    Adapters enforce capacity admission, deadlines and output bounds without host
    changes. They do not reopen downloaded paths, normalize away effective policy,
    or treat a timed-out Docker client as cancellation of the daemon operation.
    Exact-ID operations retain one independently admitted local daemon context;
    adapters must not resolve inherited Docker contexts or environments per call.
    A real adapter and authenticated runner construction remain separate native work.
    """

    def create_credentials(self, directory_fd: int) -> None:
        """Create only disposable fixed-name credentials beneath the held VM directory."""
        ...

    def create_os_disk(self, directory_fd: int, image_fd: int, virtual_size_bytes: int) -> None:
        """Create standalone os.qcow2 from held authenticated bytes, without backing."""
        ...

    def create_encrypted_disk(self, directory_fd: int, virtual_size_bytes: int) -> None:
        """Create encrypted.qcow2 at the admitted virtual size; this is not LUKS."""
        ...

    def inspect_disk(self, disk_fd: int, retained: OwnedPath) -> DiskEvidence:
        """Inspect this offline disk without reopening its path or repairing it."""
        ...

    def create_seed(self, directory_fd: int, token: str) -> None:
        """Create bounded trusted NoCloud seed.iso, CIDATA, using disposable keys."""
        ...

    def inspect_seed(self, seed_fd: int, retained: OwnedPath) -> SeedEvidence:
        """Inspect the held ISO label and bytes; never execute downloaded code."""
        ...

    def create_container(self, vm: PreparedVm, runner: RunnerIdentity) -> str | None:
        """Create, never start, with the fixed complete runner policy and no pull.

        Return an exact 64-character lowercase ID. A lost or failed response does
        not prove absence. Never reconcile unknown ownership by name or label.
        """
        ...

    def start_container(self, container_id: str) -> None:
        """Start this exact ID only after its durable record and final admission."""
        ...

    def stop_container(self, container_id: str, timeout_seconds: int) -> None:
        """Bounded exact-ID fallback after guest power-down; no host reboot/logout."""
        ...

    def remove_container(self, container_id: str) -> None:
        """Remove only a proven stopped ID, without force, volumes or prune."""
        ...


def _created_record(owned: OwnedPath) -> dict[str, object]:
    return {
        **_vm._path_record(owned),
        "ancestors": [list(pair) for pair in owned.ancestors],
        "content_sha256": owned.content_sha256,
    }


def _record(
    stage: str,
    token: str,
    container_id: str | None,
    paths: tuple[OwnedPath, ...],
    *,
    directory: OwnedPath,
    checkout: OwnedPath,
    cleanup_directory: OwnedPath | None = None,
) -> dict:
    document = {
        "schema": JOURNAL_SCHEMA,
        "stage": stage,
        "token": token,
        "container_id": container_id,
        "directory": _created_record(directory),
        "checkout": _created_record(checkout),
        "created_paths": [_created_record(path) for path in paths],
    }
    if cleanup_directory is not None:
        document["cleanup_directory"] = _created_record(cleanup_directory)
    return document


def _rename_noreplace(source_fd: int, source: str, destination_fd: int, destination: str) -> None:
    """Atomic detachment, never an overwrite or a check-then-rename fallback."""
    if sys.platform == "linux":
        native = ctypes.CDLL("libc.so.6", use_errno=True)
        operation = native.renameat2
        flags = 1  # RENAME_NOREPLACE
    elif sys.platform == "darwin":
        native = ctypes.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True)
        operation = native.renameatx_np
        flags = 4  # RENAME_EXCL
    else:
        raise _vm._Denied(Classification.NATIVE_UNAVAILABLE)
    operation.argtypes = (
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_uint,
    )
    operation.restype = ctypes.c_int
    if operation(source_fd, os.fsencode(source), destination_fd, os.fsencode(destination), flags):
        raise OSError(ctypes.get_errno(), "native detachment failed")


def _document_bytes(document: dict) -> bytes:
    data = json.dumps(document, ensure_ascii=True, separators=(",", ":"), allow_nan=False).encode(
        "ascii"
    )
    if not 0 < len(data) <= _vm._MAX_DOCUMENT_BYTES:
        raise _vm._Denied(Classification.INVALID_RESOURCES)
    return data


def _parse_document(data: bytes) -> object:
    try:
        return json.loads(
            data,
            object_pairs_hook=_vm._unique_json_object,
            parse_constant=_vm._invalid_json_constant,
        )
    except (ValueError, UnicodeError, RecursionError):
        raise _vm._Denied(Classification.OWNERSHIP_MISMATCH) from None


def _write_bytes(fd: int, data: bytes) -> None:
    os.lseek(fd, 0, os.SEEK_SET)
    remaining = memoryview(data)
    while remaining:
        written = os.write(fd, remaining)
        if written <= 0:
            raise _vm._Denied(Classification.NATIVE_UNAVAILABLE)
        remaining = remaining[written:]
    os.ftruncate(fd, len(data))


class _Journal:
    """One held bounded record, updated in place without changing its authority.

    The execution holds exclusive namespace locks before this record is opened.
    A partial write or either sync failure closes execution. There is no rollback,
    successful retry claim, automatic resume, or adoption from the on-disk record.
    """

    def __init__(self, execution: "_Execution", retained: OwnedPath | None = None):
        self.execution = execution
        self.expected: dict | None = None
        self.synchronized = retained is not None
        failure = Classification.OWNERSHIP_MISMATCH
        state = execution.state
        flags = os.O_RDWR | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK
        if retained is None:
            flags |= os.O_CREAT | os.O_EXCL
        self.fd = os.open(JOURNAL_NAME, flags, 0o600, dir_fd=state.fd)
        execution.handles.enter_context(_vm._closing_descriptor(self.fd))
        with _vm._open_path(state.owned.path / JOURNAL_NAME, failure) as current:
            if _vm._stamp(current.observation) != _vm._stamp(os.fstat(self.fd)):
                raise _vm._Denied(failure)
            if retained is not None and current.owned != retained:
                raise _vm._Denied(failure)
            if retained is None:
                # Constructor assignment has not completed. Retain the proved
                # allocated inode before any further validation or release.
                # It grants cleanup scope, never prepared or replacement authority.
                execution.journal_identity = current.owned
            self.opened = replace(current, fd=self.fd)
            _vm._private(self.opened, directory=False, failure=failure)
            self.retained = self.opened.owned
        if retained is not None:
            data = _vm._read_bounded(self.opened, _vm._MAX_DOCUMENT_BYTES, failure)
            if hashlib.sha256(data).hexdigest() != retained.content_sha256:
                raise _vm._Denied(failure)
            self.retained = retained
        else:
            execution.refresh_directory(state=True)

    def check(self) -> None:
        failure = Classification.OWNERSHIP_MISMATCH
        _vm._private(self.opened, directory=False, failure=failure)
        _vm._unchanged(self.opened, failure)
        data = _vm._read_bounded(self.opened, _vm._MAX_DOCUMENT_BYTES, failure)
        if hashlib.sha256(
            data
        ).hexdigest() != self.retained.content_sha256 or not _vm._same_typed_value(
            _parse_document(data), self.expected
        ):
            raise _vm._Denied(failure)

    def commit(self, stage: str) -> None:
        execution = self.execution
        execution.check_state()
        if self.expected is not None:
            self.check()
        document = _record(
            stage,
            execution.token,
            execution.container_id,
            execution.created_paths,
            directory=execution.directory_identity,
            checkout=execution.checkout_identity,
            cleanup_directory=(
                execution.cleanup_directory.owned
                if execution.cleanup_directory is not None
                else None
            ),
        )
        data = _document_bytes(document)
        # Visible bytes cannot admit the next lifecycle operation after a failed
        # write, sync or final validation. The receipt retains cleanup scope only.
        self.synchronized = False
        _write_bytes(self.fd, data)
        self.opened = replace(self.opened, observation=os.fstat(self.fd))
        self.retained = replace(self.opened.owned, content_sha256=hashlib.sha256(data).hexdigest())
        self.expected = document
        _vm._private(self.opened, directory=False, failure=Classification.OWNERSHIP_MISMATCH)
        _vm._unchanged(self.opened, Classification.OWNERSHIP_MISMATCH)
        os.fsync(self.fd)
        os.fsync(execution.state.fd)
        self.check()
        execution.check_state()
        self.synchronized = True


class _Execution:
    """Descriptor-held authority and exclusive namespaces for one invocation.

    Nonblocking flock claims the independently admitted state and VM directories
    without creating a lock file or changing either inode. Both descriptors stay
    open through admission, every native effect, and the last journal transition.
    Separate invocations and processes cannot read or overwrite a journal while
    its owner is executing. A busy contender is rejected rather than resumed.
    Shared filesystem policy stays in server_vm.
    """

    def __init__(
        self,
        planner: VmPlanner,
        handles: ExitStack,
        request: PrepareRequest,
        state: OwnedPath,
        *,
        directory: OwnedPath | None = None,
        checkout: OwnedPath | None = None,
        runtime_started: bool = False,
    ):
        self.planner, self.handles, self.request = planner, handles, request
        failure = Classification.OWNERSHIP_MISMATCH
        if not _vm._valid_owned_path(state):
            raise _vm._Denied(failure)
        self.state = handles.enter_context(_vm._open_path(state.path, failure))
        if self.state.owned != state:
            raise _vm._Denied(failure)
        self.directory = handles.enter_context(
            _vm._open_path(request.directory, Classification.UNSAFE_PATH)
        )
        self.checkout = handles.enter_context(_vm._open_path(planner._checkout, failure))
        if (directory is not None and self.directory.owned != directory) or (
            checkout is not None and self.checkout.owned != checkout
        ):
            raise _vm._Denied(failure)
        self.state_identity = state
        self.directory_identity = self.directory.owned
        self.checkout_identity = self.checkout.owned
        self.paths: dict[Path, _vm._OpenedPath] = {}
        self.token = ""
        self.container_id: str | None = None
        self.prepared: PreparedVm | None = None
        self.prepared_admitted = False
        self.image_evidence: ImageEvidence | None = None
        self.image: tuple[_vm._OpenedPath, ...] = ()
        self.journal: _Journal | None = None
        self.journal_identity: OwnedPath | None = None
        self.cleanup_directory: _vm._OpenedPath | None = None
        self.start_attempted = runtime_started
        self.check_roots(runtime_started=runtime_started)
        for opened in (self.state, self.directory):
            try:
                fcntl.flock(opened.fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError as error:
                if error.errno in (errno.EACCES, errno.EAGAIN):
                    raise _vm._Denied(failure) from None
                raise
        # Never adopt a newer stamp or stage after obtaining the namespace claim.
        # ExitStack closes the descriptors and releases both locks on every exit,
        # including KeyboardInterrupt and SystemExit.
        self.check_roots(runtime_started=runtime_started)

    @property
    def created_paths(self) -> tuple[OwnedPath, ...]:
        return tuple(opened.owned for opened in self.paths.values())

    def file_limit(self, path: Path) -> int:
        """Bound pre-start streaming by fixed artifact policy, not document size."""
        if path.parent == self.request.directory:
            if path.name in ("os.qcow2", "encrypted.qcow2"):
                gib = (
                    self.request.resources.disk_gib
                    if path.name == "os.qcow2"
                    else self.request.resources.encrypted_disk_gib
                )
                return gib * 1024**3 + _vm._MAX_DISK_OVERHEAD_BYTES
            if path.name == "seed.iso":
                return _vm._MAX_SEED_BYTES
        return _vm._MAX_DOCUMENT_BYTES

    def check_state(self) -> None:
        """A returned ID can be recorded despite lost bind/source authority.

        Recording uncertainty in the independent state does not admit a VM effect.
        The complete roots, bytes and policy are still required before every native
        effect. A replaced/unsafe state directory cannot receive even this record.
        """
        failure = Classification.OWNERSHIP_MISMATCH
        self.planner._checkout_matches(failure)
        _vm._unchanged(self.checkout, failure)
        _vm._private(self.state, directory=True, failure=failure)
        _vm._unchanged(self.state, failure)
        self.planner._outside_checkout(self.state.owned.path, Classification.UNSAFE_PATH)
        state, bind = self.state.owned.path, self.directory.owned.path
        downloads = (
            self.request.image.path.parent,
            self.request.image.checksums.parent,
            self.request.image.signature.parent,
        )
        for other in (bind, *downloads):
            if state.is_relative_to(other) or other.is_relative_to(state):
                raise _vm._Denied(Classification.UNSAFE_PATH)

    def check_roots(self, *, runtime_started: bool = False) -> None:
        self.check_state()
        self.planner._configuration(self.request)
        _vm._private(self.directory, directory=True, failure=Classification.OWNERSHIP_MISMATCH)
        if runtime_started:
            _vm._unchanged_directory(self.directory, Classification.OWNERSHIP_MISMATCH)
        else:
            _vm._unchanged(self.directory, Classification.OWNERSHIP_MISMATCH)
        self.planner._outside_checkout(self.directory.owned.path, Classification.UNSAFE_PATH)

    def refresh_directory(self, *, state: bool = False) -> None:
        """Accept entry changes, retaining the original identity and protection."""
        opened = self.state if state else self.directory
        failure = Classification.OWNERSHIP_MISMATCH
        _vm._unchanged_directory(opened, failure)
        updated = replace(opened, observation=os.fstat(opened.fd))
        if _vm._directory_protection(updated.observation) != _vm._directory_protection(
            opened.observation
        ):
            raise _vm._Denied(failure)
        if self.start_attempted and not state:
            # The admitted child can bind qmp.sock after this observation too.
            # Entry enumeration, not directory timestamps, validates runtime names.
            _vm._unchanged_directory(updated, failure)
        else:
            _vm._unchanged(updated, failure)
        if state:
            self.state = updated
        else:
            self.directory = updated

    def expected_names(self, creating: str | None = None) -> frozenset[str]:
        expected = frozenset(
            name for name in _VM_NAMES if self.request.directory / name in self.paths
        )
        if creating is not None:
            if creating not in _VM_NAMES:
                raise _vm._Denied(Classification.OWNERSHIP_MISMATCH)
            expected |= frozenset((creating,))
        return expected

    def check_namespaces(
        self, *, runtime_started: bool = False, creating: str | None = None
    ) -> None:
        failure = Classification.OWNERSHIP_MISMATCH
        actual = _vm._directory_names(
            self.directory.fd,
            self.expected_names(creating),
            failure,
            optional=_RUNTIME_NAMES if runtime_started else frozenset(),
        )
        if "qmp.sock" in actual:
            socket = os.stat("qmp.sock", dir_fd=self.directory.fd, follow_symlinks=False)
            if (
                not stat.S_ISSOCK(socket.st_mode)
                or socket.st_uid != os.geteuid()
                or stat.S_IMODE(socket.st_mode) & 0o077
            ):
                raise _vm._Denied(failure)
        credentials = self.paths.get(self.request.directory / "credentials")
        if credentials is not None:
            _vm._directory_names(
                credentials.fd, _CREDENTIAL_NAMES, failure, optional=_HOST_CREDENTIAL_NAMES
            )
        if self.journal is not None:
            expected_state = _STATE_NAMES
            if self.cleanup_directory is not None:
                expected_state |= frozenset((self.cleanup_directory.owned.path.name,))
                _vm._unchanged_directory(self.cleanup_directory, failure)
            _vm._directory_names(self.state.fd, expected_state, failure)
            self.journal.check()

    def check(self, *, runtime_started: bool = False, creating: str | None = None) -> None:
        self.check_roots(runtime_started=runtime_started)
        self.check_namespaces(runtime_started=runtime_started, creating=creating)
        failure = Classification.OWNERSHIP_MISMATCH
        for path, opened in self.paths.items():
            if runtime_started and path.name in ("os.qcow2", "encrypted.qcow2"):
                # QEMU can write disk bytes, but cannot replace the retained object or
                # change its protection. Do not run offline inspection on a live disk.
                _vm._private(opened, directory=False, failure=failure)
                with _vm._open_path(path, failure) as current:
                    _vm._private(current, directory=False, failure=failure)
                    if current.owned != opened.owned:
                        raise _vm._Denied(failure)
                    gib = (
                        self.request.resources.disk_gib
                        if path.name == "os.qcow2"
                        else self.request.resources.encrypted_disk_gib
                    )
                    maximum = gib * 1024**3 + _vm._MAX_DISK_OVERHEAD_BYTES
                    if (
                        not 0 < current.observation.st_size <= maximum
                        or current.observation.st_blocks * 512 > maximum
                    ):
                        raise _vm._Denied(Classification.INVALID_RESOURCES)
                    _vm._private(current, directory=False, failure=failure)
                continue
            _vm._private(
                opened, directory=stat.S_ISDIR(opened.observation.st_mode), failure=failure
            )
            _vm._unchanged(opened, failure)
            if opened.owned.content_sha256 is not None:
                _, digest = _vm._hash_file(opened, self.file_limit(path), failure)
                if digest != opened.owned.content_sha256:
                    raise _vm._Denied(failure)
                _vm._unchanged(opened, failure)
        if self.image_evidence is not None:
            for opened in self.image:
                _vm._unchanged(opened, Classification.UNVERIFIED_IMAGE)
            self.planner._image_matches(self.image_evidence)
        if self.prepared is not None:
            self.planner._ownership(self.prepared, runtime_started=runtime_started)
            if not runtime_started:
                with ExitStack() as handles:
                    self.planner._hold_artifacts(self.prepared, handles)
        # Recheck after the held-byte and ownership reads. Runtime entry changes
        # cannot excuse replaced roots, foreign entries or altered protection.
        self.check_roots(runtime_started=runtime_started)
        self.check_namespaces(runtime_started=runtime_started, creating=creating)

    def authenticate(self, retained: ImageEvidence | None = None) -> None:
        evidence = self.planner._image(self.request.image, self.request.directory, self.handles)
        if retained is not None and not _vm._same_typed_value(evidence, retained):
            raise _vm._Denied(Classification.UNVERIFIED_IMAGE)
        self.image_evidence = evidence
        self.image = self.planner._hold_image(evidence, self.handles)
        self.check()

    def remember(
        self, path: Path, *, directory: bool = False, empty: bool = False
    ) -> _vm._OpenedPath:
        failure = Classification.OWNERSHIP_MISMATCH
        opened = self.handles.enter_context(_vm._open_path(path, failure))
        _vm._private(opened, directory=directory, failure=failure)
        # Keep the admitted inode for cleanup if its size or content read fails.
        self.paths[path] = opened
        if not directory and not empty:
            maximum = self.file_limit(path)
            if opened.observation.st_size > maximum:
                raise _vm._Denied(Classification.INVALID_RESOURCES)
            retained = _vm._retained_file(opened, failure, maximum=maximum)
            opened = replace(opened, owned=retained)
            self.paths[path] = opened
        return opened

    def admit_created(self, name: str, *, credentials: bool = False) -> _vm._OpenedPath:
        _vm._directory_names(
            self.directory.fd, self.expected_names(name), Classification.CLEANUP_INCOMPLETE
        )
        self.refresh_directory()
        self.check(creating=name)  # Previously retained bytes must survive the callback.
        opened = self.remember(self.request.directory / name, directory=credentials)
        if credentials:
            names = _vm._directory_names(
                opened.fd,
                _CREDENTIAL_NAMES,
                Classification.CLEANUP_INCOMPLETE,
                optional=_HOST_CREDENTIAL_NAMES,
            )
            if names & _HOST_CREDENTIAL_NAMES not in (frozenset(), _HOST_CREDENTIAL_NAMES):
                raise _vm._Denied(Classification.CLEANUP_INCOMPLETE)
            for leaf in ("id_ed25519", "id_ed25519.pub", "host_ed25519", "host_ed25519.pub"):
                if leaf not in names:
                    continue
                child = self.remember(opened.owned.path / leaf)
                os.fsync(child.fd)
            os.fsync(opened.fd)
        else:
            os.fsync(opened.fd)
        os.fsync(self.directory.fd)
        self.check()
        return opened

    def receipt(self) -> ExecutionReceipt | None:
        journal = self.journal.retained if self.journal is not None else self.journal_identity
        if journal is None:
            return None
        prepared = (
            self.prepared
            if self.prepared_admitted and self.journal is not None and self.journal.synchronized
            else None
        )
        if prepared is not None and self.container_id is not None:
            prepared = replace(
                prepared, ownership=replace(prepared.ownership, container_id=self.container_id)
            )
        return ExecutionReceipt(
            state_directory=self.state_identity,
            journal=journal,
            token=self.token,
            created_paths=self.created_paths,
            prepared=prepared,
            container_id=self.container_id,
            image_evidence=self.image_evidence,
            directory=self.directory_identity,
            checkout=self.checkout_identity,
            cleanup_directory=(
                self.cleanup_directory.owned if self.cleanup_directory is not None else None
            ),
        )

    def create_manifest(self) -> _vm._OpenedPath:
        self.check()
        flags = os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC
        fd = os.open(_vm.MANIFEST_NAME, flags, 0o600, dir_fd=self.directory.fd)
        self.handles.enter_context(_vm._closing_descriptor(fd))
        self.refresh_directory()
        self.check(creating=_vm.MANIFEST_NAME)
        opened = self.remember(self.request.directory / _vm.MANIFEST_NAME, empty=True)
        if _vm._stamp(os.fstat(fd)) != _vm._stamp(opened.observation):
            raise _vm._Denied(Classification.OWNERSHIP_MISMATCH)
        return opened

    def publish_manifest(self, vm: PreparedVm) -> PreparedVm:
        self.check()
        path = self.request.directory / _vm.MANIFEST_NAME
        flags = os.O_RDWR | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK
        fd = os.open(_vm.MANIFEST_NAME, flags, 0o600, dir_fd=self.directory.fd)
        self.handles.enter_context(_vm._closing_descriptor(fd))
        opened = self.paths[path]
        if _vm._stamp(os.fstat(fd)) != _vm._stamp(opened.observation):
            raise _vm._Denied(Classification.OWNERSHIP_MISMATCH)
        _write_bytes(fd, _document_bytes(_vm._manifest(vm)))
        updated = replace(opened, observation=os.fstat(opened.fd))
        updated = replace(
            updated, owned=_vm._retained_file(updated, Classification.OWNERSHIP_MISMATCH)
        )
        self.paths[path] = updated
        os.fsync(fd)
        os.fsync(self.directory.fd)
        prepared = replace(vm, ownership=replace(vm.ownership, manifest=updated.owned))
        self.prepared = prepared
        self.check()
        return prepared


class VmExecutor:
    """Admit independently pinned inputs, then retain descriptors through each effect.

    prepare takes a fresh independently captured private state directory. start
    accepts a retained prepared receipt, not a manifest or proposal. stop and remove
    require the original started and stopped receipts respectively. Interrupted
    phases and uncertain durability require reviewed recovery, not re-execution.
    """

    def __init__(
        self,
        checkout: Path = ROOT,
        signatures: SignatureVerifier | None = None,
        native: VmNative | None = None,
        runner: RunnerIdentity | None = None,
        runner_image_pin: str | None = None,
        runner_image_id_pin: str | None = None,
    ):
        self._native = native
        self._runner = runner
        self._planner = VmPlanner(
            checkout, signatures, native, runner, runner_image_pin, runner_image_id_pin
        )

    def _disk(self, execution: _Execution, opened: _vm._OpenedPath, size: int) -> DiskEvidence:
        assert self._native is not None
        execution.check()
        disk = self._native.inspect_disk(opened.fd, opened.owned)
        execution.check()
        if not isinstance(disk, DiskEvidence) or not _vm._same_typed_value(disk.file, opened.owned):
            raise _vm._Denied(Classification.OWNERSHIP_MISMATCH)
        _vm._admit_disk(disk, opened, size)
        return disk

    def _seed(self, execution: _Execution, opened: _vm._OpenedPath) -> SeedEvidence:
        assert self._native is not None
        execution.check()
        seed = self._native.inspect_seed(opened.fd, opened.owned)
        execution.check()
        if not isinstance(seed, SeedEvidence) or not _vm._same_typed_value(seed.file, opened.owned):
            raise _vm._Denied(Classification.OWNERSHIP_MISMATCH)
        _vm._admit_seed(seed, opened)
        return seed

    def prepare(self, request: PrepareRequest, *, state_directory: OwnedPath) -> ExecutionResult:
        execution = None
        try:
            self._planner._configuration(request)
            if self._native is None:
                raise _vm._Denied(Classification.NATIVE_UNAVAILABLE)
            with ExitStack() as handles:
                execution = _Execution(self._planner, handles, request, state_directory)
                if not _vm._empty_directory(execution.directory.fd) or not _vm._empty_directory(
                    execution.state.fd
                ):
                    raise _vm._Denied(Classification.UNSAFE_PATH)
                execution.authenticate()
                execution.token = secrets.token_hex(32)
                execution.journal = _Journal(execution)
                execution.journal.commit("credentials-pending")
                execution.check()
                self._native.create_credentials(execution.directory.fd)
                execution.admit_created("credentials", credentials=True)
                execution.journal.commit("credentials-created")

                size = request.resources.disk_gib * 1024**3
                execution.journal.commit("os-disk-pending")
                execution.check()
                os.lseek(execution.image[0].fd, 0, os.SEEK_SET)
                self._native.create_os_disk(execution.directory.fd, execution.image[0].fd, size)
                os_disk = execution.admit_created("os.qcow2")
                execution.journal.commit("os-disk-created")
                os_evidence = self._disk(execution, os_disk, size)

                size = request.resources.encrypted_disk_gib * 1024**3
                execution.journal.commit("data-disk-pending")
                execution.check()
                self._native.create_encrypted_disk(execution.directory.fd, size)
                data_disk = execution.admit_created("encrypted.qcow2")
                execution.journal.commit("data-disk-created")
                data_evidence = self._disk(execution, data_disk, size)

                execution.journal.commit("seed-pending")
                execution.check()
                self._native.create_seed(execution.directory.fd, execution.token)
                seed = execution.admit_created("seed.iso")
                execution.journal.commit("seed-created")
                seed_evidence = self._seed(execution, seed)

                execution.journal.commit("manifest-pending")
                manifest = execution.create_manifest()
                credentials = execution.paths[request.directory / "credentials"].owned
                owner = Ownership(
                    execution.token,
                    execution.checkout.owned,
                    execution.directory.owned,
                    manifest.owned,
                    credentials,
                    tuple(
                        execution.paths[credentials.path / leaf].owned
                        for leaf in (
                            "id_ed25519",
                            "id_ed25519.pub",
                            "host_ed25519",
                            "host_ed25519.pub",
                        )
                        if credentials.path / leaf in execution.paths
                    ),
                )
                vm = PreparedVm(
                    request, owner, VmArtifacts(os_evidence, data_evidence, seed_evidence)
                )
                execution.publish_manifest(vm)
                execution.journal.commit("prepared")
                execution.check()
                # Readable bytes after a failed fsync are cleanup evidence, not
                # completed preparation. Only this successful boundary grants start.
                execution.prepared_admitted = True
                return ExecutionResult(Phase.PREPARE, Classification.PREPARED, execution.receipt())
        except _vm._Denied as denial:
            if execution is not None:
                execution.prepared_admitted = False
            receipt = execution.receipt() if execution is not None else None
            classification = (
                Classification.CLEANUP_INCOMPLETE if receipt is not None else denial.classification
            )
            return ExecutionResult(Phase.PREPARE, classification, receipt)
        except Exception:
            if execution is not None:
                execution.prepared_admitted = False
            receipt = execution.receipt() if execution is not None else None
            classification = (
                Classification.CLEANUP_INCOMPLETE
                if receipt is not None
                else Classification.NATIVE_UNAVAILABLE
            )
            return ExecutionResult(Phase.PREPARE, classification, receipt)

    def _admit_receipt(self, receipt: ExecutionReceipt, handles: ExitStack) -> _Execution:
        failure = Classification.OWNERSHIP_MISMATCH
        if not isinstance(receipt, ExecutionReceipt) or not isinstance(
            receipt.prepared, PreparedVm
        ):
            raise _vm._Denied(Classification.INVALID_STATE)
        vm = receipt.prepared
        self._planner._configuration(vm.request)
        if (
            not isinstance(vm.ownership, Ownership)
            or not _vm._valid_owned_path(receipt.state_directory)
            or not isinstance(receipt.directory, OwnedPath)
            or not isinstance(receipt.checkout, OwnedPath)
            or not _vm._valid_owned_path(receipt.directory)
            or not _vm._valid_owned_path(receipt.checkout)
            or receipt.directory != vm.ownership.directory
            or receipt.checkout != vm.ownership.checkout
        ):
            raise _vm._Denied(failure)
        with _vm._open_path(vm.request.image.path, Classification.UNVERIFIED_IMAGE) as image:
            if image.observation.st_size != vm.request.image.size_bytes:
                raise _vm._Denied(Classification.UNVERIFIED_IMAGE)
        if (
            not isinstance(receipt.token, str)
            or not _vm._HEX_DIGEST.fullmatch(receipt.token)
            or receipt.token != vm.ownership.token
            or receipt.container_id != vm.ownership.container_id
            or not _vm._valid_owned_path(receipt.journal)
            or receipt.journal.path != receipt.state_directory.path / JOURNAL_NAME
            or type(receipt.created_paths) is not tuple
            or len(receipt.created_paths) != 5 + len(vm.ownership.credential_files)
            or not all(_vm._valid_owned_path(path) for path in receipt.created_paths)
        ):
            raise _vm._Denied(failure)
        if receipt.container_id is not None:
            raise _vm._Denied(Classification.INVALID_STATE)
        if self._native is None:
            raise _vm._Denied(Classification.NATIVE_UNAVAILABLE)
        if not _vm._local_image_id(self._planner._runner_image_id_pin):
            raise _vm._Denied(Classification.UNVERIFIED_IMAGE)
        execution = _Execution(
            self._planner,
            handles,
            vm.request,
            receipt.state_directory,
            directory=receipt.directory,
            checkout=receipt.checkout,
        )
        execution.token = receipt.token
        self._planner._hold_artifacts(vm, handles)
        self._planner._hold_ownership(vm, handles)
        assert vm.artifacts is not None
        expected = (
            vm.ownership.credential_directory,
            *vm.ownership.credential_files,
            vm.artifacts.os_disk.file,
            vm.artifacts.encrypted_disk.file,
            vm.artifacts.seed.file,
            vm.ownership.manifest,
        )
        if tuple(receipt.created_paths) != expected:
            raise _vm._Denied(failure)
        for retained in receipt.created_paths:
            opened = handles.enter_context(_vm._open_path(retained.path, failure))
            execution.paths[retained.path] = replace(opened, owned=retained)
        execution.prepared = vm
        execution.journal = _Journal(execution, receipt.journal)
        execution.journal.expected = _record(
            "prepared",
            receipt.token,
            None,
            receipt.created_paths,
            directory=execution.directory_identity,
            checkout=execution.checkout_identity,
        )
        execution.check()
        execution.authenticate(receipt.image_evidence)
        execution.prepared_admitted = True
        return execution

    def start(self, receipt: ExecutionReceipt) -> ExecutionResult:
        execution = None
        try:
            with ExitStack() as handles:
                execution = self._admit_receipt(receipt, handles)
                assert self._native is not None and self._runner is not None
                assert execution.prepared is not None and execution.journal is not None
                vm = execution.prepared
                artifacts = vm.artifacts
                assert artifacts is not None
                for disk in (artifacts.os_disk, artifacts.encrypted_disk):
                    observed = self._disk(
                        execution, execution.paths[disk.file.path], disk.virtual_size_bytes
                    )
                    if not _vm._same_typed_value(observed, disk):
                        raise _vm._Denied(Classification.OWNERSHIP_MISMATCH)
                observed_seed = self._seed(execution, execution.paths[artifacts.seed.file.path])
                if not _vm._same_typed_value(observed_seed, artifacts.seed):
                    raise _vm._Denied(Classification.OWNERSHIP_MISMATCH)
                execution.journal.commit("create-pending")
                execution.check()
                try:
                    container_id = self._native.create_container(vm, self._runner)
                except Exception:
                    return ExecutionResult(
                        Phase.START, Classification.CREATION_UNCERTAIN, execution.receipt()
                    )
                if type(container_id) is not str or not _vm._HEX_DIGEST.fullmatch(container_id):
                    return ExecutionResult(
                        Phase.START, Classification.CREATION_UNCERTAIN, execution.receipt()
                    )
                # Retain the independently returned ID even if journaling fails. Never
                # inspect/start it until both real synchronizations have succeeded.
                execution.container_id = container_id
                known_vm = replace(vm, ownership=replace(vm.ownership, container_id=container_id))
                execution.journal.commit("created")
                execution.check()
                admitted_vm = execution.publish_manifest(known_vm)
                execution.check()
                execution.journal.commit("created")  # Retain the newly published manifest pin too.
                execution.check()
                container = self._native.inspect(container_id)
                execution.check()
                self._planner._container_matches(admitted_vm, container)
                execution.check()  # Last native callback is behind the final admission.
                execution.start_attempted = True
                self._native.start_container(container_id)
                # Running QEMU may modify disks and create its fixed private socket.
                # Keys/seed/sources/state and every root remain independently checked.
                execution.refresh_directory()
                execution.check(runtime_started=True)
                execution.journal.commit("started")
                return ExecutionResult(Phase.START, Classification.STARTED, execution.receipt())
        except _vm._Denied as denial:
            retained = execution.receipt() if execution is not None else None
            classification = (
                Classification.CLEANUP_INCOMPLETE
                if execution is not None and execution.start_attempted
                else denial.classification
            )
            return ExecutionResult(Phase.START, classification, retained)
        except Exception:
            retained = execution.receipt() if execution is not None else None
            classification = (
                Classification.CLEANUP_INCOMPLETE
                if retained is not None
                else Classification.NATIVE_UNAVAILABLE
            )
            return ExecutionResult(Phase.START, classification, retained)

    def _admit_cleanup(
        self, receipt: ExecutionReceipt, handles: ExitStack, stage: str
    ) -> _Execution:
        failure = Classification.OWNERSHIP_MISMATCH
        if (
            not isinstance(receipt, ExecutionReceipt)
            or not isinstance(receipt.prepared, PreparedVm)
            or not isinstance(receipt.container_id, str)
            or not _vm._HEX_DIGEST.fullmatch(receipt.container_id)
        ):
            raise _vm._Denied(Classification.INVALID_STATE)
        vm = receipt.prepared
        owner, artifacts = vm.ownership, vm.artifacts
        if (
            not isinstance(owner, Ownership)
            or not isinstance(artifacts, VmArtifacts)
            or receipt.container_id != owner.container_id
            or receipt.token != owner.token
            or not isinstance(receipt.token, str)
            or not _vm._HEX_DIGEST.fullmatch(receipt.token)
            or receipt.directory != owner.directory
            or receipt.checkout != owner.checkout
            or not _vm._valid_owned_path(receipt.journal)
            or receipt.journal.path != receipt.state_directory.path / JOURNAL_NAME
        ):
            raise _vm._Denied(failure)
        expected = (
            owner.credential_directory,
            *owner.credential_files,
            artifacts.os_disk.file,
            artifacts.encrypted_disk.file,
            artifacts.seed.file,
            owner.manifest,
        )
        if (
            type(receipt.created_paths) is not tuple
            or receipt.created_paths != expected
            or len(expected) != 5 + len(owner.credential_files)
            or not all(_vm._valid_owned_path(p) for p in expected)
        ):
            raise _vm._Denied(failure)
        if self._native is None:
            raise _vm._Denied(Classification.NATIVE_UNAVAILABLE)
        execution = _Execution(
            self._planner,
            handles,
            vm.request,
            receipt.state_directory,
            directory=receipt.directory,
            checkout=receipt.checkout,
            runtime_started=True,
        )
        execution.token, execution.container_id = receipt.token, receipt.container_id
        execution.prepared = vm
        for retained in receipt.created_paths:
            opened = handles.enter_context(_vm._open_path(retained.path, failure))
            if opened.owned != retained:
                raise _vm._Denied(failure)
            execution.paths[retained.path] = replace(opened, owned=retained)
        execution.journal = _Journal(execution, receipt.journal)
        execution.journal.expected = _record(
            stage,
            receipt.token,
            receipt.container_id,
            receipt.created_paths,
            directory=execution.directory_identity,
            checkout=execution.checkout_identity,
        )
        execution.check(runtime_started=True)
        execution.prepared_admitted = True
        return execution

    def _runtime_observation(self, execution: _Execution) -> ContainerObservation:
        assert self._native is not None and execution.prepared is not None
        assert execution.container_id is not None
        execution.check(runtime_started=True)
        observed = self._native.inspect(execution.container_id)
        execution.check(runtime_started=True)
        if not isinstance(observed, _vm.ContainerObservation) or type(observed.running) is not bool:
            raise _vm._Denied(Classification.OWNERSHIP_MISMATCH)
        # Running is the sole permitted difference from the pre-start policy.
        self._planner._container_matches(execution.prepared, replace(observed, running=False))
        return observed

    def stop(self, receipt: ExecutionReceipt) -> ExecutionResult:
        execution = None
        effect = False
        try:
            with ExitStack() as handles:
                execution = self._admit_cleanup(receipt, handles, "started")
                self._runtime_observation(execution)
                assert self._native is not None and execution.journal is not None
                assert execution.container_id is not None
                execution.journal.commit("stop-pending")
                execution.check(runtime_started=True)
                effect = True
                self._native.stop_container(execution.container_id, timeout_seconds=30)
                if self._runtime_observation(execution).running:
                    raise _vm._Denied(Classification.CLEANUP_INCOMPLETE)
                execution.journal.commit("stopped")
                return ExecutionResult(Phase.STOP, Classification.STOPPED, execution.receipt())
        except _vm._Denied as denial:
            return ExecutionResult(
                Phase.STOP,
                Classification.CLEANUP_INCOMPLETE if effect else denial.classification,
                execution.receipt() if execution is not None else None,
            )
        except Exception:
            return ExecutionResult(
                Phase.STOP,
                Classification.CLEANUP_INCOMPLETE,
                execution.receipt() if execution is not None else None,
            )

    def _remove_files(self, execution: _Execution) -> None:
        failure = Classification.OWNERSHIP_MISMATCH
        directory = execution.directory
        credentials = execution.paths[execution.request.directory / "credentials"]
        assert execution.journal is not None
        journal = execution.journal
        # The runner can write only the VM bind. This independently admitted state
        # namespace is outside that bind, never exposed to the runner or guest.
        # Same-account host compromise is not contained by a disposable VM receipt.
        name = "cleanup-" + secrets.token_hex(16)
        execution.check(runtime_started=True)
        os.mkdir(name, 0o700, dir_fd=execution.state.fd)
        execution.cleanup_directory = execution.handles.enter_context(
            _vm._open_path(execution.state.owned.path / name, failure)
        )
        cleanup = execution.cleanup_directory
        _vm._private(cleanup, directory=True, failure=failure)
        _vm._directory_names(cleanup.fd, frozenset(), failure)
        execution.refresh_directory(state=True)
        os.fsync(cleanup.fd)
        os.fsync(execution.state.fd)
        # Retain this recovery namespace durably before detaching anything. Failed
        # detachment, identity or sync leaves its contents for reviewed recovery.
        journal.commit("detach-pending")

        def detach(parent_fd: int, source: str, target: str, retained: _vm._OpenedPath) -> None:
            execution.check_state()
            journal.check()
            _vm._unchanged_directory(cleanup, failure)
            current = os.stat(source, dir_fd=parent_fd, follow_symlinks=False)
            expected = _vm._directory_protection(retained.observation)
            if _vm._directory_protection(current) != expected:
                raise _vm._Denied(failure)
            _rename_noreplace(parent_fd, source, cleanup.fd, target)
            os.fsync(parent_fd)
            os.fsync(cleanup.fd)
            # The source may have been replaced after the observation. Inspect
            # the atomically detached object, not the still-writable source name.
            current = os.stat(target, dir_fd=cleanup.fd, follow_symlinks=False)
            if (
                _vm._directory_protection(current) != expected
                or _vm._directory_protection(os.fstat(retained.fd)) != expected
            ):
                raise _vm._Denied(failure)

        for index, retained in enumerate(tuple(execution.paths.values())):
            if retained.owned.path == credentials.owned.path:
                continue
            parent = (
                credentials if retained.owned.path.parent == credentials.owned.path else directory
            )
            execution.check_roots(runtime_started=True)
            _vm._directory_names(directory.fd, execution.expected_names(), failure)
            _vm._directory_names(
                credentials.fd,
                frozenset(
                    path.name for path in execution.paths if path.parent == credentials.owned.path
                ),
                failure,
            )
            target = f"entry-{index}"
            detach(parent.fd, retained.owned.path.name, target, retained)
            # Only the controller can change this unexposed destination namespace.
            os.unlink(target, dir_fd=cleanup.fd)
            os.fsync(cleanup.fd)
            execution.paths.pop(retained.owned.path)
            if parent is credentials:
                credentials = replace(credentials, observation=os.fstat(credentials.fd))
                execution.paths[credentials.owned.path] = credentials
            execution.refresh_directory()
        _vm._directory_names(credentials.fd, frozenset(), failure)
        detach(directory.fd, "credentials", "credentials", credentials)
        os.rmdir("credentials", dir_fd=cleanup.fd)
        os.fsync(cleanup.fd)
        execution.paths.clear()
        execution.refresh_directory()
        _vm._directory_names(directory.fd, frozenset(), failure)
        with _vm._open_path(directory.owned.path.parent, failure) as parent:
            if (
                parent.owned.ancestors != directory.owned.ancestors[:-1]
                or (parent.owned.device, parent.owned.inode) != directory.owned.ancestors[-1]
            ):
                raise _vm._Denied(failure)
            detach(parent.fd, directory.owned.path.name, "vm", directory)
            os.rmdir("vm", dir_fd=cleanup.fd)
            os.fsync(cleanup.fd)
        _vm._directory_names(cleanup.fd, frozenset(), failure)
        os.rmdir(name, dir_fd=execution.state.fd)
        execution.cleanup_directory = None
        execution.refresh_directory(state=True)
        os.fsync(execution.state.fd)

    def remove(self, receipt: ExecutionReceipt) -> ExecutionResult:
        execution = None
        effect = False
        try:
            with ExitStack() as handles:
                execution = self._admit_cleanup(receipt, handles, "stopped")
                if self._runtime_observation(execution).running:
                    raise _vm._Denied(Classification.INVALID_STATE)
                # Optional runtime entries are not independently retained cleanup
                # authority. QEMU must retire its socket; otherwise keep the whole
                # directory for reviewed recovery rather than unlinking by name.
                _vm._directory_names(
                    execution.directory.fd,
                    execution.expected_names(),
                    Classification.CLEANUP_INCOMPLETE,
                )
                # Atomic detachment has no cross-filesystem fallback. Refuse this
                # unsupported cleanup layout before retiring the native container.
                if execution.directory.owned.device != execution.state.owned.device:
                    raise _vm._Denied(Classification.UNSAFE_PATH)
                assert self._native is not None and execution.journal is not None
                assert execution.container_id is not None
                execution.journal.commit("remove-pending")
                execution.check(runtime_started=True)
                effect = True
                self._native.remove_container(execution.container_id)
                execution.check(runtime_started=True)
                # None must mean positively observed absence, not failed inspection.
                if self._native.inspect(execution.container_id) is not None:
                    raise _vm._Denied(Classification.CLEANUP_INCOMPLETE)
                execution.check(runtime_started=True)
                execution.prepared = None
                self._remove_files(execution)
                execution.journal.commit("removed")
                return ExecutionResult(Phase.REMOVE, Classification.REMOVED, execution.receipt())
        except _vm._Denied as denial:
            return ExecutionResult(
                Phase.REMOVE,
                Classification.CLEANUP_INCOMPLETE if effect else denial.classification,
                execution.receipt() if execution is not None else None,
            )
        except Exception:
            return ExecutionResult(
                Phase.REMOVE,
                Classification.CLEANUP_INCOMPLETE,
                execution.receipt() if execution is not None else None,
            )
