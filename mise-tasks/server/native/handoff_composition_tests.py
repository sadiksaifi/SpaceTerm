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
    for name, selection, minimum in (
        ("native-crypto-and-reaping", ["--lib", "startup_exchange::"], 4),
        ("exported-host-exchange", ["--test", "startup_exchange"], 22),
    ):
        command = [
            "cargo",
            "test",
            "--package",
            "spaceterm-host",
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
    driver.emit("driver", "exchange-green-native-authority-gated")
    return 0


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
