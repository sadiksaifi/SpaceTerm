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


def reexec_with(requirements):
    """Re-execute the current task under a virtual environment holding `requirements`."""
    requirements = Path(requirements)
    environment = ROOT / "target" / "task-venvs" / requirements.parent.name
    if Path(sys.prefix).resolve() == environment.resolve():
        return
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
            ]
        )
        if installed.returncode:
            raise TaskError("the task requirements could not be installed")
        marker.write_text(digest)
    os.execv(python, [str(python), *sys.argv])
