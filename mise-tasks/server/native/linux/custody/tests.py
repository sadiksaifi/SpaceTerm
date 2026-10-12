#!/usr/bin/env -S python -I
"""Unprivileged custody controls. These are not disposable Jammy evidence."""

import argparse
import os
import selectors
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(ROOT / "mise-tasks/lib"))

from spaceterm_tasks import TaskError, main  # noqa: E402
from spaceterm_tasks.processes import stop_group  # noqa: E402


def bounded(command, *, timeout):
    environment = {"PATH": "/usr/bin:/bin", "LC_ALL": "C"}
    process = None
    pending = []
    previous = {}
    deadline = time.monotonic() + timeout
    # The outer retirement scope precedes acquisition, including restoration of
    # handlers that can raise as soon as they become active again.
    try:
        try:
            for number in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
                previous[number] = signal.signal(
                    number, lambda number, frame: pending.append((number, frame))
                )
            process = subprocess.Popen(
                command,
                cwd=ROOT,
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
            )
        finally:
            active = sys.exception()
            first = None
            for number, handler in previous.items():
                try:
                    signal.signal(number, handler)
                except BaseException as error:
                    if first is None:
                        first = error
            if first is not None and active is None:
                raise first
        for number, frame in pending:
            handler = previous[number]
            if callable(handler):
                handler(number, frame)
            elif handler != signal.SIG_IGN:
                raise SystemExit(128 + number)
        output = bytearray()
        totals = {}
        with selectors.DefaultSelector() as selector:
            for stream in (process.stdout, process.stderr):
                assert stream is not None
                os.set_blocking(stream.fileno(), False)
                selector.register(stream.fileno(), selectors.EVENT_READ)
                totals[stream.fileno()] = 0
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TaskError("timed-out")
                for key, _ in selector.select(min(remaining, 0.1)):
                    descriptor = key.fileobj
                    assert type(descriptor) is int
                    data = os.read(descriptor, 4096)
                    if not data:
                        selector.unregister(key.fileobj)
                        continue
                    totals[descriptor] += len(data)
                    if totals[descriptor] > 8192:
                        raise TaskError("limit-exceeded")
                    assert process.stdout is not None
                    if descriptor == process.stdout.fileno():
                        output.extend(data)
        status = process.wait(timeout=max(0.01, deadline - time.monotonic()))
        return status, bytes(output)
    except subprocess.TimeoutExpired:
        raise TaskError("timed-out") from None
    finally:
        if process is not None:
            active = sys.exception()
            # Retirement is bounded. Defer further terminating signals until the
            # owned group has been settled and its pipe descriptors are closed.
            # This mask is installed after spawning, never inherited by the child.
            mask = signal.pthread_sigmask(signal.SIG_BLOCK, set(previous))
            try:
                try:
                    stop_group(process, timeout=1)
                finally:
                    for stream in (process.stdout, process.stderr):
                        if stream is not None:
                            stream.close()
            finally:
                try:
                    signal.pthread_sigmask(signal.SIG_SETMASK, mask)
                except BaseException:
                    if active is None:
                        raise


def test(sanitizers=False):
    if sys.platform != "linux" or os.getuid() == 0 or os.geteuid() == 0:
        raise TaskError("unsupported-host")
    suite = unittest.defaultTestLoader.loadTestsFromName("tests.test_linux_custody_evidence")
    count = suite.countTestCases()
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    if not count or not result.wasSuccessful() or result.skipped or result.testsRun != count:
        raise TaskError("invalid-record")
    with tempfile.TemporaryDirectory(prefix="spaceterm-custody-build-") as directory:
        binary = Path(directory) / "controls"
        source = ROOT / "tests/server_native/linux"
        command = [
            "/usr/bin/cc",
            "-std=c11",
            "-O1",
            "-g",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-Wpedantic",
            "-fno-omit-frame-pointer",
        ]
        if sanitizers:
            command += ["-fsanitize=address,undefined"]
        command += [
            str(source / name)
            for name in (
                "memory_probe.c",
                "dac_alias_probe.c",
                "storage_probe.c",
                "custody/probe_tests.c",
            )
        ]
        command += ["-o", str(binary)]
        status, _ = bounded(command, timeout=60)
        if status:
            raise TaskError("fixture-build-failed")
        status, output = bounded([str(binary)], timeout=20)
        if status or output != b"classification=mechanics-passed\n":
            raise TaskError("fixture-controls-failed")
    print(
        f"Executed {count} driver tests and 3 native control groups; privileged evidence remains unrun."
    )


class Arguments(argparse.ArgumentParser):
    def error(self, message):
        del message
        print("classification=invalid-request", file=sys.stderr)
        raise SystemExit(2)


if __name__ == "__main__":
    if not sys.flags.isolated:
        print("classification=isolated-python-required", file=sys.stderr)
        raise SystemExit(2)
    parser = Arguments(description=__doc__)
    parser.add_argument("--sanitizers", action="store_true")
    arguments = parser.parse_args()
    main(lambda: test(arguments.sanitizers))
