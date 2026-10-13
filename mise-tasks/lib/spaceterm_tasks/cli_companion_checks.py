"""Check the headless management CLI without a display, session bus, or desktop installation.

`no-display` runs spaceterm-cli with only a fixed PATH and a private, empty home and
temporary directory. No display, Wayland, D-Bus session, XDG runtime, SSH agent, or terminal
variable reaches it, and standard input is not a terminal. Help, version, and status must
answer. Validation never submits Start, Stop, enrollment, or serve, even when a real
account service is installed. Status may return existing read-only facts. A case also fails when the CLI writes into its private
directories or leaves a process running, as a created listener would. The check installs
nothing, starts no service, and never requests privilege.
"""

import json
import os
import re
import selectors
import subprocess
import sys
import tempfile
import time
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

from spaceterm_tasks import TaskError, main
from spaceterm_tasks.cargo import build_executable
from spaceterm_tasks.processes import OWN_GROUP, group_alive, stop_group

PATH = "/usr/bin:/bin"
CASE_TIMEOUT = 10
# One structured CLI result is at most 64 KiB.
OUTPUT_LIMIT = 64 * 1024
# How often a case that has not closed its output is checked for having exited.
POLL_INTERVAL = 0.05
USAGE = "usage: cli_companion_checks no-display [--executable PATH]"


def record(data):
    """A schema 1 JSON object, or None."""
    try:
        value = json.loads(data)
    except (ValueError, UnicodeDecodeError):
        return None
    return value if isinstance(value, dict) and value.get("schema") == 1 else None


def usage_text(stdout, stderr):
    return not stderr and stdout.startswith(b"Usage: spaceterm-cli ")


def version_text(stdout, stderr):
    return not stderr and re.fullmatch(rb"spaceterm-cli [!-~]+\n", stdout) is not None


def usage_record(stdout, stderr):
    value = record(stdout)
    return not stderr and value is not None and isinstance(value.get("usage"), str)


def version_record(stdout, stderr):
    value = record(stdout)
    return (
        not stderr
        and value is not None
        and isinstance(value.get("version"), str)
        and re.fullmatch(r"spaceterm-cli [!-~]+", value["version"]) is not None
    )


def passive_status(stdout, stderr):
    """Status may discover an established account service but must remain read-only."""
    value = record(stdout)
    result = value.get("result") if value is not None else None
    if stderr or not isinstance(result, dict) or result.get("kind") != "status":
        return False
    native_failure = result.get("native_failure")
    if native_failure is not None:
        return (
            native_failure
            in ("native_unavailable", "missing", "storage_locked", "storage_unavailable")
            and result.get("native") is None
            and result.get("authenticated") is None
            and result.get("negotiated_protocol_major") is None
        )
    return (
        isinstance(result.get("native"), dict)
        and (result.get("authenticated") is None or isinstance(result.get("authenticated"), dict))
        and result.get("negotiated_protocol_major") in (None, 1, 2)
    )


def failure(kind):
    def check(stdout, stderr):
        value = record(stderr)
        return not stdout and value is not None and value.get("error") == kind

    return check


@dataclass(frozen=True)
class Case:
    arguments: tuple[str, ...]
    exit_code: int
    check: Callable[[bytes, bytes], bool]


CASES = (
    Case(("--help",), 0, usage_text),
    Case(("--version",), 0, version_text),
    Case(("--json",), 0, usage_record),
    Case(("--json", "--help"), 0, usage_record),
    Case(("--json", "--version"), 0, version_record),
    Case(("--json", "status"), 0, passive_status),
    Case(("--json", "invalid-command"), 2, failure("usage")),
)


def no_display_environment(home, temporary):
    """Only a fixed PATH and private directories; no graphical, session, or terminal value."""
    return {"PATH": PATH, "HOME": str(home), "TMPDIR": str(temporary)}


def collect(process, deadline):
    """Read stdout and stderr without blocking until both close, one exceeds its bound,
    or `deadline` passes.

    Returns the output by stream name and whether both streams closed. Once the CLI has
    exited, only output already in the pipes remains, so a pipe still open after it is read
    is held by a descendant. Such a descendant may have left the case's process group, so no
    read waits for it.
    """
    output = {"stdout": bytearray(), "stderr": bytearray()}
    with selectors.DefaultSelector() as selector:
        for name, stream in (("stdout", process.stdout), ("stderr", process.stderr)):
            os.set_blocking(stream.fileno(), False)
            selector.register(stream.fileno(), selectors.EVENT_READ, name)
        exited = False
        while selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return output, False
            events = selector.select(0 if exited else min(remaining, POLL_INTERVAL))
            if exited and not events:
                return output, False
            for key, _ in events:
                try:
                    chunk = os.read(key.fd, OUTPUT_LIMIT + 1)
                except BlockingIOError:
                    continue
                if not chunk:
                    selector.unregister(key.fd)
                    continue
                output[key.data] += chunk
                if len(output[key.data]) > OUTPUT_LIMIT:
                    return output, False
            exited = exited or process.poll() is not None
    return output, True


def run_case(executable, case, environment, directory, label):
    """Run one case in its own process group. Returns its exit status, stdout, and stderr."""
    process = subprocess.Popen(
        [str(executable), *case.arguments],
        cwd=directory,
        env=environment,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=OWN_GROUP,
    )
    deadline = time.monotonic() + CASE_TIMEOUT
    finished = lingering = False
    try:
        output, closed = collect(process, deadline)
        try:
            process.wait(timeout=max(0, deadline - time.monotonic()))
            finished = True
        except subprocess.TimeoutExpired:
            pass
        lingering = finished and group_alive(process.pid)
    finally:
        # Stopping an exited group only reaps it. A descendant cannot outlive the check
        # unless it left the group, and then its open pipes report it below.
        stop_group(process)
        for stream in (process.stdout, process.stderr):
            if stream is not None:
                stream.close()
    if any(len(output[name]) > OUTPUT_LIMIT for name in ("stdout", "stderr")):
        raise TaskError(f"no-display {label}: exceeded its 64 KiB output bound")
    if not finished:
        raise TaskError(f"no-display {label}: did not finish within {CASE_TIMEOUT} seconds")
    if lingering or not closed:
        raise TaskError(f"no-display {label}: left a process running after it exited")
    return process.returncode, bytes(output["stdout"]), bytes(output["stderr"])


def no_display(executable):
    """Run every case against `executable` and return how many passed."""
    with tempfile.TemporaryDirectory(prefix="spaceterm-no-display-") as sandbox:
        private = [Path(sandbox) / name for name in ("home", "tmp", "work")]
        for directory in private:
            directory.mkdir(mode=0o700)
        home, temporary, work = private
        for case in CASES:
            label = " ".join(case.arguments)
            returncode, stdout, stderr = run_case(
                executable, case, no_display_environment(home, temporary), work, label
            )
            if any(any(directory.iterdir()) for directory in private):
                raise TaskError(
                    f"no-display {label}: wrote to its private home, temporary, "
                    "or working directory"
                )
            if returncode != case.exit_code:
                raise TaskError(
                    f"no-display {label}: exited {returncode}, expected {case.exit_code}"
                )
            if not case.check(stdout, stderr):
                raise TaskError(f"no-display {label}: reported an unexpected result")
            print(f"no-display {label}: exit {returncode}")
    print(f"{len(CASES)} no-display checks passed without a display, session bus, or desktop")
    return len(CASES)


def run(arguments):
    """`no-display` builds the development CLI; `--executable` checks a packaged one."""
    arguments = list(arguments)
    if arguments == ["no-display"]:
        executable = build_executable(
            "--locked", "-p", "spaceterm-client-native", "--features", "cli", binary="spaceterm-cli"
        )
    elif len(arguments) == 3 and arguments[:2] == ["no-display", "--executable"]:
        executable = Path(arguments[2]).resolve()
        if not executable.is_file() or not os.access(executable, os.X_OK):
            raise TaskError("the headless executable to check is missing or not executable")
    else:
        raise TaskError(USAGE)
    no_display(executable)
    return 0


if __name__ == "__main__":
    main(lambda: run(sys.argv[1:]))
