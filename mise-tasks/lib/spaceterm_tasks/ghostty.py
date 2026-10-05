"""Run a prepared Ghostty source copy with authority from an explicit local directory."""

import subprocess
from pathlib import Path

from spaceterm_tasks import TaskError
from spaceterm_tasks.processes import OWN_GROUP, stop_group


def run_native_tests(source, *, test_filter=None):
    """Keep native fixture lookup inside the caller-selected prepared source directory."""
    source = Path(source).resolve()
    if not source.is_dir() or not (source / "build.zig").is_file():
        raise TaskError("prepared Ghostty source directory is missing its build file")
    command = [
        "zig",
        "build",
        "test-lib-vt",
        "-Demit-lib-vt=true",
        "-Demit-xcframework=false",
        "-Dapp-runtime=none",
        "-Dcpu=baseline",
    ]
    if test_filter is not None:
        command.append(f"-Dtest-filter={test_filter}")
    process = subprocess.Popen(command, cwd=source, start_new_session=OWN_GROUP)
    try:
        return process.wait()
    except BaseException:
        stop_group(process)
        raise
