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
import os
import re
import secrets
import selectors
import signal
import stat
import struct
import subprocess
import sys
import threading
import time
from collections.abc import Callable
from dataclasses import dataclass, field
from datetime import UTC, date, datetime
from enum import StrEnum
from pathlib import Path
from types import FrameType
from typing import Never

from spaceterm_tasks import ROOT

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
    """Defer Python handlers until the new child has an owned group lease.

    No signal mask changes during spawning, so the child inherits the caller's
    original mask. Only handler restoration briefly blocks these signals, after
    acquisition (or a failed spawn). Restore handlers and the exact prior mask
    before dispatching bounded pending callbacks with their actual signal frames.
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
                self.handlers[signum] = (handler, self.native.snapshot(signum))
                signal.signal(signum, self.handler)

    def restore(self) -> None:
        if not self.handlers:
            return
        # A restored handler may raise. Block only during restoration, never
        # across Popen, and finish all handlers before unmasking pending signals.
        original_mask = signal.pthread_sigmask(signal.SIG_BLOCK, set(self.handlers))
        try:
            for signum, (handler, action) in self.handlers.items():
                if signal.getsignal(signum) is self.handler:
                    self.cleanup.attempt(lambda: signal.signal(signum, handler))
                if signal.getsignal(signum) is handler:
                    # signal.signal resets siginterrupt/SA_RESTART and other
                    # kernel action state. Restore that state, not just the
                    # Python callable; do not overwrite an intentional new handler.
                    self.cleanup.attempt(lambda: self.native.restore(signum, action))
        finally:
            self.cleanup.attempt(lambda: signal.pthread_sigmask(signal.SIG_SETMASK, original_mask))
        for signum, frame in self.pending:
            try:
                self.handlers[signum][0](signum, frame)
            except (KeyboardInterrupt, SystemExit) as error:
                self.cleanup.interrupted(error)
            except BaseException as error:
                if self.failure is None:
                    self.failure = error
        self.pending.clear()
        self.handlers.clear()

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
    # Native Linux glibc statfs on the supported 64-bit hosts. Reject another
    # native word size before passing this storage to libc.
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
    """Four fixed libc calls; no native executable or syscall-number injection."""

    def __init__(self):
        if ctypes.sizeof(ctypes.c_long) != 8 or ctypes.sizeof(ctypes.c_void_p) != 8:
            raise _Denied(NativeClassification.UNAVAILABLE)
        try:
            library = ctypes.CDLL(None, use_errno=True)
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
    if authority and group_present:
        try:
            _signal_owned(child, signal.SIGKILL)
        except ProcessLookupError:
            pass
        except (KeyboardInterrupt, SystemExit) as error:
            cleanup.interrupted(error)
        except Exception:
            cleanup.complete = False
    # Escaped/inherited writers have no authority to extend pipe closure or reap.
    child.close_pipes(cleanup)
    return _reap_child(child, cleanup)


def _read_output(process: subprocess.Popen, deadline: float) -> bytes:
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
                    chunk = os.read(key.fd, min(_READ_BYTES, _MAX_OUTPUT_BYTES + 1 - counts[index]))
                except BlockingIOError:
                    continue
                if not chunk:
                    selector.unregister(key.fd)
                    continue
                counts[index] += len(chunk)
                if counts[index] > _MAX_OUTPUT_BYTES:
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
            acquisition.restore()
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
            try:
                returncode = _retire_child(child, exited=exited, cleanup=cleanup)
            except (KeyboardInterrupt, SystemExit) as error:
                cleanup.interrupted(error)
                if child.signals_open:
                    cleanup.attempt(lambda: _signal_owned(child, signal.SIGKILL))
                cleanup.attempt(lambda: _reap_child(child, cleanup))
            except Exception:
                cleanup.complete = False
                if child.signals_open:
                    cleanup.attempt(lambda: _signal_owned(child, signal.SIGKILL))
                cleanup.attempt(lambda: _reap_child(child, cleanup))
            finally:
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
    The production operation is Linux-only VM tooling on the main Python thread.
    Unsupported hosts and worker threads return UNAVAILABLE before native effects.
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
