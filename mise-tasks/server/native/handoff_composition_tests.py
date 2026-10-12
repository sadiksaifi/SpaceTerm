#!/usr/bin/env -S python -I
# MISE description="Test exported host exchange mechanics and cleanup regressions"
# ruff: noqa: E402
"""Run actual host exchange tests, not privileged launcher or release evidence."""

import sys

if not sys.flags.isolated:
    raise SystemExit(2)
sys.dont_write_bytecode = True

import importlib.util
import io
import json
import os
import re
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]


def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError("adapter-unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def run(driver):
    tests = load(
        ROOT / "crates/spaceterm-host/tests/native_handoff/launcher_tools.py",
        "_unpublished_witness_regressions",
    )
    result = unittest.TextTestRunner(stream=io.StringIO()).run(
        unittest.defaultTestLoader.loadTestsFromModule(tests)
    )
    if result.testsRun != 5 or result.skipped or not result.wasSuccessful():
        driver.emit("unpublished-witness-cleanup", "red")
        return 1
    driver.emit("unpublished-witness-cleanup", "pass")
    # Cargo receives only ordinary build inputs. Do not forward signing inputs
    # or credentials, and do not print raw native errors or subprocess output.
    environment = {
        key: os.environ[key]
        for key in ("HOME", "PATH", "CARGO_HOME", "RUSTUP_HOME", "CARGO_TARGET_DIR")
        if key in os.environ
    }
    environment["CARGO_BUILD_JOBS"] = "3" if sys.platform == "darwin" else "4"
    environment["LC_ALL"] = "C"
    suites = [
        ("native-crypto-and-reaping", "spaceterm-host", ["--lib", "startup_exchange::"], 4),
        ("exported-host-exchange", "spaceterm-host", ["--test", "startup_exchange"], 22),
        ("native-startup-peers", "spaceterm-server", ["--test", "native_startup"], 5),
    ]
    if sys.platform == "linux":
        suites.append(
            (
                "credential-recipe",
                "spaceterm-host",
                ["--lib", "linux_launcher::credential_tool::tests::"],
                3,
            )
        )
    for name, package, selection, minimum in suites:
        command = [
            "cargo",
            "test",
            "--package",
            package,
            "--features",
            "native-tests",
            "--locked",
            *selection,
        ]
        # Preserve the compiler process-group supervisor, including on signal.
        # Its stdout is suppressed, so collect only this test run's bounded
        # output in a private scratch directory rather than claiming native scope.
        with tempfile.TemporaryDirectory(prefix="exchange-output-") as directory:
            output_path = Path(directory) / "result"
            capture = [
                "/bin/sh",
                "-c",
                'umask 077; output=$1; shift; exec "$@" > "$output"',
                "exchange-test-output",
                str(output_path),
                *command,
            ]
            code, _ = driver.bounded_process(capture, environment, timeout=120.0)
            with output_path.open("rb") as stream:
                output = stream.read(32769)
        counts = re.findall(rb"test result: ok\. (\d+) passed; 0 failed;", output)
        if len(output) > 32768:
            raise driver.TaskFailure("driver-invalid-output")
        if code != 0 or len(counts) != 1 or not minimum <= int(counts[0]) <= 128:
            driver.emit(name, "red")
            return 1
        driver.emit(name, "pass")
    if not guard_fixture(driver, environment) or not receiver_entry_fixture(driver, environment):
        return 1
    driver.emit("driver", "exchange-green-native-authority-gated")
    return 0


def guard_fixture(driver, environment):
    """Build a standalone peer so native fork starts from a single thread."""
    with tempfile.TemporaryDirectory(prefix="retriever-guard-") as directory:
        build = Path(directory)
        artifacts = build / "artifacts"
        command = [
            "/bin/sh",
            "-c",
            'umask 077; output=$1; shift; exec "$@" > "$output"',
            "guard-library",
            str(artifacts),
            "cargo",
            "build",
            "--package",
            "spaceterm-host",
            "--features",
            "native-tests,test-support" if sys.platform == "linux" else "native-tests",
            "--locked",
            "--lib",
            "--message-format=json",
        ]
        code, _ = driver.bounded_process(command, environment, timeout=120.0)
        if code != 0:
            driver.emit("guard-build", "red")
            return False
        with artifacts.open("rb") as stream:
            data = stream.read(2_097_153)
        if len(data) > 2_097_152:
            raise driver.TaskFailure("driver-invalid-output")
        libraries = []
        for line in data.splitlines():
            try:
                record = json.loads(line)
            except (ValueError, UnicodeError):
                raise driver.TaskFailure("driver-invalid-output") from None
            if (
                record.get("reason") == "compiler-artifact"
                and record.get("target", {}).get("name") == "spaceterm_host"
            ):
                libraries.extend(
                    Path(name) for name in record.get("filenames", []) if name.endswith(".rlib")
                )
        if len(libraries) != 1:
            raise driver.TaskFailure("driver-invalid-output")
        library = libraries[0]
        binary = build / "guard-peer"
        command = [
            "rustc",
            "--edition=2024",
            "-Dwarnings",
            str(ROOT / "crates/spaceterm-host/tests/native_handoff/guard_fixture.rs"),
            "--extern",
            f"spaceterm_host={library}",
            "-L",
            f"dependency={library.parent / 'deps'}",
            "-o",
            str(binary),
        ]
        code, _ = driver.bounded_process(command, environment, timeout=60.0)
        if code != 0:
            driver.emit("guard-build", "red")
            return False
        code, data = driver.bounded_process([str(binary)], environment, timeout=10.0, native=True)
        original = driver.CASES
        driver.CASES = (
            "retriever-guard-exact-retirement",
            "retriever-timeout-retires-fixed-worker",
        )
        try:
            records = driver.validate_records(data, code)
        finally:
            driver.CASES = original
        for record in records:
            driver.emit(record["case"], record["result"])
        if code != 0:
            return False
        return sys.platform != "linux" or credential_executor_fixture(
            driver, environment, library, build
        )


def credential_executor_fixture(driver, environment, library, build):
    """Use an explicitly injected release lease, not a second output recognizer."""
    fixture = ROOT / "crates/spaceterm-host/tests/native_handoff"
    tool, replacement = build / "synthetic-tool", build / "replacement-tool"
    for binary, defines in ((tool, []), (replacement, ["-DFIXTURE_REPLACEMENT"])):
        command = [
            "cc",
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-Wpedantic",
            *defines,
            str(fixture / "credential_tool.c"),
            "-o",
            str(binary),
        ]
        code, _ = driver.bounded_process(command, environment, timeout=60.0)
        if code != 0:
            driver.emit("credential-executor-build", "red")
            return False
    binary = build / "credential-executor-peer"
    command = [
        "rustc",
        "--edition=2024",
        "-Dwarnings",
        str(fixture / "credential_executor_fixture.rs"),
        "--extern",
        f"spaceterm_host={library}",
        "-L",
        f"dependency={library.parent / 'deps'}",
        "-o",
        str(binary),
    ]
    code, _ = driver.bounded_process(command, environment, timeout=60.0)
    if code != 0:
        driver.emit("credential-executor-build", "red")
        return False
    scratch = build / "credential-cases"
    scratch.mkdir(mode=0o700)
    code, data = driver.bounded_process(
        [str(binary), str(tool), str(replacement), str(scratch)],
        environment,
        timeout=15.0,
        native=True,
    )
    original = driver.CASES
    driver.CASES = (
        "retained-tool-descriptor-exec",
        "retained-tool-short-output",
        "retained-tool-oversize-output",
        "retained-tool-requires-eof",
        "retained-tool-requires-zero-exit",
        "retained-tool-timeout-reaped-and-wiped",
        "retained-tool-empty-output",
        "wrong-recipe-zero-tool-spawns",
        "retired-generation-zero-tool-spawns",
        "retired-generation-zero-delivery",
    )
    try:
        records = driver.validate_records(data, code)
    finally:
        driver.CASES = original
    for record in records:
        driver.emit(record["case"], record["result"])
    return code == 0


def receiver_entry_fixture(driver, environment):
    """Execute the actual native entry object without any privileged startup."""
    with tempfile.TemporaryDirectory(prefix="receiver-entry-") as directory:
        binary = Path(directory) / "entry-peer"
        native = ROOT / "crates/spaceterm-host/native"
        source = native / (
            "macos/receiver_entry.c" if sys.platform == "darwin" else "linux/receiver/entry.c"
        )
        command = [
            "cc",
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-Wpedantic",
            "-I",
            str(native / "include"),
        ]
        if sys.platform == "darwin":
            command.extend(["-arch", "arm64", "-mmacosx-version-min=26.0"])
        command.extend(
            [
                str(source),
                str(ROOT / "crates/spaceterm-host/tests/native_handoff/receiver_entry.c"),
                "-o",
                str(binary),
            ]
        )
        code, _ = driver.bounded_process(command, environment, timeout=60.0)
        if code != 0:
            driver.emit("receiver-entry-build", "red")
            return False
        code, data = driver.bounded_process([str(binary)], environment, timeout=10.0, native=True)
        cases = (
            "entry-protects-all-descriptors",
            "entry-wrong-direction-closes-all",
            "entry-missing-descriptor-closes-all",
        )
        if sys.platform == "linux":
            cases += ("entry-dumpable-one-rejects",)
        original = driver.CASES
        driver.CASES = cases
        try:
            records = driver.validate_records(data, code)
        finally:
            driver.CASES = original
        for record in records:
            driver.emit(record["case"], record["result"])
        return code == 0


def entry():
    if sys.argv[1:] or os.getuid() == 0 or os.geteuid() == 0:
        return 2
    driver = load(
        ROOT / "mise-tasks/server/native/macos/handoff_tests.py", "_exchange_test_supervisor"
    )
    driver.SCOPE = "startup-exchange-composition"
    driver.run_task = lambda **_kwargs: run(driver)
    return driver.entry()


if __name__ == "__main__":
    raise SystemExit(entry())
