#!/usr/bin/env -S python -I
# MISE description="Compile the macOS startup wire candidate; execute only with --run or --faults"
# ruff: noqa: E402
"""Compile trusted sources; optionally run bounded, unprivileged wire-candidate tests."""

import sys

# Check isolation before importing anything from the script directory or cwd.
if not sys.flags.isolated:
    print(
        '{"schema":1,"scope":"handoff-wire-candidate",'
        '"case":"arguments","result":"isolated-python-required"}'
    )
    raise SystemExit(2)

sys.dont_write_bytecode = True

import importlib.util
import json
import os
import selectors
import shutil
import signal
import stat
import subprocess
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]
SCOPE = "handoff-wire-candidate"
MAX_OUTPUT_BYTES = 32768
BUILD_TIMEOUT = 60.0
DRIVER_TIMEOUT = 45.0
STOP_TIMEOUT = 3.0
REAP_TIMEOUT = 2.0
CANCELLATION_POLL_INTERVAL = 0.1
FAULT_SUITE_TIMEOUT = 180.0
FAULT_GROUPS = (
    ("fault-cancellation-14", 14),
    ("fault-build-resources-3", 3),
    ("fault-native-reap-7", 7),
    ("fault-readerless-output-4", 4),
    ("fault-inherited-sigchld-1", 1),
)
# Harness-only exit interface, shared with ST_HANDOFF_TEST_CLEANUP_UNRESOLVED_EXIT
# in tests/server_native/macos/handoff.h. Neither records nor signal exits carry
# this outcome reliably when cancellation bypasses output validation.
NATIVE_CLEANUP_UNRESOLVED_EXIT = 125
CASES = (
    "readiness-exact",
    "confirmation-exact",
    "readiness-fragmented",
    "confirmation-fragmented",
    "empty-closed",
    "arbitrary-byte",
    "readiness-truncated",
    "confirmation-truncated",
    "readiness-trailing",
    "confirmation-trailing",
    "delayed-trailing",
    "unknown-type",
    "unknown-version",
    "wrong-magic",
    "wrong-declared-size",
    "oversized-declared-size",
    "header-reserved",
    "frame-reserved",
    "missing-group-drop",
    "missing-gid-drop",
    "missing-uid-drop",
    "missing-core-protection",
    "missing-memory-protection",
    "missing-post-exec",
    "extra-obligation",
    "wrong-uid",
    "wrong-gid",
    "wrong-build",
    "wrong-generation",
    "wrong-installation",
    "wrong-server-artifact",
    "wrong-nonce",
    "wrong-peer-pid",
    "same-pid-wrong-start",
    "wrong-start-fraction",
    "root-target",
    "zero-generation",
    "zero-nonce",
    "zero-installation",
    "zero-artifact",
    "invalid-build",
    "invalid-pid",
    "invalid-start-fraction",
    "invalid-expected-kind",
    "invalid-expectation-open",
    "readiness-expected-challenge",
    "readiness-expected-mac",
    "confirmation-not-readiness",
    "readiness-not-confirmation",
    "wrong-confirmation-challenge",
    "wrong-confirmation-mac",
    "confirmation-stale-generation",
    "confirmation-wrong-nonce",
    "confirmation-obligations",
    "open-empty-deadline",
    "open-partial-deadline",
    "open-readiness-deadline",
    "open-confirmation-deadline",
    "interrupted-wait",
    "expired-deadline",
    "zero-deadline",
    "unbounded-deadline",
    "missing-cloexec",
    "write-endpoint",
    "nonpipe-endpoint",
    "socket-endpoint",
    "negative-endpoint",
    "null-expectation",
    "null-output",
    "null-endpoint",
    "consumed-endpoint",
    "signal-semantics",
    "native-gate-unavailable",
)


class TaskFailure(Exception):
    """Carry only a fixed local classification, never subprocess diagnostics."""

    def __init__(self, classification: str) -> None:
        super().__init__()
        self.classification = classification


class Terminated(Exception):
    def __init__(self, number: int) -> None:
        super().__init__()
        self.number = number


def emit(case: str, result: str) -> None:
    print(
        json.dumps(
            {"schema": 1, "scope": SCOPE, "case": case, "result": result},
            separators=(",", ":"),
        ),
        flush=True,
    )


def _quench_stdout() -> None:
    """Retire a failed output channel without a second shutdown diagnostic.

    A failed buffered write may leave bytes for Python's final stream flush.
    Redirect the same descriptor, including sys.__stdout__'s alias, to /dev/null.
    If that fails, close the stream before withdrawing it from final flushing.
    Neither path bypasses child, resource, or signal-handler cleanup.
    """
    stream = sys.stdout
    if stream is None:
        return
    try:
        target = stream.fileno()
        descriptor = os.open("/dev/null", os.O_WRONLY | os.O_CLOEXEC)
        try:
            os.dup2(descriptor, target)
        finally:
            if descriptor != target:
                os.close(descriptor)
    except (AttributeError, OSError, ValueError):
        try:
            stream.close()
        except (OSError, ValueError):
            pass
        sys.stdout = None
        if sys.__stdout__ is stream:
            sys.__stdout__ = None


def _emit_final(case: str, result: str) -> None:
    """Keep a selected outcome when its final diagnostic cannot be delivered."""
    try:
        emit(case, result)
    except (OSError, ValueError):
        _quench_stdout()


def _finish_output(result: int) -> int:
    """Flush before restoring handlers; suppress later interpreter flush errors."""
    if sys.stdout is None:
        return result if result != 0 else 2
    try:
        sys.stdout.flush()
    except (OSError, ValueError):
        _quench_stdout()
        return result if result != 0 else 2
    return result


class _Cancellation:
    """Latch cancellation without interrupting child publication or retirement."""

    def __init__(self) -> None:
        self._first: dict[str, int] = {}

    @property
    def number(self) -> int | None:
        return self._first.get("number")

    def __call__(self, number: int, _frame: object) -> None:
        self._first.setdefault("number", number)


def _cancellation_checkpoint() -> None:
    handler = signal.getsignal(signal.SIGTERM)
    if isinstance(handler, _Cancellation):
        number = handler.number
        if number is not None:
            raise Terminated(number)


def _compiler_group_absent(process: subprocess.Popen) -> bool:
    """Observe absence only while the unreaped leader reserves the group ID.

    Popen created this session/group. No wait/poll may consume its leader before
    this observation. ESRCH from killpg(group, 0) is the sole production absence
    result. A successful query means presence; EPERM and other errors mean
    uncertainty. In particular, Darwin may retain an unsignalable zombie group.
    Neither its EPERM nor getpgid(exited_leader)'s ESRCH proves group absence.

    The private cancellation fixtures have a different OS retirement witness:
    EOF from their fixed, source-controlled nonforking writers. They inject that
    witness at this private boundary, not a production authority flag. Arbitrary
    compiler helpers can close inherited pipes, so that proof is unavailable to
    the actual xcrun/clang group. No native group-enumeration SPI is used here.
    """
    if process.returncode is not None:
        return False
    try:
        os.killpg(process.pid, 0)
    except ProcessLookupError:
        return True
    except OSError:
        return False
    return False


def _native_scope_retired(returncode: int) -> bool:
    """Recognize only the fixed supervisor's orderly retirement exit interface.

    This trusted supervisor retires its cases before ordinary exits or its
    latched TERM/INT exit. Other signals, including SIGKILL, cannot attest case
    retirement. Its cases have separate groups, so supervisor EOF is no proof.
    """
    if returncode == NATIVE_CLEANUP_UNRESOLVED_EXIT:
        return False
    return returncode in (
        0,
        1,
        2,
        -signal.SIGTERM,
        -signal.SIGINT,
        128 + signal.SIGTERM,
        128 + signal.SIGINT,
    )


def stop_process(process: subprocess.Popen, *, native: bool) -> bool:
    """Bound scoped retirement without signaling a released PID/group ID.

    The native supervisor owns its fixed cases and reports inner reap uncertainty
    through a dedicated exit status. Only its orderly exits attest retirement;
    unexpected supervisor death leaves its separately grouped cases unresolved.
    Compiler leader exit cannot provide that contract: retain its unreaped ID
    until group absence is observed or cleanup is classified unresolved.
    Escalation is always unresolved, even if the final leader reap succeeds.
    An unresolved task retains its private build directory.
    """
    if native and process.returncode is not None:
        return _native_scope_retired(process.returncode)
    # A compiler wait may already have consumed the leader before cancellation.
    # Production cannot then identify the old group safely. Only a private
    # fixture's independent, complete retirement witness can still prove absence.
    if not native and process.returncode is not None:
        return _compiler_group_absent(process)
    deadline = time.monotonic() + STOP_TIMEOUT
    signal_failed = False
    try:
        if native:
            os.kill(process.pid, signal.SIGTERM)
        else:
            os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    except OSError:
        signal_failed = True
    if native:
        try:
            returncode = process.wait(timeout=STOP_TIMEOUT)
            return not signal_failed and _native_scope_retired(returncode)
        except (OSError, subprocess.TimeoutExpired):
            pass
    else:
        # Never call wait() merely because TERM was delivered or the leader
        # exited. The unreaped child reserves its ID throughout these probes.
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            if _compiler_group_absent(process):
                try:
                    process.wait(timeout=remaining)
                    return not signal_failed
                except (OSError, subprocess.TimeoutExpired):
                    break
            time.sleep(min(remaining, CANCELLATION_POLL_INTERVAL, STOP_TIMEOUT / 10))
    # No wait above may have consumed an ID on its failure path. Still check
    # returncode before escalation, and never signal after the final reap.
    if process.returncode is None:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except OSError:
            pass
        try:
            process.wait(timeout=REAP_TIMEOUT)
        except (OSError, subprocess.TimeoutExpired):
            pass
    return False


def bounded_process(
    command: list[str],
    environment: dict[str, str],
    *,
    timeout: float,
    native: bool = False,
) -> tuple[int, bytes]:
    process = None
    try:
        _cancellation_checkpoint()
        # The entry handlers only record signals. Do not block terminating
        # signals across Popen: the child must not inherit a blocked mask.
        try:
            process = subprocess.Popen(
                command,
                cwd=ROOT,
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE if native else subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                close_fds=True,
                start_new_session=True,
            )
        except OSError:
            raise TaskFailure("process-unavailable") from None
        deadline = time.monotonic() + timeout
        data = bytearray()
        if native:
            if process.stdout is None:
                raise TaskFailure("driver-invalid-output")
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)
                while True:
                    _cancellation_checkpoint()
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise TaskFailure("deadline")
                    events = selector.select(min(remaining, CANCELLATION_POLL_INTERVAL))
                    _cancellation_checkpoint()
                    if not events:
                        continue
                    block = os.read(
                        process.stdout.fileno(), min(4096, MAX_OUTPUT_BYTES + 1 - len(data))
                    )
                    if not block:
                        break
                    data.extend(block)
                    if len(data) > MAX_OUTPUT_BYTES:
                        raise TaskFailure("driver-invalid-output")
        while True:
            _cancellation_checkpoint()
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TaskFailure("deadline")
            try:
                returncode = process.wait(timeout=min(remaining, CANCELLATION_POLL_INTERVAL))
            except subprocess.TimeoutExpired:
                continue
            # Consuming a killed leader does not retire its scope. Preserve
            # uncertainty before cancellation or ordinary failure can mask it.
            # stop_process must not regain control of these now-released IDs.
            if (native and not _native_scope_retired(returncode)) or (
                not native and returncode < 0
            ):
                raise TaskFailure("cleanup-unresolved")
            _cancellation_checkpoint()
            return returncode, bytes(data)
    except BaseException as error:
        # No cancellation checkpoint may interrupt retirement. No raw output is
        # relayed on an error, cancellation, or deadline.
        if process is not None and not stop_process(process, native=native):
            raise TaskFailure("cleanup-unresolved") from None
        if isinstance(error, subprocess.TimeoutExpired):
            raise TaskFailure("deadline") from None
        raise
    finally:
        if process is not None and process.stdout is not None:
            process.stdout.close()


def unique_object(pairs: list[tuple[str, object]]) -> dict[str, object]:
    result: dict[str, object] = {}
    for key, value in pairs:
        if key in result:
            raise TaskFailure("driver-invalid-output")
        result[key] = value
    return result


def validate_records(data: bytes, returncode: int) -> list[dict[str, object]]:
    if not data.endswith(b"\n"):
        raise TaskFailure("driver-invalid-output")
    lines = data.splitlines()
    if len(lines) != len(CASES):
        raise TaskFailure("driver-invalid-output")
    records: list[dict[str, object]] = []
    for name, line in zip(CASES, lines, strict=True):
        try:
            record = json.loads(line, object_pairs_hook=unique_object)
        except (UnicodeError, ValueError):
            raise TaskFailure("driver-invalid-output") from None
        allowed = {"red", "fixture-error", "deadline", "cleanup-unresolved"}
        allowed.add("gated" if name == "native-gate-unavailable" else "pass")
        if (
            not isinstance(record, dict)
            or set(record) != {"schema", "scope", "case", "result"}
            or type(record["schema"]) is not int
            or record["schema"] != 1
            or record["scope"] != SCOPE
            or record["case"] != name
            or not isinstance(record["result"], str)
            or record["result"] not in allowed
        ):
            raise TaskFailure("driver-invalid-output")
        records.append(record)
    expected_returncode = (
        2
        if any(
            record["result"] in {"fixture-error", "deadline", "cleanup-unresolved"}
            for record in records
        )
        else 1
        if any(record["result"] == "red" for record in records)
        else 0
    )
    if returncode != expected_returncode:
        raise TaskFailure("driver-invalid-output")
    return records


def _load_native_fault_suite():
    source = ROOT / "mise-tasks/lib/tests/test_server_handoff.py"
    spec = importlib.util.spec_from_file_location("_handoff_native_fault_suite", source)
    if spec is None or spec.loader is None:
        raise TaskFailure("fixture-error")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def _validate_fault_reports(reports: object) -> list[tuple[str, str]]:
    """Require all 29 methods, including seven native probes, with no skipped proof."""
    if not isinstance(reports, list) or len(reports) != len(FAULT_GROUPS):
        raise TaskFailure("driver-invalid-output")
    counts = {
        "tests",
        "skipped",
        "failures",
        "errors",
        "expected_failures",
        "unexpected_successes",
    }
    records = []
    for (name, expected), report in zip(FAULT_GROUPS, reports, strict=True):
        if (
            not isinstance(report, dict)
            or set(report) != counts | {"case"}
            or report["case"] != name
            or any(type(report[key]) is not int or not 0 <= report[key] <= 64 for key in counts)
        ):
            raise TaskFailure("driver-invalid-output")
        if (
            report["tests"] != expected
            or report["skipped"]
            or report["errors"]
            or report["expected_failures"]
        ):
            outcome = "fixture-error"
        elif report["failures"] or report["unexpected_successes"]:
            outcome = "red"
        else:
            outcome = "pass"
        records.append((name, outcome))
    return records


def _run_native_fault_tests(binary: Path) -> int:
    suite = _load_native_fault_suite()
    deadline = time.monotonic() + FAULT_SUITE_TIMEOUT

    def checkpoint() -> None:
        _cancellation_checkpoint()
        if time.monotonic() >= deadline:
            raise TaskFailure("deadline")

    try:
        try:
            reports = suite.run_native_fault_suite(binary, checkpoint)
            checkpoint()
            records = _validate_fault_reports(reports)
        except BaseException:
            # An incomplete/invalid private report is not a retirement witness.
            # Keep the build even if cancellation or an assertion arrived first.
            if not suite.native_fault_resources_retired():
                raise TaskFailure("cleanup-unresolved") from None
            raise
        if not suite.native_fault_resources_retired():
            raise TaskFailure("cleanup-unresolved")
        for name, result in records:
            emit(name, result)
        if any(result == "fixture-error" for _, result in records):
            outcome, returncode = "fixture-error", 2
        elif any(result == "red" for _, result in records):
            outcome, returncode = "red", 1
        else:
            outcome, returncode = "fault-green-native-gated", 0
        emit("driver", outcome)
        return returncode
    finally:
        sys.modules.pop(suite.__name__, None)


def run_task(*, execute: bool = False, sanitizers: bool = False, faults: bool = False) -> int:
    if sys.platform != "darwin":
        emit("platform", "unavailable")
        return 2
    if os.getuid() == 0 or os.geteuid() == 0:
        emit("privilege", "privileged-run-refused")
        return 2
    # Import only the existing repository helper. Never enter release_keychain,
    # inspect recovered signing material, select an identity, or invoke codesign.
    sys.path.insert(0, str(ROOT / "mise-tasks" / "lib"))
    from spaceterm_tasks.macos_signing import signing_environment

    stripped = signing_environment(os.environ)
    # TemporaryDirectory's context exit/finalizer removes the tree even when an
    # owned helper may still be running. Keep unresolved resources deliberately;
    # the fixed diagnostic must not export their path or try to adopt helpers.
    directory = tempfile.mkdtemp(prefix="spaceterm-handoff-", dir="/private/tmp")
    retain_build = True
    try:
        build = Path(directory)
        metadata = build.stat(follow_symlinks=False)
        if (
            not stat.S_ISDIR(metadata.st_mode)
            or metadata.st_uid != os.geteuid()
            or stat.S_IMODE(metadata.st_mode) != 0o700
        ):
            raise TaskFailure("owned-build-unavailable")
        retain_build = False
        # Strip signing inputs, then keep only HOME for xcrun. Fixed toolchain,
        # locale and temp values also exclude caller CPATH/SDK/DYLD injection and
        # unrelated credentials from compiler and test subprocess environments.
        environment = {key: stripped[key] for key in ("HOME",) if key in stripped}
        environment.update(
            PATH="/usr/bin:/bin:/usr/sbin:/sbin",
            LC_ALL="C",
            TMPDIR=str(build),
        )
        if sanitizers:
            environment.update(
                ASAN_OPTIONS="abort_on_error=1:disable_coredump=1",
                UBSAN_OPTIONS="halt_on_error=1:print_stacktrace=0",
            )
        sources = ROOT / "tests" / "server_native" / "macos"
        binary = build / "handoff-tests"
        command = [
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
            command.append("-DST_HANDOFF_PRIVATE_REAP_FAULT")
        if sanitizers:
            command.extend(["-g", "-O1", "-fsanitize=address,undefined", "-fno-omit-frame-pointer"])
        else:
            command.append("-O0")
        command.extend(
            [str(sources / "handoff.c"), str(sources / "handoff_tests.c"), "-o", str(binary)]
        )
        compiled, _ = bounded_process(command, environment, timeout=BUILD_TIMEOUT)
        if compiled != 0:
            emit("build", "build-failed")
            return 2
        emit("build", "source-compiled")
        if faults:
            return _run_native_fault_tests(binary)
        if not execute:
            return 0
        returncode, data = bounded_process(
            [str(binary)], environment, timeout=DRIVER_TIMEOUT, native=True
        )
        records = validate_records(data, returncode)
        for record in records:
            # Reconstruct only allowlisted values, never relay native text.
            emit(str(record["case"]), str(record["result"]))
        if any(record["result"] == "cleanup-unresolved" for record in records):
            outcome = "cleanup-unresolved"
        elif any(record["result"] == "deadline" for record in records):
            outcome = "deadline"
        elif returncode == 2:
            outcome = "fixture-error"
        elif returncode == 1:
            outcome = "red"
        else:
            outcome = "wire-green-native-gated"
        retain_build = outcome == "cleanup-unresolved"
        emit("driver", outcome)
        return returncode
    except TaskFailure as error:
        retain_build = retain_build or error.classification == "cleanup-unresolved"
        raise
    finally:
        if not retain_build:
            # Recheck the exact mkdtemp owner before removal. Neither a replaced
            # pathname nor a symlink grants authority to remove another tree.
            current = build.stat(follow_symlinks=False)
            if (
                not stat.S_ISDIR(current.st_mode)
                or current.st_uid != os.geteuid()
                or stat.S_IMODE(current.st_mode) != 0o700
                or (current.st_dev, current.st_ino) != (metadata.st_dev, metadata.st_ino)
            ):
                raise TaskFailure("owned-build-unavailable")
            shutil.rmtree(directory)


def entry() -> int:
    arguments = tuple(sys.argv[1:])
    if arguments not in (
        (),
        ("--run",),
        ("--sanitizers",),
        ("--run", "--sanitizers"),
        ("--sanitizers", "--run"),
        ("--faults",),
        ("--faults", "--sanitizers"),
        ("--sanitizers", "--faults"),
    ):
        _emit_final("arguments", "invalid-arguments")
        return _finish_output(2)
    cancellation = _Cancellation()
    previous_term = signal.signal(signal.SIGTERM, cancellation)
    try:
        previous_int = signal.signal(signal.SIGINT, cancellation)
        try:
            classification = None
            try:
                _cancellation_checkpoint()
                if "--faults" in arguments:
                    result = run_task(faults=True, sanitizers="--sanitizers" in arguments)
                else:
                    result = run_task(
                        execute="--run" in arguments, sanitizers="--sanitizers" in arguments
                    )
            except TaskFailure as error:
                classification = error.classification
                result = 2
            except KeyboardInterrupt:
                cancellation(signal.SIGINT, None)
                classification = "interrupted"
                result = 130
            except Terminated as error:
                cancellation(error.number, None)
                classification = "interrupted"
                result = 128 + error.number
            except Exception:
                classification = "fixture-error"
                result = 2
            # All owned-child and FD cleanup has finished. The first signal wins
            # unless retirement itself remained unresolved, including escalation.
            number = cancellation.number
            if number is not None and classification != "cleanup-unresolved":
                classification = "interrupted"
                result = 128 + number
            if classification is not None:
                _emit_final("driver", classification)
            return _finish_output(result)
        finally:
            signal.signal(signal.SIGINT, previous_int)
    finally:
        signal.signal(signal.SIGTERM, previous_term)


if __name__ == "__main__":
    raise SystemExit(entry())
