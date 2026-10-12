"""Public-interface tests for exact-byte authentication and bounded children.

All documents, signatures, keyrings and GPG facts below are synthetic. Published
fingerprints are independent input facts from the VM design, not evidence that an
Ubuntu artifact, simplestreams document, archive or systemd tag was verified.
Portable policy tests enter the public verifier through a fixed private operation
adapter. Linux tests keep real files, descriptors, seals and processes; only the
fixed native spawn boundary runs a trusted Python fixture with -I. Unsupported
platform tests use the production operation selection and forbid effects.
Acquisition cancellation sends real SIGINT after a disposable new-session child
starts, inside its Popen constructor or before the verifier assigns its lease.
The controls check unchanged child masks, bounded custom-handler delivery and
close-once streams. An autonomous deadline and independent marker-based fixture
cleanup bound a failing verifier's child.
No test imports keys, runs GPG, downloads artifacts or operates Docker/QEMU/SSH.
Watcher silence is not authentication or complete mutation history. Sealed input
bytes, not timestamps or observation alone, supply the immutable-byte contract.
"""

import base64
import errno
import fcntl
import hashlib
import io
import json
import math
import mmap
import os
import re
import signal
import stat
import struct
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack, redirect_stderr, redirect_stdout
from dataclasses import replace
from pathlib import Path
from types import FrameType
from typing import TYPE_CHECKING, TypedDict, Unpack, cast
from unittest.mock import patch

from spaceterm_tasks import ROOT, TaskError
from spaceterm_tasks import server_vm_linux as native
from spaceterm_tasks.server_vm_linux import (
    ApprovedKeyring,
    NativeClassification,
    NativeSignatureVerifier,
)

CHECKSUM_KEY = "D2EB44626FDDC30B513D5BB71A5D6C4C7DB87C81"
SIMPLESTREAMS_KEY = "4A3CE3CD565D7EB5C810E2B97FF3F408476CF100"
ARCHIVE_KEY = "F6ECB3762474EDA9D21B7022871920D1991BC93C"
SIGNING_SUBKEY = "B" * 40  # Synthetic, not an upstream signing subkey.
DOCUMENT = b"0" * 64 + b" *synthetic-image.img\r\n"
SIGNATURE = b"\x00synthetic detached signature\xff\r\n"
KEYRING_BYTES = b"synthetic approved keyring\x00not an OpenPGP keyring"
LEAK = "PRIVATE /untrusted/download/credential SECRET_VALUE=do-not-report"
_REAL_POPEN = subprocess.Popen
# Linux 5.15 UAPI values. Expected seal bits do not come from the implementation.
_F_ADD_SEALS = 1033
_F_GET_SEALS = 1034
_REQUIRED_SEALS = 0x0001 | 0x0002 | 0x0004 | 0x0008
_IN_ATTRIB = 0x00000004
_IN_MOVED_FROM = 0x00000040
_IN_MOVED_TO = 0x00000080
_IN_DELETE_SELF = 0x00000400
_IN_MOVE_SELF = 0x00000800
_IN_UNMOUNT = 0x00002000
_IN_Q_OVERFLOW = 0x00004000
_IN_IGNORED = 0x00008000
_PROC_FD = re.compile(r"/proc/self/fd/([0-9]+)\Z")


def good_status(
    *,
    primary=CHECKSUM_KEY,
    signing=None,
    include_primary=True,
    expires=0,
    uid="fixture signer, not authority",
):
    """Known GPG status grammar, not the result of a cryptographic operation."""
    signing = signing or primary
    binding = f" {primary}" if include_primary else ""
    return (
        "[GNUPG:] NEWSIG\n"
        f"[GNUPG:] KEY_CONSIDERED {primary} 0\n"
        f"[GNUPG:] GOODSIG {signing[-16:]} {uid}\n"
        f"[GNUPG:] VALIDSIG {signing} 2020-01-01 1577836800 {expires} "
        f"4 0 1 8 00{binding}\n"
        "[GNUPG:] TRUST_UNDEFINED 0 pgp\n"
    ).encode()


# The fixture is written into a trusted script directory, separate from each
# download and from native scratch. It emits controlled facts, not GPG evidence.
_CHILD = r"""
import base64, json, os, pathlib, signal, sys, time
control = json.loads(pathlib.Path(sys.argv[1]).read_bytes())
resources = control.get("resources")
if resources:
    inherited = set()
    for name in os.listdir("/proc/self/fd"):
        fd = int(name)
        if fd < 3:
            continue
        try:
            os.fstat(fd)
        except OSError:
            continue  # The temporary directory-listing descriptor is already closed.
        inherited.add(fd)
    if inherited != set(resources["fds"]):
        raise SystemExit(91)
    for item in resources["inputs"]:
        # Open exactly the resource references passed to GPG, not a parent-side
        # alias. The fixture proves inherited procfd readability, not OpenPGP.
        with open(item["path"], "rb") as stream:
            if stream.read() != base64.b64decode(item["bytes"]):
                raise SystemExit(92)
    home = os.stat(resources["home"])
    if [home.st_dev, home.st_ino] != resources["home_identity"]:
        raise SystemExit(93)
stdout = base64.b64decode(control["stdout"])
stderr = base64.b64decode(control["stderr"])
def emit(fd, data):
    for offset in range(0, len(data), 4096):
        chunk = memoryview(data)[offset:offset + 4096]
        while chunk:
            chunk = chunk[os.write(fd, chunk):]
mode = control["mode"]
if mode == "normal":
    emit(1, stdout)
    emit(2, stderr)
    raise SystemExit(control["exit_code"])
signal.signal(signal.SIGTERM, signal.SIG_IGN)
child = None
if control["descendant"]:
    child = os.fork()
    if child == 0:
        if mode == "escaped-holder":
            os.setsid()
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        pathlib.Path(control["ready"]).write_text("ready")
        # The fixture, not the verifier, owns this retirement protocol. In
        # particular, teardown must not signal an escaped or reused numeric PID.
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if pathlib.Path(control["retire"]).exists():
                break
            time.sleep(0.01)
        os._exit(0)
    ready_deadline = time.monotonic() + 3
    while not pathlib.Path(control["ready"]).exists():
        if time.monotonic() >= ready_deadline:
            raise SystemExit(97)
        time.sleep(0.005)
pathlib.Path(control["record"]).write_text(
    json.dumps({"leader": os.getpid(), "descendant": child})
)
if mode == "constructor-interrupt":
    pathlib.Path(control["ready"]).write_text("ready")
if mode in ("leader-exit", "escaped-holder"):
    emit(1, stdout)
    os._exit(0)
if mode == "stdout-overflow":
    emit(1, stdout)
if mode == "stderr-overflow":
    emit(2, stderr)
if mode == "interrupt-owner":
    time.sleep(0.2)
    os.kill(os.getppid(), signal.SIGINT)
# Constructor cancellation has a shorter autonomous safety bound, independent
# of verifier retirement and the fixture owner's explicit retirement marker.
deadline = time.monotonic() + (10 if mode == "constructor-interrupt" else 20)
while time.monotonic() < deadline:
    if pathlib.Path(control["retire"]).exists():
        os._exit(0)
    if mode == "trickle":
        emit(1, b"[GNUPG:] KEY_CONSIDERED D2EB44626FDDC30B513D5BB71A5D6C4C7DB87C81 0\n")
    time.sleep(0.02)
"""


class ControlledChild:
    """A real child, with guards against unbounded native capture or waits."""

    def __init__(self, process, *, retire, unreapable=False, resources=()):
        self.process = process
        self.retire = retire
        self.unreapable = unreapable
        self.resources = tuple(resources)
        self.group_signals_open = True
        self.group_kill_sent = False
        self.waits = []
        self.communications = 0

    def __getattr__(self, name):
        return getattr(self.process, name)

    def poll(self):
        self.group_signals_open = False
        return None if self.unreapable else self.process.poll()

    def wait(self, timeout=None):
        self.waits.append(timeout)
        if timeout is None or not math.isfinite(timeout) or timeout < 0:
            raise AssertionError("native child wait must have a finite bound")
        # Once a consuming operation starts, returncode is no longer sufficient
        # to authorize group signals, including fixture fallback signals.
        self.group_signals_open = False
        for fd in self.resources:
            os.fstat(fd)  # Retain all four resources until child retirement finishes.
        if self.unreapable:
            raise subprocess.TimeoutExpired(LEAK, timeout)
        return self.process.wait(timeout=timeout)

    def communicate(self, *args, **kwargs):
        self.communications += 1
        raise AssertionError("native output must be drained incrementally")


class _VerifierConfiguration(TypedDict, total=False):
    expected_primary: str | None
    keyring: ApprovedKeyring | None
    scratch_directory: Path | None


class VerificationFixture(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name).resolve()
        self.directory.chmod(0o700)
        self.calls = []
        self.children = []
        self.scripts = self.directory / "trusted-scripts"
        self.scratch = self.directory / "trusted-scratch"
        self.keys = self.directory / "approved-keys"
        self.checksum_download = self.directory / "checksum-download"
        self.signature_download = self.directory / "signature-download"
        for directory in (
            self.scripts,
            self.scratch,
            self.keys,
            self.checksum_download,
            self.signature_download,
        ):
            directory.mkdir(mode=0o700)
        self.script = self.scripts / "controlled_child.py"
        self.script.write_text(_CHILD)
        self.script.chmod(0o600)
        self.checksums_path = self.checksum_download / "SHA256SUMS"
        self.signature_path = self.signature_download / "SHA256SUMS.gpg"
        self.checksums_path.write_bytes(DOCUMENT)
        self.signature_path.write_bytes(SIGNATURE)
        self.checksums_path.chmod(0o600)
        self.signature_path.chmod(0o600)
        self.keyring_path = self.keys / "approved.gpg"
        self.keyring_path.write_bytes(KEYRING_BYTES)
        self.keyring_path.chmod(0o600)
        self.approved = ApprovedKeyring(
            path=self.keyring_path,
            sha256=hashlib.sha256(KEYRING_BYTES).hexdigest(),
            size_bytes=len(KEYRING_BYTES),
            primary_fingerprints=(CHECKSUM_KEY, SIMPLESTREAMS_KEY, ARCHIVE_KEY),
        )
        self.sentinel = self.scratch / "unrelated"
        self.sentinel.write_bytes(b"caller-owned file, not verifier cleanup authority")
        self.sentinel.chmod(0o600)

    def verifier(self, **changes: Unpack[_VerifierConfiguration]) -> NativeSignatureVerifier:
        configuration: _VerifierConfiguration = {
            "expected_primary": CHECKSUM_KEY,
            "keyring": self.approved,
            "scratch_directory": self.scratch,
        }
        configuration.update(changes)
        return NativeSignatureVerifier(**configuration)


class NativeFixture(VerificationFixture):
    def setUp(self):
        super().setUp()
        # Reap only fixture orphans. Linux PID 1 does not always reap them promptly.
        if sys.platform == "linux":
            import ctypes

            libc = ctypes.CDLL(None, use_errno=True)
            previous = ctypes.c_int()
            self.assertEqual(libc.prctl(37, ctypes.byref(previous), 0, 0, 0), 0)
            self.assertEqual(libc.prctl(36, 1, 0, 0, 0), 0)
            self.addCleanup(libc.prctl, 36, previous.value, 0, 0, 0)
        self.addCleanup(self.retire_children)

    def signal_flags(self, signum: int) -> int:
        import ctypes

        # Independent Linux glibc layout, not the verifier's opaque action buffer.
        class Action(ctypes.Structure):
            _fields_ = [
                ("handler", ctypes.c_void_p),
                ("mask", ctypes.c_ulong * 16),
                ("flags", ctypes.c_int),
                ("restorer", ctypes.c_void_p),
            ]

        library = ctypes.CDLL(None, use_errno=True)
        operation = library.sigaction
        operation.argtypes = (ctypes.c_int, ctypes.c_void_p, ctypes.c_void_p)
        operation.restype = ctypes.c_int
        action = Action()
        self.assertEqual(operation(signum, None, ctypes.byref(action)), 0)
        return int(action.flags)

    def invoke(
        self,
        verifier,
        *,
        document=DOCUMENT,
        signature=SIGNATURE,
        status=None,
        stderr=b"",
        exit_code=0,
        mode="normal",
        descendant=False,
        before_spawn=None,
        unreapable=False,
        constructor_created=None,
    ):
        ordinal = len(self.calls)
        control = self.scripts / f"control-{ordinal}.json"
        record = self.scripts / f"children-{ordinal}.json"
        ready = self.scripts / f"ready-{ordinal}"
        retire = self.scripts / f"retire-{ordinal}"
        control.write_text(
            json.dumps(
                {
                    "stdout": base64.b64encode(good_status() if status is None else status).decode(
                        "ascii"
                    ),
                    "stderr": base64.b64encode(stderr).decode("ascii"),
                    "exit_code": exit_code,
                    "mode": mode,
                    "descendant": descendant,
                    "record": str(record),
                    "ready": str(ready),
                    "retire": str(retire),
                }
            )
        )
        control.chmod(0o600)

        boundary_errors = []

        def spawn(argv, **options):
            self.calls.append((tuple(argv), dict(options)))
            inherited_resources = ()
            try:
                self.assertEqual(options["stdin"], subprocess.DEVNULL)
                self.assertEqual(options["stdout"], subprocess.PIPE)
                self.assertEqual(options["stderr"], subprocess.PIPE)
                self.assertTrue(options["start_new_session"])
                self.assertTrue(options["close_fds"])
                self.assertFalse(options.get("shell", False))
                self.assertFalse(options.get("text", False))
                self.assertIsNone(options.get("encoding"))
                self.assertNotIn("capture_output", options)
                if before_spawn is not None:
                    before_spawn(tuple(argv), options)
                detached, data = self.scratch_inputs(argv)
                keyring = self.option(argv, "--keyring")
                home = self.option(argv, "--homedir")
                paths = (str(detached), str(data), keyring, home)
                references = [_PROC_FD.fullmatch(path) for path in paths]
                if all(reference is not None for reference in references):
                    fds = [int(reference.group(1)) for reference in references if reference]
                    inherited_resources = tuple(fds)
                    home_stat = os.fstat(fds[3])
                    facts = json.loads(control.read_bytes())
                    facts["resources"] = {
                        "fds": fds,
                        "inputs": [
                            {"path": path, "bytes": base64.b64encode(value).decode("ascii")}
                            for path, value in zip(
                                paths[:3], (signature, document, KEYRING_BYTES), strict=True
                            )
                        ],
                        "home": home,
                        "home_identity": [home_stat.st_dev, home_stat.st_ino],
                    }
                    control.write_text(json.dumps(facts))
            except AssertionError as error:
                # Re-raise fixture assertions outside the native error sanitizer.
                boundary_errors.append(error)
                raise
            # Only tests replace the trusted executable. Production callers have
            # no executable/argv parameter. Preserve its cwd/env/fd/group policy.
            command = [str(Path(sys.executable).resolve()), "-I", str(self.script), str(control)]

            def retain_child(child):
                controlled = ControlledChild(
                    child, retire=retire, unreapable=unreapable, resources=inherited_resources
                )
                self.children.append((controlled, record))
                return controlled

            if constructor_created is not None:

                class ConstructorWindowPopen(_REAL_POPEN):
                    def __init__(self, command, **child_options):
                        super().__init__(command, **child_options)
                        # Retain the real process before cancellation can escape
                        # the constructor. This is independent fixture cleanup,
                        # not a transfer of ownership to the production verifier.
                        self.fixture_child = retain_child(self)
                        try:
                            constructor_created(self.fixture_child)
                        except AssertionError as error:
                            boundary_errors.append(error)
                            raise

                return ConstructorWindowPopen(command, **options).fixture_child

            child = _REAL_POPEN(command, **options)
            return retain_child(child)

        killpg = os.killpg

        def signal_group(pgid, signum):
            controlled = self.children[-1][0]
            try:
                self.assertEqual(pgid, controlled.pid, "only the original fixture group is owned")
                self.assertTrue(
                    controlled.group_signals_open, "a consuming reap closed the group lease"
                )
                self.assertIsNone(
                    controlled.process.returncode, "a reaped group number is not owned"
                )
            except AssertionError as error:
                boundary_errors.append(error)
                raise
            try:
                result = killpg(pgid, signum)
            except ProcessLookupError:
                controlled.group_signals_open = False
                raise
            if signum == signal.SIGKILL:
                controlled.group_kill_sent = True
            return result

        try:
            with (
                patch.object(native, "_popen", side_effect=spawn),
                patch.object(native.os, "killpg", side_effect=signal_group),
                patch("subprocess.Popen", side_effect=AssertionError("uncontrolled native child")),
                patch("subprocess.run", side_effect=AssertionError("uncontrolled native command")),
                patch.object(native, "_VERIFY_TIMEOUT_SECONDS", 1.0),
                patch.object(native, "_TERMINATE_GRACE_SECONDS", 0.05),
                patch.object(native, "_POST_KILL_WAIT_SECONDS", 0.2),
            ):
                result = verifier.verify(document, signature)
        finally:
            # A preserved native interruption must not hide a fixture assertion
            # that the implementation signaled after consuming reap had started.
            if boundary_errors:
                raise boundary_errors[0]
        return result

    def retire_children(self):
        complete = True
        interruption = None

        def attempt(operation):
            nonlocal complete, interruption
            try:
                operation()
            except (KeyboardInterrupt, SystemExit) as error:
                complete = False
                if interruption is None:
                    interruption = error
            except Exception:
                complete = False

        for controlled, record in reversed(self.children):
            process = controlled.process
            attempt(lambda: controlled.retire.write_bytes(b"retire fixture children"))
            # The fixture has its own lease. Neither returncode=None nor a failed
            # simulated reap permits signaling after a consuming wait started.
            # A successful native SIGKILL also needs no duplicate fallback signal.
            if (
                process.returncode is None
                and controlled.group_signals_open
                and not controlled.group_kill_sent
            ):

                def retire_group():
                    if os.name == "posix":
                        try:
                            os.killpg(process.pid, signal.SIGKILL)
                        except ProcessLookupError:
                            return
                        except PermissionError:
                            # Not proof of retirement. The fixture protocol and
                            # bounded reaps below must retire all known children.
                            return
                        controlled.group_kill_sent = True
                    else:
                        process.kill()

                attempt(retire_group)
            controlled.group_signals_open = False
            attempt(lambda: process.wait(timeout=3))

            def retire_descendant():
                if record.exists():
                    descendant = json.loads(record.read_bytes())["descendant"]
                    if descendant is not None:
                        self.assertTrue(self.wait_for_retirement(descendant))

            attempt(retire_descendant)
            for pipe in (process.stdout, process.stderr):
                if pipe is not None:
                    attempt(pipe.close)
        if interruption is not None:
            raise interruption
        self.assertTrue(complete, "fixture-owned child retirement was incomplete")

    def wait_for_retirement(self, pid):
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            try:
                retired, _status = os.waitpid(pid, os.WNOHANG)
                if retired == pid:
                    return True  # Do not probe a PID just freed by our own reap.
            except ChildProcessError:
                pass
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return True
            time.sleep(0.01)
        return False

    def assert_group_retired(self):
        controlled, record = self.children[-1]
        self.assertIsNotNone(controlled.process.returncode, "the direct child must be reaped")
        self.assertTrue(record.exists(), "the controlled child must have started")
        descendant = json.loads(record.read_bytes())["descendant"]
        if descendant is not None:
            self.assertTrue(self.wait_for_retirement(descendant), "the owned group must be retired")
        self.assertEqual(controlled.communications, 0)
        self.assertTrue(
            all(value is not None and math.isfinite(value) for value in controlled.waits)
        )
        for pipe in (controlled.process.stdout, controlled.process.stderr):
            self.assertIsNotNone(pipe)
            self.assertTrue(pipe.closed)

    def assert_scratch_clean(self):
        self.assertEqual(set(self.scratch.iterdir()), {self.sentinel})
        self.assertEqual(
            self.sentinel.read_bytes(), b"caller-owned file, not verifier cleanup authority"
        )

    def option(self, argv, name):
        for index, value in enumerate(argv):
            if value == name:
                return argv[index + 1]
            if value.startswith(name + "="):
                return value[len(name) + 1 :]
        self.fail(f"missing fixed native option {name}")

    def scratch_inputs(self, argv):
        tail = argv[argv.index("--verify") + 1 :]
        if tail and tail[0] == "--":
            tail = tail[1:]
        self.assertEqual(len(tail), 2, "verify accepts exactly a signature and data object")
        return Path(tail[0]), Path(tail[1])

    def resource_fds(self, argv, options=None):
        detached, data = self.scratch_inputs(argv)
        paths = (
            str(detached),
            str(data),
            self.option(argv, "--keyring"),
            self.option(argv, "--homedir"),
        )
        fds = []
        for path in paths:
            reference = _PROC_FD.fullmatch(path)
            self.assertIsNotNone(reference, "native resources must use retained procfd references")
            assert reference is not None
            fd = int(reference.group(1))
            self.assertGreaterEqual(fd, 3)
            fds.append(fd)
        self.assertEqual(len(set(fds)), 4, "three inputs and scratch must be distinct objects")
        if options is not None:
            inherited = options.get("pass_fds", ())
            self.assertEqual(len(inherited), 4)
            self.assertEqual(set(inherited), set(fds), "inherit only three inputs and scratch")
        for path, fd in zip(paths, fds, strict=True):
            held, referenced = os.fstat(fd), os.stat(path)
            self.assertEqual(
                (held.st_dev, held.st_ino, held.st_mode, held.st_uid, held.st_gid),
                (
                    referenced.st_dev,
                    referenced.st_ino,
                    referenced.st_mode,
                    referenced.st_uid,
                    referenced.st_gid,
                ),
            )
            self.assertTrue(fcntl.fcntl(fd, fcntl.F_GETFD) & fcntl.FD_CLOEXEC)
        home = os.fstat(fds[3])
        self.assertTrue(stat.S_ISDIR(home.st_mode))
        self.assertEqual(stat.S_IMODE(home.st_mode), 0o700)
        self.assertEqual(home.st_uid, os.geteuid())
        return tuple(fds)

    def assert_sealed_input(self, fd, expected):
        observation = os.fstat(fd)
        self.assertTrue(stat.S_ISREG(observation.st_mode))
        self.assertEqual(stat.S_IMODE(observation.st_mode), 0o600)
        self.assertEqual(observation.st_uid, os.geteuid())
        self.assertEqual(
            observation.st_nlink, 0, "immutable inputs have no writable named fallback"
        )
        self.assertEqual(observation.st_size, len(expected))
        self.assertEqual(fcntl.fcntl(fd, _F_GET_SEALS) & _REQUIRED_SEALS, _REQUIRED_SEALS)
        self.assertEqual(os.pread(fd, len(expected) + 1, 0), expected)

    def owned_scratch(self, argv):
        home = self.option(argv, "--homedir")
        reference = _PROC_FD.fullmatch(home)
        # Resolve only to locate the owned path for mutation scenarios. Production
        # and the independent resource assertions authenticate held procfd objects.
        self.assertIsNotNone(reference)
        directory = Path(os.readlink(home))
        self.assertTrue(directory.is_absolute())
        self.assertTrue(directory.is_relative_to(self.scratch))
        self.assertNotEqual(directory, self.scratch)
        return directory

    def assert_descriptors_closed(self, fds):
        for fd in fds:
            with self.assertRaises(OSError) as raised:
                os.fstat(fd)
            self.assertEqual(raised.exception.errno, errno.EBADF)

    def inotify_watches(self):
        watches = {}
        for name in os.listdir("/proc/self/fd"):
            fd = int(name)
            try:
                target = os.readlink(f"/proc/self/fd/{fd}")
            except OSError:
                continue
            if target != "anon_inode:inotify":
                continue
            records = {}
            for line in Path(f"/proc/self/fdinfo/{fd}").read_text().splitlines():
                if not line.startswith("inotify wd:"):
                    continue
                fields = dict(value.split(":", 1) for value in line.split()[1:] if ":" in value)
                records[int(fields["wd"], 16)] = int(fields["ino"], 16)
            watches[fd] = records
        return watches


class ControlledOperation(native._VerificationOperation):
    """One authentication operation, with no executable or path factory injection."""

    def __init__(self, fixture, *, status, returncode, failure=None, interruption=None):
        self.fixture: NativeFixture = fixture
        self.status: bytes = status
        self.returncode: int | None = returncode
        self.failure: NativeClassification | None = failure
        self.interruption: KeyboardInterrupt | SystemExit | None = interruption
        self.request: tuple[ApprovedKeyring, Path, bytes, bytes] | None = None
        self.cleaned = False

    def prepare(self, approved, scratch, checksums, signature):
        # Model the independently approved authority bytes at the native boundary.
        # Linux tests separately exercise the actual no-follow read and digest.
        if approved.sha256 != hashlib.sha256(
            KEYRING_BYTES
        ).hexdigest() or approved.size_bytes != len(KEYRING_BYTES):
            raise native._Denied(NativeClassification.INVALID_AUTHORITY)
        self.request = (approved, scratch, checksums, signature)

    def run(self):
        if self.request is None:
            raise AssertionError("operation ran without exact input preparation")
        self.fixture.calls.append(self.request)
        if self.interruption is not None:
            raise self.interruption
        if self.failure is not None:
            raise native._Denied(self.failure)
        return native._OperationResult(self.status, self.returncode)

    def check(self):
        if self.request is None:
            raise AssertionError("operation ran without exact input preparation")

    def cleanup(self, cleanup):
        self.cleaned = True


class PortableFixture(NativeFixture):
    def setUp(self):
        # No subreaper, child or process-group operation belongs to portable policy.
        VerificationFixture.setUp(self)
        self.operations = []

    def invoke(
        self,
        verifier,
        *,
        document=DOCUMENT,
        signature=SIGNATURE,
        status=None,
        stderr=b"",
        exit_code=0,
        mode="normal",
        descendant=False,
        before_spawn=None,
        unreapable=False,
        constructor_created=None,
        failure=None,
        interruption=None,
    ):
        self.assertEqual(mode, "normal", "portable policy cannot simulate process retirement")
        self.assertFalse(descendant)
        self.assertIsNone(before_spawn)
        self.assertFalse(unreapable)
        self.assertIsNone(constructor_created)
        operation = ControlledOperation(
            self,
            status=good_status() if status is None else status,
            returncode=exit_code,
            failure=failure,
            interruption=interruption,
        )

        def select_operation():
            self.operations.append(operation)
            return operation

        with (
            patch.object(native, "_new_verification_operation", side_effect=select_operation),
            patch.object(
                native.os,
                "open",
                side_effect=AssertionError("portable policy opened native authority"),
            ),
            patch.object(
                native.os,
                "mkdir",
                side_effect=AssertionError("portable policy created native scratch"),
            ),
            patch.object(
                native, "_popen", side_effect=AssertionError("portable policy spawned a child")
            ),
            patch.object(
                native.os, "killpg", side_effect=AssertionError("portable policy signaled a group")
            ),
            patch("subprocess.Popen", side_effect=AssertionError("uncontrolled native child")),
            patch("subprocess.run", side_effect=AssertionError("uncontrolled native command")),
        ):
            # stderr is deliberately absent from the status-only operation result.
            result = verifier.verify(document, signature)
        if operation in self.operations:
            self.assertTrue(operation.cleaned, "the selected operation must finish cleanup")
        return result


# The holder is not a TestCase at runtime, so unittest does not discover a
# duplicate suite. Every method below binds to a NativeFixture subclass.
if TYPE_CHECKING:
    _ScenarioBase = NativeFixture
else:
    _ScenarioBase = object


class _SignatureAuthenticationScenarios(_ScenarioBase):
    def test_exact_snapshot_bytes_are_authenticated_in_owned_private_scratch(self):
        document = self.checksums_path.read_bytes() + b"\x00\xff\r\n"
        signature = self.signature_path.read_bytes()
        # The caller has read bounded bytes. Neither source pathname is authority.
        self.checksum_download.rename(self.directory / "old-checksum-download")
        self.signature_download.rename(self.directory / "old-signature-download")
        self.checksum_download.mkdir(mode=0o700)
        self.signature_download.mkdir(mode=0o700)
        self.checksums_path.write_bytes(b"replacement download")
        self.signature_path.symlink_to(self.keyring_path)
        observed = []

        def inspect(argv, options):
            detached, data, keyring, home = self.resource_fds(argv, options)
            for fd, expected in ((detached, signature), (data, document), (keyring, KEYRING_BYTES)):
                self.assert_sealed_input(fd, expected)
            copied, authority = os.fstat(keyring), self.keyring_path.stat()
            self.assertNotEqual(
                (copied.st_dev, copied.st_ino), (authority.st_dev, authority.st_ino)
            )
            self.assertEqual(list(self.owned_scratch(argv).iterdir()), [])
            observed.extend((detached, data, keyring, home))

        verifier = self.verifier()
        result = self.invoke(verifier, document=document, signature=signature, before_spawn=inspect)
        self.assertEqual(result, CHECKSUM_KEY)
        self.assertEqual(verifier.last_classification, NativeClassification.VERIFIED)
        self.assertEqual(len(self.calls), 1)
        self.assertTrue(observed)
        self.assert_descriptors_closed(observed)
        self.assert_scratch_clean()

    def test_full_primary_pin_is_independent_of_the_signed_document(self):
        for pin in (None, "", CHECKSUM_KEY[-16:], CHECKSUM_KEY[-8:], "G" * 40):
            with self.subTest(pin=pin):
                verifier = self.verifier(expected_primary=pin)
                result = self.invoke(verifier, document=DOCUMENT + CHECKSUM_KEY.encode("ascii"))
                self.assertEqual(
                    verifier.last_classification, NativeClassification.INVALID_AUTHORITY
                )
                self.assertIsNone(result)
                self.assertEqual(self.calls, [])
                self.assert_scratch_clean()

    def test_approved_keyring_metadata_is_required_and_checked_before_spawning(self):
        for keyring in (
            None,
            replace(self.approved, sha256="0" * 64),
            replace(self.approved, sha256="not-a-digest"),
            replace(self.approved, size_bytes=len(KEYRING_BYTES) + 1),
            replace(self.approved, size_bytes=True),
            replace(self.approved, size_bytes=0),
            replace(self.approved, size_bytes=32 * 1024 * 1024),
            replace(self.approved, primary_fingerprints=(SIMPLESTREAMS_KEY,)),
            replace(self.approved, primary_fingerprints=(CHECKSUM_KEY[-16:],)),
            replace(self.approved, primary_fingerprints=(CHECKSUM_KEY, CHECKSUM_KEY.lower())),
            replace(self.approved, primary_fingerprints=()),
        ):
            with self.subTest(keyring=keyring):
                verifier = self.verifier(keyring=keyring)
                result = self.invoke(verifier)
                self.assertEqual(
                    verifier.last_classification, NativeClassification.INVALID_AUTHORITY
                )
                self.assertIsNone(result)
                self.assertEqual(self.calls, [])

    def test_symlinked_linked_or_writable_keyrings_are_not_native_authority(self):
        unsafe = self.keys / "unsafe.gpg"
        for kind in ("symlink", "hardlink", "writable"):
            with self.subTest(kind=kind):
                if unsafe.exists() or unsafe.is_symlink():
                    unsafe.unlink()
                if kind == "symlink":
                    unsafe.symlink_to(self.keyring_path)
                elif kind == "hardlink":
                    os.link(self.keyring_path, unsafe)
                else:
                    unsafe.write_bytes(KEYRING_BYTES)
                    unsafe.chmod(0o666)
                verifier = self.verifier(keyring=replace(self.approved, path=unsafe))
                result = self.invoke(verifier)
                self.assertEqual(verifier.last_classification, NativeClassification.UNSAFE_PATH)
                self.assertIsNone(result)
                self.assertEqual(self.calls, [])

    def test_wrong_full_primary_and_other_approved_artifact_keys_are_rejected(self):
        for expected, actual in (
            (CHECKSUM_KEY, SIMPLESTREAMS_KEY),
            (SIMPLESTREAMS_KEY, CHECKSUM_KEY),
            (CHECKSUM_KEY, ARCHIVE_KEY),
            (CHECKSUM_KEY, "C" * 40),
            (CHECKSUM_KEY, "C" * 24 + CHECKSUM_KEY[-16:]),
        ):
            with self.subTest(expected=expected, actual=actual):
                verifier = self.verifier(expected_primary=expected)
                result = self.invoke(verifier, status=good_status(primary=actual))
                self.assertEqual(
                    verifier.last_classification, NativeClassification.SIGNATURE_REJECTED
                )
                self.assertIsNone(result)
        self.assertEqual(len(self.calls), 5)

    def test_signing_subkey_returns_only_its_bound_expected_primary(self):
        verifier = self.verifier()
        result = self.invoke(verifier, status=good_status(signing=SIGNING_SUBKEY))
        self.assertEqual(result, CHECKSUM_KEY)
        self.assertEqual(verifier.last_classification, NativeClassification.VERIFIED)
        self.assertEqual(len(self.calls), 1)
        self.assert_scratch_clean()

    def test_signing_subkey_without_the_expected_primary_binding_is_rejected(self):
        for status in (
            good_status(signing=SIGNING_SUBKEY, include_primary=False),
            good_status(signing=SIGNING_SUBKEY, primary=SIMPLESTREAMS_KEY),
        ):
            with self.subTest(status=status):
                verifier = self.verifier()
                result = self.invoke(verifier, status=status)
                self.assertEqual(
                    verifier.last_classification, NativeClassification.SIGNATURE_REJECTED
                )
                self.assertIsNone(result)

    def test_bad_invalid_revoked_and_expired_native_signature_facts_are_rejected(self):
        failures = (
            f"BADSIG {CHECKSUM_KEY[-16:]} fixture signer",
            f"ERRSIG {CHECKSUM_KEY[-16:]} 1 8 00 1577836800 9 {CHECKSUM_KEY}",
            f"EXPSIG {CHECKSUM_KEY[-16:]} fixture signer",
            f"EXPKEYSIG {CHECKSUM_KEY[-16:]} fixture signer",
            f"REVKEYSIG {CHECKSUM_KEY[-16:]} fixture signer",
            "KEYEXPIRED 1577923200",
            "SIGEXPIRED 1577923200",
            "KEYREVOKED",
            f"NO_PUBKEY {CHECKSUM_KEY[-16:]}",
            "FAILURE verify 9",
            "ERROR verify 9",
            "NODATA 1",
        )
        for failure in failures:
            with self.subTest(failure=failure):
                verifier = self.verifier()
                status = good_status() + f"[GNUPG:] {failure}\n".encode("ascii")
                result = self.invoke(verifier, status=status)
                self.assertEqual(
                    verifier.last_classification, NativeClassification.SIGNATURE_REJECTED
                )
                self.assertIsNone(result)
        verifier = self.verifier()
        result = self.invoke(verifier, status=good_status(expires=1577923200))
        self.assertEqual(verifier.last_classification, NativeClassification.SIGNATURE_REJECTED)
        self.assertIsNone(result)

    def test_binary_modern_signature_algorithms_are_required(self):
        for replacement in (
            b"4 0 1 1 00",  # MD5
            b"4 0 1 2 00",  # SHA1
            b"4 0 1 3 00",  # RIPEMD160
            b"4 0 1 11 00",  # SHA224
            b"4 0 17 8 00",  # DSA is outside this adapter's approved policy.
            b"4 0 18 8 00",  # Encryption-only ECDH
            b"3 0 1 8 00",  # Legacy signature version
            b"4 0 1 8 01",  # Canonical text does not authenticate byte identity.
        ):
            with self.subTest(replacement=replacement):
                verifier = self.verifier()
                result = self.invoke(
                    verifier, status=good_status().replace(b"4 0 1 8 00", replacement)
                )
                self.assertIsNone(result)
                self.assertEqual(
                    verifier.last_classification, NativeClassification.SIGNATURE_REJECTED
                )
                self.assert_scratch_clean()

    def test_unexpired_signature_status_with_a_supported_hash_can_authenticate(self):
        verifier = self.verifier()
        status = good_status(expires=4102444800).replace(b"4 0 1 8 00", b"4 0 1 10 00")
        self.assertEqual(self.invoke(verifier, status=status), CHECKSUM_KEY)
        self.assertEqual(verifier.last_classification, NativeClassification.VERIFIED)
        self.assert_scratch_clean()

    def test_native_signature_records_must_agree_on_the_signing_identity(self):
        verifier = self.verifier()
        status = good_status().replace(
            f"GOODSIG {CHECKSUM_KEY[-16:]}".encode("ascii"),
            f"GOODSIG {SIMPLESTREAMS_KEY[-16:]}".encode("ascii"),
        )
        self.assertIsNone(self.invoke(verifier, status=status))
        self.assertEqual(verifier.last_classification, NativeClassification.SIGNATURE_REJECTED)
        self.assert_scratch_clean()

    def test_repeated_key_consideration_and_signature_id_are_not_extra_signatures(self):
        verifier = self.verifier()
        # This is a maintained tool's record shape, still synthetic facts rather
        # than evidence that a real key or its binding was cryptographically valid.
        status = good_status(signing=SIGNING_SUBKEY).replace(
            b"[GNUPG:] GOODSIG ",
            (
                f"[GNUPG:] KEY_CONSIDERED {CHECKSUM_KEY} 0\n"
                "[GNUPG:] SIG_ID Zml4dHVyZQ== 2020-01-01 1577836800\n"
                "[GNUPG:] GOODSIG "
            ).encode("ascii"),
        )
        self.assertEqual(self.invoke(verifier, status=status), CHECKSUM_KEY)
        self.assertEqual(verifier.last_classification, NativeClassification.VERIFIED)
        self.assert_scratch_clean()

    def test_missing_or_duplicate_signature_facts_are_not_verification(self):
        complete = good_status()
        for status in (
            b"",
            b"[GNUPG:] NEWSIG\n",
            b"\n".join(line for line in complete.split(b"\n") if b"VALIDSIG" not in line),
            b"\n".join(line for line in complete.split(b"\n") if b"GOODSIG" not in line),
            b"\n".join(line for line in complete.split(b"\n") if b"NEWSIG" not in line),
            complete + complete,
            complete + next(line for line in complete.splitlines() if b"VALIDSIG" in line) + b"\n",
            complete + next(line for line in complete.splitlines() if b"GOODSIG" in line) + b"\n",
        ):
            with self.subTest(status=status):
                verifier = self.verifier()
                result = self.invoke(verifier, status=status)
                self.assertEqual(verifier.last_classification, NativeClassification.INVALID_STATUS)
                self.assertIsNone(result)

    def test_unknown_malformed_and_short_fingerprint_status_shapes_fail_closed(self):
        complete = good_status()
        for status in (
            complete + b"[GNUPG:] UNKNOWN_NATIVE_FACT 1\n",
            complete + b"[GNUPG:] VALIDSIG\n",
            complete.replace(CHECKSUM_KEY.encode("ascii"), CHECKSUM_KEY[-16:].encode("ascii")),
            complete.replace(b"1577836800 0 4", b"not-a-time 0 4"),
            complete.replace(b"1577836800 0 4", b"-1 0 4"),
            complete.replace(b"4 0 1 8 00", b"4 0 999 8 00"),
            complete.replace(b"4 0 1 8 00", b"4 0 1 999 00"),
            complete.replace(b"4 0 1 8 00", b"4 1 1 8 00"),
            complete.replace(b"2020-01-01", b"not-a-date"),
            complete.replace(b"2020-01-01", b"2020-02-30"),
            complete.replace(b"2020-01-01", b"2020-01-02"),
            complete[:-1],
            complete + b"\n",
            complete.replace(b"TRUST_UNDEFINED", b"\x00TRUST_UNDEFINED"),
            complete + b"unframed stdout\n",
            complete.replace(b"VALIDSIG ", b"VALIDSIG \xff"),
        ):
            with self.subTest(status=status):
                verifier = self.verifier()
                result = self.invoke(verifier, status=status)
                self.assertEqual(verifier.last_classification, NativeClassification.INVALID_STATUS)
                self.assertIsNone(result)

    def test_english_stderr_and_github_verified_flags_are_not_signature_authority(self):
        for status in (
            b"",
            b"gpg: Good signature from fixture signer\n",
            b'{"verified": true, "key_id": "7DB87C81"}\n',
        ):
            with self.subTest(status=status):
                verifier = self.verifier()
                result = self.invoke(verifier, status=status, stderr=b"gpg: Good signature\n")
                self.assertEqual(verifier.last_classification, NativeClassification.INVALID_STATUS)
                self.assertIsNone(result)
        verifier = self.verifier()
        output, error = io.StringIO(), io.StringIO()
        with redirect_stdout(output), redirect_stderr(error):
            result = self.invoke(verifier, stderr=f"gpg: BAD signature {LEAK}\n".encode("ascii"))
        self.assertEqual(
            result, CHECKSUM_KEY, "structured status, not English stderr, is authority"
        )
        self.assertEqual((output.getvalue(), error.getvalue()), ("", ""))

    def test_nonzero_native_exit_cannot_be_overridden_by_validsig(self):
        verifier = self.verifier()
        output, error = io.StringIO(), io.StringIO()
        with redirect_stdout(output), redirect_stderr(error):
            result = self.invoke(verifier, exit_code=2, stderr=LEAK.encode("ascii"))
        self.assertEqual(verifier.last_classification, NativeClassification.PROCESS_FAILED)
        self.assertIsNone(result)
        self.assertEqual((output.getvalue(), error.getvalue()), ("", ""))
        self.assert_scratch_clean()

    def test_invalid_byte_inputs_are_rejected_before_spawning(self):
        for document, signature in (
            (b"", SIGNATURE),
            (DOCUMENT, b""),
            (bytearray(DOCUMENT), SIGNATURE),
            (DOCUMENT, memoryview(SIGNATURE)),
            (self.checksums_path, SIGNATURE),
            (DOCUMENT, "not bytes"),
            (None, SIGNATURE),
        ):
            with self.subTest(document_type=type(document), signature_type=type(signature)):
                verifier = self.verifier()
                result = self.invoke(
                    verifier, document=cast(bytes, document), signature=cast(bytes, signature)
                )
                self.assertEqual(verifier.last_classification, NativeClassification.INVALID_INPUT)
                self.assertIsNone(result)
                self.assertEqual(self.calls, [])

    def test_input_byte_caps_are_inclusive_and_reject_overflow_before_spawning(self):
        verifier = self.verifier()
        result = self.invoke(verifier, document=b"d" * 65536, signature=b"s" * 65536)
        self.assertEqual(result, CHECKSUM_KEY)
        self.assertEqual(len(self.calls), 1)
        for document, signature in (
            (b"d" * 65537, SIGNATURE),
            (DOCUMENT, b"s" * 65537),
        ):
            with self.subTest(document_size=len(document), signature_size=len(signature)):
                verifier = self.verifier()
                result = self.invoke(verifier, document=document, signature=signature)
                self.assertEqual(verifier.last_classification, NativeClassification.INPUT_LIMIT)
                self.assertIsNone(result)
                self.assertEqual(len(self.calls), 1)
                self.assert_scratch_clean()

    def test_trusted_executable_cwd_and_environment_ignore_inherited_authority(self):
        poisoned = {
            "PATH": str(self.checksum_download),
            "HOME": str(self.signature_download),
            "GNUPGHOME": str(self.checksum_download),
            "GPG_AGENT_INFO": LEAK,
            "GPG_TTY": LEAK,
            "SSH_AUTH_SOCK": LEAK,
            "PYTHONPATH": str(self.signature_download),
            "PYTHONHOME": str(self.signature_download),
            "LD_PRELOAD": LEAK,
            "DYLD_INSERT_LIBRARIES": LEAK,
            "BASH_ENV": LEAK,
            "ENV": LEAK,
            "SIGNING_SECRET": LEAK,
            "APPLICATION_SECRET": LEAK,
        }
        planted = self.checksum_download / "json.py"
        marker = self.directory / "download-code-executed"
        planted.write_text(f"from pathlib import Path\nPath({str(marker)!r}).touch()\n")

        def inspect(argv, options):
            self.assertEqual(argv[0], "/usr/bin/gpg")
            self.assertEqual(Path(options["cwd"]), ROOT)
            for option in (
                "--no-options",
                "--batch",
                "--no-tty",
                "--no-default-keyring",
                "--no-auto-key-retrieve",
                "--no-auto-key-import",
                "--no-autostart",
                "--require-cross-certification",
                "--no-auto-check-trustdb",
                "--no-random-seed-file",
                "--lock-never",
            ):
                self.assertIn(option, argv)
            self.assertEqual(self.option(argv, "--auto-key-locate"), "clear")
            self.assertEqual(self.option(argv, "--trust-model"), "always")
            self.assertEqual(self.option(argv, "--status-fd"), "1")
            for option in (
                "--import",
                "--recv-keys",
                "--auto-key-retrieve",
                "--ignore-time-conflict",
            ):
                self.assertNotIn(option, argv)
            self.resource_fds(argv, options)
            homedir = Path(self.option(argv, "--homedir"))
            self.assertTrue(self.owned_scratch(argv).is_relative_to(self.scratch))
            self.assertEqual(stat.S_IMODE(homedir.stat().st_mode), 0o700)
            environment = options["env"]
            self.assertEqual(set(environment), {"PATH", "LANG", "LC_ALL", "HOME", "TMPDIR"})
            self.assertEqual(environment["PATH"], "/usr/bin:/bin")
            self.assertEqual(environment["LANG"], "C")
            self.assertEqual(environment["LC_ALL"], "C")
            self.assertEqual(Path(environment["HOME"]), homedir)
            self.assertEqual(Path(environment["TMPDIR"]), homedir)
            self.assertNotIn(LEAK, environment.values())
            self.assertEqual(options["stdin"], subprocess.DEVNULL)
            self.assertEqual(options["stdout"], subprocess.PIPE)
            self.assertEqual(options["stderr"], subprocess.PIPE)
            self.assertTrue(options["start_new_session"])
            self.assertTrue(options["close_fds"])
            self.assertFalse(options.get("shell", False))
            self.assertFalse(options.get("text", False))
            self.assertIsNone(options.get("encoding"))
            self.assertNotIn("capture_output", options)

        verifier = self.verifier()
        with patch.dict(os.environ, poisoned):
            result = self.invoke(verifier, before_spawn=inspect)
        self.assertEqual(result, CHECKSUM_KEY)
        self.assertEqual(len(self.calls), 1)
        self.assertFalse(marker.exists())
        self.assertEqual(self.children[0][0].communications, 0)
        self.assert_scratch_clean()

    def test_symlinked_or_writable_scratch_roots_are_rejected_without_effects(self):
        foreign = self.directory / "foreign"
        foreign.mkdir(mode=0o700)
        sentinel = foreign / "foreign-file"
        sentinel.write_bytes(b"foreign")
        link = self.directory / "scratch-link"
        link.symlink_to(foreign, target_is_directory=True)
        foreign_child = foreign / "child"
        foreign_child.mkdir(mode=0o700)
        writable = self.directory / "writable-scratch"
        writable.mkdir(mode=0o777)
        writable.chmod(0o777)
        for root in (link, link / "child", writable):
            with self.subTest(root=root):
                verifier = self.verifier(scratch_directory=root)
                result = self.invoke(verifier)
                self.assertEqual(verifier.last_classification, NativeClassification.UNSAFE_PATH)
                self.assertIsNone(result)
                self.assertEqual(self.calls, [])
                self.assertEqual(sentinel.read_bytes(), b"foreign")
                self.assertEqual(set(foreign.iterdir()), {sentinel, foreign_child})

    def test_replaced_scratch_file_is_not_authentication_or_cleanup_authority(self):
        replaced = []

        def substitute(argv, options):
            detached_fd, _data, _keyring, _home = self.resource_fds(argv, options)
            # A named shadow can no longer replace an anonymous input. It remains
            # foreign scratch, so successful status grants no cleanup authority.
            detached = self.owned_scratch(argv) / "signature"
            detached.write_bytes(b"foreign replacement")
            detached.chmod(0o600)
            self.assert_sealed_input(detached_fd, SIGNATURE)
            replaced.append(detached)

        verifier = self.verifier()
        result = self.invoke(verifier, before_spawn=substitute)
        self.assertEqual(verifier.last_classification, NativeClassification.CLEANUP_INCOMPLETE)
        self.assertIsNone(result)
        self.assertEqual(len(self.calls), 1)
        self.assertEqual(replaced[0].read_bytes(), b"foreign replacement")
        self.assertEqual(
            self.sentinel.read_bytes(), b"caller-owned file, not verifier cleanup authority"
        )

    def test_changed_private_input_bytes_modes_and_links_are_preserved_and_rejected(self):
        changed = []
        for kind in ("same-inode-content", "mode", "hardlink"):
            with self.subTest(kind=kind):
                verifier = self.verifier()

                def change(argv, options):
                    detached_fd, _data, _keyring, _home = self.resource_fds(argv, options)
                    detached = self.owned_scratch(argv) / "signature"
                    detached.write_bytes(SIGNATURE)
                    detached.chmod(0o600)
                    if kind == "same-inode-content":
                        detached.write_bytes(b"x" * len(SIGNATURE))
                    elif kind == "mode":
                        detached.chmod(0o644)
                    else:
                        os.link(detached, self.directory / "foreign-signature-link")
                    self.assert_sealed_input(detached_fd, SIGNATURE)
                    changed.append(
                        (detached, detached.read_bytes(), stat.S_IMODE(detached.lstat().st_mode))
                    )

                self.assertIsNone(self.invoke(verifier, before_spawn=change))
                self.assertEqual(
                    verifier.last_classification, NativeClassification.CLEANUP_INCOMPLETE
                )
        self.assertEqual(len(changed), 3, "all three foreign shadow-input mutations must execute")
        for path, data, mode in changed:
            self.assertEqual(path.read_bytes(), data)
            self.assertEqual(stat.S_IMODE(path.lstat().st_mode), mode)
        self.assertEqual((self.directory / "foreign-signature-link").read_bytes(), SIGNATURE)
        self.assertEqual(
            self.sentinel.read_bytes(), b"caller-owned file, not verifier cleanup authority"
        )

    def test_approved_keyring_replacement_during_native_verification_is_rejected(self):
        original = self.keys / "retained-original.gpg"

        def replace_authority(argv, _options):
            private_keyring = Path(self.option(argv, "--keyring"))
            self.assertEqual(private_keyring.read_bytes(), KEYRING_BYTES)
            self.keyring_path.rename(original)
            self.keyring_path.write_bytes(b"foreign replacement authority")
            self.keyring_path.chmod(0o600)

        verifier = self.verifier()
        self.assertIsNone(self.invoke(verifier, before_spawn=replace_authority))
        self.assertEqual(verifier.last_classification, NativeClassification.UNSAFE_PATH)
        self.assertEqual(original.read_bytes(), KEYRING_BYTES)
        self.assertEqual(self.keyring_path.read_bytes(), b"foreign replacement authority")
        self.assert_scratch_clean()

    def test_replaced_scratch_root_preserves_its_foreign_replacement(self):
        moved = self.directory / "retained-original-root"
        foreign = self.scratch / "keep"

        def replace_root(_argv, _options):
            self.scratch.rename(moved)
            self.scratch.mkdir(mode=0o700)
            foreign.write_bytes(b"foreign root contents")
            foreign.chmod(0o600)

        verifier = self.verifier()
        self.assertIsNone(self.invoke(verifier, before_spawn=replace_root))
        self.assertEqual(verifier.last_classification, NativeClassification.CLEANUP_INCOMPLETE)
        self.assertEqual(set(self.scratch.iterdir()), {foreign})
        self.assertEqual(foreign.read_bytes(), b"foreign root contents")
        self.assertEqual(
            (moved / "unrelated").read_bytes(), b"caller-owned file, not verifier cleanup authority"
        )

    def test_replaced_scratch_directory_never_traverses_or_removes_foreign_objects(self):
        foreign = self.directory / "foreign-scratch"
        foreign.mkdir(mode=0o700)
        sentinel = foreign / "keep"
        sentinel.write_bytes(b"foreign contents")

        def substitute(argv, _options):
            owned = self.owned_scratch(argv)
            owned.rename(self.directory / "moved-owned-scratch")
            owned.symlink_to(foreign, target_is_directory=True)

        verifier = self.verifier()
        result = self.invoke(verifier, before_spawn=substitute)
        self.assertEqual(verifier.last_classification, NativeClassification.CLEANUP_INCOMPLETE)
        self.assertIsNone(result)
        self.assertEqual(len(self.calls), 1)
        self.assertEqual(set(foreign.iterdir()), {sentinel})
        self.assertEqual(sentinel.read_bytes(), b"foreign contents")

    def test_renamed_and_restored_private_directory_is_not_clean_authentication(self):
        moves = []
        real_stamp = native._stamp

        def equal_timestamps(observation, *, contents=False):
            stamp = real_stamp(observation, contents=contents)
            # Model Linux's equal timestamp observations without changing object
            # identities, protection, lengths or bytes. Both renames remain real.
            return (*stamp[:-2], 0, 0) if len(stamp) > 5 else stamp

        def rename_and_restore(argv, _options):
            owned = self.owned_scratch(argv)
            moved = self.directory / "briefly-moved-scratch"
            owned.rename(moved)
            moves.append(moved)
            moved.rename(owned)
            moves.append(owned)

        verifier = self.verifier()
        with patch.object(native, "_stamp", side_effect=equal_timestamps):
            result = self.invoke(verifier, before_spawn=rename_and_restore)
        self.assertEqual(len(moves), 2, "the rename-and-restore must really execute")
        self.assertIsNone(result)
        self.assertEqual(verifier.last_classification, NativeClassification.UNSAFE_PATH)
        self.assert_scratch_clean()

    def test_unexpected_scratch_entries_do_not_grant_recursive_cleanup_authority(self):
        unexpected = []

        def add_foreign_entry(argv, _options):
            directory = self.owned_scratch(argv) / "unexpected"
            directory.mkdir(mode=0o700)
            entry = directory / "keep"
            entry.write_bytes(b"not an owned scratch object")
            unexpected.append(entry)

        verifier = self.verifier()
        result = self.invoke(verifier, before_spawn=add_foreign_entry)
        self.assertEqual(verifier.last_classification, NativeClassification.CLEANUP_INCOMPLETE)
        self.assertIsNone(result)
        self.assertEqual(len(self.calls), 1)
        self.assertEqual(unexpected[0].read_bytes(), b"not an owned scratch object")

    def test_native_ordinary_exceptions_are_typed_content_free_and_cleanup_owned_scratch(self):
        for exception in (
            OSError(5, LEAK, LEAK),
            PermissionError(13, LEAK, LEAK),
            ValueError(LEAK),
            RuntimeError(LEAK),
        ):
            with self.subTest(exception_type=type(exception)):
                verifier = self.verifier()
                output, error = io.StringIO(), io.StringIO()
                with (
                    patch.object(native, "_popen", side_effect=exception) as spawn,
                    patch(
                        "subprocess.Popen", side_effect=AssertionError("uncontrolled native child")
                    ),
                    patch(
                        "subprocess.run", side_effect=AssertionError("uncontrolled native command")
                    ),
                    redirect_stdout(output),
                    redirect_stderr(error),
                ):
                    result = verifier.verify(DOCUMENT, SIGNATURE)
                self.assertEqual(verifier.last_classification, NativeClassification.NATIVE_FAILED)
                self.assertIsNone(result)
                self.assertEqual(spawn.call_count, 1)
                self.assertEqual((output.getvalue(), error.getvalue()), ("", ""))
                self.assertNotIn(LEAK, repr(verifier))
                self.assertNotIn(str(self.directory), repr(self.approved))
                self.assert_scratch_clean()

    def test_keyboard_interrupt_and_system_exit_survive_owned_cleanup(self):
        for interrupt in (KeyboardInterrupt(), SystemExit(73)):
            with self.subTest(interrupt_type=type(interrupt)):
                verifier = self.verifier()
                with (
                    patch.object(native, "_popen", side_effect=interrupt) as spawn,
                    patch(
                        "subprocess.Popen", side_effect=AssertionError("uncontrolled native child")
                    ),
                    patch(
                        "subprocess.run", side_effect=AssertionError("uncontrolled native command")
                    ),
                    self.assertRaises(type(interrupt)) as raised,
                ):
                    verifier.verify(DOCUMENT, SIGNATURE)
                self.assertIs(raised.exception, interrupt)
                self.assertEqual(spawn.call_count, 1)
                self.assert_scratch_clean()

    def test_a_later_rejection_does_not_retain_a_previous_verified_result(self):
        verifier = self.verifier()
        self.assertEqual(self.invoke(verifier), CHECKSUM_KEY)
        result = self.invoke(verifier, status=b"[GNUPG:] UNKNOWN_NATIVE_FACT\n")
        self.assertEqual(verifier.last_classification, NativeClassification.INVALID_STATUS)
        self.assertIsNone(result)
        self.assert_scratch_clean()

    def test_missing_native_configuration_remains_unavailable_without_effects(self):
        verifier = NativeSignatureVerifier()
        result = self.invoke(verifier)
        self.assertIsNone(result)
        self.assertEqual(verifier.last_classification, NativeClassification.UNAVAILABLE)
        self.assertEqual(self.calls, [])
        self.assert_scratch_clean()


class SignaturePolicyTests(PortableFixture):
    # These original scenarios exercise the unchanged public interface on every
    # policy-test host. Native storage and retirement are not simulated as success.
    test_full_primary_pin_is_independent_of_the_signed_document = _SignatureAuthenticationScenarios.test_full_primary_pin_is_independent_of_the_signed_document
    test_approved_keyring_metadata_is_required_and_checked_before_spawning = _SignatureAuthenticationScenarios.test_approved_keyring_metadata_is_required_and_checked_before_spawning
    test_wrong_full_primary_and_other_approved_artifact_keys_are_rejected = _SignatureAuthenticationScenarios.test_wrong_full_primary_and_other_approved_artifact_keys_are_rejected
    test_signing_subkey_returns_only_its_bound_expected_primary = _SignatureAuthenticationScenarios.test_signing_subkey_returns_only_its_bound_expected_primary
    test_signing_subkey_without_the_expected_primary_binding_is_rejected = _SignatureAuthenticationScenarios.test_signing_subkey_without_the_expected_primary_binding_is_rejected
    test_bad_invalid_revoked_and_expired_native_signature_facts_are_rejected = _SignatureAuthenticationScenarios.test_bad_invalid_revoked_and_expired_native_signature_facts_are_rejected
    test_binary_modern_signature_algorithms_are_required = (
        _SignatureAuthenticationScenarios.test_binary_modern_signature_algorithms_are_required
    )
    test_unexpired_signature_status_with_a_supported_hash_can_authenticate = _SignatureAuthenticationScenarios.test_unexpired_signature_status_with_a_supported_hash_can_authenticate
    test_native_signature_records_must_agree_on_the_signing_identity = _SignatureAuthenticationScenarios.test_native_signature_records_must_agree_on_the_signing_identity
    test_repeated_key_consideration_and_signature_id_are_not_extra_signatures = _SignatureAuthenticationScenarios.test_repeated_key_consideration_and_signature_id_are_not_extra_signatures
    test_missing_or_duplicate_signature_facts_are_not_verification = _SignatureAuthenticationScenarios.test_missing_or_duplicate_signature_facts_are_not_verification
    test_unknown_malformed_and_short_fingerprint_status_shapes_fail_closed = _SignatureAuthenticationScenarios.test_unknown_malformed_and_short_fingerprint_status_shapes_fail_closed
    test_english_stderr_and_github_verified_flags_are_not_signature_authority = _SignatureAuthenticationScenarios.test_english_stderr_and_github_verified_flags_are_not_signature_authority
    test_nonzero_native_exit_cannot_be_overridden_by_validsig = (
        _SignatureAuthenticationScenarios.test_nonzero_native_exit_cannot_be_overridden_by_validsig
    )
    test_invalid_byte_inputs_are_rejected_before_spawning = (
        _SignatureAuthenticationScenarios.test_invalid_byte_inputs_are_rejected_before_spawning
    )
    test_input_byte_caps_are_inclusive_and_reject_overflow_before_spawning = _SignatureAuthenticationScenarios.test_input_byte_caps_are_inclusive_and_reject_overflow_before_spawning
    test_a_later_rejection_does_not_retain_a_previous_verified_result = _SignatureAuthenticationScenarios.test_a_later_rejection_does_not_retain_a_previous_verified_result
    test_missing_native_configuration_remains_unavailable_without_effects = _SignatureAuthenticationScenarios.test_missing_native_configuration_remains_unavailable_without_effects

    def test_utf8_goodsig_uid_is_opaque_to_the_full_primary_pin(self):
        for signing, uid in (
            (None, f"José, display text for {SIMPLESTREAMS_KEY}, not authority"),
            (SIGNING_SUBKEY, "署名者 %0A[GNUPG:] BADSIG is still opaque display text"),
        ):
            with self.subTest(signing=signing):
                verifier = self.verifier()
                # Only the native status grammar is modeled. These bytes and the
                # adapter's zero exit are not evidence of a real signed document.
                status = good_status(signing=signing, uid=uid)
                self.assertEqual(self.invoke(verifier, status=status), CHECKSUM_KEY)
                self.assertEqual(verifier.last_classification, NativeClassification.VERIFIED)
                self.assert_scratch_clean()

    def test_non_ascii_machine_fields_remain_invalid_with_a_utf8_opaque_uid(self):
        complete = good_status(uid="José, opaque fixture signer")
        replacements = (
            ("prefix", b"[GNUPG:] ", "[GNÜPG:] ".encode()),
            ("record-name", b"GOODSIG ", "GÖODSIG ".encode()),
            (
                "long-key-id",
                f"GOODSIG {CHECKSUM_KEY[-16:]} ".encode("ascii"),
                f"GOODSIG {CHECKSUM_KEY[-16:-1]}１ ".encode(),
            ),
            (
                "considered-fingerprint",
                f"KEY_CONSIDERED {CHECKSUM_KEY}".encode("ascii"),
                f"KEY_CONSIDERED Ｄ{CHECKSUM_KEY[1:]}".encode(),
            ),
            (
                "signing-fingerprint",
                f"VALIDSIG {CHECKSUM_KEY}".encode("ascii"),
                f"VALIDSIG Ｄ{CHECKSUM_KEY[1:]}".encode(),
            ),
            (
                "primary-fingerprint",
                f"00 {CHECKSUM_KEY}\n".encode("ascii"),
                f"00 Ｄ{CHECKSUM_KEY[1:]}\n".encode(),
            ),
            ("date", b"2020-01-01", "２０２０-01-01".encode()),
            ("timestamp", b"1577836800", "١577836800".encode()),
            ("signature-version", b"4 0 1 8 00", "４ 0 1 8 00".encode()),
            ("digest", b"4 0 1 8 00", "4 0 1 ８ 00".encode()),
            ("trust-field", b"TRUST_UNDEFINED 0", "TRUST_UNDEFINED ０".encode()),
        )
        for field, old, new in replacements:
            with self.subTest(field=field):
                self.assertIn(old, complete)
                verifier = self.verifier()
                self.assertIsNone(self.invoke(verifier, status=complete.replace(old, new)))
                self.assertEqual(verifier.last_classification, NativeClassification.INVALID_STATUS)
                self.assert_scratch_clean()

    def test_only_physical_lf_can_frame_records_inside_a_utf8_uid(self):
        valid_record = next(
            line for line in good_status().split(b"\n") if line.startswith(b"[GNUPG:] VALIDSIG ")
        )
        for separator in (chr(0x85), chr(0x2028), chr(0x2029)):
            complete = good_status(uid=f"José{separator}{valid_record.decode('ascii')}")
            with self.subTest(separator=ord(separator), physical_validsig=True):
                verifier = self.verifier()
                self.assertEqual(self.invoke(verifier, status=complete), CHECKSUM_KEY)
                self.assertEqual(verifier.last_classification, NativeClassification.VERIFIED)
            with self.subTest(separator=ord(separator), physical_validsig=False):
                # A Unicode-aware splitlines() must not manufacture the missing
                # VALIDSIG from the GOODSIG display suffix. Split on byte LF only.
                missing = b"\n".join(
                    line
                    for line in complete.split(b"\n")
                    if not line.startswith(b"[GNUPG:] VALIDSIG ")
                )
                verifier = self.verifier()
                self.assertIsNone(self.invoke(verifier, status=missing))
                self.assertEqual(verifier.last_classification, NativeClassification.INVALID_STATUS)
            self.assert_scratch_clean()
        verifier = self.verifier()
        physical = good_status(uid=f"José\n{valid_record.decode('ascii')}")
        self.assertIsNone(self.invoke(verifier, status=physical))
        self.assertEqual(verifier.last_classification, NativeClassification.INVALID_STATUS)

    def test_utf8_uid_keeps_malformed_byte_bounds_and_strong_primary_controls(self):
        uid = f"José, {CHECKSUM_KEY} in a UID supplies no authority"
        complete = good_status(uid=uid)
        prefix = f"[GNUPG:] GOODSIG {CHECKSUM_KEY[-16:]} ".encode("ascii")
        remaining = 8192 - len(prefix)
        bounded_uid = "é" * (remaining // 2) + "x" * (remaining % 2)
        with self.subTest(case="inclusive-byte-line-bound"):
            verifier = self.verifier()
            self.assertEqual(
                self.invoke(verifier, status=good_status(uid=bounded_uid)), CHECKSUM_KEY
            )
            self.assertEqual(verifier.last_classification, NativeClassification.VERIFIED)
        considered = f"[GNUPG:] KEY_CONSIDERED {CHECKSUM_KEY} 0\n".encode("ascii")
        rejected = NativeClassification.SIGNATURE_REJECTED
        invalid = NativeClassification.INVALID_STATUS
        for case, status, classification in (
            ("wrong-primary", good_status(primary=SIMPLESTREAMS_KEY, uid=uid), rejected),
            (
                "unbound-subkey",
                good_status(signing=SIGNING_SUBKEY, include_primary=False, uid=uid),
                rejected,
            ),
            ("weak-digest", complete.replace(b"4 0 1 8 00", b"4 0 1 2 00"), rejected),
            ("canonical-text", complete.replace(b"4 0 1 8 00", b"4 0 1 8 01"), rejected),
            ("missing-final-lf", complete[:-1], invalid),
            ("extra-record", complete + b"[GNUPG:] UNKNOWN_NATIVE_FACT 1\n", invalid),
            ("record-overflow", complete.replace(considered, considered * 253), invalid),
            ("byte-line-overflow", good_status(uid=bounded_uid + "x"), invalid),
            ("raw-nul", good_status(uid=uid + "\x00"), invalid),
            ("raw-cr", good_status(uid=uid + "\r"), invalid),
            ("raw-tab", good_status(uid=uid + "\t"), invalid),
            ("raw-del", good_status(uid=uid + "\x7f"), invalid),
            ("invalid-utf8-byte", complete.replace(uid.encode(), b"\xff"), invalid),
            ("truncated-utf8", complete.replace(uid.encode(), b"\xc3"), invalid),
            ("overlong-lf", complete.replace(uid.encode(), b"\xc0\x8a"), invalid),
            ("utf8-surrogate", complete.replace(uid.encode(), b"\xed\xa0\x80"), invalid),
            (
                "utf8-out-of-range",
                complete.replace(uid.encode(), b"\xf4\x90\x80\x80"),
                invalid,
            ),
            (
                "utf8-badsig",
                complete + f"[GNUPG:] BADSIG {CHECKSUM_KEY[-16:]} {uid}\n".encode(),
                rejected,
            ),
        ):
            with self.subTest(case=case):
                verifier = self.verifier()
                self.assertIsNone(self.invoke(verifier, status=status))
                self.assertEqual(verifier.last_classification, classification)
                self.assert_scratch_clean()

    def test_exact_input_and_independent_authority_handoff_is_portable(self):
        document = DOCUMENT + b"\x00\xff\r\n"
        signature = SIGNATURE + b"\x00\r\n"
        verifier = self.verifier()
        self.assertEqual(
            self.invoke(verifier, document=document, signature=signature), CHECKSUM_KEY
        )
        self.assertEqual(verifier.last_classification, NativeClassification.VERIFIED)
        self.assertEqual(len(self.calls), 1)
        approved, scratch, consumed_document, consumed_signature = self.calls[0]
        self.assertIs(approved, self.approved)
        self.assertEqual(scratch, self.scratch)
        self.assertIs(consumed_document, document)
        self.assertIs(consumed_signature, signature)
        self.assert_scratch_clean()

    def test_native_exit_policy_is_not_implemented_by_the_controlled_adapter(self):
        for exit_code in (None, True, False, -9, 1, 2, 127):
            with self.subTest(exit_code=exit_code):
                verifier = self.verifier()
                self.assertIsNone(self.invoke(verifier, exit_code=exit_code))
                self.assertEqual(verifier.last_classification, NativeClassification.PROCESS_FAILED)
        self.assertEqual(len(self.calls), 7)

    def test_operation_denial_clears_the_previous_verified_result(self):
        verifier = self.verifier()
        self.assertEqual(self.invoke(verifier), CHECKSUM_KEY)
        for classification in (
            NativeClassification.UNAVAILABLE,
            NativeClassification.UNSAFE_PATH,
            NativeClassification.NATIVE_FAILED,
            NativeClassification.TIMED_OUT,
            NativeClassification.OUTPUT_LIMIT,
            NativeClassification.CLEANUP_INCOMPLETE,
        ):
            with self.subTest(classification=classification):
                self.assertIsNone(self.invoke(verifier, failure=classification))
                self.assertEqual(verifier.last_classification, classification)
        # A rejection in the final check must not return a candidate fingerprint
        # already parsed from otherwise valid status.
        with patch.object(
            ControlledOperation,
            "check",
            side_effect=native._Denied(NativeClassification.UNSAFE_PATH),
        ):
            self.assertIsNone(self.invoke(verifier))
        self.assertEqual(verifier.last_classification, NativeClassification.UNSAFE_PATH)
        self.assert_scratch_clean()

    def test_operation_interrupt_preserves_the_first_exception_through_cleanup(self):
        for first, later in (
            (KeyboardInterrupt(), SystemExit(74)),
            (SystemExit(73), KeyboardInterrupt()),
        ):
            for failure in (later, OSError(errno.EIO, LEAK)):
                with self.subTest(first_type=type(first), cleanup_type=type(failure)):
                    verifier = self.verifier()

                    def cleanup(operation, _state):
                        operation.cleaned = True
                        raise failure

                    with (
                        patch.object(ControlledOperation, "cleanup", cleanup),
                        self.assertRaises(type(first)) as raised,
                    ):
                        self.invoke(verifier, interruption=first)
                    self.assertIs(raised.exception, first)
                    self.assert_scratch_clean()


class UnsupportedNativePlatformTests(PortableFixture):
    def assert_unsupported_before_effects(self, verifier):
        with (
            patch.object(native.os, "open", side_effect=OSError(errno.EIO, LEAK)) as opened,
            patch.object(native.os, "mkdir", side_effect=OSError(errno.EIO, LEAK)) as made,
            patch.object(
                native.os, "memfd_create", side_effect=OSError(errno.ENOSYS, LEAK), create=True
            ) as sealed,
            patch.object(native.ctypes, "CDLL", side_effect=OSError(errno.EIO, LEAK)) as loaded,
            patch.object(native, "_popen", side_effect=OSError(errno.EIO, LEAK)) as spawned,
            patch.object(native.os, "killpg", side_effect=OSError(errno.EPERM, LEAK)) as signaled,
            patch("subprocess.Popen", side_effect=AssertionError("uncontrolled native child")),
            patch("subprocess.run", side_effect=AssertionError("uncontrolled native command")),
        ):
            result = verifier.verify(DOCUMENT, SIGNATURE)
        opened.assert_not_called()
        made.assert_not_called()
        sealed.assert_not_called()
        loaded.assert_not_called()
        spawned.assert_not_called()
        signaled.assert_not_called()
        self.assertIsNone(result)
        self.assertEqual(verifier.last_classification, NativeClassification.UNAVAILABLE)
        self.assert_scratch_clean()

    def test_configured_unsupported_os_is_unavailable_before_any_native_effect(self):
        for platform in ("darwin", "freebsd14", "win32"):
            with self.subTest(platform=platform), patch.object(sys, "platform", platform):
                self.assert_unsupported_before_effects(self.verifier())
        with patch.object(sys, "platform", "linux"):
            with (
                self.subTest(context="worker-thread"),
                ThreadPoolExecutor(max_workers=1) as worker,
            ):
                worker.submit(self.assert_unsupported_before_effects, self.verifier()).result(
                    timeout=3
                )
            for capability in ("valid_signals", "pthread_sigmask"):
                with (
                    self.subTest(missing_capability=capability),
                    patch.object(native.signal, capability, None),
                ):
                    self.assert_unsupported_before_effects(self.verifier())

    def test_unvalidated_structure_abis_are_unavailable_before_native_effects(self):
        for machine in ("mips64", "ppc64le", "riscv64", "aarch64_be", "unknown"):
            with (
                self.subTest(machine=machine),
                patch.object(sys, "platform", "linux"),
                patch.object(
                    native.os,
                    "uname",
                    return_value=os.uname_result(("Linux", "fixture", "", "", machine)),
                ),
            ):
                self.assert_unsupported_before_effects(self.verifier())
        real_sizeof = native.ctypes.sizeof
        for structure in (native._StatFS, native._SignalAction):
            with (
                self.subTest(structure=structure.__name__),
                patch.object(sys, "platform", "linux"),
                patch.object(
                    native.os,
                    "uname",
                    return_value=os.uname_result(("Linux", "fixture", "", "", "x86_64")),
                ),
                patch.object(
                    native.ctypes,
                    "sizeof",
                    side_effect=lambda value: real_sizeof(value) + (value is structure) * 8,
                ),
            ):
                self.assert_unsupported_before_effects(self.verifier())

    def test_unsupported_selection_clears_a_previous_verified_result(self):
        verifier = self.verifier()
        self.assertEqual(self.invoke(verifier), CHECKSUM_KEY)
        with patch.object(sys, "platform", "darwin"):
            self.assert_unsupported_before_effects(verifier)

    def test_unconfigured_verifier_is_effect_free_on_every_platform(self):
        for platform in ("linux", "darwin", "freebsd14", "win32"):
            with self.subTest(platform=platform), patch.object(sys, "platform", platform):
                self.assert_unsupported_before_effects(NativeSignatureVerifier())


@unittest.skipUnless(sys.platform == "linux", "Linux memfd, procfd, inotify and process groups")
class LinuxSignatureAuthenticationTests(NativeFixture):
    test_exact_snapshot_bytes_are_authenticated_in_owned_private_scratch = _SignatureAuthenticationScenarios.test_exact_snapshot_bytes_are_authenticated_in_owned_private_scratch
    test_symlinked_linked_or_writable_keyrings_are_not_native_authority = _SignatureAuthenticationScenarios.test_symlinked_linked_or_writable_keyrings_are_not_native_authority
    test_trusted_executable_cwd_and_environment_ignore_inherited_authority = _SignatureAuthenticationScenarios.test_trusted_executable_cwd_and_environment_ignore_inherited_authority
    test_symlinked_or_writable_scratch_roots_are_rejected_without_effects = _SignatureAuthenticationScenarios.test_symlinked_or_writable_scratch_roots_are_rejected_without_effects
    test_replaced_scratch_file_is_not_authentication_or_cleanup_authority = _SignatureAuthenticationScenarios.test_replaced_scratch_file_is_not_authentication_or_cleanup_authority
    test_changed_private_input_bytes_modes_and_links_are_preserved_and_rejected = _SignatureAuthenticationScenarios.test_changed_private_input_bytes_modes_and_links_are_preserved_and_rejected
    test_approved_keyring_replacement_during_native_verification_is_rejected = _SignatureAuthenticationScenarios.test_approved_keyring_replacement_during_native_verification_is_rejected
    test_replaced_scratch_root_preserves_its_foreign_replacement = _SignatureAuthenticationScenarios.test_replaced_scratch_root_preserves_its_foreign_replacement
    test_replaced_scratch_directory_never_traverses_or_removes_foreign_objects = _SignatureAuthenticationScenarios.test_replaced_scratch_directory_never_traverses_or_removes_foreign_objects
    test_renamed_and_restored_private_directory_is_not_clean_authentication = _SignatureAuthenticationScenarios.test_renamed_and_restored_private_directory_is_not_clean_authentication
    test_unexpected_scratch_entries_do_not_grant_recursive_cleanup_authority = _SignatureAuthenticationScenarios.test_unexpected_scratch_entries_do_not_grant_recursive_cleanup_authority
    test_native_ordinary_exceptions_are_typed_content_free_and_cleanup_owned_scratch = _SignatureAuthenticationScenarios.test_native_ordinary_exceptions_are_typed_content_free_and_cleanup_owned_scratch
    test_keyboard_interrupt_and_system_exit_survive_owned_cleanup = _SignatureAuthenticationScenarios.test_keyboard_interrupt_and_system_exit_survive_owned_cleanup

    def test_non_glibc_is_unavailable_before_opening_authority_or_allocating_resources(self):
        library = native.ctypes.CDLL(None, use_errno=True)

        class WithoutGlibcMarker:
            def __getattr__(self, name):
                if name == "gnu_get_libc_version":
                    raise AttributeError(name)
                return getattr(library, name)

        verifier = self.verifier()
        with (
            patch.object(native.ctypes, "CDLL", return_value=WithoutGlibcMarker()),
            patch.object(native.os, "open", side_effect=OSError(errno.EIO, LEAK)) as opened,
            patch.object(native.os, "mkdir", side_effect=OSError(errno.EIO, LEAK)) as made,
            patch.object(native.os, "memfd_create", side_effect=OSError(errno.EIO, LEAK)) as sealed,
            patch.object(native, "_popen", side_effect=OSError(errno.EIO, LEAK)) as spawned,
        ):
            self.assertIsNone(verifier.verify(DOCUMENT, SIGNATURE))
        opened.assert_not_called()
        made.assert_not_called()
        sealed.assert_not_called()
        spawned.assert_not_called()
        self.assertEqual(verifier.last_classification, NativeClassification.UNAVAILABLE)
        self.assert_scratch_clean()

    def test_approved_keyring_real_bytes_match_the_independent_digest_and_size(self):
        for approved in (
            replace(self.approved, sha256="0" * 64),
            replace(self.approved, size_bytes=len(KEYRING_BYTES) + 1),
        ):
            with self.subTest(approved=approved):
                verifier = self.verifier(keyring=approved)
                self.assertIsNone(self.invoke(verifier))
                self.assertEqual(
                    verifier.last_classification, NativeClassification.INVALID_AUTHORITY
                )
                self.assertEqual(self.calls, [])
                self.assert_scratch_clean()

    def test_all_three_sealed_inputs_deny_write_truncate_and_shared_writable_mmap(self):
        observed = []

        def attack(argv, options):
            detached, data, keyring, home = self.resource_fds(argv, options)
            for fd, expected in ((detached, SIGNATURE), (data, DOCUMENT), (keyring, KEYRING_BYTES)):
                self.assert_sealed_input(fd, expected)
                writable = os.open(f"/proc/self/fd/{fd}", os.O_RDWR | os.O_CLOEXEC)
                try:
                    # Use a writable handle so EBADF cannot masquerade as seal
                    # enforcement. Linux seals must deny each actual mutation.
                    with self.assertRaises(OSError) as write_error:
                        os.pwrite(writable, b"x", 0)
                    self.assertEqual(write_error.exception.errno, errno.EPERM)
                    for size in (0, len(expected) - 1, len(expected) + 1):
                        with self.assertRaises(OSError) as truncate_error:
                            os.ftruncate(writable, size)
                        self.assertEqual(truncate_error.exception.errno, errno.EPERM)
                    with self.assertRaises(OSError) as mmap_error:
                        # ACCESS_WRITE is shared read/write on Unix, not copy-on-write.
                        mmap.mmap(writable, len(expected), access=mmap.ACCESS_WRITE)
                    self.assertEqual(mmap_error.exception.errno, errno.EPERM)
                finally:
                    os.close(writable)
                self.assert_sealed_input(fd, expected)
            observed.extend((detached, data, keyring, home))

        verifier = self.verifier()
        self.assertEqual(self.invoke(verifier, before_spawn=attack), CHECKSUM_KEY)
        self.assertEqual(verifier.last_classification, NativeClassification.VERIFIED)
        self.assertEqual(len(self.calls), 1)
        self.assert_descriptors_closed(observed)
        self.assert_scratch_clean()

    def assert_seal_failure_before_spawn(self, verifier):
        real_open = os.open
        real_memfd_create = getattr(os, "memfd_create", None)
        named_writes = []
        allocated = []

        def capture_memfd(name, flags=0):
            assert callable(real_memfd_create)
            fd = real_memfd_create(name, flags)
            allocated.append(fd)
            return fd

        def inspect_open(path, flags, *args, **kwargs):
            if flags & os.O_CREAT and not flags & os.O_DIRECTORY:
                named_writes.append(path)
            return real_open(path, flags, *args, **kwargs)

        with ExitStack() as patches:
            patches.enter_context(patch.object(native.os, "open", side_effect=inspect_open))
            if callable(real_memfd_create):
                patches.enter_context(
                    patch.object(native.os, "memfd_create", side_effect=capture_memfd)
                )
            result = self.invoke(verifier)
        self.assertIsNone(result)
        self.assertIn(
            verifier.last_classification,
            (
                NativeClassification.UNAVAILABLE,
                NativeClassification.NATIVE_FAILED,
                NativeClassification.UNSAFE_PATH,
            ),
        )
        self.assertEqual(self.calls, [], "failed sealing must deny before spawning")
        self.assertEqual(
            named_writes, [], "failed sealing must not fall back to writable named inputs"
        )
        self.assert_descriptors_closed(allocated)
        self.assert_scratch_clean()

    def test_missing_memfd_capability_has_no_writable_fallback_or_child(self):
        with patch.object(native.os, "memfd_create", None, create=True):
            self.assert_seal_failure_before_spawn(self.verifier())

    def test_failed_memfd_creation_has_no_writable_fallback_or_child(self):
        with patch.object(
            native.os, "memfd_create", side_effect=OSError(errno.ENOSYS, LEAK), create=True
        ):
            self.assert_seal_failure_before_spawn(self.verifier())

    def test_failed_seal_installation_has_no_writable_fallback_or_child(self):
        real_fcntl = fcntl.fcntl
        attempted = []

        def deny_seals(fd, command, *args):
            if command == _F_ADD_SEALS:
                attempted.append(fd)
                raise OSError(errno.EPERM, LEAK)
            return real_fcntl(fd, command, *args)

        with patch.object(fcntl, "fcntl", side_effect=deny_seals):
            self.assert_seal_failure_before_spawn(self.verifier())
        self.assertTrue(attempted)

    def test_each_missing_seal_bit_is_rejected_before_spawn(self):
        real_fcntl = fcntl.fcntl
        for missing in (0x0001, 0x0002, 0x0004, 0x0008):
            with self.subTest(missing=missing):

                def missing_seal(fd, command, *args):
                    if command == _F_GET_SEALS:
                        return real_fcntl(fd, command, *args) & ~missing
                    return real_fcntl(fd, command, *args)

                with patch.object(fcntl, "fcntl", side_effect=missing_seal):
                    self.assert_seal_failure_before_spawn(self.verifier())

    def test_procfd_identity_mismatch_denies_before_spawn_and_preserves_foreign_input(self):
        foreign = self.directory / "foreign-procfd-input"
        foreign.write_bytes(b"foreign retained input")
        foreign.chmod(0o600)
        fd = os.open(foreign, os.O_RDONLY | os.O_CLOEXEC)
        self.addCleanup(os.close, fd)
        real_open, real_stat = os.open, os.stat
        mismatches = []

        def substituted_open(path, flags, *args, **kwargs):
            if _PROC_FD.fullmatch(str(path)):
                mismatches.append(str(path))
                return os.dup(fd)
            return real_open(path, flags, *args, **kwargs)

        def substituted_stat(path, *args, **kwargs):
            if _PROC_FD.fullmatch(str(path)):
                mismatches.append(str(path))
                return os.fstat(fd)
            return real_stat(path, *args, **kwargs)

        verifier = self.verifier()
        with (
            patch.object(native.os, "open", side_effect=substituted_open),
            patch.object(native.os, "stat", side_effect=substituted_stat),
        ):
            result = self.invoke(verifier)
        self.assertIsNone(result)
        self.assertEqual(verifier.last_classification, NativeClassification.UNSAFE_PATH)
        self.assertTrue(mismatches, "validate procfd identity before allowing the fixed child")
        self.assertEqual(self.calls, [])
        self.assertEqual(foreign.read_bytes(), b"foreign retained input")
        self.assert_scratch_clean()

    def test_input_protection_change_does_not_restore_or_remove_an_external_descriptor(self):
        external = []

        def change_mode(argv, options):
            detached, _data, _keyring, _home = self.resource_fds(argv, options)
            held = os.dup(detached)
            self.addCleanup(os.close, held)
            external.append(held)
            os.fchmod(detached, 0o644)

        verifier = self.verifier()
        self.assertIsNone(self.invoke(verifier, before_spawn=change_mode))
        self.assertEqual(verifier.last_classification, NativeClassification.UNSAFE_PATH)
        self.assertEqual(len(external), 1)
        self.assertEqual(stat.S_IMODE(os.fstat(external[0]).st_mode), 0o644)
        self.assertEqual(os.pread(external[0], len(SIGNATURE) + 1, 0), SIGNATURE)
        self.assert_scratch_clean()

    def test_changed_authority_bytes_modes_and_links_are_not_cleanup_targets(self):
        changed = []
        for kind in ("same-inode-content", "mode", "hardlink"):
            with self.subTest(kind=kind):
                source = self.keys / f"authority-{kind}.gpg"
                source.write_bytes(KEYRING_BYTES)
                source.chmod(0o600)
                foreign_link = self.directory / "foreign-authority-link"

                def change(argv, options):
                    _detached, _data, keyring, _home = self.resource_fds(argv, options)
                    self.assert_sealed_input(keyring, KEYRING_BYTES)
                    if kind == "same-inode-content":
                        source.write_bytes(b"x" * len(KEYRING_BYTES))
                    elif kind == "mode":
                        source.chmod(0o644)
                    else:
                        os.link(source, foreign_link)
                    self.assert_sealed_input(keyring, KEYRING_BYTES)
                    changed.append(
                        (source, source.read_bytes(), stat.S_IMODE(source.stat().st_mode))
                    )

                verifier = self.verifier(keyring=replace(self.approved, path=source))
                self.assertIsNone(self.invoke(verifier, before_spawn=change))
                self.assertEqual(verifier.last_classification, NativeClassification.UNSAFE_PATH)
                self.assert_scratch_clean()
        self.assertEqual(len(changed), 3)
        for source, data, mode in changed:
            self.assertEqual(source.read_bytes(), data)
            self.assertEqual(stat.S_IMODE(source.stat().st_mode), mode)
        self.assertEqual((self.directory / "foreign-authority-link").read_bytes(), KEYRING_BYTES)


@unittest.skipUnless(sys.platform == "linux", "Linux inotify observation")
class LinuxObservationTests(NativeFixture):
    def equal_timestamps(self, observation, *, contents=False):
        stamp = self.real_stamp(observation, contents=contents)
        return (*stamp[:-2], 0, 0) if len(stamp) > 5 else stamp

    def setUp(self):
        super().setUp()
        self.real_stamp = native._stamp

    def test_guard_is_armed_before_spawn_and_not_inherited(self):
        observed = []

        def inspect(argv, options):
            _detached, _data, _keyring, home = self.resource_fds(argv, options)
            watches = self.inotify_watches()
            self.assertTrue(watches, "a real Linux observation queue must precede spawn")
            watched_inodes = {inode for records in watches.values() for inode in records.values()}
            for inode in (
                os.fstat(home).st_ino,
                self.scratch.stat().st_ino,
                self.keys.stat().st_ino,
            ):
                self.assertIn(inode, watched_inodes)
            self.assertFalse(set(watches) & set(options["pass_fds"]))
            observed.extend(watches)

        verifier = self.verifier()
        self.assertEqual(self.invoke(verifier, before_spawn=inspect), CHECKSUM_KEY)
        self.assertEqual(verifier.last_classification, NativeClassification.VERIFIED)
        self.assert_descriptors_closed(observed)
        self.assert_scratch_clean()

    def test_move_and_protection_changes_remain_sticky_after_restoration(self):
        for target in ("owned-directory", "scratch-root", "authority", "ancestor", "sealed-input"):
            with self.subTest(target=target):
                changed = []

                def change_and_restore(argv, options):
                    if target == "sealed-input":
                        detached, _data, _keyring, _home = self.resource_fds(argv, options)
                        mode = stat.S_IMODE(os.fstat(detached).st_mode)
                        os.fchmod(detached, mode | 0o044)
                        changed.append(detached)
                        os.fchmod(detached, mode)
                    else:
                        path = {
                            "owned-directory": self.owned_scratch(argv),
                            "scratch-root": self.scratch,
                            "authority": self.keyring_path,
                            "ancestor": self.directory,
                        }[target]
                        mode = stat.S_IMODE(path.stat().st_mode)
                        path.chmod(mode | 0o044)
                        changed.append(path)
                        path.chmod(mode)

                verifier = self.verifier()
                with patch.object(native, "_stamp", side_effect=self.equal_timestamps):
                    result = self.invoke(verifier, before_spawn=change_and_restore)
                self.assertEqual(len(changed), 1)
                self.assertIsNone(result)
                self.assertEqual(verifier.last_classification, NativeClassification.UNSAFE_PATH)
                self.assert_scratch_clean()

    def test_authority_rename_and_restore_with_equal_timestamps_is_rejected(self):
        moved = self.keys / "temporarily-moved.gpg"
        operations = []

        def rename_and_restore(_argv, _options):
            self.keyring_path.rename(moved)
            operations.append(moved)
            moved.rename(self.keyring_path)
            operations.append(self.keyring_path)

        verifier = self.verifier()
        with patch.object(native, "_stamp", side_effect=self.equal_timestamps):
            result = self.invoke(verifier, before_spawn=rename_and_restore)
        self.assertEqual(len(operations), 2)
        self.assertIsNone(result)
        self.assertEqual(verifier.last_classification, NativeClassification.UNSAFE_PATH)
        self.assertEqual(self.keyring_path.read_bytes(), KEYRING_BYTES)
        self.assert_scratch_clean()

    def test_unrelated_ancestor_entries_do_not_become_authentication_failures(self):
        def unrelated_entries(_argv, _options):
            for parent in (self.directory, self.keys):
                unrelated = parent / "unrelated-observation-entry"
                unrelated.write_bytes(b"not a covered binding")
                unrelated.rename(parent / "unrelated-observation-moved")
                (parent / "unrelated-observation-moved").unlink()

        verifier = self.verifier()
        self.assertEqual(self.invoke(verifier, before_spawn=unrelated_entries), CHECKSUM_KEY)
        self.assertEqual(verifier.last_classification, NativeClassification.VERIFIED)
        self.assert_scratch_clean()

    def invoke_observation_fault(self, verifier, make_record, *, repeated=False, interrupt=False):
        real_read = os.read
        active = False
        watches = {}
        reads = []
        escaped_budget = []
        payload = b""

        def arm(_argv, _options):
            nonlocal active, payload
            active = True
            watches.update(self.inotify_watches())
            # Use a watched ancestor and an unrelated basename for ignored-event
            # drain tests. Overflow and loss records remain independent of names.
            parent_inode = self.keys.stat().st_ino
            relevant = [
                wd
                for records in watches.values()
                for wd, inode in records.items()
                if inode == parent_inode
            ]
            wd = relevant[0] if relevant else 1
            payload = make_record(wd)

        def controlled_read(fd, size):
            if not active or fd not in watches:
                return real_read(fd, size)
            reads.append(size)
            if len(reads) > 256:
                escaped_budget.append(len(reads))
                raise OSError(errno.EIO, "test rescue after exhausted observation budget")
            if interrupt:
                raise InterruptedError(errno.EINTR, LEAK)
            if repeated or len(reads) == 1:
                # Never claim more bytes than a real read could return.
                return payload[:size]
            raise BlockingIOError(errno.EAGAIN, "empty controlled observation queue")

        started = time.monotonic()
        with patch.object(native.os, "read", side_effect=controlled_read):
            result = self.invoke(verifier, before_spawn=arm)
        self.assertIsNone(result)
        self.assertEqual(verifier.last_classification, NativeClassification.UNSAFE_PATH)
        self.assertTrue(watches, "the native operation must own an observation queue")
        self.assertTrue(reads, "drain observation after the verification interval")
        self.assertEqual(
            escaped_budget, [], "observation reads and interruptions need a finite budget"
        )
        self.assertLess(time.monotonic() - started, 3)
        self.assert_descriptors_closed(watches)
        self.assert_scratch_clean()
        return reads

    def test_overflow_watch_loss_and_unmount_are_rejections(self):
        for mask in (_IN_Q_OVERFLOW, _IN_IGNORED, _IN_UNMOUNT, _IN_DELETE_SELF, _IN_MOVE_SELF):
            with self.subTest(mask=mask):

                def record(wd):
                    return struct.pack("=iIII", -1 if mask == _IN_Q_OVERFLOW else wd, mask, 0, 0)

                self.invoke_observation_fault(self.verifier(), record)

    def test_malformed_observation_records_are_rejections(self):
        for malformed in (
            lambda _wd: b"\x00" * 15,
            lambda wd: struct.pack("=iIII", wd, _IN_ATTRIB, 0, 16) + b"name\x00",
            lambda wd: struct.pack("=iIII", wd, _IN_ATTRIB, 0, 4) + b"oops",
            lambda wd: struct.pack("=iIII", wd, _IN_ATTRIB, 0, 0xFFFFFFFF),
            lambda wd: struct.pack("=iIII", wd, _IN_ATTRIB, 0, 4) + b"a/b\x00",
            lambda _wd: struct.pack("=iIII", -2, _IN_ATTRIB, 0, 0),
            lambda wd: struct.pack("=iIII", wd, _IN_ATTRIB | 0x10000000, 0, 0),
            lambda wd: struct.pack("=iIII", wd, 0, 0, 0),
        ):
            with self.subTest(record=malformed):
                self.invoke_observation_fault(self.verifier(), malformed)

    def test_continuous_ignored_event_drain_has_a_finite_read_and_event_budget(self):
        def unrelated_record(wd):
            name = b"unrelated-entry\x00"
            name += b"\x00" * (-len(name) % 4)
            moved_from = struct.pack("=iIII", wd, _IN_MOVED_FROM, 42, len(name)) + name
            moved_to = struct.pack("=iIII", wd, _IN_MOVED_TO, 42, len(name)) + name
            return (moved_from + moved_to) * 4

        reads = self.invoke_observation_fault(self.verifier(), unrelated_record, repeated=True)
        self.assertGreater(
            len(reads), 1, "ignore an unrelated event rather than rejecting every record"
        )

    def test_continuous_interrupted_observation_drain_has_a_finite_budget(self):
        reads = self.invoke_observation_fault(self.verifier(), lambda _wd: b"", interrupt=True)
        self.assertGreater(len(reads), 1, "a single transient interruption is not queue loss")


@unittest.skipUnless(sys.platform == "linux", "Linux owned process groups")
class BoundedNativeProcessTests(NativeFixture):
    def assert_worker_interrupt_during_restoration(self, phase, first, later=None):
        delivered = []
        ready = threading.Event()
        send = [threading.Event() for _ in range(2 if later is not None else 1)]
        sent = [threading.Event() for _ in send]
        triggered = []
        recording = []
        record_interruption = native._Cleanup.interrupted
        worker_masks = []
        original_mask = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGUSR2})
        expected_mask = original_mask | {signal.SIGUSR2}

        def interrupted(signum, _frame):
            delivered.append(signum)
            raise first if delivered.count(signal.SIGINT) == 1 or later is None else later

        def interrupted_recording(cleanup, error):
            if later is not None and error is first and not recording:
                # Deliver again before the first exception reaches its recorder.
                # The first exception and restoration progress must survive this
                # second unwind, not just interruptions in the restoration loop.
                recording.append(error)
                send[1].set()
                self.assertTrue(sent[1].wait(3), "worker did not receive the second SIGINT")
            record_interruption(cleanup, error)

        def terminated(signum, _frame):
            delivered.append(signum)
            raise SystemExit(143)

        def received(signum, _frame):
            delivered.append(signum)

        handlers = {
            signal.SIGINT: interrupted,
            signal.SIGTERM: terminated,
            signal.SIGUSR1: received,
        }
        previous = {
            signum: (signal.getsignal(signum), self.signal_flags(signum)) for signum in handlers
        }
        for signum, handler in handlers.items():
            signal.signal(signum, handler)
            signal.siginterrupt(signum, False)
        expected_flags = {signum: self.signal_flags(signum) for signum in handlers}

        def receive_on_worker():
            # pthread_sigmask in restore() cannot block this thread's delivery.
            signal.pthread_sigmask(signal.SIG_UNBLOCK, {signal.SIGINT})
            worker_masks.append(signal.pthread_sigmask(signal.SIG_BLOCK, set()))
            ready.set()
            for requested, delivered_signal in zip(send, sent, strict=True):
                if requested.wait(3):
                    signal.pthread_kill(threading.get_ident(), signal.SIGINT)
                    delivered_signal.set()

        worker = threading.Thread(target=receive_on_worker)
        original_trace = sys.gettrace()

        def trace_restoration(frame, event, _argument):
            if frame.f_code is not native._AcquisitionSignals.restore.__code__:
                return trace_restoration
            if event == "call" and not triggered:
                # Queue callbacks before restoration. An asynchronous caller
                # interrupt must not strand them, even when one callback raises.
                signal.pthread_kill(threading.get_ident(), signal.SIGTERM)
                signal.pthread_kill(threading.get_ident(), signal.SIGUSR1)
            if (
                event == "line"
                and not triggered
                and signal.getsignal(signal.SIGINT) is interrupted
                and signal.getsignal(signal.SIGUSR1) is not received
            ):
                flags_restored = self.signal_flags(signal.SIGINT) == expected_flags[signal.SIGINT]
                if flags_restored == (phase != "native-action"):
                    triggered.append(phase)
                    send[0].set()
                    self.assertTrue(sent[0].wait(3), "worker did not receive the targeted SIGINT")
            return trace_restoration

        verifier = self.verifier()
        try:
            worker.start()
            self.assertTrue(ready.wait(3), "unmasked worker did not become ready")
            sys.settrace(trace_restoration)
            with (
                patch.object(native.os, "open", side_effect=OSError(errno.EIO, LEAK)) as opened,
                patch.object(native.os, "mkdir", side_effect=OSError(errno.EIO, LEAK)) as made,
                patch.object(
                    native.os, "memfd_create", side_effect=OSError(errno.EIO, LEAK)
                ) as sealed,
                patch.object(native, "_popen", side_effect=OSError(errno.EIO, LEAK)) as spawned,
                patch.object(native._Cleanup, "interrupted", interrupted_recording),
                self.assertRaises((KeyboardInterrupt, SystemExit)) as raised,
            ):
                verifier.verify(DOCUMENT, SIGNATURE)
            sys.settrace(original_trace)
            self.assertIs(raised.exception, first)
            self.assertEqual(triggered, [phase])
            self.assertEqual(len(worker_masks), 1)
            self.assertNotIn(signal.SIGINT, worker_masks[0])
            for signum, handler in handlers.items():
                self.assertIs(signal.getsignal(signum), handler)
                self.assertEqual(self.signal_flags(signum), expected_flags[signum])
            self.assertEqual(signal.pthread_sigmask(signal.SIG_BLOCK, set()), expected_mask)
            self.assertEqual(
                delivered, [signal.SIGINT] * len(send) + [signal.SIGTERM, signal.SIGUSR1]
            )
            self.assertEqual(recording, [first] if later is not None else [])
            opened.assert_not_called()
            made.assert_not_called()
            sealed.assert_not_called()
            spawned.assert_not_called()
            self.assert_scratch_clean()
        finally:
            sys.settrace(original_trace)
            worker.join(timeout=4)
            # Rescue the caller's process state after a failing regression. No
            # fixture signal or abandoned guard may leak into the next test.
            signal.pthread_sigmask(signal.SIG_BLOCK, set(handlers))
            for signum, (handler, flags) in previous.items():
                if signum in signal.sigpending():
                    signal.sigwait({signum})
                signal.signal(signum, handler)
                signal.siginterrupt(signum, not bool(flags & 0x10000000))
            signal.pthread_sigmask(signal.SIG_SETMASK, original_mask)
            self.assertFalse(worker.is_alive(), "fixture signal worker did not finish")

    def test_worker_interrupt_between_python_and_native_handler_restoration_finishes_cleanup(self):
        for first in (KeyboardInterrupt(), SystemExit(73)):
            with self.subTest(interrupt_type=type(first)):
                self.assert_worker_interrupt_during_restoration("native-action", first)

    def test_worker_interrupt_between_handlers_restores_pending_callbacks_and_original_mask(self):
        for first in (KeyboardInterrupt(), SystemExit(73)):
            with self.subTest(interrupt_type=type(first)):
                self.assert_worker_interrupt_during_restoration("next-handler", first)

    def test_second_worker_interrupt_during_interruption_recording_finishes_restoration(self):
        for first, later in (
            (KeyboardInterrupt(), SystemExit(74)),
            (SystemExit(73), KeyboardInterrupt()),
        ):
            with self.subTest(first_type=type(first)):
                self.assert_worker_interrupt_during_restoration("bookkeeping", first, later)

    def test_constructor_interrupt_retires_the_real_child_before_releasing_resources(self):
        for first, phase in (
            (KeyboardInterrupt(), "constructor"),
            (SystemExit(73), "constructor"),
            (KeyboardInterrupt(), "handoff"),
            (SystemExit(73), "handoff"),
        ):
            with self.subTest(interrupt_type=type(first), phase=phase):
                verifier = self.verifier()
                first_child = len(self.children)
                ready = self.scripts / f"ready-{len(self.calls)}"
                inherited = []
                resources = {}
                home = []
                created = []
                delivered = []
                signals = []
                reaps = []
                lifetime_checks = []
                lifetime_errors = []
                sent_at = []
                pipe_closures = []
                pipe_patches = ExitStack()
                real_killpg = os.killpg
                real_wait = ControlledChild.wait
                original_mask = signal.pthread_sigmask(signal.SIG_BLOCK, set())
                self.assertNotIn(signal.SIGINT, original_mask)

                def identity(fd):
                    observation = os.fstat(fd)
                    return (
                        observation.st_dev,
                        observation.st_ino,
                        observation.st_mode,
                        observation.st_uid,
                        observation.st_gid,
                    )

                def inspect_resources(argv, options):
                    fds = self.resource_fds(argv, options)
                    inherited.extend(fds)
                    for fd, expected in zip(
                        fds[:3], (SIGNATURE, DOCUMENT, KEYRING_BYTES), strict=True
                    ):
                        self.assert_sealed_input(fd, expected)
                    home.append(self.owned_scratch(argv))
                    for fd in (*fds, *self.inotify_watches()):
                        resources[fd] = identity(fd)

                def check_lifetime(phase):
                    lifetime_checks.append(phase)
                    for fd, expected in resources.items():
                        try:
                            if identity(fd) != expected:
                                lifetime_errors.append((phase, "replaced-resource", fd))
                        except OSError:
                            lifetime_errors.append((phase, "released-resource", fd))
                    if not home[0].is_dir():
                        lifetime_errors.append((phase, "released-scratch"))

                def interrupt_constructor(controlled):
                    created.append(controlled)
                    # No poll or consuming wait may release this child's identity.
                    deadline = time.monotonic() + 3
                    while not ready.exists():
                        self.assertLess(
                            time.monotonic(), deadline, "real child did not become ready"
                        )
                        time.sleep(0.005)
                    record = json.loads(self.children[-1][1].read_bytes())
                    self.assertEqual(record, {"leader": controlled.pid, "descendant": None})
                    self.assertEqual(os.getpgid(controlled.pid), controlled.pid)
                    self.assertEqual(os.getsid(controlled.pid), controlled.pid)
                    self.assertIsNone(controlled.process.returncode)
                    self.assertEqual(controlled.waits, [])
                    self.assertEqual(controlled.resources, tuple(inherited))
                    status = Path(f"/proc/{controlled.pid}/status").read_text()
                    blocked = next(
                        line for line in status.splitlines() if line.startswith("SigBlk:")
                    )
                    expected_mask = sum(1 << (int(signum) - 1) for signum in original_mask)
                    self.assertEqual(int(blocked.partition(":")[2].strip(), 16), expected_mask)
                    for pipe in (controlled.process.stdout, controlled.process.stderr):
                        self.assertIsNotNone(pipe)
                        assert pipe is not None
                        pipe_closures.append(
                            pipe_patches.enter_context(
                                patch.object(pipe, "close", wraps=pipe.close)
                            )
                        )
                    check_lifetime("constructor")
                    if phase == "constructor":
                        sent_at.append(time.monotonic())
                        # Send real SIGINT after creation, before constructor return.
                        # Handler exceptions may be caught until the lease exists.
                        os.kill(os.getpid(), signal.SIGINT)

                def trace_handoff(frame: FrameType, event: str, _argument: object):
                    if (
                        event == "line"
                        and frame.f_code is native._verify_native.__code__
                        and frame.f_locals.get("process") is not None
                        and frame.f_locals.get("child") is None
                        and not sent_at
                    ):
                        # Exercise the source window after Popen returns but before
                        # lease assignment. Constructor-only protection is too short.
                        check_lifetime("handoff")
                        sent_at.append(time.monotonic())
                        os.kill(os.getpid(), signal.SIGINT)
                    return trace_handoff

                def interrupted(signum, _frame):
                    delivered.append(signum)
                    raise first

                def observe_signal(pgid, signum):
                    controlled = created[0]
                    signals.append(
                        (pgid, signum, controlled.process.returncode, tuple(controlled.waits))
                    )
                    check_lifetime("signal")
                    return real_killpg(pgid, signum)

                def observe_wait(controlled, timeout=None):
                    reaps.append((timeout, controlled.group_kill_sent))
                    check_lifetime("reap")
                    result = real_wait(controlled, timeout=timeout)
                    check_lifetime("reaped")
                    return result

                previous_flags = self.signal_flags(signal.SIGINT)
                previous = signal.signal(signal.SIGINT, interrupted)
                signal.siginterrupt(signal.SIGINT, False)
                original_flags = self.signal_flags(signal.SIGINT)
                self.assertTrue(original_flags & 0x10000000, "enable native SA_RESTART")
                original_trace = sys.gettrace()
                try:
                    try:
                        if phase == "handoff":
                            sys.settrace(trace_handoff)
                        with (
                            pipe_patches,
                            patch.object(native.os, "killpg", side_effect=observe_signal),
                            patch.object(ControlledChild, "wait", observe_wait),
                            self.assertRaises(type(first)) as raised,
                        ):
                            self.invoke(
                                verifier,
                                mode="constructor-interrupt",
                                before_spawn=inspect_resources,
                                constructor_created=interrupt_constructor,
                            )
                        self.assertIs(raised.exception, first)
                        self.assertIs(signal.getsignal(signal.SIGINT), interrupted)
                        self.assertEqual(self.signal_flags(signal.SIGINT), original_flags)
                    finally:
                        sys.settrace(original_trace)
                        # Rescue a broken signal-mask implementation without
                        # delivering a pending fixture SIGINT to the next test.
                        mask_after = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGINT})
                        try:
                            if signal.SIGINT in signal.sigpending():
                                signal.sigwait({signal.SIGINT})
                        finally:
                            signal.signal(signal.SIGINT, previous)
                            signal.siginterrupt(
                                signal.SIGINT, not bool(previous_flags & 0x10000000)
                            )
                            signal.pthread_sigmask(signal.SIG_SETMASK, original_mask)
                    self.assertEqual(signal.getsignal(signal.SIGINT), previous)
                    self.assertEqual(mask_after, original_mask)
                    self.assertEqual(delivered, [signal.SIGINT])
                    self.assertEqual(len(created), 1)
                    controlled = created[0]
                    self.assertTrue(signals, "constructor cancellation must retire the owned group")
                    self.assertIn(signal.SIGKILL, [entry[1] for entry in signals])
                    for pgid, _signum, returncode, waits in signals:
                        self.assertEqual(pgid, controlled.pid)
                        self.assertIsNone(
                            returncode, "no consuming reap may precede the last signal"
                        )
                        self.assertEqual(waits, ())
                    self.assertTrue(reaps, "constructor cancellation must finitely reap its child")
                    for timeout, killed in reaps:
                        self.assertIsNotNone(timeout)
                        self.assertTrue(math.isfinite(timeout))
                        self.assertGreater(timeout, 0)
                        self.assertLessEqual(timeout, 0.2)
                        self.assertTrue(killed, "group retirement must precede consuming reap")
                    self.assertEqual(controlled.process.returncode, -signal.SIGKILL)
                    self.assertEqual(len(pipe_closures), 2)
                    self.assertEqual([closed.call_count for closed in pipe_closures], [1, 1])
                    self.assertEqual(lifetime_errors, [])
                    self.assertIn("reaped", lifetime_checks)
                    self.assertLess(time.monotonic() - sent_at[0], 3)
                    self.assertNotEqual(verifier.last_classification, NativeClassification.VERIFIED)
                    self.assert_group_retired()
                    self.assert_descriptors_closed(resources)
                    self.assert_scratch_clean()
                finally:
                    # Assert native retirement before this independent safety
                    # cleanup. It uses only the fixture marker and the retained
                    # process object, never an escaped or reused numeric PID.
                    # The child's own 10-second deadline bounds a failed marker.
                    for controlled, _record in self.children[first_child:]:
                        try:
                            controlled.retire.write_bytes(b"retire constructor fixture")
                        finally:
                            controlled.group_signals_open = False
                            try:
                                _REAL_POPEN.wait(controlled.process, timeout=12)
                            finally:
                                for pipe in (controlled.process.stdout, controlled.process.stderr):
                                    if pipe is not None:
                                        pipe.close()

        # A non-throwing custom handler also needs bounded acquisition delivery.
        # Saturating its queue denies success but still retires the real child.
        verifier = self.verifier()
        ready = self.scripts / f"ready-{len(self.calls)}"
        delivered = []

        def received(signum, _frame):
            delivered.append(signum)

        def burst(_controlled):
            deadline = time.monotonic() + 3
            while not ready.exists():
                self.assertLess(time.monotonic(), deadline, "real child did not become ready")
                time.sleep(0.005)
            for _ in range(65):
                os.kill(os.getpid(), signal.SIGUSR1)

        previous = signal.signal(signal.SIGUSR1, received)
        try:
            result = self.invoke(verifier, mode="constructor-interrupt", constructor_created=burst)
            self.assertIsNone(result)
            self.assertEqual(verifier.last_classification, NativeClassification.CLEANUP_INCOMPLETE)
            self.assertEqual(delivered, [signal.SIGUSR1] * 64)
            self.assertIs(signal.getsignal(signal.SIGUSR1), received)
            self.assert_group_retired()
            self.assert_scratch_clean()
        finally:
            signal.signal(signal.SIGUSR1, previous)

    def test_interrupts_during_native_execution_retire_the_group_and_are_preserved(self):
        for interrupt in (KeyboardInterrupt(), SystemExit(73)):
            with self.subTest(interrupt_type=type(interrupt)):
                verifier = self.verifier()

                def interrupted(_signum, _frame):
                    raise interrupt

                previous = signal.signal(signal.SIGINT, interrupted)
                try:
                    with self.assertRaises(type(interrupt)) as raised:
                        self.invoke(verifier, mode="interrupt-owner", descendant=True)
                    self.assertIs(raised.exception, interrupt)
                    self.assert_group_retired()
                    self.assert_scratch_clean()
                finally:
                    signal.signal(signal.SIGINT, previous)

    def test_second_interrupt_before_final_group_signal_cannot_abandon_owned_child(self):
        for first, later, delivery in (
            (KeyboardInterrupt(), SystemExit(74), "main"),
            (SystemExit(73), KeyboardInterrupt(), "worker"),
            (KeyboardInterrupt(), SystemExit(74), "guard-setup"),
            (KeyboardInterrupt(), SystemExit(74), "guard-entry"),
            (SystemExit(73), KeyboardInterrupt(), "guard-entry"),
        ):
            with self.subTest(first_type=type(first), delivery=delivery):
                verifier = self.verifier()
                delivered = []
                second = []
                killed = []
                cleanup_state = []
                resources = []
                ready = threading.Event()
                send = threading.Event()
                sent = threading.Event()
                real_killpg = os.killpg
                real_rmdir = os.rmdir
                real_library = native.ctypes.CDLL
                original_mask = signal.pthread_sigmask(signal.SIG_BLOCK, set())
                original_trace = sys.gettrace()

                def trace_kill_entry(frame, event, _argument):
                    if (
                        frame.f_code is native._kill_owned.__code__
                        and event == "call"
                        and not second
                    ):
                        # CPython can dispatch before the helper executes any
                        # bookkeeping or enters its own protected retry region.
                        second.append(self.children[-1][0].pid)
                        os.kill(os.getpid(), signal.SIGINT)
                    return trace_kill_entry

                def receive_on_worker():
                    signal.pthread_sigmask(signal.SIG_UNBLOCK, {signal.SIGINT})
                    ready.set()
                    if send.wait(3):
                        signal.pthread_kill(threading.get_ident(), signal.SIGINT)
                        sent.set()

                worker = threading.Thread(target=receive_on_worker)

                def interrupted(signum, _frame):
                    delivered.append(signum)
                    raise first if len(delivered) == 1 else later

                def inspect_resources(argv, options):
                    resources.extend(self.resource_fds(argv, options))

                def load_library(*args, **options):
                    if delivery == "guard-setup" and len(delivered) == 1 and not second:
                        # Deferral setup itself must not open another cancellation
                        # gap before the final group signal has been protected.
                        second.append(self.children[-1][0].pid)
                        os.kill(os.getpid(), signal.SIGINT)
                    return real_library(*args, **options)

                def signal_group(pgid, signum):
                    if signum == signal.SIGKILL and not second:
                        second.append(pgid)
                        # Real delivery at the last pre-killpg boundary, not an
                        # injected exception or a consuming-reap interruption.
                        if delivery == "main":
                            os.kill(os.getpid(), signal.SIGINT)
                        else:
                            send.set()
                            self.assertTrue(sent.wait(3), "worker did not send the second SIGINT")
                    for fd in resources:
                        os.fstat(fd)
                    result = real_killpg(pgid, signum)
                    if signum == signal.SIGKILL:
                        killed.append(pgid)
                    return result

                def remove_scratch(name, *, dir_fd=None):
                    controlled, record = self.children[-1]
                    descendant = json.loads(record.read_bytes())["descendant"]
                    cleanup_state.append(
                        (
                            controlled.process.returncode,
                            controlled.group_kill_sent,
                            self.wait_for_retirement(descendant),
                            controlled.retire.exists(),
                        )
                    )
                    for fd in resources:
                        os.fstat(fd)
                    return real_rmdir(name, dir_fd=dir_fd)

                previous_flags = self.signal_flags(signal.SIGINT)
                previous = signal.signal(signal.SIGINT, interrupted)
                signal.siginterrupt(signal.SIGINT, False)
                expected_flags = self.signal_flags(signal.SIGINT)
                started = time.monotonic()
                try:
                    if delivery == "worker":
                        worker.start()
                        self.assertTrue(ready.wait(3), "unmasked worker did not become ready")
                    if delivery == "guard-entry":
                        sys.settrace(trace_kill_entry)
                    with (
                        patch.object(native.os, "killpg", side_effect=signal_group),
                        patch.object(native.os, "rmdir", side_effect=remove_scratch),
                        patch.object(native.ctypes, "CDLL", side_effect=load_library),
                        self.assertRaises(type(first)) as raised,
                    ):
                        self.invoke(
                            verifier,
                            mode="interrupt-owner",
                            descendant=True,
                            before_spawn=inspect_resources,
                        )
                    self.assertIs(raised.exception, first)
                    controlled = self.children[-1][0]
                    self.assertEqual(second, [controlled.pid])
                    self.assertEqual(killed, [controlled.pid], "the final group signal must finish")
                    self.assertEqual(cleanup_state, [(-signal.SIGKILL, True, True, False)])
                    self.assertEqual(delivered, [signal.SIGINT, signal.SIGINT])
                    self.assertIs(signal.getsignal(signal.SIGINT), interrupted)
                    self.assertEqual(self.signal_flags(signal.SIGINT), expected_flags)
                    self.assertEqual(signal.pthread_sigmask(signal.SIG_BLOCK, set()), original_mask)
                    self.assertLess(time.monotonic() - started, 3)
                    self.assert_group_retired()
                    self.assert_descriptors_closed(resources)
                    self.assert_scratch_clean()
                finally:
                    sys.settrace(original_trace)
                    if delivery == "worker":
                        worker.join(timeout=4)
                        self.assertFalse(worker.is_alive(), "fixture signal worker did not finish")
                    signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGINT})
                    if signal.SIGINT in signal.sigpending():
                        signal.sigwait({signal.SIGINT})
                    signal.signal(signal.SIGINT, previous)
                    signal.siginterrupt(signal.SIGINT, not bool(previous_flags & 0x10000000))
                    signal.pthread_sigmask(signal.SIG_SETMASK, original_mask)

    def test_timeout_uses_a_monotonic_deadline_and_retires_the_owned_group(self):
        verifier = self.verifier()
        started = time.monotonic()
        with patch("time.time", side_effect=AssertionError("wall clock is not deadline authority")):
            result = self.invoke(verifier, mode="timeout", descendant=True)
        elapsed = time.monotonic() - started
        self.assertEqual(verifier.last_classification, NativeClassification.TIMED_OUT)
        self.assertIsNone(result)
        self.assertLess(elapsed, 3, "timeout and post-kill waits must remain bounded")
        self.assertEqual(len(self.calls), 1)
        self.assert_group_retired()
        self.assert_scratch_clean()

    def test_incremental_stdout_and_stderr_overflow_retire_the_owned_group(self):
        for stream in ("stdout", "stderr"):
            with self.subTest(stream=stream):
                verifier = self.verifier()
                started = time.monotonic()
                result = self.invoke(
                    verifier,
                    status=b"x" * 65537 if stream == "stdout" else good_status(),
                    stderr=b"e" * 65537 if stream == "stderr" else b"",
                    mode=f"{stream}-overflow",
                    descendant=True,
                )
                self.assertEqual(verifier.last_classification, NativeClassification.OUTPUT_LIMIT)
                self.assertIsNone(result)
                self.assertLess(time.monotonic() - started, 3)
                self.assert_group_retired()
                self.assert_scratch_clean()

    def test_stderr_cap_is_inclusive_and_native_messages_are_never_emitted(self):
        verifier = self.verifier()
        output, error = io.StringIO(), io.StringIO()
        with redirect_stdout(output), redirect_stderr(error):
            result = self.invoke(verifier, stderr=b"e" * 65536)
        self.assertEqual(result, CHECKSUM_KEY)
        self.assertEqual(verifier.last_classification, NativeClassification.VERIFIED)
        self.assertEqual(len(self.calls), 1)
        self.assertEqual((output.getvalue(), error.getvalue()), ("", ""))
        self.assertEqual(self.children[0][0].communications, 0)
        self.assert_scratch_clean()

    def test_output_progress_cannot_restart_the_original_deadline(self):
        verifier = self.verifier()
        started = time.monotonic()
        result = self.invoke(verifier, mode="trickle", descendant=True)
        self.assertEqual(verifier.last_classification, NativeClassification.TIMED_OUT)
        self.assertIsNone(result)
        self.assertLess(time.monotonic() - started, 3)
        self.assert_group_retired()
        self.assert_scratch_clean()

    def test_exited_leader_does_not_allow_inherited_pipe_writers_to_wait_forever(self):
        verifier = self.verifier()
        started = time.monotonic()
        result = self.invoke(verifier, mode="leader-exit", descendant=True)
        self.assertEqual(verifier.last_classification, NativeClassification.TIMED_OUT)
        self.assertIsNone(result)
        self.assertLess(time.monotonic() - started, 3)
        self.assertEqual(self.children[0][0].process.returncode, 0)
        self.assert_group_retired()
        self.assert_scratch_clean()

    def test_group_retirement_never_signals_after_reaping_the_leader(self):
        verifier = self.verifier()
        signals = []
        real_killpg = os.killpg

        def observe_signal(pgid, signum):
            controlled = self.children[-1][0]
            signals.append((pgid, signum, controlled.process.returncode, tuple(controlled.waits)))
            return real_killpg(pgid, signum)

        with patch.object(native.os, "killpg", side_effect=observe_signal):
            self.assertIsNone(self.invoke(verifier, mode="leader-exit", descendant=True))
        self.assertEqual(verifier.last_classification, NativeClassification.TIMED_OUT)
        self.assertTrue(signals)
        self.assertIn(signal.SIGKILL, [entry[1] for entry in signals])
        for pgid, _signum, returncode, waits in signals:
            self.assertEqual(pgid, self.children[-1][0].pid)
            self.assertIsNone(
                returncode, "the leader's PID must remain reserved until the last signal"
            )
            self.assertEqual(waits, (), "no consuming wait can precede group retirement")
        self.assert_group_retired()
        self.assert_scratch_clean()

    def test_exited_leader_keeps_group_authority_without_nonconsuming_wait_support(self):
        verifier = self.verifier()
        started = time.monotonic()
        with (
            patch.object(native.os, "waitid", None, create=True),
            patch.object(native.os, "WNOWAIT", None, create=True),
        ):
            result = self.invoke(verifier, mode="leader-exit", descendant=True)
        self.assertIsNone(result)
        self.assertEqual(verifier.last_classification, NativeClassification.TIMED_OUT)
        self.assertLess(time.monotonic() - started, 3)
        self.assertEqual(self.children[-1][0].process.returncode, 0)
        self.assert_group_retired()
        self.assert_scratch_clean()

    def test_denied_live_group_is_not_reported_retired_after_its_leader_exits(self):
        verifier = self.verifier()
        output, error = io.StringIO(), io.StringIO()
        started = time.monotonic()
        with (
            patch.object(native.os, "killpg", side_effect=PermissionError(1, LEAK)) as denied,
            redirect_stdout(output),
            redirect_stderr(error),
        ):
            result = self.invoke(verifier, mode="leader-exit", descendant=True)
        self.assertTrue(denied.called, "the original group must reach the OS signal boundary")
        self.assertIsNone(result)
        self.assertEqual(verifier.last_classification, NativeClassification.CLEANUP_INCOMPLETE)
        self.assertLess(time.monotonic() - started, 3)
        controlled, record = self.children[-1]
        self.assertEqual(controlled.process.returncode, 0)
        descendant = json.loads(record.read_bytes())["descendant"]
        self.assertIsNotNone(descendant)
        os.kill(descendant, 0)  # Denied live members are not zombie-only cleanup.
        self.assertFalse(controlled.group_signals_open)
        self.assertFalse(controlled.group_kill_sent)
        self.assertEqual(controlled.communications, 0)
        self.assertTrue(
            all(value is not None and math.isfinite(value) for value in controlled.waits)
        )
        for pipe in (controlled.process.stdout, controlled.process.stderr):
            self.assertIsNotNone(pipe)
            self.assertTrue(pipe.closed)
        self.assertEqual((output.getvalue(), error.getvalue()), ("", ""))
        self.assert_scratch_clean()

    def test_escaped_pipe_writer_does_not_extend_waits_or_expand_signal_authority(self):
        verifier = self.verifier()
        started = time.monotonic()
        result = self.invoke(verifier, mode="escaped-holder", descendant=True)
        self.assertEqual(verifier.last_classification, NativeClassification.TIMED_OUT)
        self.assertIsNone(result)
        self.assertLess(time.monotonic() - started, 3)
        controlled, record = self.children[-1]
        self.assertEqual(controlled.process.returncode, 0)
        escaped = json.loads(record.read_bytes())["descendant"]
        os.kill(escaped, 0)  # Native cleanup may signal only its owned process group.
        self.assertEqual(controlled.communications, 0)
        self.assert_scratch_clean()

    def test_second_interrupt_after_signal_lease_closes_finishes_reaping_before_cleanup(self):
        for first, later in (
            (KeyboardInterrupt(), SystemExit(74)),
            (SystemExit(73), KeyboardInterrupt()),
        ):
            with self.subTest(first_type=type(first)):
                verifier = self.verifier()
                delivered = []
                interrupted_reap = []
                signals = []
                reaps = []
                cleanup_state = []
                resources = []
                real_monotonic = time.monotonic
                real_wait = ControlledChild.wait
                real_killpg = os.killpg
                real_rmdir = os.rmdir

                def interrupted(signum, _frame):
                    delivered.append(signum)
                    raise first if len(delivered) == 1 else later

                def interrupt_before_wait():
                    frame = sys._getframe(1)
                    if frame.f_code is native._reap_child.__code__ and not interrupted_reap:
                        child = frame.f_locals["child"]
                        # Send a real second SIGINT after signaling authority closes,
                        # before the protected wait region or any consuming reap.
                        interrupted_reap.append(
                            (
                                child,
                                child.reap_deadline,
                                child.signals_open,
                                tuple(child.process.waits),
                            )
                        )
                        os.kill(os.getpid(), signal.SIGINT)
                    return real_monotonic()

                def inspect_resources(argv, options):
                    resources.extend(self.resource_fds(argv, options))

                def signal_group(pgid, signum):
                    lease_open = not interrupted_reap or interrupted_reap[0][0].signals_open
                    signals.append((pgid, signum, lease_open))
                    return real_killpg(pgid, signum)

                def observe_wait(controlled, timeout=None):
                    child = interrupted_reap[0][0]
                    reaps.append((child.reap_deadline, child.signals_open, timeout))
                    return real_wait(controlled, timeout=timeout)

                def remove_scratch(name, *, dir_fd=None):
                    controlled = self.children[-1][0]
                    cleanup_state.append(
                        (
                            controlled.process.returncode,
                            bool(controlled.waits),
                            controlled.retire.exists(),
                        )
                    )
                    for fd in resources:
                        os.fstat(fd)
                    return real_rmdir(name, dir_fd=dir_fd)

                previous_flags = self.signal_flags(signal.SIGINT)
                previous = signal.signal(signal.SIGINT, interrupted)
                signal.siginterrupt(signal.SIGINT, False)
                expected_flags = self.signal_flags(signal.SIGINT)
                original_mask = signal.pthread_sigmask(signal.SIG_BLOCK, set())
                started = real_monotonic()
                try:
                    with (
                        patch.object(native.time, "monotonic", interrupt_before_wait),
                        patch.object(native.os, "killpg", side_effect=signal_group),
                        patch.object(native.os, "rmdir", side_effect=remove_scratch),
                        patch.object(ControlledChild, "wait", observe_wait),
                        self.assertRaises(type(first)) as raised,
                    ):
                        self.invoke(
                            verifier,
                            mode="interrupt-owner",
                            descendant=True,
                            before_spawn=inspect_resources,
                        )
                    self.assertIs(raised.exception, first)
                    self.assertEqual(delivered, [signal.SIGINT, signal.SIGINT])
                    self.assertEqual(len(interrupted_reap), 1)
                    child, deadline, lease_open, waits = interrupted_reap[0]
                    self.assertFalse(lease_open)
                    self.assertEqual(waits, ())
                    self.assertIsNotNone(deadline)
                    self.assertEqual(child.reap_deadline, deadline)
                    self.assertEqual(cleanup_state, [(-signal.SIGKILL, True, False)])
                    self.assertTrue(reaps, "closed signaling authority must not skip reaping")
                    for reap_deadline, signals_open, timeout in reaps:
                        self.assertEqual(reap_deadline, deadline)
                        self.assertFalse(signals_open)
                        self.assertIsNotNone(timeout)
                        self.assertTrue(math.isfinite(timeout))
                        self.assertGreater(timeout, 0)
                        self.assertLessEqual(timeout, 0.2)
                    self.assertIn(signal.SIGKILL, [entry[1] for entry in signals])
                    for pgid, _signum, signals_open in signals:
                        self.assertEqual(pgid, child.pid)
                        self.assertTrue(signals_open, "no group signal may follow lease closure")
                    self.assertIs(signal.getsignal(signal.SIGINT), interrupted)
                    self.assertEqual(self.signal_flags(signal.SIGINT), expected_flags)
                    self.assertEqual(signal.pthread_sigmask(signal.SIG_BLOCK, set()), original_mask)
                    self.assertLess(real_monotonic() - started, 3)
                    self.assert_group_retired()
                    self.assert_descriptors_closed(resources)
                    self.assert_scratch_clean()
                finally:
                    signal.signal(signal.SIGINT, previous)
                    signal.siginterrupt(signal.SIGINT, not bool(previous_flags & 0x10000000))
                    signal.pthread_sigmask(signal.SIG_SETMASK, original_mask)

    def test_first_execution_interrupt_survives_a_later_consuming_reap_interrupt(self):
        for first, later in (
            (KeyboardInterrupt(), SystemExit(74)),
            (SystemExit(73), KeyboardInterrupt()),
        ):
            with self.subTest(first_type=type(first)):
                real_wait = ControlledChild.wait
                interrupted_reap = []

                def consume_then_interrupt(child, timeout=None):
                    returncode = real_wait(child, timeout=timeout)
                    if not interrupted_reap:
                        interrupted_reap.append(child.pid)
                        raise later
                    return returncode

                def interrupted(_signum, _frame):
                    raise first

                previous = signal.signal(signal.SIGINT, interrupted)
                verifier = self.verifier()
                try:
                    with (
                        patch.object(ControlledChild, "wait", consume_then_interrupt),
                        self.assertRaises(type(first)) as raised,
                    ):
                        self.invoke(verifier, mode="interrupt-owner", descendant=True)
                    self.assertIs(raised.exception, first)
                    self.assertEqual(len(interrupted_reap), 1)
                    self.assert_group_retired()
                    self.assert_scratch_clean()
                finally:
                    signal.signal(signal.SIGINT, previous)

    def test_consuming_reap_interruption_never_reopens_the_group_signal_lease(self):
        real_wait = ControlledChild.wait
        first = KeyboardInterrupt()
        consumed = []

        def consume_then_interrupt(child, timeout=None):
            returncode = real_wait(child, timeout=timeout)
            if not consumed:
                consumed.append(child.pid)
                raise first
            return returncode

        verifier = self.verifier()
        with (
            patch.object(ControlledChild, "wait", consume_then_interrupt),
            self.assertRaises(KeyboardInterrupt) as raised,
        ):
            self.invoke(verifier)
        self.assertIs(raised.exception, first)
        self.assertEqual(len(consumed), 1)
        controlled = self.children[-1][0]
        self.assertFalse(controlled.group_signals_open)
        self.assertEqual(controlled.process.returncode, 0)
        self.assertGreaterEqual(len(controlled.waits), 2)
        self.assertTrue(all(math.isfinite(timeout) for timeout in controlled.waits))
        self.assert_scratch_clean()

    def test_post_kill_reap_failure_is_bounded_and_reports_cleanup_incomplete(self):
        verifier = self.verifier()
        output, error = io.StringIO(), io.StringIO()
        started = time.monotonic()
        with redirect_stdout(output), redirect_stderr(error):
            result = self.invoke(verifier, mode="timeout", unreapable=True)
        self.assertEqual(verifier.last_classification, NativeClassification.CLEANUP_INCOMPLETE)
        self.assertIsNone(result)
        self.assertLess(time.monotonic() - started, 3)
        self.assertEqual(len(self.calls), 1)
        controlled = self.children[0][0]
        self.assertEqual(controlled.communications, 0)
        self.assertTrue(
            all(value is not None and math.isfinite(value) for value in controlled.waits)
        )
        for pipe in (controlled.process.stdout, controlled.process.stderr):
            self.assertIsNotNone(pipe)
            self.assertTrue(pipe.closed)
        self.assertEqual((output.getvalue(), error.getvalue()), ("", ""))
        self.assert_scratch_clean()


class VmProvisioningTests(unittest.TestCase):
    def test_stock_seed_binds_original_guest_and_separate_accounts(self):
        from spaceterm_tasks import server_vm_linux as native

        token = "6" * 64
        # Public comparison data only. Private key generation is a native boundary.
        public = (
            b"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZm\n"
        )
        seed = native.jammy_seed(token, public, public, b"synthetic noncredential key data")
        document = native.read_jammy_seed(seed)
        config = json.loads(document["user-data"].removeprefix(b"#cloud-config\n"))
        self.assertEqual(config["users"][0]["name"], "stvm-admin")
        self.assertEqual(config["users"][0]["uid"], 20000)
        self.assertEqual(config["users"][1]["name"], "stvm-server")
        self.assertEqual(config["users"][1]["uid"], 20001)
        self.assertEqual(config["users"][1]["sudo"], False)
        self.assertEqual(config["users"][1]["groups"], [])
        owner = json.loads(config["write_files"][0]["content"])
        self.assertEqual(owner["token"], token)
        self.assertEqual(owner["server_uid"], 20001)
        self.assertEqual(config["write_files"][0]["permissions"], "0600")
        self.assertFalse(config["ssh_pwauth"])
        self.assertEqual(config["ssh_keys"]["ed25519_public"], public.decode().strip())
        for word in ("apport", "apparmor", "suid_dumpable", "sysctl", "apt", "package"):
            self.assertNotIn(word, document["user-data"].decode())
        with self.assertRaises(native._Denied):
            native.read_jammy_seed(seed[:-1])

    def test_explicit_task_composition_dispatches_owned_receipts_not_paths(self):
        from spaceterm_tasks import server_vm as vm

        from tests.test_server_vm_executor import VmExecutorTests

        fixture = VmExecutorTests()
        fixture.new_fixture()
        self.addCleanup(fixture.doCleanups)
        output = io.StringIO()
        with redirect_stdout(output):
            result = vm.run_task(
                vm.Phase.PREPARE,
                executor=fixture.executor,
                state_directory=fixture.state_identity,
                request=fixture.request,
            )
        self.assertEqual(result, 0)
        self.assertTrue(fixture.native.calls)
        self.assertEqual(output.getvalue(), "phase=prepare classification=prepared\n")
        fixture.native.calls.clear()
        with redirect_stdout(io.StringIO()):
            result = vm.run_task(
                vm.Phase.START, executor=fixture.executor, receipt=Path("/not-authority")
            )
        self.assertEqual(result, 2)
        self.assertEqual(fixture.native.calls, [])


class OwnedJammyTransportTests(unittest.TestCase):
    def fixture(self):
        from spaceterm_tasks import server_vm as vm_module

        from tests import test_server_vm_executor as fixtures
        from tests.test_server_vm import bounded_configuration, bounded_policy

        fixture = fixtures.VmExecutorTests()
        fixture.new_fixture()
        self.addCleanup(fixture.doCleanups)

        class Native(fixtures.ControlledVmNative):
            def create_credentials(self, directory_fd):
                super().create_credentials(directory_fd)
                credentials = os.open(
                    "credentials", os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC, dir_fd=directory_fd
                )
                try:
                    fixtures.write_private(
                        credentials, "host_ed25519", b"noncredential fixture object"
                    )
                    fixtures.write_private(
                        credentials,
                        "host_ed25519.pub",
                        b"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZm\n",
                    )
                finally:
                    os.close(credentials)

            def create_container(self, vm, runner):
                prepared = vm
                identifier = super().create_container(prepared, runner)
                assert identifier is not None
                self.observation = vm_module.ContainerObservation(
                    identifier,
                    prepared.ownership.token,
                    prepared.request.directory,
                    False,
                    bounded_configuration(
                        prepared.request.directory, runner, prepared.request.runner_image
                    ),
                    bounded_policy(),
                )
                return identifier

            def start_container(self, container_id):
                super().start_container(container_id)
                assert self.observation is not None
                self.observation = replace(self.observation, running=True)

            def stop_container(self, container_id, timeout_seconds):
                super().stop_container(container_id, timeout_seconds)
                assert self.observation is not None
                self.observation = replace(self.observation, running=False)

            def remove_container(self, container_id):
                super().remove_container(container_id)
                self.observation = None

        fixture.native = Native()
        fixture.executor._native = fixture.executor._planner._containers = fixture.native
        return fixture

    def test_owned_sequence_checks_transport_and_retires_original_guest_on_interruption(self):
        from spaceterm_tasks import server_vm_linux as native
        from spaceterm_tasks.linux_custody_evidence import GuestObservation

        for failure in (
            None,
            KeyboardInterrupt(),
            SystemExit(143),
            TaskError("ownership-mismatch"),
        ):
            with self.subTest(failure=type(failure)):
                fixture = self.fixture()
                seen = []

                class Peer:
                    def __init__(self, receipt):
                        self.receipt = receipt
                        seen.append(receipt)

                    def observe(self):
                        if failure is not None:
                            raise failure
                        return GuestObservation(
                            self.receipt.container_id,
                            self.receipt.token,
                            1000,
                            1001,
                            (22, 4, 0),
                            (5, 15, 0),
                            (2, 35, 0),
                            249,
                            True,
                            True,
                            2,
                        )

                if failure is not None:
                    with self.assertRaises(type(failure)):
                        native.run_owned_jammy_transport(
                            fixture.executor, fixture.request, fixture.state_identity, Peer
                        )
                else:
                    native.run_owned_jammy_transport(
                        fixture.executor, fixture.request, fixture.state_identity, Peer
                    )
                self.assertEqual(len(seen), 1)
                self.assertIsNone(fixture.native.observation)
                self.assertEqual(
                    [name for name, _ in fixture.native.calls].count("create-container"), 1
                )
                self.assertEqual(fixture.journal()["stage"], "removed")
                self.assertFalse(fixture.directory.exists())

    def test_late_guest_observation_cannot_pass_the_original_boot_deadline(self):
        from spaceterm_tasks import server_vm_linux as native
        from spaceterm_tasks.linux_custody_evidence import GuestObservation

        fixture = self.fixture()

        class Peer:
            def __init__(self, receipt):
                self.receipt = receipt

            def observe(self):
                return GuestObservation(
                    self.receipt.container_id,
                    self.receipt.token,
                    1000,
                    1001,
                    (22, 4, 0),
                    (5, 15, 0),
                    (2, 35, 0),
                    249,
                    True,
                    True,
                    2,
                )

        with (
            patch.object(native.time, "monotonic", side_effect=(0, 180)),
            self.assertRaisesRegex(TaskError, "^timed-out$"),
        ):
            native.run_owned_jammy_transport(
                fixture.executor, fixture.request, fixture.state_identity, Peer
            )
        self.assertIsNone(fixture.native.observation)
        self.assertEqual(fixture.journal()["stage"], "removed")

    def test_cleanup_refusal_preserves_original_receipt_and_never_removes_running_guest(self):
        from spaceterm_tasks import server_vm_linux as native

        fixture = self.fixture()
        fixture.native.errors["stop-container"] = OSError("private native detail")

        class Peer:
            def __init__(self, _receipt):
                pass

            def observe(self):
                raise KeyboardInterrupt()

        with self.assertRaisesRegex(TaskError, "^cleanup-unresolved$"):
            native.run_owned_jammy_transport(
                fixture.executor, fixture.request, fixture.state_identity, Peer
            )
        self.assertNotIn("remove-container", [name for name, _ in fixture.native.calls])
        self.assertTrue(fixture.directory.exists())
        self.assertEqual(fixture.journal()["stage"], "stop-pending")

    @unittest.skipUnless(sys.platform == "linux", "Linux sealed SSH descriptor contract")
    def test_actual_owned_jammy_transport_binds_guest_and_accounts(self):
        from spaceterm_tasks import server_vm_linux as native

        fixture = self.fixture()
        prepared = fixture.executor.prepare(fixture.request, state_directory=fixture.state_identity)
        assert prepared.receipt is not None
        started = fixture.executor.start(prepared.receipt)
        receipt = started.receipt
        assert receipt is not None
        peer = native.LinuxGuestTransport(receipt, fixture.native)
        calls = []
        observation = dict(
            token=receipt.token,
            administrator_uid=20000,
            server_uid=20001,
            os_version=[22, 4, 0],
            kernel_version=[5, 15, 0],
            libc_version=[2, 35, 0],
            systemd_version=249,
            apparmor_enabled=True,
            apport_enabled=True,
            suid_dumpable=2,
        )

        def ssh(program, arguments, *, inherited, **_kwargs):
            self.assertEqual(program, "ssh")
            self.assertIn("StrictHostKeyChecking=yes", arguments)
            self.assertIn("IdentityAgent=none", arguments)
            self.assertIn("ForwardAgent=no", arguments)
            self.assertIn("ControlPath=none", arguments)
            self.assertIn("stvm-admin@127.0.0.1", arguments)
            self.assertIn("HostKeyAlias=stvm-" + receipt.token, arguments)
            self.assertEqual(len(inherited), 2)
            for descriptor in inherited:
                self.assertEqual(
                    fcntl.fcntl(descriptor, native._F_GET_SEALS) & native._REQUIRED_SEALS,
                    native._REQUIRED_SEALS,
                )
            known = os.pread(inherited[1], 8192, 0)
            self.assertTrue(known.startswith(("stvm-" + receipt.token + " ssh-ed25519 ").encode()))
            calls.append(arguments)
            return native._OperationResult(json.dumps(observation).encode(), 0)

        with patch.object(native, "_run_vm_tool", side_effect=ssh):
            value = peer.observe()
            self.assertEqual(value.container_id, receipt.container_id)
            self.assertEqual((value.administrator_uid, value.server_uid), (20000, 20001))
            observation["token"] = "0" * 64
            with self.assertRaisesRegex(TaskError, "^ownership-mismatch$"):
                peer.observe()
            assert fixture.native.observation is not None
            fixture.native.observation = replace(fixture.native.observation, owner_token="0" * 64)
            with self.assertRaisesRegex(TaskError, "^ownership-mismatch$"):
                peer.observe()
            self.assertEqual(len(calls), 2)


class RunnerObservationTests(unittest.TestCase):
    def raw_observation(self, request, runner):
        from tests.test_server_vm import (
            CONTAINER_ID,
            LOCAL_RUNNER_IMAGE,
            TOKEN,
            bounded_configuration,
        )

        config = {
            "Image": request.runner_image,
            "User": f"{runner.uid}:{runner.gid}",
            "Env": None,
            "WorkingDir": "/vm",
            "Entrypoint": ["/usr/bin/qemu-system-x86_64"],
            "Cmd": list(bounded_configuration(request.directory, runner).arguments),
            "Volumes": None,
            "ExposedPorts": {"2222/tcp": {}},
            "Tty": False,
            "OpenStdin": False,
            "StopTimeout": 30,
            "Labels": {"io.github.sadiksaifi.spaceterm.issue520.vm-owner": TOKEN},
        }
        host = dict(
            NanoCpus=2000000000,
            Memory=6144 * 1024**2,
            MemorySwap=6144 * 1024**2,
            PidsLimit=256,
            ReadonlyRootfs=True,
            Privileged=False,
            CapDrop=["ALL"],
            CapAdd=None,
            SecurityOpt=["no-new-privileges:true"],
            GroupAdd=[str(runner.kvm_gid)],
            Tmpfs={"/tmp": "rw,noexec,nosuid,size=64m"},
            Devices=[
                dict(PathOnHost="/dev/kvm", PathInContainer="/dev/kvm", CgroupPermissions="rw")
            ],
            NetworkMode="bridge",
            PidMode="",
            IpcMode="private",
            UTSMode="",
            UsernsMode="",
            CgroupnsMode="private",
            PublishAllPorts=False,
            PortBindings={"2222/tcp": [dict(HostIp="127.0.0.1", HostPort="25022")]},
            LogConfig=dict(Type="local", Config={"max-size": "10m", "max-file": "2"}),
            RestartPolicy=dict(Name="no", MaximumRetryCount=0),
            AutoRemove=False,
            Runtime="runc",
            OomKillDisable=False,
            OomScoreAdj=0,
            CgroupParent="",
            Binds=None,
            VolumesFrom=None,
            DeviceRequests=None,
            DeviceCgroupRules=None,
            Sysctls=None,
            StorageOpt=None,
            Ulimits=None,
            CpusetCpus="",
            CpusetMems="",
            BlkioWeightDevice=None,
            BlkioDeviceReadBps=None,
            BlkioDeviceWriteBps=None,
            BlkioDeviceReadIOps=None,
            BlkioDeviceWriteIOps=None,
            Mounts=[
                dict(
                    Type="bind",
                    Source=str(request.directory),
                    Target="/vm",
                    BindOptions=dict(NonRecursive=True),
                )
            ],
        )
        return [
            dict(
                Id=CONTAINER_ID,
                Image=LOCAL_RUNNER_IMAGE,
                Config=config,
                HostConfig=host,
                State=dict(Running=False),
                Mounts=[
                    dict(
                        Type="bind",
                        Source=str(request.directory),
                        Destination="/vm",
                        RW=True,
                        Propagation="rprivate",
                    )
                ],
                AppArmorProfile="docker-default",
            )
        ]

    def test_effective_runner_policy_rejects_extra_rights_and_inherited_configuration(self):
        import copy

        from spaceterm_tasks import server_vm_linux as native

        from tests.test_server_vm import (
            CONTAINER_ID,
            LOCAL_RUNNER_IMAGE,
            bounded_configuration,
            bounded_policy,
        )
        from tests.test_server_vm_executor import VmExecutorTests

        fixture = VmExecutorTests()
        fixture.new_fixture()
        self.addCleanup(fixture.doCleanups)
        original = self.raw_observation(fixture.request, fixture.runner)
        value = native._container_observation(
            original, fixture.request, fixture.runner, LOCAL_RUNNER_IMAGE, CONTAINER_ID
        )
        self.assertEqual(
            value.configuration,
            bounded_configuration(fixture.directory, fixture.runner, fixture.request.runner_image),
        )
        self.assertEqual(value.policy, bounded_policy())
        for section, field, altered in (
            ("HostConfig", "Privileged", True),
            ("HostConfig", "CapAdd", ["SYS_ADMIN"]),
            ("HostConfig", "DeviceRequests", [dict(Count=-1)]),
            ("HostConfig", "PidMode", "host"),
            ("HostConfig", "Memory", 0),
            ("HostConfig", "NanoCpus", 0),
            ("HostConfig", "Ulimits", []),
            (
                "HostConfig",
                "PortBindings",
                {"2222/tcp": [dict(HostIp="0.0.0.0", HostPort="25022")]},
            ),
            ("Config", "Env", ["LD_PRELOAD=untrusted"]),
            ("Config", "Healthcheck", dict(Test=["CMD", "untrusted"])),
            ("Config", "Volumes", {"/host": {}}),
        ):
            with self.subTest(section=section, field=field):
                raw = copy.deepcopy(original)
                raw[0][section][field] = altered
                with self.assertRaises(native._Denied):
                    native._container_observation(
                        raw, fixture.request, fixture.runner, LOCAL_RUNNER_IMAGE, CONTAINER_ID
                    )
        raw = copy.deepcopy(original)
        del raw[0]["HostConfig"]["SecurityOpt"]
        with self.assertRaises(native._Denied):
            native._container_observation(
                raw, fixture.request, fixture.runner, LOCAL_RUNNER_IMAGE, CONTAINER_ID
            )

    @unittest.skipUnless(sys.platform == "linux", "Linux protected utility descriptor contract")
    def test_protected_tool_uses_original_executable_descriptor_and_bounded_process(self):
        from spaceterm_tasks import server_vm_linux as native

        real = subprocess.Popen
        calls = []

        def spawn(arguments, **kwargs):
            descriptor = int(arguments[0].rsplit("/", 1)[1])
            self.assertEqual(os.fstat(descriptor).st_uid, 0)
            self.assertIn(descriptor, kwargs["pass_fds"])
            self.assertNotIn("SSH_AUTH_SOCK", kwargs["env"])
            self.assertEqual(kwargs["stdin"], subprocess.DEVNULL)
            calls.append(arguments)
            return real((sys.executable, "-I", "-c", "print('bounded-control')"), **kwargs)

        with patch.object(native, "_popen", side_effect=spawn):
            result = native._run_vm_tool("ssh", ("-V",))
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.status, b"bounded-control\n")
        self.assertEqual(len(calls), 1)
        with (
            patch.object(native, "_popen", side_effect=spawn),
            self.assertRaises(native._Denied) as error,
        ):
            native._run_vm_tool("ssh", ("-V",), maximum=2)
        self.assertEqual(error.exception.classification, native.NativeClassification.OUTPUT_LIMIT)


class NativeVmArtifactTests(unittest.TestCase):
    @unittest.skipUnless(sys.platform == "linux", "Linux provisioning tool contract")
    def test_concrete_native_adapter_produces_bounded_disks_keys_and_stock_seed(self):
        from spaceterm_tasks import server_vm as vm
        from spaceterm_tasks import server_vm_linux as native

        from tests.test_server_vm import LOCAL_RUNNER_IMAGE
        from tests.test_server_vm_executor import VmExecutorTests, write_private

        fixture = VmExecutorTests()
        fixture.new_fixture()
        self.addCleanup(fixture.doCleanups)
        adapter = native.NativeVm(fixture.request, LOCAL_RUNNER_IMAGE)
        public = (
            b"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZm\n"
        )
        calls = []
        capacities = {}

        def tool(program, arguments, *, inherited, **_kwargs):
            calls.append((program, arguments))
            if program == "ssh-keygen":
                leaf = arguments[-1].rsplit("/", 1)[1]
                write_private(inherited[0], leaf, b"noncredential test object")
                write_private(inherited[0], leaf + ".pub", public)
            elif program == "qemu-img":
                operation = arguments[0]
                if operation == "convert":
                    self.assertEqual(os.pread(inherited[0], 8192, 0), fixture.image_bytes)
                    os.write(inherited[1], b"controlled unprivileged image output")
                elif operation == "resize":
                    capacities[os.fstat(inherited[0]).st_ino] = int(arguments[-1])
                elif operation == "create":
                    os.write(inherited[0], b"controlled unprivileged data output")
                    capacities[os.fstat(inherited[0]).st_ino] = int(arguments[-1])
                elif operation == "info":
                    current = os.fstat(inherited[0])
                    data = {
                        "format": "qcow2",
                        "virtual-size": capacities[current.st_ino],
                        "actual-size": current.st_blocks * 512,
                    }
                    return native._OperationResult(json.dumps(data).encode(), 0)
            else:
                self.fail("unexpected utility")
            return native._OperationResult(b"", 0)

        directory = os.open(fixture.directory, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
        image = os.open(fixture.request.image.path, os.O_RDONLY | os.O_CLOEXEC)
        try:
            with patch.object(native, "_run_vm_tool", side_effect=tool):
                adapter.create_credentials(directory)
                unavailable = os.statvfs_result((4096, 4096, 0, 0, 0, 1, 1, 1, 0, 255))
                with (
                    patch.object(os, "fstatvfs", return_value=unavailable),
                    self.assertRaises(native._Denied),
                ):
                    adapter.create_os_disk(directory, image, 24 * 1024**3)
                self.assertFalse((fixture.directory / "os.qcow2").exists())
                available = os.statvfs_result(
                    (4096, 4096, 10000000, 10000000, 10000000, 1, 1, 1, 0, 255)
                )
                with patch.object(os, "fstatvfs", return_value=available):
                    adapter.create_os_disk(directory, image, 24 * 1024**3)
                adapter.create_encrypted_disk(directory, 8 * 1024**3)
                adapter.create_seed(directory, "6" * 64)
                for name, size in (("os.qcow2", 24 * 1024**3), ("encrypted.qcow2", 8 * 1024**3)):
                    descriptor = os.open(name, os.O_RDONLY | os.O_CLOEXEC, dir_fd=directory)
                    try:
                        evidence = adapter.inspect_disk(
                            descriptor, vm.OwnedPath.capture(fixture.directory / name)
                        )
                        self.assertEqual(evidence.virtual_size_bytes, size)
                        self.assertFalse(evidence.has_backing_file)
                        self.assertFalse(evidence.has_external_data_file)
                    finally:
                        os.close(descriptor)
                descriptor = os.open("seed.iso", os.O_RDONLY | os.O_CLOEXEC, dir_fd=directory)
                try:
                    evidence = adapter.inspect_seed(
                        descriptor, vm.OwnedPath.capture(fixture.directory / "seed.iso")
                    )
                    self.assertEqual(evidence.label, "CIDATA")
                    self.assertLess(evidence.size_bytes, 4 * 1024**2)
                finally:
                    os.close(descriptor)
        finally:
            os.close(image)
            os.close(directory)
        self.assertEqual(
            set(path.name for path in fixture.directory.iterdir()),
            {"credentials", "os.qcow2", "encrypted.qcow2", "seed.iso"},
        )
        self.assertEqual([name for name, _ in calls].count("ssh-keygen"), 2)
        for path in (fixture.directory / "credentials").iterdir():
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)


if __name__ == "__main__":
    unittest.main()
