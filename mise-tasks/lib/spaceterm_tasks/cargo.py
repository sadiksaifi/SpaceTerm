"""Build one executable and return the path Cargo reports for it."""

import json
import os
import subprocess
import sys
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError
from spaceterm_tasks.processes import OWN_GROUP, stop_group, terminating_signals_raise


def build_executable(*cargo_args, binary="spaceterm", env=None):
    """Build `binary` and return Cargo's artifact path, never a stale guess under target/."""
    command = [
        "cargo",
        "build",
        "--bin",
        binary,
        "--message-format=json-render-diagnostics",
        *cargo_args,
    ]
    with terminating_signals_raise():
        process = subprocess.Popen(
            command,
            cwd=ROOT,
            env=env,
            stdout=subprocess.PIPE,
            text=True,
            start_new_session=OWN_GROUP,
        )
        try:
            executables = read_executables(process, binary)
            returncode = process.wait()
        except BaseException:
            # An interrupted task must not leave Cargo, rustc, or Zig building in the background.
            stop_group(process)
            raise
    if returncode:
        raise TaskError(f"cargo build failed for {binary}")
    if len(executables) != 1:
        raise TaskError(f"Cargo reported {len(executables)} executable paths for {binary}")
    executable = executables.pop()
    if not os.access(executable, os.X_OK):
        raise TaskError(f"Cargo did not produce an executable {binary}")
    return executable


def read_executables(process, binary):
    """Relay Cargo's diagnostics and collect the paths it reports for `binary`."""
    assert process.stdout is not None  # stdout=PIPE
    executables = set()
    for line in process.stdout:
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            print(line, end="")
            continue
        rendered = message.get("message", {}).get("rendered")
        if rendered:
            print(rendered, end="", file=sys.stderr)
        if (
            message.get("reason") == "compiler-artifact"
            and message.get("target", {}).get("name") == binary
            and message.get("executable")
        ):
            executables.add(Path(message["executable"]).resolve())
    return executables
