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


def sign_framework(framework, sign):
    """Remove Sparkle's XPC Services, then call `sign` on its code inside out.

    The order is Sparkle's Developer ID order: Autoupdate, Updater.app, then the framework.
    The XPC Services serve only sandboxed apps that enable them in Info.plist. SpaceTerm is not
    sandboxed and enables neither, so they are removed rather than signed.
    """
    version = framework / "Versions" / "B"
    current = framework / "Versions" / "Current"
    if not current.is_symlink() or current.resolve() != version.resolve():
        raise TaskError("Sparkle.framework does not hold version B")
    link = framework / "XPCServices"
    if link.is_symlink():
        link.unlink()
    shutil.rmtree(version / "XPCServices", ignore_errors=True)
    if link.exists() or (version / "XPCServices").exists():
        raise TaskError("Sparkle's XPC Services could not be removed")
    for code in (version / "Autoupdate", version / "Updater.app", framework):
        if not code.exists():
            raise TaskError(f"Sparkle.framework is missing {code.name}")
        sign(code)
