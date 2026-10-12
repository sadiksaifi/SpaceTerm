"""Linux VM artifact authentication, not VM execution or the GUI updater.

Only an independently pinned primary fingerprint and approved public keyring can
supply authority. Download paths, signed documents and VmPlan objects cannot.
Unsupported platforms return UNAVAILABLE before native effects. This tooling
scope does not narrow macOS server support.

The fixed GPG operation reads three sealed memfds through a trusted host procfs,
with a retained private homedir, the repository cwd and an allowlisted environment.
It neither imports keys nor repairs or downloads trust. Seals protect exact bytes
and length, not mode, ownership or directory history. A bounded descriptor-bound
inotify interval rejects covered self, protection and binding events after setup.
Unrelated shared-ancestor entries are ignored. Silence is not authentication,
complete mutation history or an atomic snapshot. Inotify cannot prove mmap or
transient over-mount safety, or isolate arbitrary same-UID code or a hostile mount
namespace. Approved digests and sealed inputs, not watcher silence, bind bytes.

The Linux operation requires the main Python thread and exclusive child reaping.
Bounded handler deferral protects process creation through group-lease assignment
without changing the child's inherited signal mask. The child keeps its original
monotonic deadline and independent output caps. Its leader remains unreaped until
the last owned-group signal. No escaped writer
is adopted or signaled, and every reap has a finite bound. This requires the task's
exclusive-child-reaper convention; non-reaping waitid detects lost ownership where
available. Sealed inputs and scratch stay open through retirement and cleanup.
Group signaling does not promise comprehensive descendant reaping.

Public-key scratch is not credential retention. Controlled subprocess tests are
not evidence of real GPG compatibility, upstream cryptographic validation, VM
operation or daemon cancellation. Later download readers still require isolated
Python (-I) from a trusted directory, separate from downloaded or extracted code.
"""

import ctypes
import errno
import hashlib
import json
import os
import re
import secrets
import selectors
import shlex
import signal
import stat
import struct
import subprocess
import sys
import threading
import time
from collections.abc import Callable
from contextlib import ExitStack, contextmanager
from dataclasses import dataclass, field
from datetime import UTC, date, datetime
from enum import StrEnum
from pathlib import Path
from types import FrameType
from typing import TYPE_CHECKING, Never, Protocol

from spaceterm_tasks import ROOT, TaskError
from spaceterm_tasks import server_vm as vm

if TYPE_CHECKING:
    from spaceterm_tasks.linux_custody_evidence import GuestObservation
    from spaceterm_tasks.server_vm_executor import ExecutionReceipt, VmExecutor

# Private OS seam. Callers cannot select an executable or supply command vectors.
_popen = subprocess.Popen
_MAX_DOCUMENT_BYTES = 64 * 1024
_MAX_SIGNATURE_BYTES = 64 * 1024
_MAX_KEYRING_BYTES = 4 * 1024 * 1024
_MAX_OUTPUT_BYTES = 64 * 1024  # Applied independently to stdout and stderr.
_VERIFY_TIMEOUT_SECONDS = 15.0
_TERMINATE_GRACE_SECONDS = 0.25
_POST_KILL_WAIT_SECONDS = 1.0
_READ_BYTES = 8192
# Linux 5.15 UAPI. No writable fallback is allowed when these operations fail.
_F_ADD_SEALS = 1033
_F_GET_SEALS = 1034
_REQUIRED_SEALS = 0x0001 | 0x0002 | 0x0004 | 0x0008
_PROC_SUPER_MAGIC = 0x9FA0
_IN_MODIFY = 0x00000002
_IN_ATTRIB = 0x00000004
_IN_MOVED_FROM = 0x00000040
_IN_MOVED_TO = 0x00000080
_IN_CREATE = 0x00000100
_IN_DELETE = 0x00000200
_IN_DELETE_SELF = 0x00000400
_IN_MOVE_SELF = 0x00000800
_IN_UNMOUNT = 0x00002000
_IN_Q_OVERFLOW = 0x00004000
_IN_IGNORED = 0x00008000
_IN_ONLYDIR = 0x01000000
_IN_MASK_ADD = 0x20000000
_IN_ISDIR = 0x40000000
_IN_MUTATIONS = (
    _IN_MODIFY
    | _IN_ATTRIB
    | _IN_MOVED_FROM
    | _IN_MOVED_TO
    | _IN_CREATE
    | _IN_DELETE
    | _IN_DELETE_SELF
    | _IN_MOVE_SELF
)
_IN_LOSSES = _IN_UNMOUNT | _IN_IGNORED
_IN_NAMES = _IN_MOVED_FROM | _IN_MOVED_TO | _IN_CREATE | _IN_DELETE
_MAX_HELD_DESCRIPTORS = 384
_MAX_WATCHES = 128
_OBSERVATION_READ_BYTES = 4096
_MAX_OBSERVATION_CALLS = 64
_MAX_OBSERVATION_INTERRUPTS = 8
_MAX_OBSERVATION_BYTES = 64 * 1024
_MAX_OBSERVATION_EVENTS = 1024
_MAX_ACQUISITION_SIGNALS = 64
_FINGERPRINT = re.compile(r"(?:[0-9a-fA-F]{40}|[0-9a-fA-F]{64})\Z")
_DIGEST = re.compile(r"[0-9a-f]{64}\Z")
_COMPONENT = re.compile(r"[A-Za-z0-9_. +\-]+\Z")
_KEY_ID = re.compile(r"[0-9A-F]{16}\Z")
_DECIMAL = re.compile(r"(?:0|[1-9][0-9]{0,19})\Z")
_DATE = re.compile(r"[0-9]{4}-[0-9]{2}-[0-9]{2}\Z")
_SIG_ID = re.compile(r"[A-Za-z0-9+/=]{1,128}\Z")
_LOCATION = re.compile(r"[A-Za-z0-9_.\-]{1,64}\Z")


class NativeClassification(StrEnum):
    UNAVAILABLE = "native-unavailable"
    VERIFIED = "signature-verified"
    INVALID_AUTHORITY = "invalid-authentication-authority"
    INVALID_INPUT = "invalid-authentication-input"
    INPUT_LIMIT = "authentication-input-limit"
    UNSAFE_PATH = "unsafe-native-path"
    SIGNATURE_REJECTED = "signature-rejected"
    INVALID_STATUS = "invalid-signature-status"
    PROCESS_FAILED = "native-process-failed"
    NATIVE_FAILED = "native-operation-failed"
    TIMED_OUT = "native-timeout"
    OUTPUT_LIMIT = "native-output-limit"
    CLEANUP_INCOMPLETE = "native-cleanup-incomplete"


@dataclass(frozen=True, slots=True)
class ApprovedKeyring:
    """Independent local authority, never metadata learned from a signed payload.

    The configured digest, exact size and approved primary fingerprints must come
    from the trusted caller. This record does not import or approve a key itself.
    Paths and key metadata are deliberately absent from its diagnostic repr.
    """

    path: Path = field(repr=False)
    sha256: str = field(repr=False)
    size_bytes: int = field(repr=False)
    primary_fingerprints: tuple[str, ...] = field(repr=False)


class _Denied(Exception):
    """A content-free failure; native messages never become diagnostics."""

    def __init__(self, classification: NativeClassification):
        self.classification = classification
        super().__init__(classification.value)


class _Cleanup:
    """Finish owned cleanup without replacing the first interruption."""

    def __init__(self):
        self.complete = True
        self.interruption: KeyboardInterrupt | SystemExit | None = None

    def interrupted(self, error: KeyboardInterrupt | SystemExit) -> None:
        self.complete = False
        if self.interruption is None:
            self.interruption = error

    def attempt(self, operation: Callable[[], object]) -> bool:
        try:
            operation()
            return True
        except (KeyboardInterrupt, SystemExit) as error:
            self.interrupted(error)
        except Exception:
            self.complete = False
        return False


class _SignalAction(ctypes.Structure):
    # Opaque, aligned storage for 64-bit Linux glibc's sigaction (152 bytes).
    # Keep its flags, mask and restorer without interpreting architecture fields.
    _fields_ = [("words", ctypes.c_ulong * 32)]


class _AcquisitionSignals:
    """Defer Python handlers across child acquisition or the final group signal.

    No signal mask changes during spawning, so the child inherits the caller's
    original mask. Only handler restoration briefly blocks these signals, after
    the guarded operation. Restore handlers and the exact prior mask before
    dispatching bounded pending callbacks with their actual signal frames.
    A custom handler cannot replace our guard or unwind the acquisition window.
    """

    def __init__(self, cleanup: _Cleanup):
        self.cleanup = cleanup
        self.native = _LinuxNative()
        self.handlers: dict[
            int, tuple[Callable[[int, FrameType | None], object], _SignalAction]
        ] = {}
        self.pending: list[tuple[int, FrameType | None]] = []
        self.handler = self._record
        self.failure: BaseException | None = None
        # Restoration can unwind before its retry body or while recording an
        # interruption. Keep progress across the caller's restoration retries.
        self.original_mask: set[int] | None = None
        self.restored: set[int] = set()
        self.dispatched = 0

    def _record(self, signum: int, frame: FrameType | None) -> None:
        if len(self.pending) < _MAX_ACQUISITION_SIGNALS:
            self.pending.append((signum, frame))
        else:
            self.cleanup.complete = False

    def install(self) -> None:
        for signum in sorted(signal.valid_signals()):
            handler = signal.getsignal(signum)
            if callable(handler):
                # Record before installation, including an interrupted install.
                # A retirement retry must not snapshot our guard as the caller.
                if signum not in self.handlers:
                    self.handlers[signum] = (handler, self.native.snapshot(signum))
                signal.signal(signum, self.handler)

    def restore(self) -> None:
        if not self.handlers:
            return
        # Blocking affects only this thread. An unmasked worker can still cause
        # an already restored caller handler to raise on the main Python thread.
        # Resume unfinished handlers, native actions, mask restoration and callback
        # dispatch after those interruptions, including at loop back edges.
        while True:
            try:
                try:
                    if self.original_mask is None:
                        # Snapshot without changing the mask. If delivery interrupts
                        # the following block, a retry must not adopt our blocked mask.
                        self.original_mask = signal.pthread_sigmask(signal.SIG_BLOCK, set())
                    signal.pthread_sigmask(signal.SIG_BLOCK, set(self.handlers))
                except Exception:
                    self.cleanup.complete = False
                if self.cleanup.interruption is not None:
                    # Bookkeeping is itself interruptible by restored handlers.
                    # Record the first exception before entering this retry body.
                    self.cleanup.interrupted(self.cleanup.interruption)
                for signum, (handler, action) in self.handlers.items():
                    if signum in self.restored:
                        continue
                    try:
                        if signal.getsignal(signum) is self.handler:
                            signal.signal(signum, handler)
                        if signal.getsignal(signum) is handler:
                            # Retrying this native action is idempotent. Python's
                            # signal.signal resets SA_RESTART and other state;
                            # do not overwrite an intentional new caller handler.
                            self.native.restore(signum, action)
                    except Exception:
                        self.cleanup.complete = False
                    self.restored.add(signum)
                try:
                    if self.original_mask is not None:
                        signal.pthread_sigmask(signal.SIG_SETMASK, self.original_mask)
                except Exception:
                    self.cleanup.complete = False
                while self.dispatched < len(self.pending):
                    try:
                        # Advance before calling arbitrary caller code, which can
                        # perform effects before raising. Never replay a callback
                        # or free queue capacity beyond the operation-wide cap.
                        signum, frame = self.pending[self.dispatched]
                        self.dispatched += 1
                        self.handlers[signum][0](signum, frame)
                    except (KeyboardInterrupt, SystemExit):
                        raise
                    except BaseException as error:
                        if self.failure is None:
                            self.failure = error
                self.pending.clear()
                self.handlers.clear()
                return
            except (KeyboardInterrupt, SystemExit) as error:
                # Preserve the first exception before an interruptible recorder
                # can unwind. The next iteration resumes its bookkeeping too.
                if self.cleanup.interruption is None:
                    self.cleanup.interruption = error

    def check(self) -> None:
        if self.cleanup.interruption is not None:
            raise self.cleanup.interruption
        if self.failure is not None:
            raise self.failure
        if not self.cleanup.complete:
            raise _Denied(NativeClassification.CLEANUP_INCOMPLETE)


def _fingerprint(value: object) -> str | None:
    if isinstance(value, str) and _FINGERPRINT.fullmatch(value):
        return value.upper()
    return None


def _authority(expected: object, keyring: object) -> tuple[str, ApprovedKeyring]:
    primary = _fingerprint(expected)
    if (
        primary is None
        or not isinstance(keyring, ApprovedKeyring)
        or not isinstance(keyring.sha256, str)
        or not _DIGEST.fullmatch(keyring.sha256)
        or type(keyring.size_bytes) is not int
        or not 0 < keyring.size_bytes <= _MAX_KEYRING_BYTES
        or type(keyring.primary_fingerprints) is not tuple
        or not 0 < len(keyring.primary_fingerprints) <= 64
    ):
        raise _Denied(NativeClassification.INVALID_AUTHORITY)
    approved = tuple(_fingerprint(value) for value in keyring.primary_fingerprints)
    if None in approved or len(set(approved)) != len(approved) or primary not in approved:
        raise _Denied(NativeClassification.INVALID_AUTHORITY)
    return primary, keyring


def _path(path: Path) -> None:
    if not isinstance(path, Path) or not path.is_absolute() or len(str(path)) > 4096:
        raise _Denied(NativeClassification.UNSAFE_PATH)
    if len(path.parts) < 2 or any(
        component in (".", "..") or len(component) > 255 or not _COMPONENT.fullmatch(component)
        for component in path.parts[1:]
    ):
        raise _Denied(NativeClassification.UNSAFE_PATH)


def _stamp(observation: os.stat_result, *, contents: bool = False) -> tuple[int, ...]:
    identity = (
        observation.st_dev,
        observation.st_ino,
        observation.st_mode,
        observation.st_uid,
        observation.st_gid,
    )
    if stat.S_ISDIR(observation.st_mode) and not contents:
        # Shared ancestors may change unrelated entries. The private scratch root
        # and new directory also retain a contents stamp across the native call.
        return identity
    return (
        *identity,
        observation.st_nlink,
        observation.st_size,
        observation.st_mtime_ns,
        observation.st_ctime_ns,
    )


@dataclass(slots=True, repr=False)
class _Entry:
    fd: int
    parent: int | None
    name: str
    observation: os.stat_result
    anonymous: bool = False

    def matches(self, *, contents: bool = False) -> bool:
        def stamp(observation: os.stat_result) -> tuple[int, ...]:
            value = _stamp(observation, contents=contents)
            # Seals and exact content checks protect anonymous inputs' bytes and
            # length. Timestamps are not their immutability or protection proof.
            return value[:-2] if self.anonymous else value

        try:
            expected = stamp(self.observation)
            if stamp(os.fstat(self.fd)) != expected:
                return False
            return (
                self.parent is None
                or stamp(os.stat(self.name, dir_fd=self.parent, follow_symlinks=False)) == expected
            )
        except OSError:
            return False

    def private(self, *, directory: bool, anonymous: bool = False) -> None:
        observation = os.fstat(self.fd)
        kind = stat.S_ISDIR if directory else stat.S_ISREG
        if (
            not kind(observation.st_mode)
            or observation.st_uid != os.geteuid()
            or stat.S_IMODE(observation.st_mode) != (0o700 if directory else 0o600)
            or (not directory and observation.st_nlink != (0 if anonymous else 1))
            or not _cloexec(self.fd)
        ):
            raise _Denied(NativeClassification.UNSAFE_PATH)


@dataclass(frozen=True, slots=True, repr=False)
class _HeldPath:
    entries: tuple[_Entry, ...]

    @property
    def entry(self) -> _Entry:
        return self.entries[-1]

    def matches(self) -> bool:
        return all(entry.matches() for entry in self.entries)


def _cloexec(fd: int) -> bool:
    # Import only behind production Linux selection. The policy facade also works
    # on hosts without fcntl; it need not load a native backend to reject inputs.
    import fcntl

    return bool(fcntl.fcntl(fd, fcntl.F_GETFD) & fcntl.FD_CLOEXEC)


@dataclass(frozen=True, slots=True, repr=False)
class _SealedInput:
    entry: _Entry
    expected: bytes

    def matches(self) -> bool:
        import fcntl

        try:
            self.entry.private(directory=False, anonymous=True)
            seals = fcntl.fcntl(self.entry.fd, _F_GET_SEALS)
            return (
                self.entry.matches()
                and seals & _REQUIRED_SEALS == _REQUIRED_SEALS
                and _read_bytes(self.entry.fd, len(self.expected)) == self.expected
            )
        except (OSError, _Denied):
            return False


def _read_bytes(fd: int, size: int) -> bytes:
    """Read at most the approved size plus one byte without sharing a seek offset."""
    data = bytearray()
    # Every successful nonempty call advances at least one byte; EINTR is not an
    # unbounded retry here. No caller can increase an approved size during a read.
    for _ in range(size + 1):
        chunk = os.pread(fd, min(_READ_BYTES, size + 1 - len(data)), len(data))
        if not chunk:
            break
        data.extend(chunk)
        if len(data) > size:
            break
    if len(data) != size:
        raise _Denied(NativeClassification.INVALID_AUTHORITY)
    return bytes(data)


class _StatFS(ctypes.Structure):
    # Linux glibc statfs on x86_64 and little-endian aarch64 only. A 64-bit
    # word size alone does not validate another architecture's structure ABI.
    _fields_ = [
        ("kind", ctypes.c_long),
        ("block_size", ctypes.c_long),
        ("blocks", ctypes.c_ulong),
        ("free_blocks", ctypes.c_ulong),
        ("available_blocks", ctypes.c_ulong),
        ("files", ctypes.c_ulong),
        ("free_files", ctypes.c_ulong),
        ("filesystem_id", ctypes.c_int * 2),
        ("name_length", ctypes.c_long),
        ("fragment_size", ctypes.c_long),
        ("flags", ctypes.c_long),
        ("spare", ctypes.c_long * 4),
    ]


class _LinuxNative:
    """Fixed glibc calls on validated ABIs; no executable or syscall injection."""

    def __init__(self):
        if (
            os.uname().machine not in {"x86_64", "aarch64"}
            or sys.byteorder != "little"
            or ctypes.sizeof(ctypes.c_long) != 8
            or ctypes.sizeof(ctypes.c_void_p) != 8
            or ctypes.sizeof(_StatFS) != 120
            or ctypes.alignment(_StatFS) != 8
            or ctypes.sizeof(_SignalAction) != 256
            or ctypes.alignment(_SignalAction) != 8
        ):
            raise _Denied(NativeClassification.UNAVAILABLE)
        try:
            library = ctypes.CDLL(None, use_errno=True)
            # Other libcs can export these calls with different structure ABIs.
            # Require glibc before even snapshotting a native signal action.
            _ = library.gnu_get_libc_version
            self.statfs = library.fstatfs
            self.statfs.argtypes = (ctypes.c_int, ctypes.POINTER(_StatFS))
            self.statfs.restype = ctypes.c_int
            self.init = library.inotify_init1
            self.init.argtypes = (ctypes.c_int,)
            self.init.restype = ctypes.c_int
            self.enroll = library.inotify_add_watch
            self.enroll.argtypes = (ctypes.c_int, ctypes.c_char_p, ctypes.c_uint32)
            self.enroll.restype = ctypes.c_int
            self.action = library.sigaction
            self.action.argtypes = (ctypes.c_int, ctypes.c_void_p, ctypes.c_void_p)
            self.action.restype = ctypes.c_int
        except (AttributeError, OSError):
            raise _Denied(NativeClassification.UNAVAILABLE) from None

    def snapshot(self, signum: int) -> _SignalAction:
        action = _SignalAction()
        if self.action(signum, None, ctypes.byref(action)) != 0:
            raise _Denied(NativeClassification.UNAVAILABLE)
        return action

    def restore(self, signum: int, action: _SignalAction) -> None:
        if self.action(signum, ctypes.byref(action), None) != 0:
            raise _Denied(NativeClassification.CLEANUP_INCOMPLETE)

    def require_procfs(self, fd: int) -> None:
        observation = _StatFS()
        if self.statfs(fd, ctypes.byref(observation)) != 0 or observation.kind != _PROC_SUPER_MAGIC:
            raise _Denied(NativeClassification.UNSAFE_PATH)


class _ProcBindings:
    """Bind fixed procfd references to held objects in a trusted host namespace."""

    def __init__(self, files: "_VerificationFiles", native: _LinuxNative):
        self.native = native
        self.proc = files.hold(Path("/proc"), directory=True, private=False)
        self.directory = files.hold(
            Path("/proc") / str(os.getpid()) / "fd", directory=True, private=False
        )
        native.require_procfs(self.proc.entry.fd)
        native.require_procfs(self.directory.entry.fd)
        if (
            self.proc.entry.observation.st_uid != 0
            or self.proc.entry.observation.st_mode & 0o022
            or self.directory.entry.observation.st_uid != os.geteuid()
        ):
            raise _Denied(NativeClassification.UNSAFE_PATH)
        self.check()

    def check(self) -> None:
        if (
            not self.proc.matches()
            or not self.directory.matches()
            or os.readlink("self", dir_fd=self.proc.entry.fd) != str(os.getpid())
            or _stamp(os.stat("/proc/self/fd")) != _stamp(os.fstat(self.directory.entry.fd))
        ):
            raise _Denied(NativeClassification.UNSAFE_PATH)
        self.native.require_procfs(self.proc.entry.fd)
        self.native.require_procfs(self.directory.entry.fd)

    def reference(self, entry: _Entry) -> str:
        self.check()
        reference = f"/proc/self/fd/{entry.fd}"
        if (
            entry.fd < 3
            or not _cloexec(entry.fd)
            or not entry.matches()
            or _stamp(os.stat(reference)) != _stamp(os.fstat(entry.fd))
        ):
            raise _Denied(NativeClassification.UNSAFE_PATH)
        # proc_fd_link selects the held file object's path, including an anonymous
        # inode. The operation retains this descriptor number until queue teardown.
        # These comparisons do not authenticate a hostile namespace or exclude a
        # transient over-mount between syscalls.
        return reference


@dataclass(slots=True, repr=False)
class _Watch:
    wd: int
    entry: _Entry
    names: set[bytes] = field(default_factory=set)
    all_children: bool = False


class _Observation:
    """Sticky covered-event rejection with independent operation-wide budgets."""

    def __init__(self, files: "_VerificationFiles", proc: _ProcBindings, native: _LinuxNative):
        fd = native.init(os.O_CLOEXEC | os.O_NONBLOCK)
        if fd < 0:
            raise _Denied(NativeClassification.UNAVAILABLE)
        files.retain(fd)
        self.queue = _Entry(fd, None, "inotify", os.fstat(fd))
        self.proc = proc
        self.watches: dict[int, _Watch] = {}
        self.identities: dict[tuple[int, int], _Watch] = {}
        self.changed = False
        self.calls = 0
        self.interruptions = 0
        self.bytes = 0
        self.events = 0
        self._protected()

    def _protected(self) -> None:
        import fcntl

        if (
            not self.queue.matches()
            or not _cloexec(self.queue.fd)
            or not fcntl.fcntl(self.queue.fd, fcntl.F_GETFL) & os.O_NONBLOCK
        ):
            self.deny()

    def deny(self) -> Never:
        self.changed = True
        raise _Denied(NativeClassification.UNSAFE_PATH)

    def arm(self, entries: tuple[_Entry, ...], directory: _Entry, native: _LinuxNative) -> None:
        for entry in entries:
            observation = os.fstat(entry.fd)
            identity = (observation.st_dev, observation.st_ino)
            if identity in self.identities:
                continue
            if len(self.watches) >= _MAX_WATCHES:
                self.deny()
            reference = self.proc.reference(entry)
            mask = _IN_MUTATIONS | _IN_MASK_ADD
            if stat.S_ISDIR(observation.st_mode):
                mask |= _IN_ONLYDIR
            wd = native.enroll(self.queue.fd, reference.encode("ascii"), mask)
            if wd < 0 or wd in self.watches:
                self.deny()
            # Enrollment follows a validated held descriptor, not the original
            # pathname. Validate the same binding after the kernel enrolls it.
            if self.proc.reference(entry) != reference or not entry.matches():
                self.deny()
            watch = _Watch(wd, entry)
            self.watches[wd] = watch
            self.identities[identity] = watch
        for entry in entries:
            if entry.parent is not None:
                parent = os.fstat(entry.parent)
                watch = self.identities.get((parent.st_dev, parent.st_ino))
                if watch is None:
                    self.deny()
                watch.names.add(os.fsencode(entry.name))
        held = os.fstat(directory.fd)
        watch = self.identities.get((held.st_dev, held.st_ino))
        if watch is None:
            self.deny()
        watch.all_children = True
        self.finish()

    def finish(self) -> None:
        if self.changed:
            self.deny()
        self._protected()
        for watch in self.watches.values():
            self.proc.reference(watch.entry)
        while self.calls < _MAX_OBSERVATION_CALLS:
            self.calls += 1
            if self.bytes >= _MAX_OBSERVATION_BYTES:
                self.deny()
            limit = min(_OBSERVATION_READ_BYTES, _MAX_OBSERVATION_BYTES - self.bytes)
            try:
                records = os.read(self.queue.fd, limit)
            except OSError as error:
                if error.errno in (errno.EAGAIN, errno.EWOULDBLOCK):
                    return
                if error.errno == errno.EINTR:
                    self.interruptions += 1
                    if self.interruptions <= _MAX_OBSERVATION_INTERRUPTS:
                        continue
                self.deny()
            if not records or len(records) > limit:
                self.deny()
            self.bytes += len(records)
            self.inspect(records)
            if self.changed:
                self.deny()
        self.deny()

    def inspect(self, records: bytes) -> None:
        offset = 0
        while offset < len(records):
            if len(records) - offset < 16 or self.events >= _MAX_OBSERVATION_EVENTS:
                self.deny()
            self.events += 1
            wd, mask, cookie, length = struct.unpack_from("=iIII", records, offset)
            end = offset + 16 + length
            if length > 256 or length % 4 or end > len(records):
                self.deny()
            padded = records[offset + 16 : end]
            name = b""
            if padded:
                name, nul, padding = padded.partition(b"\x00")
                if not nul or not name or name in (b".", b"..") or b"/" in name or any(padding):
                    self.deny()
            if mask & _IN_Q_OVERFLOW:
                self.deny()
            watch = self.watches.get(wd)
            if (
                watch is None
                or mask & ~(_IN_MUTATIONS | _IN_LOSSES | _IN_ISDIR)
                or not mask & (_IN_MUTATIONS | _IN_LOSSES)
                or (mask & _IN_NAMES and not name)
                or (cookie and not mask & (_IN_MOVED_FROM | _IN_MOVED_TO))
                or (
                    not stat.S_ISDIR(watch.entry.observation.st_mode) and (name or mask & _IN_ISDIR)
                )
            ):
                self.deny()
            if mask & (_IN_LOSSES | _IN_DELETE_SELF | _IN_MOVE_SELF):
                self.deny()
            # A nameless event affects the retained object itself. Child events
            # matter only for its covered bindings, or any private homedir entry.
            if not name or watch.all_children or name in watch.names:
                self.changed = True
            offset = end


class _VerificationFiles:
    """One owner for held paths, sealed inputs, proc bindings and observation."""

    def __init__(self):
        self.descriptors: list[int] = []
        self.source: _HeldPath | None = None
        self.root: _HeldPath | None = None
        self.directory: _Entry | None = None
        self.created_name: str | None = None
        self.inputs: list[_SealedInput] = []
        self.proc: _ProcBindings | None = None
        self.observation: _Observation | None = None
        self.child: _ChildLease | None = None

    def retain(self, fd: int) -> None:
        # Transfer ownership immediately after allocation, including partial setup.
        self.descriptors.append(fd)
        if len(self.descriptors) > _MAX_HELD_DESCRIPTORS or fd < 3 or not _cloexec(fd):
            raise _Denied(NativeClassification.UNSAFE_PATH)

    def hold(self, path: Path, *, directory: bool, private: bool = True) -> _HeldPath:
        _path(path)
        if len(path.parts) > _MAX_WATCHES:
            raise _Denied(NativeClassification.UNSAFE_PATH)
        entries = []
        flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
        try:
            fd = os.open("/", flags)
            self.retain(fd)
            entries.append(_Entry(fd, None, "/", os.fstat(fd)))
            for index, name in enumerate(path.parts[1:]):
                parent = fd
                final = index == len(path.parts) - 2
                selected = (
                    flags
                    if not final or directory
                    else os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK
                )
                fd = os.open(name, selected, dir_fd=parent)
                self.retain(fd)
                entries.append(_Entry(fd, parent, name, os.fstat(fd)))
        except OSError:
            raise _Denied(NativeClassification.UNSAFE_PATH) from None
        held = _HeldPath(tuple(entries))
        if private:
            held.entry.private(directory=directory)
        if not held.matches():
            raise _Denied(NativeClassification.UNSAFE_PATH)
        return held

    def prepare(
        self, approved: ApprovedKeyring, scratch: Path, checksums: bytes, signature: bytes
    ) -> None:
        if (
            not callable(getattr(os, "memfd_create", None))
            or getattr(os, "MFD_CLOEXEC", None) != 0x0001
            or getattr(os, "MFD_ALLOW_SEALING", None) != 0x0002
        ):
            raise _Denied(NativeClassification.UNAVAILABLE)
        self.source = self.hold(approved.path, directory=False)
        if self.source.entry.observation.st_size != approved.size_bytes:
            raise _Denied(NativeClassification.INVALID_AUTHORITY)
        key_bytes = _read_bytes(self.source.entry.fd, approved.size_bytes)
        if not self.source.matches():
            raise _Denied(NativeClassification.UNSAFE_PATH)
        if hashlib.sha256(key_bytes).hexdigest() != approved.sha256:
            raise _Denied(NativeClassification.INVALID_AUTHORITY)
        for name, data in (
            ("signature", signature),
            ("checksums", checksums),
            ("approved-keyring", key_bytes),
        ):
            self.seal(name, data)
        self.root = self.hold(scratch, directory=True)
        if not self.source.matches() or not self.root.matches():
            raise _Denied(NativeClassification.UNSAFE_PATH)
        for _ in range(8):
            name = "spaceterm-gpg-" + secrets.token_hex(16)
            try:
                os.mkdir(name, 0o700, dir_fd=self.root.entry.fd)
            except FileExistsError:
                continue  # A colliding object is never adopted or cleaned up.
            self.created_name = name
            break
        else:
            raise _Denied(NativeClassification.NATIVE_FAILED)
        fd = os.open(
            name,
            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
            dir_fd=self.root.entry.fd,
        )
        self.retain(fd)
        self.directory = _Entry(fd, self.root.entry.fd, name, os.fstat(fd))
        os.fchmod(fd, 0o700)
        self.directory.observation = os.fstat(fd)
        self.directory.private(directory=True)
        if not self.root.matches() or not self.directory.matches():
            raise _Denied(NativeClassification.UNSAFE_PATH)
        # The only directory contents baseline is our completed, empty homedir.
        # No later observation or cleanup rebases its protection or binding facts.
        native = _LinuxNative()
        self.proc = _ProcBindings(self, native)
        self.observation = _Observation(self, self.proc, native)
        self.observation.arm(
            (
                *self.source.entries,
                *self.root.entries,
                self.directory,
                *(item.entry for item in self.inputs),
            ),
            self.directory,
            native,
        )
        self.check()

    def seal(self, name: str, data: bytes) -> None:
        import fcntl

        fd = os.memfd_create("spaceterm-gpg-" + name, os.MFD_CLOEXEC | os.MFD_ALLOW_SEALING)
        self.retain(fd)
        entry = _Entry(fd, None, name, os.fstat(fd), anonymous=True)
        os.fchmod(fd, 0o600)
        entry.private(directory=False, anonymous=True)
        view = memoryview(data)
        # A successful write consumes at least one byte. Native exceptions fail
        # closed rather than permitting retries or a named writable substitute.
        for _ in range(len(data)):
            written = os.write(fd, view)
            if not 0 < written <= len(view):
                raise _Denied(NativeClassification.NATIVE_FAILED)
            view = view[written:]
            if not view:
                break
        if view:
            raise _Denied(NativeClassification.NATIVE_FAILED)
        fcntl.fcntl(fd, _F_ADD_SEALS, _REQUIRED_SEALS)
        entry.observation = os.fstat(fd)
        sealed = _SealedInput(entry, data)
        self.inputs.append(sealed)
        if not sealed.matches():
            raise _Denied(NativeClassification.UNSAFE_PATH)

    def references(self) -> tuple[str, str, str, str]:
        proc = self.proc
        directory = self.directory
        if proc is None or directory is None or len(self.inputs) != 3:
            raise _Denied(NativeClassification.NATIVE_FAILED)
        signature, checksums, keyring = (proc.reference(item.entry) for item in self.inputs)
        return signature, checksums, keyring, proc.reference(directory)

    def check(self) -> None:
        if (
            self.source is None
            or self.root is None
            or self.directory is None
            or self.proc is None
            or self.observation is None
            or len(self.inputs) != 3
            or not self.source.matches()
            or not self.root.matches()
            or not self.directory.matches(contents=True)
            or not all(item.matches() for item in self.inputs)
        ):
            raise _Denied(NativeClassification.UNSAFE_PATH)
        self.source.entry.private(directory=False)
        self.root.entry.private(directory=True)
        self.directory.private(directory=True)
        self.references()
        # Last: earlier protection, binding and byte checks must not open a blind
        # interval after a supposedly final observation drain.
        self.observation.finish()

    def cleanup(self, cleanup: _Cleanup) -> None:
        try:
            if self.created_name is None:
                return
            if (
                self.root is None
                or self.directory is None
                or not self.root.matches()
                or not self.directory.matches()
            ):
                cleanup.complete = False
                return  # Do not traverse a renamed or replacement directory.
            root = self.root
            directory = self.directory
            root.entry.private(directory=True)
            directory.private(directory=True)
            # All inputs are anonymous. Even one homedir entry is foreign. Inspect
            # one name only; never descend, unlink it, or adopt GPG-created state.
            entries = os.scandir(directory.fd)
            empty = False
            try:
                empty = next(entries, None) is None
            finally:
                cleanup.attempt(entries.close)
            if not empty or not root.matches() or not directory.matches():
                cleanup.complete = False
                return
            cleanup.attempt(lambda: os.rmdir(directory.name, dir_fd=root.entry.fd))
        except (KeyboardInterrupt, SystemExit) as error:
            cleanup.interrupted(error)
        except Exception:
            cleanup.complete = False
        finally:
            # Closing our own descriptors never restores changed protections or
            # removes an external duplicate. Keep every resource until retirement
            # and path cleanup have both finished, then close in reverse ownership.
            for fd in reversed(self.descriptors):
                cleanup.attempt(lambda: os.close(fd))
            self.descriptors.clear()


def _child_exited(process: subprocess.Popen) -> bool | None:
    """Observe a waitable child without releasing its PID/process-group identity."""
    waitid = getattr(os, "waitid", None)
    nowait = getattr(os, "WNOWAIT", None)
    if waitid is None or nowait is None:
        return None
    result = waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | nowait)
    return result is not None


@dataclass(slots=True, repr=False)
class _ChildLease:
    """Reserve the original session/group number until consuming reaping starts."""

    process: subprocess.Popen
    pid: int
    signals_open: bool = True
    reap_deadline: float | None = None
    pipe_close_attempts: int = 0

    def close_pipes(self, cleanup: _Cleanup) -> None:
        for index, pipe in enumerate((self.process.stdout, self.process.stderr)):
            bit = 1 << index
            if pipe is not None and not self.pipe_close_attempts & bit:
                # A close can release a descriptor and then raise. Never retry it
                # through an outer retirement fallback against a reused number.
                self.pipe_close_attempts |= bit
                cleanup.attempt(pipe.close)


def _signal_owned(child: _ChildLease, signum: int) -> None:
    # No poll or wait precedes the last signal. A live or unreaped leader reserves
    # its group number; an external reaper removes that authority, not just a PID.
    process = child.process
    if not child.signals_open or process.returncode is not None or process.pid != child.pid:
        raise _Denied(NativeClassification.CLEANUP_INCOMPLETE)
    try:
        _child_exited(process)
        if signal.getsignal(signal.SIGCHLD) == signal.SIG_IGN:
            raise _Denied(NativeClassification.CLEANUP_INCOMPLETE)
        try:
            group = os.getpgid(child.pid)
        except ProcessLookupError:
            # Leader lookup failure is not group absence. Recheck waitability
            # without consuming the child; without WNOWAIT, authority still
            # requires the exclusive-reaper precondition.
            _child_exited(process)
        else:
            if group != child.pid:
                raise _Denied(NativeClassification.CLEANUP_INCOMPLETE)
    except OSError:
        raise _Denied(NativeClassification.CLEANUP_INCOMPLETE) from None
    # Only this group operation's ESRCH proves absence. EPERM fails cleanup;
    # an apparently exited leader cannot prove that every live member is retired.
    os.killpg(child.pid, signum)


def _kill_owned(child: _ChildLease, cleanup: _Cleanup) -> None:
    # Masking the main thread cannot prevent worker-delivered Python handlers
    # from unwinding the last group signal. Defer them until SIGKILL finishes,
    # before any consuming reap releases the lease. No retry renews this budget.
    if child.reap_deadline is None:
        child.reap_deadline = time.monotonic() + _POST_KILL_WAIT_SECONDS
    acquisition = None
    try:
        while True:
            try:
                if acquisition is None:
                    acquisition = _AcquisitionSignals(cleanup)
                acquisition.install()
                _signal_owned(child, signal.SIGKILL)
                return
            except (KeyboardInterrupt, SystemExit) as error:
                cleanup.interrupted(error)
            # An interrupted installation or syscall can be retried only while
            # the original unreaped lease and post-kill deadline still permit it.
            if not child.signals_open or time.monotonic() >= child.reap_deadline:
                raise _Denied(NativeClassification.CLEANUP_INCOMPLETE)
    finally:
        if acquisition is not None:
            while acquisition.handlers:
                cleanup.attempt(acquisition.restore)
            cleanup.attempt(acquisition.check)


def _reap_child(child: _ChildLease, cleanup: _Cleanup) -> int | None:
    # Close the lease before any consuming wait, even if wait is interrupted
    # after kernel consumption but before Popen publishes its returncode.
    child.signals_open = False
    if child.reap_deadline is None:
        child.reap_deadline = time.monotonic() + _POST_KILL_WAIT_SECONDS
    while True:
        remaining = child.reap_deadline - time.monotonic()
        if remaining <= 0:
            cleanup.complete = False
            return None
        try:
            return child.process.wait(timeout=remaining)
        except (KeyboardInterrupt, SystemExit) as error:
            cleanup.interrupted(error)
            # A repeated interrupt or outer fallback cannot restart this deadline.
        except Exception:
            cleanup.complete = False
            return None


def _retire_child(child: _ChildLease, *, exited: bool, cleanup: _Cleanup) -> int | None:
    """Signal only the reserved original group, then close pipes and finitely reap."""
    authority = True
    group_present = True
    if not exited:
        try:
            _signal_owned(child, signal.SIGTERM)
        except ProcessLookupError:
            group_present = False
        except _Denied:
            authority = False
            cleanup.complete = False
        except (KeyboardInterrupt, SystemExit) as error:
            cleanup.interrupted(error)
        except Exception:
            cleanup.complete = False
        if authority and group_present:
            cleanup.attempt(lambda: time.sleep(_TERMINATE_GRACE_SECONDS))
    if child.reap_deadline is None:
        # One post-kill phase budget, shared by pipe closure, consuming waits and
        # every outer fallback. An interruption cannot restart it.
        child.reap_deadline = time.monotonic() + _POST_KILL_WAIT_SECONDS
    while authority and group_present:
        try:
            if not child.signals_open or time.monotonic() >= child.reap_deadline:
                cleanup.complete = False
                break
            _kill_owned(child, cleanup)
            break
        except ProcessLookupError:
            break
        except (KeyboardInterrupt, SystemExit) as error:
            # A handler can unwind the helper at entry, before its guard exists.
            # Resume this phase under the same deadline before consuming reaping.
            cleanup.interrupted(error)
        except Exception:
            cleanup.complete = False
            break
    # Escaped/inherited writers have no authority to extend pipe closure or reap.
    child.close_pipes(cleanup)
    return _reap_child(child, cleanup)


def _read_output(
    process: subprocess.Popen, deadline: float, maximum: int = _MAX_OUTPUT_BYTES
) -> bytes:
    if process.stdout is None or process.stderr is None:
        raise _Denied(NativeClassification.NATIVE_FAILED)
    output = bytearray()
    counts = [0, 0]
    selector = selectors.DefaultSelector()
    interruption = None
    try:
        for index, pipe in enumerate((process.stdout, process.stderr)):
            os.set_blocking(pipe.fileno(), False)
            selector.register(pipe.fileno(), selectors.EVENT_READ, index)
        while selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise _Denied(NativeClassification.TIMED_OUT)
            for key, _ in selector.select(remaining):
                if time.monotonic() >= deadline:
                    raise _Denied(NativeClassification.TIMED_OUT)
                index = key.data
                try:
                    limit = maximum if index == 0 else _MAX_OUTPUT_BYTES
                    chunk = os.read(key.fd, min(_READ_BYTES, limit + 1 - counts[index]))
                except BlockingIOError:
                    continue
                if not chunk:
                    selector.unregister(key.fd)
                    continue
                counts[index] += len(chunk)
                if counts[index] > (maximum if index == 0 else _MAX_OUTPUT_BYTES):
                    raise _Denied(NativeClassification.OUTPUT_LIMIT)
                if index == 0:
                    output.extend(chunk)
                # stderr is counted, not retained or interpreted as authority.
    except (KeyboardInterrupt, SystemExit) as error:
        interruption = error
        raise
    finally:
        try:
            selector.close()
        except (KeyboardInterrupt, SystemExit):
            if interruption is None:
                raise
        except Exception:
            if interruption is None:
                raise _Denied(NativeClassification.CLEANUP_INCOMPLETE) from None
    return bytes(output)


def _await_exit(process: subprocess.Popen, deadline: float) -> bool:
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise _Denied(NativeClassification.TIMED_OUT)
        exited = _child_exited(process)
        if exited is True:
            return True
        if exited is None:
            # Without WNOWAIT, never consume the PID with poll/wait and then
            # signal its numeric PGID. Give normal EOF completion a finite grace,
            # then retire the still-unreaped group before the bounded wait.
            time.sleep(min(_TERMINATE_GRACE_SECONDS, remaining))
            if time.monotonic() >= deadline:
                raise _Denied(NativeClassification.TIMED_OUT)
            return False
        time.sleep(min(0.02, remaining))


@dataclass(frozen=True, slots=True, repr=False)
class _OperationResult:
    status: bytes
    returncode: int | None


class _VerificationOperation:
    """Private fixed operation boundary, not a caller-selected command executor."""

    def prepare(
        self, approved: ApprovedKeyring, scratch: Path, checksums: bytes, signature: bytes
    ) -> None:
        raise NotImplementedError

    def run(self) -> _OperationResult:
        raise NotImplementedError

    def check(self) -> None:
        raise NotImplementedError

    def cleanup(self, cleanup: _Cleanup) -> None:
        raise NotImplementedError


class _LinuxVerificationOperation(_VerificationFiles, _VerificationOperation):
    def run(self) -> _OperationResult:
        return _verify_native(self)


def _require_acquisition_context() -> None:
    # This acquisition strategy requires Python's main-thread handler API.
    # Reject a worker before opening authority or allocating native resources.
    if (
        sys.platform != "linux"
        or os.name != "posix"
        or threading.current_thread() is not threading.main_thread()
        or not callable(getattr(signal, "valid_signals", None))
        or not callable(getattr(signal, "pthread_sigmask", None))
        or signal.getsignal(signal.SIGCHLD) == signal.SIG_IGN
    ):
        raise _Denied(NativeClassification.UNAVAILABLE)


def _new_verification_operation() -> _VerificationOperation:
    # Select before any authority open, scratch allocation, child or group signal.
    # Portable tests replace this single fixed-operation boundary, not trust policy.
    _require_acquisition_context()
    # Probe handler installation before filesystem effects, including the main
    # interpreter restriction that Python's thread identity alone cannot prove.
    cleanup = _Cleanup()
    acquisition = _AcquisitionSignals(cleanup)
    unavailable = False
    try:
        acquisition.install()
    except (KeyboardInterrupt, SystemExit) as error:
        cleanup.interrupted(error)
    except (OSError, ValueError):
        unavailable = True
    finally:
        while acquisition.handlers:
            cleanup.attempt(acquisition.restore)
    acquisition.check()
    if unavailable:
        raise _Denied(NativeClassification.UNAVAILABLE)
    return _LinuxVerificationOperation()


def _verify_native(files: _VerificationFiles) -> _OperationResult:
    _require_acquisition_context()
    if files.directory is None or len(files.inputs) != 3:
        raise _Denied(NativeClassification.NATIVE_FAILED)
    files.check()
    signature, checksums, keyring, home = files.references()
    inherited = (*(item.entry.fd for item in files.inputs), files.directory.fd)
    argv = (
        "/usr/bin/gpg",
        "--no-options",
        "--batch",
        "--no-tty",
        "--no-default-keyring",
        "--no-auto-key-retrieve",
        "--no-auto-key-import",
        "--auto-key-locate",
        "clear",
        "--no-autostart",
        "--require-cross-certification",
        "--trust-model",
        "always",
        "--no-auto-check-trustdb",
        "--no-random-seed-file",
        "--lock-never",
        "--homedir",
        home,
        "--keyring",
        str(keyring),
        "--status-fd",
        "1",
        "--verify",
        str(signature),
        str(checksums),
    )
    cleanup = _Cleanup()
    process = None
    child = None
    failure = None
    output = b""
    exited = False
    returncode = None
    deadline = time.monotonic() + _VERIFY_TIMEOUT_SECONDS
    acquisition = _AcquisitionSignals(cleanup)
    try:
        try:
            acquisition.install()
            acquisition.check()
            process = _popen(
                argv,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
                close_fds=True,
                pass_fds=inherited,
                cwd=ROOT,
                env={
                    "PATH": "/usr/bin:/bin",
                    "LANG": "C",
                    "LC_ALL": "C",
                    "HOME": home,
                    "TMPDIR": home,
                },
            )
            child = files.child = _ChildLease(process, process.pid)
        except (KeyboardInterrupt, SystemExit) as error:
            cleanup.interrupted(error)
            raise
        finally:
            while acquisition.handlers:
                cleanup.attempt(acquisition.restore)
        acquisition.check()
        output = _read_output(process, deadline)
        exited = _await_exit(process, deadline)
    except _Denied as error:
        failure = error.classification
    except (KeyboardInterrupt, SystemExit) as error:
        cleanup.interrupted(error)
    except Exception:
        failure = NativeClassification.NATIVE_FAILED
    finally:
        if process is not None:
            if child is None:
                child = files.child = _ChildLease(process, process.pid)
            retirement_finished = False
            try:
                returncode = _retire_child(child, exited=exited, cleanup=cleanup)
                retirement_finished = True
            except (KeyboardInterrupt, SystemExit) as error:
                cleanup.interrupted(error)
            except Exception:
                cleanup.complete = False
            finally:
                if child.signals_open:
                    # The outer fallback has the same helper-entry cancellation
                    # window. Retain its original deadline and unreaped lease.
                    while child.signals_open:
                        try:
                            if child.reap_deadline is None:
                                child.reap_deadline = time.monotonic() + _POST_KILL_WAIT_SECONDS
                            if time.monotonic() >= child.reap_deadline:
                                cleanup.complete = False
                                break
                            _kill_owned(child, cleanup)
                            break
                        except ProcessLookupError:
                            break
                        except (KeyboardInterrupt, SystemExit) as error:
                            cleanup.interrupted(error)
                        except Exception:
                            cleanup.complete = False
                            break
                if not retirement_finished:
                    # Closing signaling authority does not prove any wait ran.
                    # Resume an aborted retirement under its original deadline.
                    cleanup.attempt(lambda: _reap_child(child, cleanup))
                cleanup.attempt(lambda: child.close_pipes(cleanup))
    if cleanup.interruption is not None:
        raise cleanup.interruption
    if not cleanup.complete:
        raise _Denied(NativeClassification.CLEANUP_INCOMPLETE)
    if failure is not None:
        raise _Denied(failure)
    return _OperationResult(output, returncode)


def _number(value: str, *, maximum: int = 2**63 - 1) -> int:
    if not _DECIMAL.fullmatch(value):
        raise _Denied(NativeClassification.INVALID_STATUS)
    parsed = int(value)
    if parsed > maximum:
        raise _Denied(NativeClassification.INVALID_STATUS)
    return parsed


def _date(value: str) -> date:
    if not _DATE.fullmatch(value):
        raise _Denied(NativeClassification.INVALID_STATUS)
    try:
        return date.fromisoformat(value)
    except ValueError:
        raise _Denied(NativeClassification.INVALID_STATUS) from None


def _status_fingerprint(value: str) -> str:
    parsed = _fingerprint(value)
    if parsed is None or parsed != value:
        raise _Denied(NativeClassification.INVALID_STATUS)
    return parsed


def _key_id_and_uid(arguments: str) -> str:
    key_id, separator, uid = arguments.partition(" ")
    if not _KEY_ID.fullmatch(key_id) or not separator or not uid:
        raise _Denied(NativeClassification.INVALID_STATUS)
    return key_id


def _failure_record(name: str, arguments: str) -> bool:
    if name in {"BADSIG", "EXPSIG", "EXPKEYSIG", "REVKEYSIG"}:
        _key_id_and_uid(arguments)
    elif name == "ERRSIG":
        values = arguments.split(" ")
        if len(values) not in (6, 7) or not _KEY_ID.fullmatch(values[0]):
            raise _Denied(NativeClassification.INVALID_STATUS)
        for index in (1, 2, 4, 5):
            _number(values[index])
        if not re.fullmatch(r"[0-9A-Fa-f]{2}", values[3]):
            raise _Denied(NativeClassification.INVALID_STATUS)
        if len(values) == 7:
            _status_fingerprint(values[6])
    elif name in {"KEYEXPIRED", "SIGEXPIRED"}:
        _number(arguments)
    elif name == "KEYREVOKED":
        if arguments:
            raise _Denied(NativeClassification.INVALID_STATUS)
    elif name == "NO_PUBKEY":
        if not _KEY_ID.fullmatch(arguments):
            raise _Denied(NativeClassification.INVALID_STATUS)
    elif name in {"FAILURE", "ERROR"}:
        values = arguments.split(" ")
        if len(values) != 2 or not _LOCATION.fullmatch(values[0]):
            raise _Denied(NativeClassification.INVALID_STATUS)
        _number(values[1], maximum=2**32 - 1)
    elif name == "NODATA":
        if _number(arguments) not in (1, 2, 3, 4):
            raise _Denied(NativeClassification.INVALID_STATUS)
    else:
        return False
    return True


@dataclass(frozen=True, slots=True, repr=False)
class _ValidSignature:
    signing: str
    primary: str | None
    created: int
    expires: int
    creation_date: date
    permitted: bool


def _valid_signature(arguments: str) -> _ValidSignature:
    values = arguments.split(" ")
    if len(values) not in (9, 10):
        raise _Denied(NativeClassification.INVALID_STATUS)
    signing = _status_fingerprint(values[0])
    creation_date = _date(values[1])
    created, expires, version, reserved, public_key, digest = (
        _number(value) for value in values[2:8]
    )
    if reserved != 0 or version not in (3, 4, 5, 6):
        raise _Denied(NativeClassification.INVALID_STATUS)
    # Recognized but disallowed algorithms are rejection, not success. Unknown
    # algorithm IDs are malformed native facts. Signing requires modern hashes;
    # encryption-only algorithms and canonical-text signatures cannot prove the
    # byte-exact detached-document contract.
    if public_key not in {1, 2, 3, 16, 17, 18, 19, 20, 21, 22, 25, 26, 27, 28}:
        raise _Denied(NativeClassification.INVALID_STATUS)
    if digest not in {1, 2, 3, 8, 9, 10, 11, 12, 14}:
        raise _Denied(NativeClassification.INVALID_STATUS)
    if not re.fullmatch(r"[0-9A-Fa-f]{2}", values[8]):
        raise _Denied(NativeClassification.INVALID_STATUS)
    try:
        if datetime.fromtimestamp(created, UTC).date() != creation_date:
            raise _Denied(NativeClassification.INVALID_STATUS)
    except (OverflowError, OSError, ValueError):
        raise _Denied(NativeClassification.INVALID_STATUS) from None
    primary = _status_fingerprint(values[9]) if len(values) == 10 else None
    return _ValidSignature(
        signing,
        primary,
        created,
        expires,
        creation_date,
        version == 4
        and public_key in {1, 3, 19, 22}
        and digest in {8, 9, 10}
        and values[8] == "00",
    )


def _authenticated_primary(output: bytes, expected: str) -> str:
    if not output or len(output) > _MAX_OUTPUT_BYTES or not output.endswith(b"\n"):
        raise _Denied(NativeClassification.INVALID_STATUS)
    # Frame and bound physical bytes before decoding any record. UTF-8 display
    # text, including Unicode line separators, cannot manufacture machine facts.
    # GPG escapes raw C0 controls and DEL in UIDs; malformed bytes fail closed.
    if any((value < 32 and value != 10) or value == 127 for value in output):
        raise _Denied(NativeClassification.INVALID_STATUS)
    lines = output[:-1].split(b"\n")
    if len(lines) > 256 or any(len(line) > 8192 for line in lines):
        raise _Denied(NativeClassification.INVALID_STATUS)
    newsig = False
    good = None
    valid = None
    sig_id = None
    trust = False
    rejected = False
    for index, line in enumerate(lines):
        if not line.startswith(b"[GNUPG:] "):
            raise _Denied(NativeClassification.INVALID_STATUS)
        raw_name, _, raw_arguments = line[9:].partition(b" ")
        try:
            name = raw_name.decode("ascii")
            # Only UID-bearing records have opaque UTF-8 suffixes. Their key IDs
            # still pass the ASCII-only grammar; every other argument is ASCII.
            encoding = (
                "utf-8"
                if name in {"GOODSIG", "BADSIG", "EXPSIG", "EXPKEYSIG", "REVKEYSIG"}
                else "ascii"
            )
            arguments = raw_arguments.decode(encoding)
        except UnicodeDecodeError:
            raise _Denied(NativeClassification.INVALID_STATUS) from None
        if _failure_record(name, arguments):
            rejected = True
        elif name == "NEWSIG":
            if newsig or index != 0:
                raise _Denied(NativeClassification.INVALID_STATUS)
            newsig = True
        elif name == "KEY_CONSIDERED":
            values = arguments.split(" ")
            if len(values) != 2:
                raise _Denied(NativeClassification.INVALID_STATUS)
            _status_fingerprint(values[0])
            flags = _number(values[1], maximum=3)
            rejected = rejected or bool(flags & 2)
            # This record is never a primary/subkey binding or signer authority.
        elif name == "SIG_ID":
            values = arguments.split(" ")
            if sig_id is not None or good is not None or len(values) != 3:
                raise _Denied(NativeClassification.INVALID_STATUS)
            if not _SIG_ID.fullmatch(values[0]):
                raise _Denied(NativeClassification.INVALID_STATUS)
            sig_id = (_date(values[1]), _number(values[2]))
        elif name == "GOODSIG":
            if good is not None or valid is not None:
                raise _Denied(NativeClassification.INVALID_STATUS)
            good = _key_id_and_uid(arguments)
        elif name == "VALIDSIG":
            if valid is not None:
                raise _Denied(NativeClassification.INVALID_STATUS)
            valid = _valid_signature(arguments)
        elif name in {
            "TRUST_UNDEFINED",
            "TRUST_NEVER",
            "TRUST_MARGINAL",
            "TRUST_FULLY",
            "TRUST_ULTIMATE",
        }:
            values = arguments.split(" ")
            if trust or valid is None or len(values) not in (1, 2) or _number(values[0]) != 0:
                raise _Denied(NativeClassification.INVALID_STATUS)
            if len(values) == 2 and values[1] not in {
                "pgp",
                "classic",
                "always",
                "direct",
                "auto",
                "tofu",
                "tofu+pgp",
            }:
                raise _Denied(NativeClassification.INVALID_STATUS)
            trust = True  # Trust-model ratings do not authenticate our pin.
        else:
            raise _Denied(NativeClassification.INVALID_STATUS)
    if rejected:
        raise _Denied(NativeClassification.SIGNATURE_REJECTED)
    if not newsig or good is None or valid is None:
        raise _Denied(NativeClassification.INVALID_STATUS)
    if sig_id is not None and sig_id != (valid.creation_date, valid.created):
        raise _Denied(NativeClassification.INVALID_STATUS)
    # Wall time is used only for signature validity, never a child deadline.
    now = int(time.time())
    if (
        not valid.permitted
        or good != valid.signing[-16:]
        or valid.created > now
        or (valid.expires != 0 and (valid.expires <= now or valid.expires <= valid.created))
        or (valid.primary is None and valid.signing != expected)
        or (valid.primary is not None and valid.primary != expected)
    ):
        raise _Denied(NativeClassification.SIGNATURE_REJECTED)
    # VALIDSIG's full primary is a result of GPG's cryptographic key/subkey and
    # cross-certification validation on the approved keyring and exact document.
    # No key-list output, UID, long key ID or GitHub flag establishes that binding.
    return expected


class NativeSignatureVerifier:
    """Authenticate only verify(checksums: bytes, signature: bytes) -> str | None.

    Success returns the full independently approved primary fingerprint, also for
    a signing subkey whose binding the maintained native verifier validated.
    last_classification contains no native output, paths or exception messages.
    An unconfigured adapter remains unavailable and has no filesystem effects.
    The production operation is main-thread Linux VM tooling, restricted to
    little-endian x86_64/aarch64 glibc ABIs. Unsupported hosts and worker threads
    return UNAVAILABLE before native effects.
    Handler-capability probes precede filesystem effects; portable policy stays here.
    """

    def __init__(
        self,
        *,
        expected_primary: str | None = None,
        keyring: ApprovedKeyring | None = None,
        scratch_directory: Path | None = None,
    ):
        self._expected_primary = expected_primary
        self._keyring = keyring
        self._scratch_directory = scratch_directory
        self.last_classification: NativeClassification = NativeClassification.UNAVAILABLE

    def verify(self, checksums: bytes, signature: bytes) -> str | None:
        self.last_classification = NativeClassification.UNAVAILABLE
        if (
            self._expected_primary is None
            and self._keyring is None
            and self._scratch_directory is None
        ):
            return None
        files: _VerificationOperation | None = None
        cleanup = _Cleanup()
        result = None
        try:
            files = _new_verification_operation()
            expected, approved = _authority(self._expected_primary, self._keyring)
            if self._scratch_directory is None:
                raise _Denied(NativeClassification.UNAVAILABLE)
            if (
                type(checksums) is not bytes
                or type(signature) is not bytes
                or not checksums
                or not signature
            ):
                raise _Denied(NativeClassification.INVALID_INPUT)
            if len(checksums) > _MAX_DOCUMENT_BYTES or len(signature) > _MAX_SIGNATURE_BYTES:
                raise _Denied(NativeClassification.INPUT_LIMIT)
            files.prepare(approved, self._scratch_directory, checksums, signature)
            outcome = files.run()
            if type(outcome.returncode) is not int or outcome.returncode != 0:
                raise _Denied(NativeClassification.PROCESS_FAILED)
            authenticated = _authenticated_primary(outcome.status, expected)
            files.check()
            result = authenticated
            self.last_classification = NativeClassification.VERIFIED
        except _Denied as error:
            self.last_classification = error.classification
        except (KeyboardInterrupt, SystemExit) as error:
            cleanup.interrupted(error)
        except Exception:
            self.last_classification = NativeClassification.NATIVE_FAILED
        finally:
            if files is not None:
                cleanup.attempt(lambda: files.cleanup(cleanup))
        if not cleanup.complete:
            result = None
            self.last_classification = NativeClassification.CLEANUP_INCOMPLETE
        if cleanup.interruption is not None:
            raise cleanup.interruption
        return result


# NoCloud reads a read-only CIDATA ISO. Joliet preserves the fixed lower-case,
# hyphenated filenames without a filesystem builder or a writable staging tree.
_ISO_BLOCK = 2048


def _both(value: int, width: int) -> bytes:
    return value.to_bytes(width, "little") + value.to_bytes(width, "big")


def _iso_record(name: bytes, extent: int, length: int, *, directory: bool = False) -> bytes:
    size = 33 + len(name) + (not len(name) % 2)
    result = bytearray(size)
    result[0] = size
    result[2:10] = _both(extent, 4)
    result[10:18] = _both(length, 4)
    result[18:25] = bytes((126, 10, 12, 0, 0, 0, 0))
    result[25] = 2 if directory else 0
    result[28:32] = _both(1, 2)
    result[32] = len(name)
    result[33 : 33 + len(name)] = name
    return bytes(result)


def _iso_volume(kind: int, blocks: int, root: int, little_table: int, big_table: int) -> bytes:
    result = bytearray(_ISO_BLOCK)
    result[:7] = bytes((kind,)) + b"CD001\x01"
    wide = kind == 2
    for start, text in ((8, "SPACETERM"), (40, "CIDATA")):
        result[start : start + 32] = (
            text.ljust(16).encode("utf-16-be") if wide else text.ljust(32).encode("ascii")
        )
    result[80:88] = _both(blocks, 4)
    if wide:
        result[88:91] = b"%/E"
    result[120:124] = _both(1, 2)
    result[124:128] = _both(1, 2)
    result[128:132] = _both(_ISO_BLOCK, 2)
    result[132:140] = _both(10, 4)
    result[140:144] = little_table.to_bytes(4, "little")
    result[148:152] = big_table.to_bytes(4, "big")
    result[156:190] = _iso_record(b"\x00", root, _ISO_BLOCK, directory=True)
    result[881] = 1
    return bytes(result)


def _nocloud_iso(user_data: bytes, meta_data: bytes) -> bytes:
    contents = (user_data, meta_data)
    sizes = tuple((len(data) + _ISO_BLOCK - 1) // _ISO_BLOCK for data in contents)
    extents = (25, 25 + sizes[0])
    blocks = extents[1] + sizes[1]
    result = bytearray(blocks * _ISO_BLOCK)
    for index, kind, root, little, big in ((16, 1, 19, 21, 22), (17, 2, 20, 23, 24)):
        result[index * _ISO_BLOCK : (index + 1) * _ISO_BLOCK] = _iso_volume(
            kind, blocks, root, little, big
        )
        records = _iso_record(b"\x00", root, _ISO_BLOCK, directory=True) + _iso_record(
            b"\x01", root, _ISO_BLOCK, directory=True
        )
        for name, extent, data in zip(("user-data", "meta-data"), extents, contents, strict=True):
            identifier = (
                name.encode("utf-16-be") if kind == 2 else (name.upper() + ";1").encode("ascii")
            )
            records += _iso_record(identifier, extent, len(data))
        result[root * _ISO_BLOCK : root * _ISO_BLOCK + len(records)] = records
        for table, endian in ((little, "little"), (big, "big")):
            path_record = (
                b"\x01\x00" + root.to_bytes(4, endian) + (1).to_bytes(2, endian) + b"\x00\x00"
            )
            result[table * _ISO_BLOCK : table * _ISO_BLOCK + 10] = path_record
    result[18 * _ISO_BLOCK : 18 * _ISO_BLOCK + 7] = b"\xffCD001\x01"
    for data, extent in zip(contents, extents, strict=True):
        result[extent * _ISO_BLOCK : extent * _ISO_BLOCK + len(data)] = data
    return bytes(result)


def jammy_seed(
    token: str, administrator_key: bytes, host_public_key: bytes, host_private_key: bytes
) -> bytes:
    """Stock security policy, separate fixed accounts and a pre-pinned SSH host key.

    The administrator is disposable orchestration authority, never the admitted
    account. Neither package installation nor host/security policy changes occur
    in cloud-init. The owner file is protected by the authenticated stock image.
    """
    if not _DIGEST.fullmatch(token) or any(
        not 0 < len(value) <= 8192
        for value in (administrator_key, host_public_key, host_private_key)
    ):
        raise _Denied(NativeClassification.INVALID_INPUT)
    owner = {"token": token, "administrator_uid": 20000, "server_uid": 20001}
    document = {
        "users": [
            {
                "name": "stvm-admin",
                "uid": 20000,
                "groups": ["sudo"],
                "shell": "/bin/bash",
                "lock_passwd": True,
                "sudo": ["ALL=(ALL) NOPASSWD:ALL"],
                "ssh_authorized_keys": [administrator_key.decode("ascii").strip()],
            },
            {
                "name": "stvm-server",
                "uid": 20001,
                "groups": [],
                "shell": "/bin/bash",
                "lock_passwd": True,
                "sudo": False,
            },
        ],
        "disable_root": True,
        "ssh_pwauth": False,
        "ssh_deletekeys": True,
        "ssh_genkeytypes": [],
        "ssh_keys": {
            "ed25519_private": host_private_key.decode("ascii"),
            "ed25519_public": host_public_key.decode("ascii").strip(),
        },
        "write_files": [
            {
                "path": "/var/lib/spaceterm-vm/owner.json",
                "owner": "root:root",
                "permissions": "0600",
                "content": json.dumps(owner, separators=(",", ":")),
            }
        ],
    }
    user = b"#cloud-config\n" + json.dumps(document, separators=(",", ":")).encode("ascii")
    meta = json.dumps({"instance-id": "stvm-" + token, "local-hostname": "stvm-jammy"}).encode(
        "ascii"
    )
    return _nocloud_iso(user, meta)


def read_jammy_seed(data: bytes) -> dict[str, bytes]:
    """Inspect only our bounded two-file Joliet image, never execute its contents."""
    failure = NativeClassification.INVALID_INPUT
    if not 26 * _ISO_BLOCK <= len(data) <= vm._MAX_SEED_BYTES or len(data) % _ISO_BLOCK:
        raise _Denied(failure)
    volume = data[17 * _ISO_BLOCK : 18 * _ISO_BLOCK]
    if (
        volume[:7] != b"\x02CD001\x01"
        or volume[40:72].decode("utf-16-be").strip() != "CIDATA"
        or volume[128:132] != _both(_ISO_BLOCK, 2)
        or volume[80:88] != _both(len(data) // _ISO_BLOCK, 4)
    ):
        raise _Denied(failure)
    records = data[20 * _ISO_BLOCK : 21 * _ISO_BLOCK]
    offset = 0
    result = {}
    while offset < len(records) and records[offset]:
        size = records[offset]
        record = records[offset : offset + size]
        if len(record) != size or size < 34:
            raise _Denied(failure)
        name_size = record[32]
        name = record[33 : 33 + name_size]
        if name not in (b"\x00", b"\x01"):
            try:
                key = name.decode("utf-16-be")
            except UnicodeError:
                raise _Denied(failure) from None
            extent = int.from_bytes(record[2:6], "little")
            length = int.from_bytes(record[10:14], "little")
            if (
                key not in ("user-data", "meta-data")
                or key in result
                or record[25] != 0
                or extent < 25
                or not 0 < length <= vm._MAX_DOCUMENT_BYTES
                or extent * _ISO_BLOCK + length > len(data)
                or record[2:10] != _both(extent, 4)
                or record[10:18] != _both(length, 4)
            ):
                raise _Denied(failure)
            result[key] = data[extent * _ISO_BLOCK : extent * _ISO_BLOCK + length]
        offset += size
    if set(result) != {"user-data", "meta-data"}:
        raise _Denied(failure)
    return result


_VM_TOOLS = {name: Path("/usr/bin") / name for name in ("ssh-keygen", "qemu-img", "docker", "ssh")}


def _run_vm_tool(
    program: str,
    arguments: tuple[str, ...],
    *,
    inherited: tuple[int, ...] = (),
    timeout: float = 30,
    maximum: int = _MAX_OUTPUT_BYTES,
) -> _OperationResult:
    """Fixed protected utility, bounded output and the existing unreaped group lease.

    The held executable is selected through procfd, including for SSH and keygen.
    No inherited environment, SSH agent, Docker context, stdin or raw stderr is
    forwarded. These processes do not own guest authority or daemon cancellation.
    """
    _require_acquisition_context()
    if program not in _VM_TOOLS or not 0 < timeout <= 900 or not 0 < maximum <= 32 * 1024**2:
        raise _Denied(NativeClassification.INVALID_INPUT)
    files, cleanup = _VerificationFiles(), _Cleanup()
    process = child = None
    failure = None
    output, returncode, exited = b"", None, False
    deadline = time.monotonic() + timeout
    acquisition = _AcquisitionSignals(cleanup)
    try:
        tool = files.hold(_VM_TOOLS[program], directory=False, private=False)
        for entry in tool.entries:
            current = entry.observation
            if current.st_uid != 0 or current.st_mode & 0o022:
                raise _Denied(NativeClassification.UNSAFE_PATH)
        if (
            not stat.S_ISREG(tool.entry.observation.st_mode)
            or tool.entry.observation.st_mode & 0o6000
            or not tool.entry.observation.st_mode & 0o111
        ):
            raise _Denied(NativeClassification.UNSAFE_PATH)
        proc = _ProcBindings(files, _LinuxNative())
        executable = proc.reference(tool.entry)
        try:
            acquisition.install()
            acquisition.check()
            process = _popen(
                (executable, *arguments),
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
                close_fds=True,
                pass_fds=tuple(dict.fromkeys((tool.entry.fd, *inherited))),
                cwd=ROOT,
                env={"PATH": "/usr/bin:/bin", "LANG": "C", "LC_ALL": "C", "HOME": "/nonexistent"},
            )
            child = _ChildLease(process, process.pid)
        finally:
            while acquisition.handlers:
                cleanup.attempt(acquisition.restore)
        acquisition.check()
        output = _read_output(process, deadline, maximum)
        exited = _await_exit(process, deadline)
        if not tool.matches():
            raise _Denied(NativeClassification.UNSAFE_PATH)
        proc.check()
    except _Denied as error:
        failure = error.classification
    except (KeyboardInterrupt, SystemExit) as error:
        cleanup.interrupted(error)
    except Exception:
        failure = NativeClassification.NATIVE_FAILED
    finally:
        if process is not None:
            if child is None:
                child = _ChildLease(process, process.pid)
            try:
                returncode = _retire_child(child, exited=exited, cleanup=cleanup)
            except (KeyboardInterrupt, SystemExit) as error:
                cleanup.interrupted(error)
            except Exception:
                cleanup.complete = False
            finally:
                if child.signals_open:
                    cleanup.attempt(lambda: _kill_owned(child, cleanup))
                    cleanup.attempt(lambda: _reap_child(child, cleanup))
                child.close_pipes(cleanup)
        files.cleanup(cleanup)
    if cleanup.interruption is not None:
        raise cleanup.interruption
    if not cleanup.complete:
        raise _Denied(NativeClassification.CLEANUP_INCOMPLETE)
    if failure is not None:
        raise _Denied(failure)
    return _OperationResult(output, returncode)


def _successful_tool(program: str, arguments: tuple[str, ...], **kwargs) -> bytes:
    result = _run_vm_tool(program, arguments, **kwargs)
    if result.returncode != 0:
        raise _Denied(NativeClassification.PROCESS_FAILED)
    return result.status


def _native_json(data: bytes):
    try:
        return json.loads(
            data, object_pairs_hook=vm._unique_json_object, parse_constant=vm._invalid_json_constant
        )
    except (ValueError, UnicodeError, RecursionError, vm._Denied):
        raise _Denied(NativeClassification.INVALID_STATUS) from None


@contextmanager
def _new_file(directory_fd: int, name: str):
    fd = os.open(
        name,
        os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
        0o600,
        dir_fd=directory_fd,
    )
    try:
        yield fd
        os.fsync(fd)
    finally:
        os.close(fd)


def _write_all(fd: int, data: bytes) -> None:
    remaining = memoryview(data)
    while remaining:
        count = os.write(fd, remaining)
        if count <= 0:
            raise _Denied(NativeClassification.NATIVE_FAILED)
        remaining = remaining[count:]


class NativeVm:
    """Concrete unprivileged QEMU provisioning and fixed local Docker operations.

    No pulls, privileged containers, host policy changes, host reboot or cleanup
    by name. The executor admits every output and records exact IDs before start.
    A lost Docker response remains uncertain in that executor, never a retry.
    """

    def __init__(self, request: vm.PrepareRequest, runner_image_id: str):
        _require_acquisition_context()
        if not vm._local_image_id(runner_image_id):
            raise _Denied(NativeClassification.INVALID_AUTHORITY)
        self.request, self.runner_image_id = request, runner_image_id
        self._credentials: vm.OwnedPath | None = None
        self._runner: vm.RunnerIdentity | None = None

    def create_credentials(self, directory_fd: int) -> None:
        os.mkdir("credentials", 0o700, dir_fd=directory_fd)
        fd = os.open(
            "credentials",
            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
            dir_fd=directory_fd,
        )
        try:
            for name in ("id_ed25519", "host_ed25519"):
                _successful_tool(
                    "ssh-keygen",
                    ("-q", "-t", "ed25519", "-N", "", "-C", "", "-f", f"/proc/self/fd/{fd}/{name}"),
                    inherited=(fd,),
                )
                for leaf in (name, name + ".pub"):
                    key = os.open(leaf, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=fd)
                    try:
                        os.fchmod(key, 0o600)
                        os.fsync(key)
                    finally:
                        os.close(key)
            os.fsync(fd)
        finally:
            os.close(fd)
        self._credentials = vm.OwnedPath.capture(self.request.directory / "credentials")

    def create_os_disk(self, directory_fd: int, image_fd: int, virtual_size_bytes: int) -> None:
        # Check capacity for the declared storage bound before allocating sparse disks.
        available = os.fstatvfs(directory_fd)
        required = (
            self.request.resources.disk_gib + self.request.resources.encrypted_disk_gib + 2
        ) * 1024**3
        if available.f_bavail * available.f_frsize < required:
            raise _Denied(NativeClassification.INPUT_LIMIT)
        with _new_file(directory_fd, "os.qcow2") as output:
            _successful_tool(
                "qemu-img",
                (
                    "convert",
                    "-f",
                    "qcow2",
                    "-O",
                    "qcow2",
                    f"/proc/self/fd/{image_fd}",
                    f"/proc/self/fd/{output}",
                ),
                inherited=(image_fd, output),
                timeout=180,
            )
            _successful_tool(
                "qemu-img",
                ("resize", "-f", "qcow2", f"/proc/self/fd/{output}", str(virtual_size_bytes)),
                inherited=(output,),
            )

    def create_encrypted_disk(self, directory_fd: int, virtual_size_bytes: int) -> None:
        with _new_file(directory_fd, "encrypted.qcow2") as output:
            _successful_tool(
                "qemu-img",
                ("create", "-f", "qcow2", f"/proc/self/fd/{output}", str(virtual_size_bytes)),
                inherited=(output,),
            )

    def inspect_disk(self, disk_fd: int, retained: vm.OwnedPath) -> vm.DiskEvidence:
        value = _native_json(
            _successful_tool(
                "qemu-img",
                ("info", "--output=json", f"/proc/self/fd/{disk_fd}"),
                inherited=(disk_fd,),
            )
        )
        if (
            type(value) is not dict
            or type(value.get("virtual-size")) is not int
            or type(value.get("actual-size")) is not int
        ):
            raise _Denied(NativeClassification.INVALID_STATUS)
        return vm.DiskEvidence(
            retained,
            value.get("format"),
            value["virtual-size"],
            any(name in value for name in ("backing-filename", "full-backing-filename")),
            b'"data-file"' in json.dumps(value).encode(),
            value["actual-size"],
        )

    def create_seed(self, directory_fd: int, token: str) -> None:
        with ExitStack() as handles:
            keys = {}
            for name in ("id_ed25519.pub", "host_ed25519.pub", "host_ed25519"):
                opened = handles.enter_context(
                    vm._open_path(
                        self.request.directory / "credentials" / name,
                        vm.Classification.OWNERSHIP_MISMATCH,
                    )
                )
                vm._private(opened, directory=False, failure=vm.Classification.OWNERSHIP_MISMATCH)
                keys[name] = vm._read_bounded(opened, 8192, vm.Classification.OWNERSHIP_MISMATCH)
                vm._unchanged(opened, vm.Classification.OWNERSHIP_MISMATCH)
            seed = jammy_seed(
                token, keys["id_ed25519.pub"], keys["host_ed25519.pub"], keys["host_ed25519"]
            )
            with _new_file(directory_fd, "seed.iso") as output:
                _write_all(output, seed)

    def inspect_seed(self, seed_fd: int, retained: vm.OwnedPath) -> vm.SeedEvidence:
        size = os.fstat(seed_fd).st_size
        data = _read_bytes(seed_fd, size) if 0 < size <= vm._MAX_SEED_BYTES else b""
        read_jammy_seed(data)
        return vm.SeedEvidence(retained, size, hashlib.sha256(data).hexdigest(), "CIDATA")

    def _docker(self, arguments: tuple[str, ...], *, timeout: float = 30) -> _OperationResult:
        if self._credentials is None:
            raise _Denied(NativeClassification.INVALID_AUTHORITY)
        with vm._open_path(self._credentials.path, vm.Classification.OWNERSHIP_MISMATCH) as config:
            vm._private(config, directory=True, failure=vm.Classification.OWNERSHIP_MISMATCH)
            if config.owned != self._credentials:
                raise _Denied(NativeClassification.UNSAFE_PATH)
            vm._directory_names(
                config.fd,
                frozenset(("id_ed25519", "id_ed25519.pub", "host_ed25519", "host_ed25519.pub")),
                vm.Classification.OWNERSHIP_MISMATCH,
            )
            # The fixed local daemon socket and ancestors cannot be replaced by
            # the account. Its connect ACL grants Docker API use, not path writes.
            for path in (Path("/"), Path("/run"), Path("/run/docker.sock")):
                current = path.lstat()
                if (
                    current.st_uid != 0
                    or (path.name != "docker.sock" and current.st_mode & 0o022)
                    or stat.S_ISLNK(current.st_mode)
                ):
                    raise _Denied(NativeClassification.UNSAFE_PATH)
            if not stat.S_ISSOCK(Path("/run/docker.sock").lstat().st_mode):
                raise _Denied(NativeClassification.UNSAFE_PATH)
            result = _run_vm_tool(
                "docker",
                (
                    "--host",
                    "unix:///run/docker.sock",
                    "--config",
                    f"/proc/self/fd/{config.fd}",
                    *arguments,
                ),
                inherited=(config.fd,),
                timeout=timeout,
            )
            vm._unchanged(config, vm.Classification.OWNERSHIP_MISMATCH)
            if result.returncode != 0:
                raise _Denied(NativeClassification.PROCESS_FAILED)
            return result

    def _image(self) -> None:
        result = _native_json(self._docker(("image", "inspect", self.request.runner_image)).status)
        if (
            type(result) is not list
            or len(result) != 1
            or type(result[0]) is not dict
            or result[0].get("Id") != self.runner_image_id
        ):
            raise _Denied(NativeClassification.INVALID_AUTHORITY)
        config = result[0].get("Config")
        if type(config) is not dict or any(
            config.get(key) not in (None, [], {})
            for key in ("Env", "Healthcheck", "Volumes", "ExposedPorts")
        ):
            raise _Denied(NativeClassification.INVALID_AUTHORITY)

    def create_container(self, vm: vm.PreparedVm, runner: vm.RunnerIdentity) -> str | None:
        from spaceterm_tasks import server_vm as vm_module

        prepared = vm
        if (
            prepared.request != self.request
            or os.geteuid() == 0
            or runner.uid != os.geteuid()
            or runner.gid != os.getegid()
        ):
            raise _Denied(NativeClassification.INVALID_AUTHORITY)
        kvm = Path("/dev/kvm").stat()
        if (
            not stat.S_ISCHR(kvm.st_mode)
            or (os.major(kvm.st_rdev), os.minor(kvm.st_rdev)) != (10, 232)
            or kvm.st_gid != runner.kvm_gid
        ):
            raise _Denied(NativeClassification.UNSAFE_PATH)
        memory = Path("/proc/meminfo").read_bytes()
        match = re.search(rb"^MemAvailable:\s+([0-9]+) kB$", memory, re.MULTILINE)
        if match is None or int(match[1]) < 8 * 1024**2:
            raise _Denied(NativeClassification.INPUT_LIMIT)
        self._image()
        self._runner = runner
        command = vm_module._start_command(prepared, runner)
        # Creation is separate from start, so the executor can durably bind the ID.
        arguments = tuple(arg for arg in command[2:] if arg != "--detach")
        output = self._docker(("create", *arguments)).status
        try:
            identifier = output.decode("ascii").strip()
        except UnicodeError:
            raise _Denied(NativeClassification.INVALID_STATUS) from None
        return identifier if _DIGEST.fullmatch(identifier) else None

    def inspect(self, container_id: str) -> vm.ContainerObservation | None:
        if not _DIGEST.fullmatch(container_id) or self._runner is None:
            raise _Denied(NativeClassification.INVALID_AUTHORITY)
        # Utility failure is unavailable, not absence. An exact-ID successful
        # listing is the independent absence observation after remove.
        found = self._docker(
            (
                "container",
                "ls",
                "--all",
                "--no-trunc",
                "--format",
                "{{.ID}}",
                "--filter",
                "id=" + container_id,
            )
        ).status
        if found == b"":
            return None
        if found != container_id.encode("ascii") + b"\n":
            raise _Denied(NativeClassification.INVALID_STATUS)
        raw = _native_json(self._docker(("container", "inspect", container_id)).status)
        return _container_observation(
            raw, self.request, self._runner, self.runner_image_id, container_id
        )

    def start_container(self, container_id: str) -> None:
        if not _DIGEST.fullmatch(container_id):
            raise _Denied(NativeClassification.INVALID_INPUT)
        self._docker(("start", container_id))
        deadline = time.monotonic() + 10
        with vm._open_path(
            self.request.directory, vm.Classification.OWNERSHIP_MISMATCH
        ) as directory:
            while True:
                try:
                    fd = os.open(
                        "qmp.sock", os.O_PATH | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=directory.fd
                    )
                except FileNotFoundError:
                    if time.monotonic() >= deadline:
                        raise _Denied(NativeClassification.TIMED_OUT) from None
                    time.sleep(0.05)
                    continue
                try:
                    current = os.fstat(fd)
                    if not stat.S_ISSOCK(current.st_mode) or current.st_uid != os.geteuid():
                        raise _Denied(NativeClassification.UNSAFE_PATH)
                    # QEMU inherits the runner's umask. Restrict only its held
                    # socket inode, never chmod a re-resolved name or host policy.
                    os.chmod(f"/proc/self/fd/{fd}", 0o600)
                    selected = os.stat("qmp.sock", dir_fd=directory.fd, follow_symlinks=False)
                    if (selected.st_dev, selected.st_ino) != (current.st_dev, current.st_ino):
                        raise _Denied(NativeClassification.UNSAFE_PATH)
                finally:
                    os.close(fd)
                return

    def stop_container(self, container_id: str, timeout_seconds: int) -> None:
        if timeout_seconds != 30 or not _DIGEST.fullmatch(container_id):
            raise _Denied(NativeClassification.INVALID_INPUT)
        self._docker(("stop", "--time", "30", container_id), timeout=45)

    def remove_container(self, container_id: str) -> None:
        if not _DIGEST.fullmatch(container_id):
            raise _Denied(NativeClassification.INVALID_INPUT)
        self._docker(("rm", container_id))


def _container_observation(
    raw, request: vm.PrepareRequest, runner: vm.RunnerIdentity, image_id: str, container_id: str
) -> vm.ContainerObservation:
    """Compare effective Docker policy without dropping inherited or extra grants."""
    failure = NativeClassification.INVALID_STATUS
    if type(raw) is not list or len(raw) != 1 or type(raw[0]) is not dict:
        raise _Denied(failure)
    value = raw[0]
    config, host, state = (value.get(key) for key in ("Config", "HostConfig", "State"))
    if any(type(item) is not dict for item in (config, host, state)):
        raise _Denied(failure)

    def matches(document: dict, expected: dict) -> None:
        for key, selected in expected.items():
            if key not in document or not vm._same_typed_value(document[key], selected):
                raise _Denied(failure)

    matches(value, {"Id": container_id, "Image": image_id})
    matches(
        config,
        {
            "Image": request.runner_image,
            "User": f"{runner.uid}:{runner.gid}",
            "Env": None,
            "WorkingDir": "/vm",
            "Entrypoint": ["/usr/bin/qemu-system-x86_64"],
            "Cmd": list(vm._guest_arguments(request)),
            "Volumes": None,
            "ExposedPorts": {"2222/tcp": {}},
            "Tty": False,
            "OpenStdin": False,
        },
    )
    if config.get("Healthcheck") not in (None, {}) or config.get("StopSignal", "") not in (
        "",
        "SIGTERM",
    ):
        raise _Denied(failure)
    matches(
        host,
        {
            "NanoCpus": 2_000_000_000,
            "Memory": 6144 * 1024**2,
            "MemorySwap": 6144 * 1024**2,
            "PidsLimit": 256,
            "ReadonlyRootfs": True,
            "Privileged": False,
            "CapDrop": ["ALL"],
            "CapAdd": None,
            "SecurityOpt": ["no-new-privileges:true"],
            "GroupAdd": [str(runner.kvm_gid)],
            "Tmpfs": {"/tmp": "rw,noexec,nosuid,size=64m"},
            "Devices": [
                {"PathOnHost": "/dev/kvm", "PathInContainer": "/dev/kvm", "CgroupPermissions": "rw"}
            ],
            "NetworkMode": "bridge",
            "PidMode": "",
            "IpcMode": "private",
            "UTSMode": "",
            "UsernsMode": "",
            "CgroupnsMode": "private",
            "PublishAllPorts": False,
            "PortBindings": {
                "2222/tcp": [{"HostIp": "127.0.0.1", "HostPort": str(request.ssh.port)}]
            },
            "LogConfig": {"Type": "local", "Config": {"max-size": "10m", "max-file": "2"}},
            "RestartPolicy": {"Name": "no", "MaximumRetryCount": 0},
            "AutoRemove": False,
            "Runtime": "runc",
            "OomKillDisable": False,
            "OomScoreAdj": 0,
            "CgroupParent": "",
            "Binds": None,
            "VolumesFrom": None,
            "DeviceRequests": None,
            "DeviceCgroupRules": None,
            "Sysctls": None,
            "StorageOpt": None,
            "Ulimits": None,
        },
    )
    for key in (
        "CpuShares",
        "CpuPeriod",
        "CpuQuota",
        "CpuRealtimePeriod",
        "CpuRealtimeRuntime",
        "BlkioWeight",
        "MemoryReservation",
        "MemorySwappiness",
    ):
        if key in host and host[key] not in (None, 0):
            raise _Denied(failure)
    for key in ("CpusetCpus", "CpusetMems"):
        if key not in host or host[key] != "":
            raise _Denied(failure)
    for key in (
        "BlkioWeightDevice",
        "BlkioDeviceReadBps",
        "BlkioDeviceWriteBps",
        "BlkioDeviceReadIOps",
        "BlkioDeviceWriteIOps",
    ):
        if key not in host or host[key] not in (None, []):
            raise _Denied(failure)
    if (
        host.get("Init") not in (None, False)
        or host.get("Links") not in (None, [])
        or host.get("ExtraHosts") not in (None, [])
    ):
        raise _Denied(failure)
    if type(host.get("Mounts")) is not list or len(host["Mounts"]) != 1:
        raise _Denied(failure)
    source = host["Mounts"][0]
    if type(source) is not dict or set(source) - {
        "Type",
        "Source",
        "Target",
        "ReadOnly",
        "BindOptions",
        "Consistency",
    }:
        raise _Denied(failure)
    matches(
        source,
        {
            "Type": "bind",
            "Source": str(request.directory),
            "Target": "/vm",
            "BindOptions": {"NonRecursive": True},
        },
    )
    if source.get("ReadOnly", False) is not False or source.get("Consistency", "") != "":
        raise _Denied(failure)
    mounts = value.get("Mounts")
    if type(mounts) is not list or len(mounts) != 1 or type(mounts[0]) is not dict:
        raise _Denied(failure)
    matches(
        mounts[0],
        {
            "Type": "bind",
            "Source": str(request.directory),
            "Destination": "/vm",
            "RW": True,
            "Propagation": "rprivate",
        },
    )
    if (
        value.get("AppArmorProfile") != "docker-default"
        or type(state.get("Running")) is not bool
        or config.get("StopTimeout") != 30
    ):
        raise _Denied(failure)
    labels = config.get("Labels")
    if (
        type(labels) is not dict
        or type(labels.get(vm.OWNER_LABEL)) is not str
        or not _DIGEST.fullmatch(labels[vm.OWNER_LABEL])
    ):
        raise _Denied(failure)
    # These closed records are returned only after every represented fact matched
    # the effective native observation. Absent required fields never default safe.
    return vm.ContainerObservation(
        container_id,
        labels[vm.OWNER_LABEL],
        request.directory,
        state["Running"],
        vm._expected_configuration(request, runner),
        vm._expected_policy(image_id),
    )


_GUEST_OBSERVE = r"""
import ctypes, json, os, pwd, re, stat, subprocess
from pathlib import Path
p = Path("/var/lib/spaceterm-vm/owner.json")
for x in (Path("/"), Path("/var"), Path("/var/lib"), p.parent, p):
    s = x.lstat()
    if s.st_uid != 0 or s.st_mode & 0o022 or stat.S_ISLNK(s.st_mode):
        raise SystemExit(2)
fd = os.open(p, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
try:
    if os.fstat(fd).st_size > 8192:
        raise SystemExit(2)
    owner = json.loads(os.read(fd, 8193))
finally:
    os.close(fd)
if set(owner) != {"token", "administrator_uid", "server_uid"} or pwd.getpwnam("stvm-admin").pw_uid != 20000 or pwd.getpwnam("stvm-server").pw_uid != 20001:
    raise SystemExit(2)
os_release = Path("/etc/os-release").read_text()
if not re.search(r'^ID=ubuntu$', os_release, re.M) or not re.search(r'^VERSION_ID="22.04"$', os_release, re.M):
    raise SystemExit(2)
libc = ctypes.CDLL(None).gnu_get_libc_version
libc.restype = ctypes.c_char_p
version = libc().decode().split(".")
kernel = re.match(r"(\d+)\.(\d+)\.(\d+)", os.uname().release)
systemd = subprocess.run(("/usr/bin/systemctl", "--version"), env={"PATH":"/usr/bin:/bin", "LC_ALL":"C"}, capture_output=True, timeout=5)
if systemd.returncode or len(systemd.stdout) > 8192:
    raise SystemExit(2)
supervisor = re.match(rb"systemd (\d+)", systemd.stdout)
if not kernel or not supervisor:
    raise SystemExit(2)
owner.update(os_version=[22,4,0], kernel_version=[int(x) for x in kernel.groups()], libc_version=[int(x) for x in version] + [0], systemd_version=int(supervisor[1]), apparmor_enabled=Path("/sys/module/apparmor/parameters/enabled").read_text().strip()=="Y", apport_enabled=bool(re.search(r"^enabled=1$", Path("/etc/default/apport").read_text(), re.M)), suid_dumpable=int(Path("/proc/sys/fs/suid_dumpable").read_text()))
print(json.dumps(owner, separators=(",", ":")))
"""


class LinuxGuestTransport:
    """Strict SSH transport to the original seeded guest, using disposable keys.

    Host-key checking is mandatory before any remote command. Agent forwarding,
    TOFU, user/system configuration, multiplexing and host-key updates are off.
    Authentication inputs are sealed descriptor copies of independently retained
    credentials. Fixed protected guest operations never accept a caller command.
    """

    def __init__(self, receipt: "ExecutionReceipt", native: vm.ContainerInspector):
        if (
            receipt.prepared is None
            or receipt.container_id is None
            or len(receipt.prepared.ownership.credential_files) != 4
        ):
            raise TaskError("ownership-mismatch")
        self.receipt, self.native = receipt, native

    def _ssh(self, script: str, *, maximum: int = 8192, timeout: float = 30) -> bytes:
        prepared = self.receipt.prepared
        assert prepared is not None
        with ExitStack() as handles:
            before = self.native.inspect(self.receipt.container_id or "")
            if before is None or not before.running or before.owner_token != self.receipt.token:
                raise TaskError("ownership-mismatch")
            pins = {item.path.name: item for item in prepared.ownership.credential_files}
            files = _VerificationFiles()
            cleanup = _Cleanup()
            handles.callback(files.cleanup, cleanup)
            for name in ("id_ed25519", "host_ed25519.pub"):
                owned = pins[name]
                opened = handles.enter_context(
                    vm._open_path(owned.path, vm.Classification.OWNERSHIP_MISMATCH)
                )
                vm._private(opened, directory=False, failure=vm.Classification.OWNERSHIP_MISMATCH)
                data = vm._read_bounded(opened, 8192, vm.Classification.OWNERSHIP_MISMATCH)
                if (
                    opened.owned != owned
                    or hashlib.sha256(data).hexdigest() != owned.content_sha256
                ):
                    raise TaskError("ownership-mismatch")
                vm._unchanged(opened, vm.Classification.OWNERSHIP_MISMATCH)
                if name == "id_ed25519":
                    files.seal("guest-identity", data)
                else:
                    alias = "stvm-" + self.receipt.token
                    public = data.decode("ascii").strip()
                    if not re.fullmatch(r"ssh-ed25519 [A-Za-z0-9+/=]{64,128}", public):
                        raise TaskError("ownership-mismatch")
                    files.seal("guest-known-hosts", (alias + " " + public + "\n").encode("ascii"))
            key, known = (entry.entry.fd for entry in files.inputs)
            command = "/usr/bin/sudo -n -- /usr/bin/python3 -I -c " + shlex.quote(script)
            arguments = (
                "-F",
                "/dev/null",
                "-p",
                str(prepared.request.ssh.port),
                "-i",
                f"/proc/self/fd/{key}",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "IdentityAgent=none",
                "-o",
                "BatchMode=yes",
                "-o",
                "PasswordAuthentication=no",
                "-o",
                "KbdInteractiveAuthentication=no",
                "-o",
                "PreferredAuthentications=publickey",
                "-o",
                "StrictHostKeyChecking=yes",
                "-o",
                "HostKeyAlgorithms=ssh-ed25519",
                "-o",
                "HostKeyAlias=" + alias,
                "-o",
                f"UserKnownHostsFile=/proc/self/fd/{known}",
                "-o",
                "GlobalKnownHostsFile=/dev/null",
                "-o",
                "UpdateHostKeys=no",
                "-o",
                "ForwardAgent=no",
                "-o",
                "ClearAllForwardings=yes",
                "-o",
                "ControlMaster=no",
                "-o",
                "ControlPath=none",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ConnectionAttempts=1",
                "-o",
                "LogLevel=ERROR",
                "-T",
                "stvm-admin@127.0.0.1",
                command,
            )
            output = _successful_tool(
                "ssh", arguments, inherited=(key, known), timeout=timeout, maximum=maximum
            )
            after = self.native.inspect(self.receipt.container_id or "")
            if after != before:
                raise TaskError("ownership-mismatch")
        if not cleanup.complete:
            raise TaskError("cleanup-unresolved")
        return output

    def observe(self) -> "GuestObservation":
        from spaceterm_tasks.linux_custody_evidence import GuestObservation, admit_guest_observation

        try:
            value = _native_json(self._ssh(_GUEST_OBSERVE))
            fields = {
                "token",
                "administrator_uid",
                "server_uid",
                "os_version",
                "kernel_version",
                "libc_version",
                "systemd_version",
                "apparmor_enabled",
                "apport_enabled",
                "suid_dumpable",
            }
            if (
                type(value) is not dict
                or set(value) != fields
                or value["token"] != self.receipt.token
                or value["administrator_uid"] != 20000
                or value["server_uid"] != 20001
            ):
                raise TaskError("ownership-mismatch")
            return admit_guest_observation(
                self.receipt,
                GuestObservation(
                    self.receipt.container_id or "",
                    value["token"],
                    value["administrator_uid"],
                    value["server_uid"],
                    tuple(value["os_version"]),
                    tuple(value["kernel_version"]),
                    tuple(value["libc_version"]),
                    value["systemd_version"],
                    value["apparmor_enabled"],
                    value["apport_enabled"],
                    value["suid_dumpable"],
                ),
            )
        except (vm._Denied, _Denied, ValueError, TypeError, KeyError, OSError):
            raise TaskError("native-unavailable") from None


class GuestObserver(Protocol):
    def observe(self) -> "GuestObservation": ...


def run_owned_jammy_transport(
    executor: "VmExecutor",
    request: vm.PrepareRequest,
    state_directory: vm.OwnedPath,
    peer_factory: "Callable[[ExecutionReceipt], GuestObserver]",
) -> None:
    """One receipt owner from fresh allocation through authenticated observation.

    This is a provisioning/transport check, not custody evidence. Failed or lost
    phase responses retain the independent journal for reviewed recovery. Cleanup
    uses only a successfully admitted Started receipt and the executor's atomic
    no-overwrite detachment. Interruption never creates a second VM or credential.
    """
    from spaceterm_tasks.linux_custody_evidence import admit_guest_observation

    prepared = executor.prepare(request, state_directory=state_directory)
    if prepared.classification != vm.Classification.PREPARED or prepared.receipt is None:
        raise TaskError(prepared.classification.value)
    started = executor.start(prepared.receipt)
    if started.classification != vm.Classification.STARTED or started.receipt is None:
        # A timeout can leave an exact native effect. A path or new observation
        # cannot supply the missing successful phase transition.
        raise TaskError(started.classification.value)
    receipt = started.receipt
    primary = None
    try:
        peer = peer_factory(receipt)
        deadline = time.monotonic() + request.resources.boot_timeout_seconds
        while True:
            try:
                admit_guest_observation(receipt, peer.observe())
                if time.monotonic() >= deadline:
                    raise TaskError("timed-out")
                break
            except TaskError as error:
                if str(error) != "native-unavailable":
                    raise
                if time.monotonic() >= deadline:
                    raise TaskError("timed-out") from None
                time.sleep(min(1, max(0, deadline - time.monotonic())))
    except BaseException as error:
        primary = error
    finally:
        # Defer further termination during bounded exact-ID stop and detachment.
        # This mask is installed after child creation, never inherited by a guest.
        previous = signal.pthread_sigmask(
            signal.SIG_BLOCK, {signal.SIGINT, signal.SIGTERM, signal.SIGHUP}
        )
        cleanup_error = None
        try:
            stopped = executor.stop(receipt)
            if stopped.classification != vm.Classification.STOPPED or stopped.receipt is None:
                cleanup_error = TaskError("cleanup-unresolved")
            else:
                removed = executor.remove(stopped.receipt)
                if removed.classification != vm.Classification.REMOVED:
                    cleanup_error = TaskError("cleanup-unresolved")
        except BaseException:
            cleanup_error = TaskError("cleanup-unresolved")
        finally:
            try:
                signal.pthread_sigmask(signal.SIG_SETMASK, previous)
            except BaseException as error:
                if primary is None:
                    primary = error
        if cleanup_error is not None:
            raise cleanup_error from None
    if primary is not None:
        if isinstance(primary, (KeyboardInterrupt, SystemExit, TaskError)):
            raise primary
        raise TaskError("native-unavailable") from None


def owned_vm_transport(approval: Path) -> None:
    """Executable one-shot composition from independently provisioned local pins.

    Approval is local operator input, separate from downloaded data and all
    checkouts. It contains public pins and allocation paths, never credentials.
    This approves only disposable VM operations, not SpaceTerm native admission.
    The original ExecutionReceipt is never reconstructed from a manifest or path.
    """
    from spaceterm_tasks.server_vm_executor import VmExecutor

    try:
        _require_acquisition_context()
        if os.geteuid() == 0 or os.uname().machine != "x86_64":
            raise TaskError("unsupported-host")
        with vm._open_path(approval, vm.Classification.UNSAFE_PATH) as opened:
            vm._private(opened, directory=False, failure=vm.Classification.UNSAFE_PATH)
            document = _native_json(
                vm._read_bounded(opened, vm._MAX_DOCUMENT_BYTES, vm.Classification.UNSAFE_PATH)
            )
            vm._unchanged(opened, vm.Classification.UNSAFE_PATH)
        fields = {
            "schema",
            "directory",
            "state_directory",
            "scratch_directory",
            "image",
            "runner_image",
            "runner_image_id",
            "ssh_port",
            "keyring",
        }
        if (
            type(document) is not dict
            or set(document) != fields
            or type(document["schema"]) is not int
            or document["schema"] != 1
        ):
            raise TaskError("invalid-request")
        image, keyring = document["image"], document["keyring"]
        if (
            type(image) is not dict
            or set(image)
            != {"path", "checksums", "signature", "sha256", "signer_fingerprint", "size_bytes"}
            or type(keyring) is not dict
            or set(keyring) != {"path", "sha256", "size_bytes", "primary_fingerprints"}
            or type(keyring["primary_fingerprints"]) is not list
        ):
            raise TaskError("invalid-request")
        paths = [
            Path(value)
            for value in (
                approval,
                document["directory"],
                document["state_directory"],
                document["scratch_directory"],
                image["path"],
                image["checksums"],
                image["signature"],
                keyring["path"],
            )
        ]
        if any(not path.is_absolute() for path in paths) or any(
            path.is_relative_to(ROOT) for path in paths
        ):
            raise TaskError("unsafe-path")
        # Approval, trust, and each downloaded object have distinct directories.
        if len({path.parent for path in (paths[0], *paths[4:])}) != 5:
            raise TaskError("unsafe-path")
        guest = vm.GuestImage(
            paths[4],
            paths[5],
            paths[6],
            image["sha256"],
            image["signer_fingerprint"],
            image["size_bytes"],
        )
        request = vm.PrepareRequest(
            paths[1], guest, document["runner_image"], ssh=vm.SshEndpoint(port=document["ssh_port"])
        )
        approved = ApprovedKeyring(
            paths[7],
            keyring["sha256"],
            keyring["size_bytes"],
            tuple(keyring["primary_fingerprints"]),
        )
        kvm = Path("/dev/kvm").stat()
        if not os.access("/dev/kvm", os.R_OK | os.W_OK, effective_ids=True):
            raise TaskError("native-unavailable")
        runner = vm.RunnerIdentity(os.geteuid(), os.getegid(), kvm.st_gid)
        native = NativeVm(request, document["runner_image_id"])
        signatures = NativeSignatureVerifier(
            expected_primary=guest.signer_fingerprint, keyring=approved, scratch_directory=paths[3]
        )
        executor = VmExecutor(
            ROOT, signatures, native, runner, request.runner_image, document["runner_image_id"]
        )
        run_owned_jammy_transport(
            executor,
            request,
            vm.OwnedPath.capture(paths[2]),
            lambda receipt: LinuxGuestTransport(receipt, native),
        )
    except (vm._Denied, _Denied, KeyError, TypeError, ValueError, OSError):
        raise TaskError("native-unavailable") from None
