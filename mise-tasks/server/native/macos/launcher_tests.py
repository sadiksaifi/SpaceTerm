#!/usr/bin/env -S python -I
# MISE description="Compile unprivileged launcher mechanics; run only with --run"
# ruff: noqa: E402
"""Exercise native process facts and injected drop calls, never privileged startup."""

import sys

if not sys.flags.isolated:
    print(
        '{"schema":1,"scope":"launcher-native-mechanics","case":"arguments",'
        '"result":"isolated-python-required"}'
    )
    raise SystemExit(2)

sys.dont_write_bytecode = True

import importlib.util
import os
import select
import shutil
import signal
import stat
import subprocess
import tempfile
from pathlib import Path
from typing import Any, cast

ROOT = Path(__file__).resolve().parents[4]
CASES = (
    "drop-groups-gid-uid-in-order",
    "darwin-primary-group-only-passes",
    "darwin-additional-group-rejects",
    "darwin-wrong-primary-group-rejects",
    "privilege-drop-stops-at-first-failure",
    "missing-native-saved-id-proof-is-unavailable-not-success",
    "missing-remote-group-proof-is-unavailable-not-success",
    "saved-root-identity-rejects-drop",
    "pid-reuse-rejects-privilege-proof",
    "stale-original-process-rejects-before-drop",
    "pending-consumed-before-unavailable-launch",
    "unresolved-release-overrides-launch-failure",
    "native-proof-not-readiness-bits-authorizes-release",
    "unknown-abi-rejects-before-effects",
    "tracked-launcher-exec-preserves-original-pid-identity",
    "terminal-child-inherits-no-exchange-descriptor",
)


def supervisor() -> Any:
    """Reuse the cancellation and cleanup precedence covered by 29 fault tests."""
    source = Path(__file__).with_name("handoff_tests.py")
    spec = importlib.util.spec_from_file_location("_launcher_handoff_supervisor", source)
    if spec is None or spec.loader is None:
        raise RuntimeError("unavailable")
    module = cast(Any, importlib.util.module_from_spec(spec))
    spec.loader.exec_module(module)
    module.SCOPE = "launcher-native-mechanics"
    module.CASES = CASES
    # Unlike the original handoff supervisor, this fixture exits with a latched
    # status after cleanup. A raw signal death never attests child retirement.
    module._native_scope_retired = lambda code: (
        code in (0, 1, 2, 128 + signal.SIGTERM, 128 + signal.SIGINT)
    )
    return module


CANCELLATION_CASES = (
    ("launcher-cancel-term-during-exec", "exec", signal.SIGTERM),
    ("launcher-cancel-int-during-exec", "exec", signal.SIGINT),
    ("launcher-cancel-term-during-descriptor-exec", "descriptor", signal.SIGTERM),
    ("launcher-cleanup-unresolved-overrides-first-signal", "unresolved", signal.SIGINT),
)


def cancellation_cases(driver, binary: Path, environment: dict[str, str]) -> bool:
    """Observe actual child retirement through a private, single-child witness."""
    passed = True
    for name, mode, number in CANCELLATION_CASES:
        read, write = os.pipe()
        process = None
        orderly = False
        try:
            driver._cancellation_checkpoint()
            process = subprocess.Popen(
                [str(binary), "--cancellation-probe", mode, str(write)],
                cwd=ROOT,
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                pass_fds=(write,),
                start_new_session=True,
            )
            os.close(write)
            write = -1
            assert process.stdout is not None
            ready = select.select([process.stdout], [], [], 3.0)[0]
            if not ready or os.read(process.stdout.fileno(), 1) != b"r":
                raise driver.TaskFailure("driver-invalid-output")
            os.kill(process.pid, number)
            if mode == "unresolved":
                latched = select.select([process.stdout], [], [], 2.0)[0]
                if not latched or os.read(process.stdout.fileno(), 1) != b"s":
                    raise driver.TaskFailure("driver-invalid-output")
                os.kill(process.pid, signal.SIGTERM)
            code = process.wait(timeout=4.0)
            retired = bool(select.select([read], [], [], 0.25)[0]) and os.read(read, 1) == b""
            expected = 125 if mode == "unresolved" else 128 + number
            orderly = retired and code == expected
            passed = passed and orderly
            driver.emit(name, "pass" if orderly else "red")
        finally:
            # Even RED leaves no live helper. The fixed child owns an alarm and
            # the witness stays open through its exec until actual retirement.
            if process is not None:
                if process.poll() is None:
                    driver.stop_process(process, native=True)
                if process.stdout is not None:
                    process.stdout.close()
            if not select.select([read], [], [], 3.0)[0] or os.read(read, 1) != b"":
                raise driver.TaskFailure("cleanup-unresolved")
            os.close(read)
            if write >= 0:
                os.close(write)
    return passed


def run_task(
    driver, *, execute: bool = False, sanitizers: bool = False, faults: bool = False
) -> int:
    if faults:
        raise driver.TaskFailure("invalid-arguments")
    if sys.platform != "darwin":
        driver.emit("platform", "unavailable")
        return 2
    if os.getuid() == 0 or os.geteuid() == 0:
        driver.emit("privilege", "privileged-run-refused")
        return 2
    sys.path.insert(0, str(ROOT / "mise-tasks/lib"))
    from spaceterm_tasks.macos_signing import signing_environment

    stripped = signing_environment(os.environ)
    target = ROOT / "target"
    target.mkdir(exist_ok=True)
    directory = tempfile.mkdtemp(prefix="launcher-tests-", dir=target)
    build = Path(directory)
    metadata = build.stat(follow_symlinks=False)
    retain = True
    try:
        if (
            not stat.S_ISDIR(metadata.st_mode)
            or metadata.st_uid != os.geteuid()
            or stat.S_IMODE(metadata.st_mode) != 0o700
        ):
            raise driver.TaskFailure("owned-build-unavailable")
        retain = False
        environment = {key: stripped[key] for key in ("HOME",) if key in stripped}
        environment.update(PATH="/usr/bin:/bin:/usr/sbin:/sbin", LC_ALL="C", TMPDIR=directory)
        if sanitizers:
            environment.update(
                ASAN_OPTIONS="abort_on_error=1:disable_coredump=1",
                UBSAN_OPTIONS="halt_on_error=1:print_stacktrace=0",
            )
        fixtures = ROOT / "tests/server_native/macos"
        native = ROOT / "crates/spaceterm-host/native/macos"
        test_include = ROOT / "crates/spaceterm-host/src/startup_exchange/test_include"
        binary = build / "launcher-tests"
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
            "-DST_NATIVE_TESTING",
            "-I",
            str(native),
            "-I",
            str(test_include),
        ]
        if sanitizers:
            command.extend(["-g", "-O1", "-fsanitize=address,undefined", "-fno-omit-frame-pointer"])
        else:
            command.append("-O0")
        command.extend(
            [
                str(fixtures / "launcher.c"),
                str(fixtures / "native_admission.c"),
                str(fixtures / "launcher_tests.c"),
                "-lproc",
                "-o",
                str(binary),
            ]
        )
        compiled, _ = driver.bounded_process(command, environment, timeout=60.0)
        if compiled != 0:
            driver.emit("build", "build-failed")
            return 2
        driver.emit("build", "source-compiled")
        if not execute:
            return 0
        code, data = driver.bounded_process([str(binary)], environment, timeout=15.0, native=True)
        records = driver.validate_records(data, code)
        for record in records:
            driver.emit(record["case"], record["result"])
        retain = any(record["result"] == "cleanup-unresolved" for record in records)
        if not cancellation_cases(driver, binary, environment):
            code = 1
        driver.emit("driver", "mechanics-green-native-gated" if code == 0 else "red")
        return code
    except driver.TaskFailure as error:
        retain = retain or error.classification == "cleanup-unresolved"
        raise
    finally:
        if not retain:
            current = build.stat(follow_symlinks=False)
            if (
                not stat.S_ISDIR(current.st_mode)
                or current.st_uid != os.geteuid()
                or stat.S_IMODE(current.st_mode) != 0o700
                or (current.st_dev, current.st_ino) != (metadata.st_dev, metadata.st_ino)
            ):
                raise driver.TaskFailure("owned-build-unavailable")
            shutil.rmtree(directory)


def entry() -> int:
    driver = supervisor()
    driver.run_task = lambda **kwargs: run_task(driver, **kwargs)
    return driver.entry()


if __name__ == "__main__":
    raise SystemExit(entry())
