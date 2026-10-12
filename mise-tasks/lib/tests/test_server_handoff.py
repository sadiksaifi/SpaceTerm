"""Ownership and output regressions for the nonauthorizing startup-pipe task wrapper.

The wrapper runs in isolated Python. Its spawn boundary starts fixed disposable
fixtures, including a real forked compiler helper and an opt-in native supervisor
compiled from repository sources by the explicit macOS fault task. It never starts Keychain,
signing tools, a server, or a privileged supervisor. The wrapper's entry,
bounded_process, signal handlers, deadlines and cleanup remain real.

Assertions run after the probe has retired every fixture. A pipe lease, a child
lifetime limit, and bounded fallback reaping protect intended RED failures.
Only fixed classifications, booleans and bounded status facts cross the probe boundary.
"""

import importlib.util
import io
import json
import math
import os
import select
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
import unittest
from contextlib import nullcontext, redirect_stderr, redirect_stdout
from dataclasses import dataclass
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

_TASK = Path(__file__).resolve().parents[2] / "server/native/macos/handoff_tests.py"
_PYTHON = str(Path(sys.executable).resolve())
_TASK_TIMEOUT = 0.25
_STOP_TIMEOUT = 0.1
_REAP_TIMEOUT = 0.2
_READY_TIMEOUT = 2.0
_PROBE_TIMEOUT = 8.0
_REPORT_LIMIT = 4096

# Each _DISPOSABLE fixture is a direct child in its own session. This complete
# source never forks or closes retirement's write end before process exit. Its
# retirement EOF therefore proves this fixed scope has no live helper. Actual
# compilers have no such contract: their helpers can fork or close inherited FDs.
# The private OS witness below tests cleanup policy, not Darwin group discovery.
# Closing either lease retires the fixture even if the wrapper abandoned it.
# Native stdout is deliberately not JSON.
_DISPOSABLE = r"""
import os, select, signal, sys, time
if not sys.flags.isolated:
    raise SystemExit(97)
signal.pthread_sigmask(signal.SIG_SETMASK, set())
lease, outer_lease, ready, retirement = map(int, sys.argv[1:5])
mode = sys.argv[5]
if mode == "ignore-term":
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
os.write(ready, b"r")
os.close(ready)
if mode == "closed-output":
    os.close(1)
if mode == "overflow":
    os.write(2, b"not-a-record\n")
    block = memoryview(b"x" * 32769)
    while block:
        block = block[os.write(1, block):]
deadline = time.monotonic() + 6.0
while time.monotonic() < deadline:
    readable, _, _ = select.select([lease, outer_lease], [], [], 0.02)
    if any(os.read(fd, 1) == b"" for fd in readable):
        break
    if mode == "trickle":
        os.write(1, b"x")
"""

# The compiler leader really forks a helper in its new session/process group.
# Only the helper retains retirement's write end. EOF therefore observes this
# fixed, nonforking helper's death without consulting or signaling a released
# PID. Both processes independently stop on either lease's EOF or a finite
# deadline, including when the probe dies before fallback cleanup can run.
_GROUP_DISPOSABLE = r"""
import os, select, signal, sys, time
if not sys.flags.isolated:
    raise SystemExit(97)
signal.pthread_sigmask(signal.SIG_SETMASK, set())
lease, outer_lease, ready, retirement = map(int, sys.argv[1:5])
mode = sys.argv[5]
leader = os.getpid()
deadline = time.monotonic() + 6.0
signal.signal(signal.SIGTERM, lambda _number, _frame: os._exit(0))
helper_ready_read, helper_ready_write = os.pipe()
helper = os.fork()
if helper == 0:
    os.close(helper_ready_read)
    os.close(ready)
    if os.getpgid(0) != leader or os.getsid(0) != leader:
        os._exit(97)
    if mode == "compiler-helper-delay":
        def delay_term(_number, _frame):
            global deadline
            deadline = min(deadline, time.monotonic() + 1.0)
        signal.signal(signal.SIGTERM, delay_term)
    else:
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
    os.write(helper_ready_write, b"h")
    os.close(helper_ready_write)
    while time.monotonic() < deadline:
        readable, _, _ = select.select([lease, outer_lease], [], [], 0.01)
        if any(os.read(fd, 1) == b"" for fd in readable):
            break
    os._exit(0)
os.close(helper_ready_write)
os.close(retirement)
readable, _, _ = select.select([helper_ready_read], [], [], 2.0)
if not readable or os.read(helper_ready_read, 2) != b"h":
    os._exit(97)
os.close(helper_ready_read)
os.write(ready, b"r")
os.close(ready)
while time.monotonic() < deadline:
    readable, _, _ = select.select([lease, outer_lease], [], [], 0.01)
    if any(os.read(fd, 1) == b"" for fd in readable):
        break
os._exit(0)
"""

_NATIVE_FIXTURE_ENV = "SPACETERM_HANDOFF_REAP_FAULT_BINARY"
_NATIVE_TASK_TIMEOUT = 3.0
_NATIVE_STOP_TIMEOUT = 1.5
_NATIVE_FAULT_ARGUMENT = "--private-cancellation-reap-fault"
# Each native probe starts with no retirement proof. Only its validated private
# report, after both fixture retirement and outer-probe reaping, completes it.
# The explicit task uses this evidence even when a later behavior assertion fails.
_NATIVE_FAULT_RETIREMENTS: list[bool] = []


def _configured_native_fault_fixture():
    value = os.environ.get(_NATIVE_FIXTURE_ENV)
    if value is None:
        return None
    candidate = Path(value)
    try:
        metadata = candidate.lstat()
        parent = candidate.parent.lstat()
        valid = (
            candidate.is_absolute()
            and stat.S_ISREG(metadata.st_mode)
            and metadata.st_uid == os.geteuid()
            and metadata.st_mode & 0o100 != 0
            and metadata.st_mode & 0o022 == 0
            and stat.S_ISDIR(parent.st_mode)
            and parent.st_uid == os.geteuid()
            and stat.S_IMODE(parent.st_mode) == 0o700
        )
    except OSError:
        valid = False
    if not valid:
        raise AssertionError(
            "native reap-fault fixture must be an owned executable in a private directory"
        )
    return candidate


def native_fault_resources_retired():
    """Report only the real private retirement observations for this suite."""
    return all(_NATIVE_FAULT_RETIREMENTS)


@dataclass(frozen=True)
class _Scenario:
    trigger: str = "none"
    first: int | None = None
    second: int | None = None
    native: bool = False
    native_fault: bool = False
    child: str = "hold"
    code: int = 143
    classification: str = "interrupted"


_SCENARIOS = {
    "spawn-term": _Scenario(trigger="clock", first=signal.SIGTERM),
    "spawn-int": _Scenario(trigger="clock", first=signal.SIGINT, code=130),
    "cleanup-term": _Scenario(trigger="wait", first=signal.SIGTERM, second=signal.SIGTERM),
    "cleanup-term-int": _Scenario(trigger="wait", first=signal.SIGTERM, second=signal.SIGINT),
    "cleanup-int-term": _Scenario(
        trigger="wait", first=signal.SIGINT, second=signal.SIGTERM, code=130
    ),
    "cleanup-native-term": _Scenario(
        trigger="wait",
        first=signal.SIGTERM,
        second=signal.SIGTERM,
        native=True,
        child="closed-output",
    ),
    "reaped-term": _Scenario(trigger="reaped", first=signal.SIGTERM),
    "deadline": _Scenario(native=True, child="trickle", code=2, classification="deadline"),
    "output-limit": _Scenario(
        native=True, child="overflow", code=2, classification="driver-invalid-output"
    ),
    "escalation": _Scenario(child="ignore-term", code=2, classification="cleanup-unresolved"),
    "compiler-helper-term": _Scenario(
        trigger="wait",
        first=signal.SIGTERM,
        child="compiler-helper-ignore",
        code=2,
        classification="cleanup-unresolved",
    ),
    "compiler-helper-int": _Scenario(
        trigger="wait",
        first=signal.SIGINT,
        child="compiler-helper-ignore",
        code=2,
        classification="cleanup-unresolved",
    ),
    "compiler-helper-deadline": _Scenario(
        child="compiler-helper-ignore", code=2, classification="cleanup-unresolved"
    ),
    "compiler-helper-delayed-term": _Scenario(
        trigger="wait",
        first=signal.SIGTERM,
        child="compiler-helper-delay",
        code=2,
        classification="cleanup-unresolved",
    ),
    "native-reap-fault-term": _Scenario(
        trigger="clock",
        first=signal.SIGTERM,
        native=True,
        native_fault=True,
        code=2,
        classification="cleanup-unresolved",
    ),
    "native-reap-fault-int": _Scenario(
        trigger="clock",
        first=signal.SIGINT,
        native=True,
        native_fault=True,
        code=2,
        classification="cleanup-unresolved",
    ),
    "native-reaped-fault-term": _Scenario(
        trigger="reaped",
        first=signal.SIGTERM,
        native=True,
        native_fault=True,
        code=2,
        classification="cleanup-unresolved",
    ),
    "native-reaped-fault-int": _Scenario(
        trigger="reaped",
        first=signal.SIGINT,
        native=True,
        native_fault=True,
        code=2,
        classification="cleanup-unresolved",
    ),
}


class _FixtureFailure(BaseException):
    """Escape the task's error sanitizer without carrying subprocess details."""


class _CappedText(io.StringIO):
    def __init__(self):
        super().__init__()
        self.overflow = False

    def write(self, text):
        remaining = _REPORT_LIMIT - self.tell()
        if len(text) > remaining:
            self.overflow = True
        super().write(text[:remaining])
        return len(text)


class _ModuleProxy:
    """Replace only task-local OS boundaries, not global stdlib operations."""

    def __init__(self, module, **overrides):
        self.module = module
        self.overrides = overrides

    def __getattr__(self, name):
        return self.overrides.get(name, getattr(self.module, name))


class _OwnedChild:
    def __init__(self, process, lease, *, retirement=None, audit=None):
        self.process = process
        self.lease = lease
        self.retirement = retirement
        self.audit = audit

    def helper_retired(self, timeout=0):
        if self.retirement is None:
            return True
        readable, _, _ = select.select([self.retirement], [], [], timeout)
        return bool(readable) and os.read(self.retirement, 1) == b""

    def fixture_retired(self, timeout):
        if not self.helper_retired(timeout=timeout):
            return False
        if self.audit is None:
            return True
        # The native case also retains this pipe until death, without writing.
        # EOF covers an unexpectedly orphaned case as well as its supervisor.
        deadline = time.monotonic() + timeout
        consumed = 0
        while consumed <= 4:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return False
            readable, _, _ = select.select([self.audit], [], [], remaining)
            if not readable:
                return False
            block = os.read(self.audit, 5 - consumed)
            if not block:
                return True
            consumed += len(block)
        return False

    def __getattr__(self, name):
        return getattr(self.process, name)

    def close_lease(self):
        if self.lease is not None:
            os.close(self.lease)
            self.lease = None


class _TaskChild:
    """Observe actual waits; never synthesize a return code or a reaped child."""

    def __init__(self, owner, child):
        self.owner = owner
        self.child = child

    def __getattr__(self, name):
        return getattr(self.child.process, name)

    def wait(self, timeout=None):
        maximum = _NATIVE_TASK_TIMEOUT if self.owner.scenario.native_fault else _TASK_TIMEOUT
        if timeout is None or not math.isfinite(timeout) or not 0 <= timeout <= maximum + 0.05:
            self.owner.bounds_ok = False
            raise _FixtureFailure()
        if self.owner.scenario.trigger == "reaped" and not self.owner.first_sent:
            if self.owner.scenario.native_fault:
                # The task's actual wait reaps the real C supervisor. Deliver the
                # original cancellation only after that wait succeeds, before
                # bounded_process can apply its cancellation checkpoint.
                if not self.owner.native_stop_sent:
                    self.owner.signal_child("pid", self.child.process.pid, signal.SIGTERM)
                    self.owner.native_stop_sent = True
            else:
                self.child.close_lease()
            result = self.child.process.wait(timeout=timeout)
            self.owner.cancel_first()
            return result
        if self.owner.scenario.trigger == "wait" and not self.owner.first_sent:
            self.owner.cancel_first()
        return self.child.process.wait(timeout=timeout)

    def communicate(self, *args, **kwargs):
        self.owner.bounds_ok = False
        raise _FixtureFailure()


class _Probe:
    def __init__(self, scenario, native_fixture=None):
        self.scenario = scenario
        self.native_fixture = native_fixture
        self.native_stop_sent = False
        self.children = []
        self.target = None
        self.first_sent = False
        self.second_sent = False
        self.authority_ok = True
        self.bounds_ok = True
        self.signal_count = 0
        self.spawned_at = None
        # Duplicate stdin before fixture stdin is redirected to DEVNULL.
        self.outer_lease = os.dup(sys.stdin.fileno())

    def start_fixture(self, *, stdout, mode):
        if mode == "native-reap-fault" and self.native_fixture is None:
            raise _FixtureFailure()
        lease_read, lease_write = os.pipe()
        ready_read, ready_write = os.pipe()
        group = mode in {"compiler-helper-ignore", "compiler-helper-delay"}
        native = mode == "native-reap-fault"
        channel_read, channel_write = os.pipe()
        descriptors = (lease_read, self.outer_lease, ready_write, channel_write)
        if native:
            command = [str(self.native_fixture), _NATIVE_FAULT_ARGUMENT]
            command.extend(str(fd) for fd in descriptors)
        else:
            script = _GROUP_DISPOSABLE if group else _DISPOSABLE
            command = [_PYTHON, "-I", "-c", script]
            command.extend(str(fd) for fd in descriptors)
            command.append(mode)
        registered = False
        previous_mask = signal.pthread_sigmask(
            signal.SIG_BLOCK, {signal.SIGTERM, signal.SIGINT, signal.SIGALRM}
        )
        try:
            process = subprocess.Popen(
                command,
                cwd=_TASK.parents[4],
                env={},
                stdin=subprocess.DEVNULL,
                stdout=stdout,
                stderr=subprocess.DEVNULL,
                close_fds=True,
                start_new_session=True,
                pass_fds=descriptors,
            )
            child = _OwnedChild(
                process,
                lease_write,
                retirement=channel_read if not native else None,
                audit=channel_read if native else None,
            )
            self.children.append(child)
            registered = True
        finally:
            os.close(lease_read)
            os.close(ready_write)
            if channel_write is not None:
                os.close(channel_write)
            if not registered:
                os.close(lease_write)
                if channel_read is not None:
                    os.close(channel_read)
            signal.pthread_sigmask(signal.SIG_SETMASK, previous_mask)
        try:
            readable, _, _ = select.select([ready_read], [], [], _READY_TIMEOUT)
            if not readable or os.read(ready_read, 2) != b"r":
                raise _FixtureFailure()
            if os.getsid(process.pid) != process.pid or os.getpgid(process.pid) != process.pid:
                raise _FixtureFailure()
        finally:
            os.close(ready_read)
        return child

    def spawn(self, command, **options):
        expected = {
            "cwd": _TASK.parents[4],
            "env": {},
            "stdin": subprocess.DEVNULL,
            "stdout": subprocess.PIPE if self.scenario.native else subprocess.DEVNULL,
            "stderr": subprocess.DEVNULL,
            "close_fds": True,
            "start_new_session": True,
        }
        expected_command = (
            [str(self.native_fixture), _NATIVE_FAULT_ARGUMENT]
            if self.scenario.native_fault
            else [_PYTHON, "-I", "-c", _DISPOSABLE]
        )
        if command != expected_command or options != expected:
            self.authority_ok = False
            raise _FixtureFailure()
        if self.target is not None:
            self.authority_ok = False
            raise _FixtureFailure()
        mode = "native-reap-fault" if self.scenario.native_fault else self.scenario.child
        self.target = self.start_fixture(stdout=options["stdout"], mode=mode)
        self.spawned_at = time.monotonic()
        return _TaskChild(self, self.target)

    def cancel_first(self):
        self.first_sent = True
        os.kill(os.getpid(), self.scenario.first)

    def monotonic(self):
        # Inject the real signal at the first task clock read after a successful,
        # registered actual spawn. In the unfixed wrapper this is the deadline
        # assignment outside its cleanup try. No fake clock advances the deadline.
        if self.target is not None and self.scenario.trigger == "clock" and not self.first_sent:
            self.cancel_first()
        return time.monotonic()

    def signal_child(self, kind, pid, number):
        # Signal zero is a read-only existence/permission query. Preserve the
        # kernel's ESRCH/EPERM distinction; neither implies a retired scope here.
        if number == 0 and self.target is not None and pid == self.target.process.pid:
            if self.target.process.returncode is not None:
                self.authority_ok = False
                raise _FixtureFailure()
            function = os.kill if kind == "pid" else os.killpg
            function(pid, number)
            return
        # Reject stale IDs or unrelated sessions BEFORE performing any signal.
        # The unreaped direct child reserves its PID and session identity.
        if (
            self.target is None
            or pid != self.target.process.pid
            or self.target.process.returncode is not None
            or number not in (signal.SIGTERM, signal.SIGKILL)
            or (kind == "pid" and (not self.scenario.native or number != signal.SIGTERM))
            or (kind == "group" and self.scenario.native and number != signal.SIGKILL)
        ):
            self.authority_ok = False
            raise _FixtureFailure()
        self.signal_count += 1
        # Repeat cancellation inside retirement, before the actual TERM/KILL.
        # The child is still owned and running when the second signal arrives.
        if self.scenario.second is not None and not self.second_sent:
            self.second_sent = True
            os.kill(os.getpid(), self.scenario.second)
        function = os.kill if kind == "pid" else os.killpg
        function(pid, number)

    def compiler_group_absent(self, process):
        """Observe real EOF for this probe's fully known fixed compiler scope.

        Unlike a production compiler, _DISPOSABLE never forks. The group fixture
        forks exactly one nonforking helper which alone retains this endpoint.
        Both complete sources are above. No boolean supplied by a caller, stale
        PID lookup, permission exception, or synthesized wait status proves this
        retirement. EOF also remains valid after the fixture leader was reaped.
        This private OS seam does not assert macOS can discover an arbitrary
        compiler group's absence; production must report uncertainty if it cannot.
        """
        if (
            not isinstance(process, _TaskChild)
            or process.owner is not self
            or process.child is not self.target
            or self.scenario.native
            or self.target.retirement is None
        ):
            self.authority_ok = False
            raise _FixtureFailure()
        return self.target.helper_retired()

    def read(self, fd, size):
        if (
            self.target is None
            or self.target.process.stdout is None
            or fd != self.target.process.stdout.fileno()
            or not 0 < size <= 4096
        ):
            self.bounds_ok = False
            raise _FixtureFailure()
        return os.read(fd, size)

    def retire_fixtures(self):
        """Retire by pipe first; signal only a still-owned, unreaped direct PID."""
        succeeded = True
        previous_mask = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGTERM, signal.SIGINT})
        try:
            for child in reversed(self.children):
                try:
                    child.close_lease()
                    process = child.process
                    try:
                        process.wait(timeout=0.25)
                    except subprocess.TimeoutExpired:
                        if process.returncode is None:
                            try:
                                os.kill(process.pid, signal.SIGKILL)
                            except ProcessLookupError:
                                pass
                        process.wait(timeout=1.0)
                    # The compiler helper may now be an orphan, but remains
                    # lease/deadline bounded. Never send its released group ID a
                    # fallback signal. Its sole retirement handle closes at death.
                    if not child.fixture_retired(timeout=2.0):
                        succeeded = False
                    if process.stdout is not None:
                        process.stdout.close()
                except (OSError, subprocess.TimeoutExpired):
                    succeeded = False
                finally:
                    for descriptor in (child.retirement, child.audit):
                        if descriptor is not None:
                            os.close(descriptor)
            os.close(self.outer_lease)
        finally:
            signal.pthread_sigmask(signal.SIG_SETMASK, previous_mask)
        return succeeded


def _run_probe(name, native_fixture=None):
    scenario = _SCENARIOS[name]
    probe = _Probe(scenario, native_fixture)
    task_timeout = _NATIVE_TASK_TIMEOUT if scenario.native_fault else _TASK_TIMEOUT
    stop_timeout = _NATIVE_STOP_TIMEOUT if scenario.native_fault else _STOP_TIMEOUT
    report: dict[str, bool | int | None] = dict.fromkeys(
        (
            "fixture_ok",
            "output_ok",
            "handlers_restored",
            "owned_reaped",
            "unrelated_alive",
            "authority_ok",
            "bounds_ok",
            "pipe_closed",
            "injection_ok",
            "deadline_ok",
            "cleanup_ok",
            "scope_result_ok",
            "native_fault_ok",
        ),
        False,
    )
    report["code"] = None
    captured = _CappedText()
    errors = _CappedText()
    previous_term = signal.getsignal(signal.SIGTERM)
    previous_int = signal.getsignal(signal.SIGINT)
    previous_alarm = signal.getsignal(signal.SIGALRM)

    def prior_term(_number, _frame):
        return None

    def expired(_number, _frame):
        raise _FixtureFailure()

    try:
        sentinel = probe.start_fixture(stdout=subprocess.DEVNULL, mode="hold")
        signal.signal(signal.SIGTERM, prior_term)
        signal.signal(signal.SIGINT, signal.default_int_handler)
        signal.signal(signal.SIGALRM, expired)
        with redirect_stdout(captured), redirect_stderr(errors):
            spec = importlib.util.spec_from_file_location("_handoff_task_under_test", _TASK)
            if spec is None or spec.loader is None:
                raise _FixtureFailure()
            task = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(task)

            def exercise(*, execute=False, sanitizers=False):
                if execute or sanitizers:
                    raise _FixtureFailure()
                command = (
                    [str(native_fixture), _NATIVE_FAULT_ARGUMENT]
                    if scenario.native_fault
                    else [_PYTHON, "-I", "-c", _DISPOSABLE]
                )
                returncode, _ = task.bounded_process(
                    command,
                    {},
                    timeout=task_timeout,
                    native=scenario.native,
                )
                return returncode

            task_os = _ModuleProxy(
                os,
                kill=lambda pid, number: probe.signal_child("pid", pid, number),
                killpg=lambda pid, number: probe.signal_child("group", pid, number),
                read=probe.read,
            )
            # Only the original nonforking compiler fixtures use their private
            # EOF witness. The new forked-helper regressions keep the production
            # killpg(0) observer, including Darwin's real permission semantics.
            scope_observer = (
                task._compiler_group_absent
                if scenario.child in {"compiler-helper-ignore", "compiler-helper-delay"}
                else probe.compiler_group_absent
            )
            with (
                patch.object(task, "subprocess", _ModuleProxy(subprocess, Popen=probe.spawn)),
                patch.object(task, "os", task_os),
                patch.object(
                    task, "time", SimpleNamespace(monotonic=probe.monotonic, sleep=time.sleep)
                ),
                patch.object(task, "_compiler_group_absent", scope_observer),
                patch.object(task, "run_task", exercise),
                patch.object(task, "STOP_TIMEOUT", stop_timeout),
                patch.object(task, "REAP_TIMEOUT", _REAP_TIMEOUT),
                patch.object(sys, "argv", [str(_TASK)]),
            ):
                signal.setitimer(signal.ITIMER_REAL, _PROBE_TIMEOUT)
                try:
                    report["code"] = task.entry()
                finally:
                    signal.setitimer(signal.ITIMER_REAL, 0)
        report["fixture_ok"] = True
        expected_output = (
            json.dumps(
                {
                    "schema": 1,
                    "scope": "handoff-wire-candidate",
                    "case": "driver",
                    "result": scenario.classification,
                },
                separators=(",", ":"),
            )
            + "\n"
        )
        report["output_ok"] = (
            not captured.overflow
            and not errors.overflow
            and captured.getvalue() == expected_output
            and errors.getvalue() == ""
        )
        report["handlers_restored"] = (
            signal.getsignal(signal.SIGTERM) is prior_term
            and signal.getsignal(signal.SIGINT) is signal.default_int_handler
        )
        if probe.target is not None:
            process = probe.target.process
            # Observe BEFORE fallback cleanup. Never poll/reap a leak into a pass.
            report["owned_reaped"] = process.returncode is not None
            if report["owned_reaped"]:
                try:
                    os.waitpid(process.pid, os.WNOHANG)
                except ChildProcessError:
                    pass
                else:
                    report["owned_reaped"] = False
            report["pipe_closed"] = process.stdout is None or process.stdout.closed
            # Observe the whole fixed compiler fixture before closing its lease.
            # A reaped leader is not evidence that its helper retired. Do not
            # interpret EPERM or getpgid(leader)'s ESRCH as group retirement.
            report["scope_result_ok"] = probe.target.helper_retired() or (
                report["code"] == 2 and report["output_ok"]
            )
            report["native_fault_ok"] = not scenario.native_fault
            if scenario.native_fault and process.returncode is not None:
                readable, _, _ = select.select([probe.target.audit], [], [], 0)
                report["native_fault_ok"] = bool(readable) and (
                    os.read(probe.target.audit, 3) == b"ur"
                )
        report["unrelated_alive"] = sentinel.process.poll() is None
        report["authority_ok"] = probe.authority_ok and (
            name != "reaped-term" or probe.signal_count == 0
        )
        report["bounds_ok"] = probe.bounds_ok
        report["injection_ok"] = probe.first_sent == (
            scenario.first is not None
        ) and probe.second_sent == (scenario.second is not None)
        report["deadline_ok"] = probe.spawned_at is not None and (
            time.monotonic() - probe.spawned_at < task_timeout + stop_timeout + _REAP_TIMEOUT + 0.75
        )
    except BaseException:
        # Never relay exception strings, argv, paths, environment, or child text.
        report["fixture_ok"] = False
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        report["cleanup_ok"] = probe.retire_fixtures()
        signal.signal(signal.SIGTERM, previous_term)
        signal.signal(signal.SIGINT, previous_int)
        signal.signal(signal.SIGALRM, previous_alarm)
    print(json.dumps(report, separators=(",", ":")), flush=True)


_FAULT_WIRING_SCENARIOS = {
    "fault-pass",
    "fault-pass-asan",
    "fault-skipped",
    "fault-count-mismatch",
    "fault-cleanup-unresolved",
}


def _run_build_resource_probe(classification, report_fd=None):
    """Exercise the real build owner with a controlled compiler/suite boundary.

    No child is started here. The cancellation probes separately establish real
    helper/inner-case uncertainty. This probe tests whether run_task retains or
    removes its owned artifacts after that classification, without a finalizer
    deleting unresolved resources. The enclosing fixture may remove its tree
    safely because this boundary never created any process.
    """
    report: dict[str, bool | int | None] = dict.fromkeys(
        ("fixture_ok", "output_ok", "retention_ok", "handlers_restored", "boundary_ok"), False
    )
    report["code"] = None
    captured = _CappedText()
    errors = _CappedText()
    previous_term = signal.getsignal(signal.SIGTERM)
    previous_int = signal.getsignal(signal.SIGINT)
    faults = classification.startswith("fault-")
    sanitizers = classification == "fault-pass-asan"
    try:
        with tempfile.TemporaryDirectory(
            prefix="spaceterm-handoff-owner-", dir="/private/tmp"
        ) as root:
            created = []
            with redirect_stdout(captured), redirect_stderr(errors):
                spec = importlib.util.spec_from_file_location(
                    "_handoff_build_owner_under_test", _TASK
                )
                if spec is None or spec.loader is None:
                    raise _FixtureFailure()
                task = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(task)

                def build_directory(*, prefix, dir):
                    if prefix != "spaceterm-handoff-" or dir != "/private/tmp" or created:
                        raise _FixtureFailure()
                    directory = tempfile.mkdtemp(prefix=prefix, dir=root)
                    created.append(Path(directory))
                    return directory

                def compile_boundary(command, environment, *, timeout, native=False):
                    if len(created) != 1:
                        raise _FixtureFailure()
                    build = created[0]
                    sources = _TASK.parents[4] / "tests/server_native/macos"
                    expected_command = [
                        "/usr/bin/xcrun",
                        "--sdk",
                        "macosx",
                        "clang",
                        "-std=c11",
                        "-arch",
                        "arm64",
                        "-mmacosx-version-min=26.0",
                        "-Wall",
                        "-Wextra",
                        "-Werror",
                        "-Wpedantic",
                    ]
                    if faults:
                        expected_command.append("-DST_HANDOFF_PRIVATE_REAP_FAULT")
                    expected_command.extend(
                        ["-g", "-O1", "-fsanitize=address,undefined", "-fno-omit-frame-pointer"]
                        if sanitizers
                        else ["-O0"]
                    )
                    expected_command.extend(
                        [
                            str(sources / "handoff.c"),
                            str(sources / "handoff_tests.c"),
                            "-o",
                            str(build / "handoff-tests"),
                        ]
                    )
                    expected_environment = {
                        "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
                        "LC_ALL": "C",
                        "TMPDIR": str(build),
                    }
                    if sanitizers:
                        expected_environment.update(
                            ASAN_OPTIONS="abort_on_error=1:disable_coredump=1",
                            UBSAN_OPTIONS="halt_on_error=1:print_stacktrace=0",
                        )
                    if (
                        command != expected_command
                        or environment != expected_environment
                        or timeout != 60.0
                        or native
                    ):
                        raise _FixtureFailure()
                    (build / "owned-artifact").write_bytes(b"fixture")
                    report["boundary_ok"] = not faults
                    if faults:
                        binary = build / "handoff-tests"
                        binary.write_bytes(b"fixed nonexecuted compile-boundary fixture")
                        binary.chmod(0o700)
                        return 0, b""
                    raise task.TaskFailure(classification)

                def fault_suite_boundary(binary, checkpoint):
                    if (
                        not faults
                        or len(created) != 1
                        or binary != created[0] / "handoff-tests"
                        or not binary.is_file()
                    ):
                        raise _FixtureFailure()
                    checkpoint()
                    report["boundary_ok"] = True
                    groups = (
                        ("fault-cancellation-14", 14),
                        ("fault-build-resources-3", 3),
                        ("fault-native-reap-7", 7),
                        ("fault-readerless-output-4", 4),
                        ("fault-inherited-sigchld-1", 1),
                    )
                    reports = [
                        {
                            "case": name,
                            "tests": count,
                            "skipped": 0,
                            "failures": 0,
                            "errors": 0,
                            "expected_failures": 0,
                            "unexpected_successes": 0,
                        }
                        for name, count in groups
                    ]
                    if classification == "fault-skipped":
                        reports[2]["skipped"] = 7
                    elif classification == "fault-count-mismatch":
                        reports[2]["tests"] = 6
                    return reports

                arguments = [str(_TASK)]
                if faults:
                    arguments.append("--faults")
                if sanitizers:
                    arguments.append("--sanitizers")
                fault_context = (
                    patch.object(
                        task,
                        "_load_native_fault_suite",
                        return_value=SimpleNamespace(
                            __name__="_handoff_fixed_suite_boundary",
                            run_native_fault_suite=fault_suite_boundary,
                            native_fault_resources_retired=lambda: (
                                classification != "fault-cleanup-unresolved"
                            ),
                        ),
                    )
                    if faults
                    else nullcontext()
                )
                with (
                    patch.object(task, "tempfile", _ModuleProxy(tempfile, mkdtemp=build_directory)),
                    patch.object(task, "bounded_process", compile_boundary),
                    patch.object(sys, "argv", arguments),
                    fault_context,
                ):
                    report["code"] = task.entry()
            retained = classification in {"cleanup-unresolved", "fault-cleanup-unresolved"}
            report["retention_ok"] = len(created) == 1 and (
                created[0].exists() == retained
                and (not retained or (created[0] / "owned-artifact").is_file())
            )
            records = [("driver", classification)]
            if faults:
                records = [("build", "source-compiled")]
                native_failed = classification in {"fault-skipped", "fault-count-mismatch"}
                if classification != "fault-cleanup-unresolved":
                    records.extend(
                        (name, "fixture-error" if index == 2 and native_failed else "pass")
                        for index, name in enumerate(
                            (
                                "fault-cancellation-14",
                                "fault-build-resources-3",
                                "fault-native-reap-7",
                                "fault-readerless-output-4",
                                "fault-inherited-sigchld-1",
                            )
                        )
                    )
                outcome = (
                    "cleanup-unresolved"
                    if classification == "fault-cleanup-unresolved"
                    else "fixture-error"
                    if classification in {"fault-skipped", "fault-count-mismatch"}
                    else "fault-green-native-gated"
                )
                records.append(("driver", outcome))
            expected_output = "".join(
                json.dumps(
                    {
                        "schema": 1,
                        "scope": "handoff-wire-candidate",
                        "case": case,
                        "result": result,
                    },
                    separators=(",", ":"),
                )
                + "\n"
                for case, result in records
            )
            report["output_ok"] = (
                not captured.overflow
                and not errors.overflow
                and captured.getvalue() == expected_output
                and errors.getvalue() == ""
            )
            report["handlers_restored"] = (
                signal.getsignal(signal.SIGTERM) is previous_term
                and signal.getsignal(signal.SIGINT) is previous_int
            )
            report["fixture_ok"] = True
    except BaseException:
        report["fixture_ok"] = False
    finally:
        signal.signal(signal.SIGTERM, previous_term)
        signal.signal(signal.SIGINT, previous_int)
    if report_fd is None:
        print(json.dumps(report, separators=(",", ":")), flush=True)
    else:
        _private_report(report_fd, report)
    return report["code"] if type(report["code"]) is int else 2


# These probes enter the real build owner or entry with an OS stdout pipe. The
# existing cancellation probes and their assertions above are unchanged.
_INTERFACE_SCENARIOS = {
    "compiler-leader-killed": (
        "compiler",
        _Scenario(child="compiler-helper-ignore", code=2, classification="cleanup-unresolved"),
    ),
    "native-leader-killed": (
        "native",
        _Scenario(native=True, native_fault=True, code=2, classification="cleanup-unresolved"),
    ),
    "native-leader-killed-reaped-term": (
        "native",
        _Scenario(
            trigger="reaped",
            first=signal.SIGTERM,
            native=True,
            native_fault=True,
            code=2,
            classification="cleanup-unresolved",
        ),
    ),
    "native-leader-killed-reaped-int": (
        "native",
        _Scenario(
            trigger="reaped",
            first=signal.SIGINT,
            native=True,
            native_fault=True,
            code=2,
            classification="cleanup-unresolved",
        ),
    ),
    "readerless-final-term": ("interruption", _Scenario(trigger="wait", first=signal.SIGTERM)),
    "readerless-final-int": (
        "interruption",
        _Scenario(trigger="wait", first=signal.SIGINT, code=130),
    ),
    "readerless-arguments": ("arguments", _Scenario(code=2)),
    "readerless-build-output": ("build-output", _Scenario(code=2)),
    "readerless-flush-term": ("interruption", _Scenario(trigger="wait", first=signal.SIGTERM)),
    "readerless-flush-int": (
        "interruption",
        _Scenario(trigger="wait", first=signal.SIGINT, code=130),
    ),
    "readerless-flush-arguments": ("arguments", _Scenario(code=2)),
    "readerless-flush-build-output": ("build-output", _Scenario(code=2)),
}
_INTERFACE_FLAGS = {
    "fixture_ok",
    "boundary_ok",
    "handlers_restored",
    "owned_reaped",
    "pipe_closed",
    "unrelated_alive",
    "authority_ok",
    "bounds_ok",
    "injection_ok",
    "deadline_ok",
    "cleanup_ok",
    "leader_killed",
    "scope_alive",
    "retention_ok",
    "output_ok",
}


def _load_task(name):
    spec = importlib.util.spec_from_file_location(name, _TASK)
    if spec is None or spec.loader is None:
        raise _FixtureFailure()
    task = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(task)
    return task


def _private_report(descriptor, report):
    # The report is independent of task stdout and is not inherited by fixtures.
    # Never block on an abandoned reader or relay an exception/output string.
    try:
        data = json.dumps(report, separators=(",", ":")).encode("ascii")
        if len(data) > _REPORT_LIMIT:
            return
        os.set_blocking(descriptor, False)
        os.write(descriptor, data)
    except OSError:
        pass
    finally:
        os.close(descriptor)


class _DeferredFlushText(io.TextIOWrapper):
    """Let one print finish buffering; fail its later flush on the real OS pipe."""

    def __init__(self, descriptor):
        super().__init__(
            io.BufferedWriter(io.FileIO(descriptor, "wb", closefd=False)), encoding="ascii"
        )
        self.write_completed = False
        self.print_flush_deferred = False
        self.readerless_flush_observed = False

    def write(self, text):
        written = super().write(text)
        if text and written == len(text):
            self.write_completed = True
        return written

    def flush(self):
        if self.write_completed and not self.print_flush_deferred:
            self.print_flush_deferred = True
            return
        try:
            super().flush()
        except BrokenPipeError:
            self.readerless_flush_observed = True
            raise


class _ConsumingChild(_TaskChild):
    def wait(self, timeout=None):
        native = self.child is self.owner.target and self.owner.scenario.native
        maximum = _NATIVE_TASK_TIMEOUT if native else _TASK_TIMEOUT
        if timeout is None or not math.isfinite(timeout) or not 0 <= timeout <= maximum + 0.05:
            self.owner.bounds_ok = False
            raise _FixtureFailure()
        result = self.child.process.wait(timeout=timeout)
        if (
            self.child is self.owner.target
            and self.owner.scenario.trigger == "reaped"
            and not self.owner.first_sent
        ):
            # The real wait has consumed -SIGKILL. Only now cancel the wrapper,
            # without touching the supervisor's released PID or its case group.
            self.owner.cancel_first()
        return result


class _BuildProbe(_Probe):
    """Replace fixed compiler/native spawn boundaries, never build ownership."""

    def __init__(self, stage, scenario, native_fixture):
        super().__init__(scenario, native_fixture)
        self.stage = stage
        self.build: Path | None = None
        self.compile_started = False
        self.native_started = False
        self.kill_sent = False
        self.boundary_ok = False

    def spawn(self, command, **options):
        if self.build is None:
            raise _FixtureFailure()
        native = self.compile_started
        sources = _TASK.parents[4] / "tests/server_native/macos"
        expected_command = (
            [str(self.build / "handoff-tests")]
            if native
            else [
                "/usr/bin/xcrun",
                "--sdk",
                "macosx",
                "clang",
                "-std=c11",
                "-arch",
                "arm64",
                "-mmacosx-version-min=26.0",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-Wpedantic",
                "-O0",
                str(sources / "handoff.c"),
                str(sources / "handoff_tests.c"),
                "-o",
                str(self.build / "handoff-tests"),
            ]
        )
        expected_options = {
            "cwd": _TASK.parents[4],
            "env": {
                "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
                "LC_ALL": "C",
                "TMPDIR": str(self.build),
            },
            "stdin": subprocess.DEVNULL,
            "stdout": subprocess.PIPE if native else subprocess.DEVNULL,
            "stderr": subprocess.DEVNULL,
            "close_fds": True,
            "start_new_session": True,
        }
        if (
            command != expected_command
            or options != expected_options
            or (native and (self.stage != "native" or self.native_started))
        ):
            self.authority_ok = False
            raise _FixtureFailure()
        if native:
            self.native_started = True
            mode = "native-reap-fault"
        else:
            self.compile_started = True
            (self.build / "owned-artifact").write_bytes(b"fixture")
            mode = "compiler-helper-ignore" if self.stage == "compiler" else "hold"
        child = self.start_fixture(stdout=options["stdout"], mode=mode)
        killed = native or self.stage == "compiler"
        if killed or self.stage == "build-output":
            self.target = child
            self.spawned_at = time.monotonic()
        if killed:
            # start_fixture captured readiness from the actual helper/case and
            # verified this unreaped direct child's new session/group. Kill only
            # that exact leader, before any consuming wait, not its descendants.
            if child.process.returncode is not None:
                raise _FixtureFailure()
            os.kill(child.process.pid, signal.SIGKILL)
            self.kill_sent = True
        else:
            # A real, fixed, nonforking successful compiler boundary. Closing
            # its lease makes it exit zero; its wait is still the task's wait.
            child.close_lease()
        self.boundary_ok = True
        return _ConsumingChild(self, child)


def _run_interface_probe(name, report_fd, native_fixture=None):
    stage, scenario = _INTERFACE_SCENARIOS[name]
    readerless = name.startswith("readerless-")
    probe = (
        _Probe(scenario)
        if stage in {"interruption", "arguments"}
        else _BuildProbe(stage, scenario, native_fixture)
    )
    report: dict[str, bool | int | None] = dict.fromkeys(_INTERFACE_FLAGS, False)
    report["code"] = None
    captured, errors = _CappedText(), _CappedText()
    previous = {
        number: signal.getsignal(number)
        for number in (
            signal.SIGTERM,
            signal.SIGINT,
            signal.SIGALRM,
        )
    }
    root = None
    sentinel = None
    deferred_output = None

    def prior_term(_number, _frame):
        return None

    def expired(_number, _frame):
        raise _FixtureFailure()

    try:
        sentinel = probe.start_fixture(stdout=subprocess.DEVNULL, mode="hold")
        signal.signal(signal.SIGTERM, prior_term)
        signal.signal(signal.SIGINT, signal.default_int_handler)
        signal.signal(signal.SIGALRM, expired)
        output_context = nullcontext() if readerless else redirect_stdout(captured)
        error_context = nullcontext() if readerless else redirect_stderr(errors)
        with output_context, error_context:
            task = _load_task("_handoff_public_interface_under_test")
            if name.startswith("readerless-flush-"):
                # Preserve the actual readerless descriptor through interpreter
                # shutdown. Only print's first flush is deferred; the later
                # stream flush performs a real OS write and must fail with EPIPE.
                deferred_output = _DeferredFlushText(sys.stdout.fileno())
                sys.stdout = deferred_output
            if stage not in {"interruption", "arguments"}:
                # No context manager/finalizer may delete live workers' files.
                root = Path(tempfile.mkdtemp(prefix="spaceterm-handoff-red-", dir="/private/tmp"))

            def build_directory(*, prefix, dir):
                if not isinstance(probe, _BuildProbe):
                    raise _FixtureFailure()
                if (
                    prefix != "spaceterm-handoff-"
                    or dir != "/private/tmp"
                    or probe.build is not None
                ):
                    raise _FixtureFailure()
                probe.build = Path(tempfile.mkdtemp(prefix=prefix, dir=root))
                return str(probe.build)

            def exercise(*, execute=False, sanitizers=False):
                if execute or sanitizers:
                    raise _FixtureFailure()
                returncode, _ = task.bounded_process(
                    [_PYTHON, "-I", "-c", _DISPOSABLE],
                    {},
                    timeout=_TASK_TIMEOUT,
                )
                return returncode

            task_os = _ModuleProxy(
                os,
                kill=lambda pid, number: probe.signal_child("pid", pid, number),
                killpg=lambda pid, number: probe.signal_child("group", pid, number),
                read=probe.read,
            )
            arguments = [str(_TASK)]
            if stage == "native":
                arguments.append("--run")
            elif stage == "arguments":
                arguments.append("--invalid-fixed-argument")
            # Only the original, source-complete nonforking interruption fixture
            # gets an EOF observer. Forked compiler/native death keeps production
            # scope logic unchanged, and never gains a general group proof.
            owner_context = nullcontext()
            if stage == "interruption":
                owner_context = patch.object(task, "run_task", exercise)
            elif root is not None:
                owner_context = patch.object(
                    task, "tempfile", _ModuleProxy(tempfile, mkdtemp=build_directory)
                )
            observer_context = (
                patch.object(task, "_compiler_group_absent", probe.compiler_group_absent)
                if stage == "interruption"
                else nullcontext()
            )
            with (
                patch.object(task, "subprocess", _ModuleProxy(subprocess, Popen=probe.spawn)),
                patch.object(task, "os", task_os),
                patch.object(task, "BUILD_TIMEOUT", _TASK_TIMEOUT),
                patch.object(task, "DRIVER_TIMEOUT", _NATIVE_TASK_TIMEOUT),
                patch.object(
                    task, "STOP_TIMEOUT", _NATIVE_STOP_TIMEOUT if scenario.native else _STOP_TIMEOUT
                ),
                patch.object(task, "REAP_TIMEOUT", _REAP_TIMEOUT),
                patch.object(sys, "argv", arguments),
                owner_context,
                observer_context,
            ):
                report["fixture_ok"] = True
                signal.setitimer(signal.ITIMER_REAL, _PROBE_TIMEOUT)
                try:
                    report["code"] = task.entry()
                finally:
                    signal.setitimer(signal.ITIMER_REAL, 0)
    except _FixtureFailure:
        report["fixture_ok"] = False
    except BaseException:
        if readerless:
            # Do not sanitize the task's escaped output error in the fixture.
            # Its actual traceback and interpreter-shutdown flush reach the
            # outer bounded stderr pipe, never the private observation channel.
            raise
        report["fixture_ok"] = False
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        report["handlers_restored"] = (
            signal.getsignal(signal.SIGTERM) is prior_term
            and signal.getsignal(signal.SIGINT) is signal.default_int_handler
        )
        report["authority_ok"] = probe.authority_ok and (
            stage not in {"compiler", "native"} or probe.signal_count == 0
        )
        report["bounds_ok"] = probe.bounds_ok
        report["injection_ok"] = probe.first_sent == (scenario.first is not None)
        report["unrelated_alive"] = sentinel is not None and sentinel.process.poll() is None
        report["boundary_ok"] = (
            stage in {"interruption", "arguments"}
            or (isinstance(probe, _BuildProbe) and probe.boundary_ok)
        ) and (
            deferred_output is None
            or (
                deferred_output.write_completed
                and deferred_output.print_flush_deferred
                and deferred_output.readerless_flush_observed
            )
        )
        report["owned_reaped"] = stage == "arguments"
        report["pipe_closed"] = stage == "arguments"
        report["leader_killed"] = stage not in {"compiler", "native"}
        report["deadline_ok"] = stage == "arguments"
        report["retention_ok"] = stage in {"interruption", "arguments"}
        if probe.target is not None:
            process = probe.target.process
            report["owned_reaped"] = process.returncode is not None
            report["pipe_closed"] = process.stdout is None or process.stdout.closed
            report["leader_killed"] = stage not in {"compiler", "native"} or (
                isinstance(probe, _BuildProbe)
                and probe.kill_sent
                and process.returncode == -signal.SIGKILL
            )
            # This private EOF is observation only, never a production cleanup
            # seam. After leader reap, the sole known helper/case still holds it.
            report["scope_alive"] = not probe.target.fixture_retired(timeout=0.01)
            task_timeout = _NATIVE_TASK_TIMEOUT if scenario.native else _TASK_TIMEOUT
            stop_timeout = _NATIVE_STOP_TIMEOUT if scenario.native else _STOP_TIMEOUT
            report["deadline_ok"] = probe.spawned_at is not None and (
                time.monotonic() - probe.spawned_at
                < task_timeout + stop_timeout + _REAP_TIMEOUT + 0.75
            )
        if isinstance(probe, _BuildProbe) and probe.build is not None:
            report["retention_ok"] = (
                (probe.build / "owned-artifact").is_file()
                if stage in {"compiler", "native"}
                else not probe.build.exists()
            )
        if not readerless:
            records = [("build", "source-compiled")] if stage == "native" else []
            records.append(("driver", "cleanup-unresolved"))
            expected = "".join(
                json.dumps(
                    {
                        "schema": 1,
                        "scope": "handoff-wire-candidate",
                        "case": case,
                        "result": result,
                    },
                    separators=(",", ":"),
                )
                + "\n"
                for case, result in records
            )
            report["output_ok"] = (
                captured.getvalue() == expected
                and errors.getvalue() == ""
                and not captured.overflow
                and not errors.overflow
            )
        else:
            report["output_ok"] = True  # The outer OS pipe observes output failure.
        report["cleanup_ok"] = probe.retire_fixtures()
        if root is not None and report["cleanup_ok"]:
            try:
                shutil.rmtree(root)
            except OSError:
                report["cleanup_ok"] = False
        for number, handler in previous.items():
            signal.signal(number, handler)
        _private_report(report_fd, report)
    return report["code"] if type(report["code"]) is int else 2


def _run_sigchld_observation(report_fd):
    # SIGCHLD was installed by the launcher before DIRECT exec. Do not reinstall
    # it here: on Darwin that would change P_NOCLDWAIT and answer another question.
    report = {
        "fixture_ok": False,
        "isolated": bool(sys.flags.isolated),
        "cpython": sys.implementation.name == "cpython",
        "python_major": sys.version_info.major,
        "python_minor": sys.version_info.minor,
        "inherited_ignore_before": signal.getsignal(signal.SIGCHLD) == signal.SIG_IGN,
        "inherited_ignore_after": False,
        "returncode_unset_after_spawn": False,
        "no_waitable_status_after_return": False,
        "no_lifecycle_signals": True,
        "cleanup_ok": False,
        "returncode": None,
    }
    children = []
    command = [_PYTHON, "-I", "-c", "import os; os._exit(7)"]

    def spawn(actual_command, **options):
        expected = {
            "cwd": _TASK.parents[4],
            "env": {},
            "stdin": subprocess.DEVNULL,
            "stdout": subprocess.DEVNULL,
            "stderr": subprocess.DEVNULL,
            "close_fds": True,
            "start_new_session": True,
        }
        if actual_command != command or options != expected or children:
            raise _FixtureFailure()
        process = subprocess.Popen(actual_command, **options)
        children.append(process)
        report["returncode_unset_after_spawn"] = process.returncode is None
        return process

    def reject_signal(_pid, _number):
        # No existence query or control is allowed under the disputed policy.
        report["no_lifecycle_signals"] = False
        raise _FixtureFailure()

    try:
        task = _load_task("_handoff_inherited_sigchld_observation")
        with (
            patch.object(task, "subprocess", _ModuleProxy(subprocess, Popen=spawn)),
            patch.object(task, "os", _ModuleProxy(os, kill=reject_signal, killpg=reject_signal)),
        ):
            report["returncode"], output = task.bounded_process(command, {}, timeout=2.0)
        if len(children) != 1 or output != b"":
            raise _FixtureFailure()
        try:
            os.waitpid(children[0].pid, os.WNOHANG)
        except ChildProcessError:
            report["no_waitable_status_after_return"] = children[0].returncode is not None
        report["fixture_ok"] = True
    except BaseException:
        report["fixture_ok"] = False
    finally:
        report["inherited_ignore_after"] = signal.getsignal(signal.SIGCHLD) == signal.SIG_IGN
        # The complete child source only exits 7. Wait finitely, never send a
        # possibly auto-reaped ID a signal, and never assert a substituted zero.
        try:
            for child in children:
                child.wait(timeout=2.0)
            report["cleanup_ok"] = True
        except (OSError, subprocess.TimeoutExpired):
            pass
        _private_report(report_fd, report)


def _private_probe(name, *, native_fixture=None):
    retirement = None
    if native_fixture is not None:
        retirement = len(_NATIVE_FAULT_RETIREMENTS)
        _NATIVE_FAULT_RETIREMENTS.append(False)
    report_read, report_write = os.pipe()
    broken_write = None
    process = None
    if name.startswith("readerless-"):
        broken_read, broken_write = os.pipe()
        os.close(broken_read)  # No OS reader exists, even at the first emit.
    command = [_PYTHON, "-I", str(Path(__file__).resolve())]
    if name == "sigchld-observation":
        command.extend(["--sigchld-launch", str(report_write)])
    elif name in _FAULT_WIRING_SCENARIOS:
        command.extend(["--fault-wiring-probe", name, str(report_write)])
    else:
        command.extend(["--interface-probe", name, str(report_write)])
        if native_fixture is not None:
            command.append(str(native_fixture))
    try:
        try:
            process = subprocess.Popen(
                command,
                cwd=_TASK.parents[4],
                env={},
                stdin=subprocess.PIPE,
                stdout=broken_write if broken_write is not None else subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                close_fds=True,
                start_new_session=True,
                pass_fds=(report_write,),
            )
        except OSError:
            raise AssertionError("isolated public-interface probe could not start") from None
        finally:
            os.close(report_write)
            if broken_write is not None:
                os.close(broken_write)
        if process.stderr is None:
            raise AssertionError("isolated public-interface probe has no error channel")
        channels = {report_read: bytearray(), process.stderr.fileno(): bytearray()}
        open_channels = set(channels)
        deadline = time.monotonic() + 20.0
        while open_channels:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise AssertionError("isolated public-interface probe exceeded its bound")
            readable, _, _ = select.select(list(open_channels), [], [], remaining)
            for descriptor in readable:
                block = os.read(
                    descriptor, min(4096, _REPORT_LIMIT + 1 - len(channels[descriptor]))
                )
                if not block:
                    open_channels.remove(descriptor)
                else:
                    channels[descriptor].extend(block)
                    if len(channels[descriptor]) > _REPORT_LIMIT:
                        raise AssertionError("isolated public-interface channel exceeded its bound")
        process.wait(timeout=max(0, deadline - time.monotonic()))
        data = bytes(channels[report_read])
        stderr_empty = not channels[process.stderr.fileno()]
    except (OSError, subprocess.TimeoutExpired):
        raise AssertionError("isolated public-interface probe exceeded its bound") from None
    finally:
        cleanup_ok = process is None or _retire_probe(process)
        if process is not None and process.stderr is not None:
            process.stderr.close()
        os.close(report_read)
    if not cleanup_ok:
        raise AssertionError("isolated public-interface probe fallback cleanup failed")
    try:
        report = json.loads(data)
    except (UnicodeError, ValueError):
        raise AssertionError("isolated public-interface probe returned an invalid report") from None
    if retirement is not None and _valid_interface_report(report) and report["cleanup_ok"]:
        _NATIVE_FAULT_RETIREMENTS[retirement] = True
    return report, process.returncode, stderr_empty


def _valid_interface_report(report):
    return (
        isinstance(report, dict)
        and set(report) == _INTERFACE_FLAGS | {"code"}
        and all(type(report[key]) is bool for key in _INTERFACE_FLAGS)
        and (report["code"] is None or type(report["code"]) is int)
    )


def _assert_interface_safety(test, report):
    test.assertTrue(
        _valid_interface_report(report),
        "isolated public-interface probe returned an invalid report",
    )
    for key in (
        "fixture_ok",
        "boundary_ok",
        "cleanup_ok",
        "handlers_restored",
        "owned_reaped",
        "pipe_closed",
        "unrelated_alive",
        "authority_ok",
        "bounds_ok",
        "injection_ok",
        "deadline_ok",
        "leader_killed",
    ):
        test.assertTrue(report[key], "public-interface fixture failed: " + key)


def _assert_leader_death(test, name, *, native_fixture=None):
    report, code, stderr_empty = _private_probe(name, native_fixture=native_fixture)
    _assert_interface_safety(test, report)
    test.assertTrue(stderr_empty, "leader death must not expose paths or native errors")
    test.assertTrue(report["scope_alive"], "the real helper/case must remain alive before fallback")
    test.assertEqual(code, 2, "cleanup uncertainty must override ordinary failure and cancellation")
    test.assertEqual(report["code"], 2)
    test.assertTrue(report["output_ok"], "leader death must report only cleanup-unresolved")
    test.assertTrue(
        report["retention_ok"], "live helper/case uncertainty must retain build resources"
    )


def _retire_probe(process):
    succeeded = True
    previous_mask = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGTERM, signal.SIGINT})
    try:
        # EOF also retires all fixture children if the probe failed midway.
        if process.stdin is not None:
            process.stdin.close()
        try:
            process.wait(timeout=3.0)
        except subprocess.TimeoutExpired:
            if process.returncode is None:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            process.wait(timeout=3.0)
        if process.stdout is not None:
            process.stdout.close()
    except (OSError, subprocess.TimeoutExpired):
        succeeded = False
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK, previous_mask)
    return succeeded


class _HandoffProbeAssertions(unittest.TestCase):
    def probe(self, name, *, native_fixture=None):
        retirement = None
        if native_fixture is not None:
            retirement = len(_NATIVE_FAULT_RETIREMENTS)
            _NATIVE_FAULT_RETIREMENTS.append(False)
        command = [_PYTHON, "-I", str(Path(__file__).resolve()), "--probe", name]
        if native_fixture is not None:
            command.append(str(native_fixture))
        try:
            process = subprocess.Popen(
                command,
                cwd=_TASK.parents[4],
                env={},
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                close_fds=True,
                start_new_session=True,
            )
        except OSError:
            raise AssertionError("isolated handoff probe could not start") from None
        try:
            try:
                process.wait(timeout=20.0)
                if process.stdout is None:
                    raise AssertionError("isolated handoff probe has no report channel")
                data = process.stdout.read(_REPORT_LIMIT + 1)
            except (OSError, subprocess.TimeoutExpired):
                raise AssertionError("isolated handoff probe exceeded its bound") from None
        finally:
            cleanup_ok = _retire_probe(process)
        self.assertTrue(cleanup_ok, "isolated handoff probe fallback cleanup failed")
        self.assertEqual(process.returncode, 0, "isolated handoff probe failed")
        self.assertTrue(len(data) <= _REPORT_LIMIT, "isolated handoff report exceeded its bound")
        try:
            report = json.loads(data)
        except (UnicodeError, ValueError):
            raise AssertionError("isolated handoff probe returned an invalid report") from None
        flags = {
            "fixture_ok",
            "output_ok",
            "handlers_restored",
            "owned_reaped",
            "unrelated_alive",
            "authority_ok",
            "bounds_ok",
            "pipe_closed",
            "injection_ok",
            "deadline_ok",
            "cleanup_ok",
            "scope_result_ok",
            "native_fault_ok",
        }
        valid = (
            isinstance(report, dict)
            and set(report) == flags | {"code"}
            and all(type(report[key]) is bool for key in flags)
            and (report["code"] is None or type(report["code"]) is int)
        )
        self.assertTrue(valid, "isolated handoff probe returned an invalid report")
        if retirement is not None and report["cleanup_ok"]:
            _NATIVE_FAULT_RETIREMENTS[retirement] = True
        self.assertTrue(report["fixture_ok"], "isolated handoff fixture failed")
        self.assertTrue(report["cleanup_ok"], "disposable handoff fixture cleanup failed")
        self.assertTrue(report["injection_ok"], "requested cancellation was not injected")
        self.assertTrue(
            report["handlers_restored"], "entry must restore the caller's signal handlers"
        )
        self.assertTrue(
            report["unrelated_alive"], "cleanup must leave the unrelated session running"
        )
        self.assertTrue(report["authority_ok"], "cleanup must never signal a stale or foreign ID")
        self.assertTrue(report["bounds_ok"], "child waits and output reads must remain bounded")
        self.assertTrue(
            report["deadline_ok"], "cleanup must preserve the original absolute deadline"
        )
        self.assertTrue(report["pipe_closed"], "native output must close before entry returns")
        self.assertTrue(
            report["scope_result_ok"],
            "entry must report cleanup-unresolved while its compiler helper remains alive",
        )
        self.assertTrue(
            report["native_fault_ok"],
            "native fixture must exhaust the real case reap budget, then reap its case before assertions",
        )
        return report

    def assert_outcome(self, report, code):
        self.assertEqual(
            report["code"], code, "entry must preserve cleanup precedence and the first signal"
        )
        self.assertTrue(
            report["output_ok"], "entry must emit only the fixed failure classification"
        )
        self.assertTrue(
            report["owned_reaped"],
            "entry returned before retiring and reaping its owned child",
        )


@unittest.skipUnless(os.name == "posix", "owned session cancellation requires POSIX")
class HandoffCancellationTests(_HandoffProbeAssertions):
    def test_sigterm_after_spawn_retires_and_reaps_the_owned_compiler_session(self):
        self.assert_outcome(self.probe("spawn-term"), 143)

    def test_sigint_after_spawn_retires_and_reaps_the_owned_compiler_session(self):
        self.assert_outcome(self.probe("spawn-int"), 130)

    def test_repeated_sigterm_during_cleanup_cannot_abandon_the_owned_compiler_session(self):
        self.assert_outcome(self.probe("cleanup-term"), 143)

    def test_sigint_during_cleanup_preserves_the_first_sigterm(self):
        self.assert_outcome(self.probe("cleanup-term-int"), 143)

    def test_sigterm_during_cleanup_preserves_the_first_sigint(self):
        self.assert_outcome(self.probe("cleanup-int-term"), 130)

    def test_repeated_sigterm_during_native_cleanup_reaps_and_closes_output(self):
        self.assert_outcome(self.probe("cleanup-native-term"), 143)

    def test_cancellation_after_reaping_never_signals_the_released_pid_or_session(self):
        self.assert_outcome(self.probe("reaped-term"), 143)

    def test_partial_native_output_does_not_restart_the_absolute_deadline(self):
        self.assert_outcome(self.probe("deadline"), 2)

    def test_native_output_limit_retires_the_child_without_relaying_its_text(self):
        self.assert_outcome(self.probe("output-limit"), 2)

    def test_forced_retirement_is_cleanup_unresolved_even_after_bounded_reaping(self):
        self.assert_outcome(self.probe("escalation"), 2)

    def test_sigterm_is_cleanup_unresolved_when_the_compiler_helper_outlives_its_leader(self):
        self.assert_outcome(self.probe("compiler-helper-term"), 2)

    def test_sigint_is_cleanup_unresolved_when_the_compiler_helper_outlives_its_leader(self):
        self.assert_outcome(self.probe("compiler-helper-int"), 2)

    def test_deadline_is_cleanup_unresolved_when_the_compiler_helper_outlives_its_leader(self):
        self.assert_outcome(self.probe("compiler-helper-deadline"), 2)

    def test_delayed_helper_sigterm_does_not_make_compiler_leader_reaping_scope_retirement(self):
        self.assert_outcome(self.probe("compiler-helper-delayed-term"), 2)


@unittest.skipUnless(sys.platform == "darwin", "macOS build-resource owner requires Darwin")
class HandoffBuildResourceTests(unittest.TestCase):
    def assert_resource_outcome(self, classification):
        command = [
            _PYTHON,
            "-I",
            str(Path(__file__).resolve()),
            "--build-resource-probe",
            classification,
        ]
        try:
            process = subprocess.Popen(
                command,
                cwd=_TASK.parents[4],
                env={},
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                close_fds=True,
                start_new_session=True,
            )
        except OSError:
            raise AssertionError("isolated build-resource probe could not start") from None
        try:
            try:
                process.wait(timeout=_PROBE_TIMEOUT)
                if process.stdout is None:
                    raise AssertionError("isolated build-resource probe has no report channel")
                data = process.stdout.read(_REPORT_LIMIT + 1)
            except (OSError, subprocess.TimeoutExpired):
                raise AssertionError("isolated build-resource probe exceeded its bound") from None
        finally:
            cleanup_ok = _retire_probe(process)
        self.assertTrue(cleanup_ok, "isolated build-resource probe fallback cleanup failed")
        self.assertEqual(process.returncode, 0, "isolated build-resource probe failed")
        self.assertLessEqual(len(data), _REPORT_LIMIT)
        try:
            report = json.loads(data)
        except (UnicodeError, ValueError):
            raise AssertionError(
                "isolated build-resource probe returned an invalid report"
            ) from None
        flags = {"fixture_ok", "output_ok", "retention_ok", "handlers_restored", "boundary_ok"}
        self.assertTrue(
            isinstance(report, dict)
            and set(report) == flags | {"code"}
            and all(type(report[name]) is bool for name in flags)
            and type(report["code"]) is int,
            "isolated build-resource probe returned an invalid report",
        )
        self.assertTrue(report["fixture_ok"], "owned build-resource fixture failed")
        self.assertTrue(report["boundary_ok"], "build command and environment must stay fixed")
        self.assertTrue(report["handlers_restored"], "entry must restore the caller's handlers")
        self.assertEqual(report["code"], 2)
        self.assertTrue(
            report["output_ok"], "entry must emit only the fixed failure classification"
        )
        self.assertTrue(report["retention_ok"], "build cleanup must preserve unresolved resources")

    def test_cleanup_uncertainty_retains_owned_build_artifacts_without_exporting_their_path(self):
        self.assert_resource_outcome("cleanup-unresolved")

    def test_process_unavailability_without_a_child_removes_owned_build_artifacts(self):
        self.assert_resource_outcome("process-unavailable")

    def test_compiler_leader_sigkill_retains_build_resources_while_its_helper_is_alive(self):
        _assert_leader_death(self, "compiler-leader-killed")


class HandoffNativeReapFaultTests(_HandoffProbeAssertions):
    def setUp(self):
        # Validate a configured path before platform gating. Even on a portable
        # host, invalid configuration is a failure rather than a skipped proof.
        candidate = _configured_native_fault_fixture()
        if sys.platform != "darwin":
            self.skipTest("native macOS supervisor fixture requires Darwin")
        # Ordinary tool discovery does not compile native prerequisites. The
        # explicit fault task builds them and rejects any skipped native case.
        if candidate is None:
            self.skipTest(
                "private native fixture absent; run mise run test:server:native:handoff:faults:macos"
            )
        self.native_fixture = candidate

    def test_native_case_reap_uncertainty_precedes_the_original_sigterm(self):
        report = self.probe("native-reap-fault-term", native_fixture=self.native_fixture)
        self.assert_outcome(report, 2)

    def test_native_case_reap_uncertainty_precedes_the_original_sigint(self):
        report = self.probe("native-reap-fault-int", native_fixture=self.native_fixture)
        self.assert_outcome(report, 2)

    def test_reaped_native_case_uncertainty_precedes_sigterm_without_late_signals(self):
        report = self.probe("native-reaped-fault-term", native_fixture=self.native_fixture)
        self.assert_outcome(report, 2)

    def test_reaped_native_case_uncertainty_precedes_sigint_without_late_signals(self):
        report = self.probe("native-reaped-fault-int", native_fixture=self.native_fixture)
        self.assert_outcome(report, 2)

    def test_native_supervisor_sigkill_retains_build_resources_while_its_case_is_alive(self):
        _assert_leader_death(self, "native-leader-killed", native_fixture=self.native_fixture)

    def test_reaped_native_supervisor_sigkill_precedes_sigterm_without_late_control(self):
        _assert_leader_death(
            self,
            "native-leader-killed-reaped-term",
            native_fixture=self.native_fixture,
        )

    def test_reaped_native_supervisor_sigkill_precedes_sigint_without_late_control(self):
        _assert_leader_death(
            self,
            "native-leader-killed-reaped-int",
            native_fixture=self.native_fixture,
        )


@unittest.skipUnless(sys.platform == "darwin", "macOS task output owner requires Darwin")
class HandoffReaderlessOutputTests(unittest.TestCase):
    def assert_readerless_outcome(self, name, code):
        report, process_code, stderr_empty = _private_probe(name)
        _assert_interface_safety(self, report)
        self.assertTrue(report["retention_ok"], "resolved build resources must be removed")
        self.assertFalse(
            report["scope_alive"], "output failure must not leave a live owned fixture"
        )
        self.assertTrue(
            stderr_empty, "readerless task stdout must not emit traceback or shutdown errors"
        )
        self.assertEqual(
            process_code, code, "output failure must preserve the selected exit status"
        )
        self.assertEqual(report["code"], code, "entry must return its selected exit status")

    def test_readerless_stdout_preserves_the_selected_interruption_status_and_cleanup(self):
        for name, code in (("readerless-final-term", 143), ("readerless-final-int", 130)):
            with self.subTest(signal="SIGTERM" if code == 143 else "SIGINT"):
                self.assert_readerless_outcome(name, code)

    def test_readerless_stdout_sanitizes_invalid_arguments_without_starting_a_child(self):
        self.assert_readerless_outcome("readerless-arguments", 2)

    def test_readerless_stdout_sanitizes_build_output_failure_and_removes_resolved_resources(self):
        self.assert_readerless_outcome("readerless-build-output", 2)

    def test_readerless_final_stream_flush_preserves_outcomes_through_interpreter_shutdown(self):
        for name, code in (
            ("readerless-flush-term", 143),
            ("readerless-flush-int", 130),
            ("readerless-flush-arguments", 2),
            ("readerless-flush-build-output", 2),
        ):
            with self.subTest(outcome=name):
                self.assert_readerless_outcome(name, code)


@unittest.skipUnless(sys.platform == "darwin", "fresh-exec SIGCHLD observation requires Darwin")
class HandoffInheritedSigchldObservationTests(unittest.TestCase):
    def test_fresh_exec_inherited_sigchld_ignore_is_observed_without_reinstalling_it(self):
        report, code, stderr_empty = _private_probe("sigchld-observation")
        self.assertTrue(isinstance(report, dict), "SIGCHLD observation must be a bounded object")
        flags = {
            "fixture_ok",
            "isolated",
            "cpython",
            "inherited_ignore_before",
            "inherited_ignore_after",
            "returncode_unset_after_spawn",
            "no_waitable_status_after_return",
            "no_lifecycle_signals",
            "cleanup_ok",
        }
        self.assertTrue(
            set(report) == flags | {"python_major", "python_minor", "returncode"}
            and all(type(report[key]) is bool for key in flags)
            and all(
                type(report[key]) is int for key in ("python_major", "python_minor", "returncode")
            ),
            "SIGCHLD observation returned invalid facts",
        )
        self.assertEqual(code, 0, "fresh-exec observation must exit normally")
        self.assertTrue(stderr_empty, "SIGCHLD observation must not expose raw errors")
        for key in flags:
            self.assertTrue(report[key], "SIGCHLD observation fixture failed: " + key)
        self.assertEqual((report["python_major"], report["python_minor"]), (3, 13))
        # Record either actual wait result without choosing the disputed verdict.
        # --sigchld-observation publishes these bounded facts for the coordinator.
        self.assertIn(
            report["returncode"], (0, 7), "fixed child must yield its real or substituted status"
        )


@unittest.skipUnless(sys.platform == "darwin", "macOS native fault task wiring requires Darwin")
class HandoffNativeFaultTaskTests(unittest.TestCase):
    def assert_task_outcome(self, name, code):
        report, process_code, stderr_empty = _private_probe(name)
        flags = {"fixture_ok", "output_ok", "retention_ok", "handlers_restored", "boundary_ok"}
        self.assertTrue(
            isinstance(report, dict)
            and set(report) == flags | {"code"}
            and all(type(report[key]) is bool for key in flags)
            and type(report["code"]) is int,
            "native fault task wiring returned an invalid report",
        )
        for key in flags:
            self.assertTrue(report[key], "native fault task wiring failed: " + key)
        self.assertTrue(stderr_empty, "native fault task wiring must not expose paths or errors")
        self.assertEqual(process_code, code)
        self.assertEqual(report["code"], code)

    def test_explicit_fault_task_builds_the_private_fixture_and_checks_the_complete_suite(self):
        for name in ("fault-pass", "fault-pass-asan"):
            with self.subTest(mode=name):
                self.assert_task_outcome(name, 0)

    def test_explicit_fault_task_rejects_skips_missing_cases_and_unresolved_retirement(self):
        for name in ("fault-skipped", "fault-count-mismatch", "fault-cleanup-unresolved"):
            with self.subTest(mode=name):
                self.assert_task_outcome(name, 2)


def run_native_fault_suite(native_fixture, checkpoint):
    """Run the original 29 methods without treating a skip as native evidence.

    Run in the build owner's process, under its existing latched handlers. There
    is no second supervisor whose abrupt exit could abandon separately grouped
    probes. Every existing probe keeps its own leases and bounded fallback reap.
    """
    groups = (
        ("fault-cancellation-14", HandoffCancellationTests),
        ("fault-build-resources-3", HandoffBuildResourceTests),
        ("fault-native-reap-7", HandoffNativeReapFaultTests),
        ("fault-readerless-output-4", HandoffReaderlessOutputTests),
        ("fault-inherited-sigchld-1", HandoffInheritedSigchldObservationTests),
    )
    reports = []
    _NATIVE_FAULT_RETIREMENTS.clear()
    with patch.dict(os.environ, {_NATIVE_FIXTURE_ENV: str(native_fixture)}):
        # An invalid supplied path fails before any probe. Missing prerequisites
        # cannot become a green suite through the ordinary discovery skip rule.
        if _configured_native_fault_fixture() is None:
            raise AssertionError("private native fault fixture is required")
        for name, case in groups:
            result = unittest.TestResult()
            for test in unittest.defaultTestLoader.loadTestsFromTestCase(case):
                checkpoint()
                test.run(result)
                checkpoint()
            reports.append(
                {
                    "case": name,
                    "tests": result.testsRun,
                    "skipped": len(result.skipped),
                    "failures": len(result.failures),
                    "errors": len(result.errors),
                    "expected_failures": len(result.expectedFailures),
                    "unexpected_successes": len(result.unexpectedSuccesses),
                }
            )
    return reports


@unittest.skipUnless(os.name == "posix", "private native fixture configuration requires POSIX")
class HandoffNativeFaultConfigurationTests(unittest.TestCase):
    def test_absent_fixture_configuration_skips_ordinary_native_setup(self):
        case = HandoffNativeReapFaultTests(
            "test_native_case_reap_uncertainty_precedes_the_original_sigterm"
        )
        with patch.dict(os.environ, {}, clear=True), self.assertRaises(unittest.SkipTest):
            case.setUp()

    def test_configured_invalid_fixtures_fail_instead_of_skipping(self):
        case = HandoffNativeReapFaultTests(
            "test_native_case_reap_uncertainty_precedes_the_original_sigterm"
        )
        with tempfile.TemporaryDirectory(prefix="spaceterm-handoff-config-") as directory:
            parent = Path(directory)
            executable = parent / "fixture"
            executable.write_bytes(b"fixed nonexecuted configuration fixture")
            executable.chmod(0o700)
            link = parent / "link"
            link.symlink_to(executable)
            nonexecutable = parent / "nonexecutable"
            nonexecutable.write_bytes(b"fixed nonexecuted configuration fixture")
            nonexecutable.chmod(0o600)
            writable = parent / "writable"
            writable.write_bytes(b"fixed nonexecuted configuration fixture")
            writable.chmod(0o722)
            for value in (
                "",
                "relative-fixture",
                str(parent / "missing"),
                str(parent),
                str(link),
                str(nonexecutable),
                str(writable),
            ):
                with (
                    self.subTest(configuration=value),
                    patch.dict(os.environ, {_NATIVE_FIXTURE_ENV: value}),
                    self.assertRaises(AssertionError),
                ):
                    case.setUp()
            parent.chmod(0o755)
            with (
                patch.dict(os.environ, {_NATIVE_FIXTURE_ENV: str(executable)}),
                self.assertRaises(AssertionError),
            ):
                case.setUp()

    def test_owned_private_configuration_is_validated_without_executing_it(self):
        with tempfile.TemporaryDirectory(prefix="spaceterm-handoff-config-") as directory:
            executable = Path(directory) / "fixture"
            executable.write_bytes(b"fixed nonexecuted configuration fixture")
            executable.chmod(0o700)
            with patch.dict(os.environ, {_NATIVE_FIXTURE_ENV: str(executable)}):
                self.assertEqual(_configured_native_fault_fixture(), executable)


if __name__ == "__main__":
    if len(sys.argv) in (3, 4) and sys.argv[1] == "--probe" and sys.argv[2] in _SCENARIOS:
        scenario = _SCENARIOS[sys.argv[2]]
        if not sys.flags.isolated or len(sys.argv) != (4 if scenario.native_fault else 3):
            raise SystemExit(2)
        sys.dont_write_bytecode = True
        _run_probe(sys.argv[2], Path(sys.argv[3]) if scenario.native_fault else None)
    elif (
        len(sys.argv) == 3
        and sys.argv[1] == "--build-resource-probe"
        and sys.argv[2] in {"cleanup-unresolved", "process-unavailable"}
    ):
        if not sys.flags.isolated or sys.platform != "darwin":
            raise SystemExit(2)
        sys.dont_write_bytecode = True
        _run_build_resource_probe(sys.argv[2])
    elif (
        len(sys.argv) == 4
        and sys.argv[1] == "--fault-wiring-probe"
        and sys.argv[2] in _FAULT_WIRING_SCENARIOS
    ):
        if not sys.flags.isolated or sys.platform != "darwin":
            raise SystemExit(2)
        sys.dont_write_bytecode = True
        raise SystemExit(_run_build_resource_probe(sys.argv[2], report_fd=int(sys.argv[3])))
    elif (
        len(sys.argv) in (4, 5)
        and sys.argv[1] == "--interface-probe"
        and sys.argv[2] in _INTERFACE_SCENARIOS
    ):
        stage, _ = _INTERFACE_SCENARIOS[sys.argv[2]]
        if not sys.flags.isolated or len(sys.argv) != (5 if stage == "native" else 4):
            raise SystemExit(2)
        sys.dont_write_bytecode = True
        raise SystemExit(
            _run_interface_probe(
                sys.argv[2],
                int(sys.argv[3]),
                Path(sys.argv[4]) if stage == "native" else None,
            )
        )
    elif len(sys.argv) == 3 and sys.argv[1] == "--sigchld-launch":
        if not sys.flags.isolated or sys.platform != "darwin":
            raise SystemExit(2)
        # Exactly one launcher-side installation, followed by direct fresh exec.
        # The observation process never re-installs or resets SIGCHLD.
        signal.signal(signal.SIGCHLD, signal.SIG_IGN)
        os.set_inheritable(int(sys.argv[2]), True)
        os.execve(
            _PYTHON,
            [
                _PYTHON,
                "-I",
                str(Path(__file__).resolve()),
                "--sigchld-probe",
                sys.argv[2],
            ],
            {},
        )
    elif len(sys.argv) == 3 and sys.argv[1] == "--sigchld-probe":
        if not sys.flags.isolated or sys.platform != "darwin":
            raise SystemExit(2)
        sys.dont_write_bytecode = True
        _run_sigchld_observation(int(sys.argv[2]))
    elif len(sys.argv) == 2 and sys.argv[1] == "--sigchld-observation":
        if not sys.flags.isolated or sys.platform != "darwin":
            raise SystemExit(2)
        report, code, stderr_empty = _private_probe("sigchld-observation")
        # The observer uses its private pipe. Only its finite, path-free facts
        # reach this independent coordinator-facing command's normal stdout.
        print(json.dumps(report, separators=(",", ":")), flush=True)
        raise SystemExit(0 if code == 0 and stderr_empty and report.get("fixture_ok") else 2)
    elif len(sys.argv) == 3 and sys.argv[1] == "--native-fault-fixture":
        os.environ[_NATIVE_FIXTURE_ENV] = sys.argv[2]
        unittest.main(argv=[sys.argv[0]])
    else:
        unittest.main()


class LauncherToolContractTests(unittest.TestCase):
    """The new task inherits the existing supervisor without weakening record bounds."""

    def probe(self, mode):
        source = _TASK.with_name("launcher_tests.py")
        script = r"""
import importlib.util, json, sys
spec = importlib.util.spec_from_file_location("_launcher_tools_contract", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
driver = module.supervisor()
records = [dict(schema=1, scope=driver.SCOPE, case=name, result="pass") for name in module.CASES]
mode = sys.argv[2]
if mode == "signal-contract":
    import signal
    print(json.dumps({"accepted": [driver._native_scope_retired(code) for code in
        (-signal.SIGTERM, -signal.SIGINT, 125, 128 + signal.SIGTERM, 128 + signal.SIGINT)]}))
    raise SystemExit(0)
if mode == "missing": records.pop()
if mode == "unknown": records[0]["case"] = "unapproved-case"
if mode == "native-text": records[0]["detail"] = "native diagnostic must never pass"
if mode == "duplicate": records.insert(0, records[0])
data = b"".join(json.dumps(record, separators=(",", ":")).encode() + b"\n" for record in records)
try:
    accepted = driver.validate_records(data, 0)
    report = {"accepted": True, "count": len(accepted)}
except driver.TaskFailure as error:
    report = {"accepted": False, "failure": error.classification}
print(json.dumps(report, separators=(",", ":")))
"""
        result = subprocess.run(
            [_PYTHON, "-I", "-c", script, str(source), mode],
            env={},
            capture_output=True,
            timeout=5,
            check=True,
        )
        self.assertEqual(result.stderr, b"")
        self.assertLess(len(result.stdout), 256)
        return json.loads(result.stdout)

    def test_launcher_requires_all_sixteen_executed_cases(self):
        self.assertEqual(self.probe("valid"), {"accepted": True, "count": 16})
        self.assertEqual(
            self.probe("missing"),
            {"accepted": False, "failure": "driver-invalid-output"},
        )

    def test_launcher_rejects_unknown_and_duplicate_cases(self):
        for mode in ("unknown", "duplicate"):
            with self.subTest(mode=mode):
                self.assertEqual(
                    self.probe(mode),
                    {"accepted": False, "failure": "driver-invalid-output"},
                )

    def test_launcher_never_relays_native_diagnostic_fields(self):
        self.assertEqual(
            self.probe("native-text"),
            {"accepted": False, "failure": "driver-invalid-output"},
        )

    def test_launcher_raw_signal_death_is_not_child_retirement(self):
        self.assertEqual(
            self.probe("signal-contract"), {"accepted": [False, False, False, True, True]}
        )
