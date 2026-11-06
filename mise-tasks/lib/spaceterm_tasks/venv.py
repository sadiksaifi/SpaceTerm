"""Run a task inside a private virtual environment with hash-pinned requirements.

Tasks use the standard library; this is only for the rare task that needs a third-party
package, such as fonttools for the fonts tasks.
"""

import hashlib
import os
import subprocess
import sys
import venv
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError


def python(requirements, env=None):
    """The Python of a private virtual environment holding `requirements`, created on first use.

    The installer runs with `env` when given, and otherwise inherits this process's environment.
    """
    requirements = Path(requirements)
    environment = ROOT / "target" / "task-venvs" / requirements.parent.name
    python = environment / ("Scripts/python.exe" if os.name == "nt" else "bin/python")
    marker = environment / "requirements.sha256"
    digest = hashlib.sha256(requirements.read_bytes()).hexdigest()
    if not marker.is_file() or marker.read_text() != digest:
        venv.EnvBuilder(clear=True, with_pip=True).create(environment)
        installed = subprocess.run(
            [
                python,
                "-m",
                "pip",
                "install",
                "--quiet",
                "--no-deps",
                "--require-hashes",
                "-r",
                requirements,
            ],
            env=env,
        )
        if installed.returncode:
            raise TaskError("the task requirements could not be installed")
        marker.write_text(digest)
    return python


def reexec_with(requirements):
    """Re-execute the current task under a virtual environment holding `requirements`."""
    environment = ROOT / "target" / "task-venvs" / Path(requirements).parent.name
    if Path(sys.prefix).resolve() == environment.resolve():
        return
    interpreter = python(requirements)
    os.execv(interpreter, [str(interpreter), *sys.argv])
