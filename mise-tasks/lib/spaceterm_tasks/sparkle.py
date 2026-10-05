"""Locate the Sparkle distribution mise installs from the pinned, checksummed release."""

import shutil
from pathlib import Path

from spaceterm_tasks import TaskError

ACCOUNT = "io.github.sadiksaifi.spaceterm"


def directory():
    """Return the distribution holding bin/ and Sparkle.framework.

    mise puts the distribution's bin/ on PATH for every task.
    """
    tool = shutil.which("sign_update")
    home = Path(tool).resolve().parents[1] if tool else None
    if home is None or not (home / "Sparkle.framework").is_dir():
        raise TaskError("Sparkle is unavailable; run mise install")
    return home


def tool(name):
    return directory() / "bin" / name
