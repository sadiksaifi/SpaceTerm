"""Build one executable and return the path Cargo reports for it."""

import json
import os
import subprocess
import sys
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError
from spaceterm_tasks.processes import OWN_GROUP, stop_group, terminating_signals_raise


def build_executable(
    *cargo_args,
    binary="spaceterm-desktop",
    example=False,
    env=None,
    portable_linux=False,
    target=None,
    metadata=None,
):
    """Return Cargo's executable path and, if requested, its package's emitted metadata.

    With `example`, `binary` names a Cargo example instead of a binary target.
    """
    build = ["build"]
    if target is not None:
        if portable_linux:
            raise TaskError("select either a headless target or the desktop Linux toolchain")
        build = ["zigbuild", "--target", target]
    if portable_linux:
        from spaceterm_tasks.linux_toolchain import build_environment

        target, env = build_environment(env if env is not None else os.environ)
        build = ["zigbuild", "--target", target]
    command = [
        "cargo",
        *build,
        "--example" if example else "--bin",
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
            packages = {}
            outputs = {}
            executables = read_executables(process, binary, packages=packages, outputs=outputs)
            returncode = process.wait()
        except BaseException:
            # An interrupted task must not leave Cargo, rustc, or Zig building in the background.
            stop_group(process)
            raise
        finally:
            if process.stdout is not None:
                process.stdout.close()
    if returncode:
        raise TaskError(f"cargo build failed for {binary}")
    if len(executables) != 1:
        raise TaskError(f"Cargo reported {len(executables)} executable paths for {binary}")
    executable = executables.pop()
    if not os.access(executable, os.X_OK):
        raise TaskError(f"Cargo did not produce an executable {binary}")
    if metadata is not None:
        package_ids = packages.get(executable, set())
        directories = {path for package in package_ids for path in outputs.get(package, set())}
        if len(package_ids) != 1 or len(directories) != 1:
            raise TaskError(f"Cargo did not report unambiguous build metadata for {binary}")
        try:
            data = (directories.pop() / metadata).read_bytes()
        except OSError:
            raise TaskError(f"Cargo build metadata is unavailable for {binary}") from None
        return executable, data
    return executable


def read_executables(process, binary, *, packages=None, outputs=None):
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
        if message.get("reason") == "build-script-executed" and outputs is not None:
            outputs.setdefault(message["package_id"], set()).add(Path(message["out_dir"]))
        if (
            message.get("reason") == "compiler-artifact"
            and message.get("target", {}).get("name") == binary
            and message.get("executable")
        ):
            executable = Path(message["executable"]).resolve()
            executables.add(executable)
            if packages is not None:
                packages.setdefault(executable, set()).add(message.get("package_id"))
    return executables
