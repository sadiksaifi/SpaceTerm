#!/usr/bin/env python3
"""Register source-built Development in the desktop's shared application registry.

This metadata belongs in XDG_DATA_HOME/applications, outside SpaceTerm's private
AppDirectories. The desktop owns discovery; each source launch replaces only the
Development identity's entry so its Exec follows the current source prefix.
"""

import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

APPLICATION_ID = "io.github.sadiksaifi.spaceterm-development"
APPLICATION_NAME = "SpaceTerm Development"


def desktop_registry() -> Path:
    override = os.environ.get("XDG_DATA_HOME", "")
    if override and Path(override).is_absolute():
        return Path(override) / "applications"
    home = os.environ.get("HOME", "")
    if not home or not Path(home).is_absolute():
        raise ValueError("an absolute HOME or XDG_DATA_HOME is required")
    return Path(home) / ".local" / "share" / "applications"


def exec_argument(source: str) -> str:
    # Desktop Entry escaping has two layers: Exec argument quoting, then string
    # value escaping. A literal percent must also escape field-code expansion.
    if any(ord(character) < 32 or ord(character) == 127 for character in source):
        raise ValueError("the source path contains characters unsupported by desktop launchers")
    quoted = "".join(
        "\\" + character if character in '\\"`$' else "%%" if character == "%" else character
        for character in source
    )
    return '"' + quoted.replace("\\", "\\\\") + '"'


def exec_value(executable: Path) -> str:
    # GIO validates the first executable before expanding literal percent codes.
    # Keep that token stable and pass the source path as an argument. Python is
    # already required by this launcher, and execv preserves exact argv without
    # shell evaluation or env interpreting an '=' in the source path as a name.
    env = shutil.which("env", path=os.defpath)
    if env is None:
        raise ValueError("the env executable is required")
    return " ".join(exec_argument(argument) for argument in (
        env, "--", "python3", "-I", "-c",
        "import os, sys; os.execv(sys.argv[1], sys.argv[1:])",
        str(executable),
    ))


def atomic_write(path: Path, contents: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=path.parent, delete=False) as file:
            temporary = Path(file.name)
            file.write(contents)
            file.flush()
            os.fchmod(file.fileno(), 0o644)
            os.fsync(file.fileno())
        os.replace(temporary, path)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def register(prefix: Path) -> None:
    executable = prefix / "bin" / "spaceterm"
    if not prefix.is_absolute() or not executable.is_file() or not os.access(executable, os.X_OK):
        raise ValueError("a staged Development executable is required")
    destination = desktop_registry() / (APPLICATION_ID + ".desktop")
    entry = prefix / "share" / "applications" / destination.name
    contents = (
        "[Desktop Entry]\n"
        "Type=Application\n"
        f"Name={APPLICATION_NAME}\n"
        f"Exec={exec_value(executable)}\n"
        "Icon=utilities-terminal\n"
        "Terminal=false\n"
        "StartupNotify=true\n"
        f"StartupWMClass={APPLICATION_ID}\n"
        "Categories=System;TerminalEmulator;\n"
    )
    atomic_write(entry, contents)
    subprocess.run(
        ["desktop-file-validate", str(entry)],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    atomic_write(destination, contents)


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: development-desktop-linux.py PREFIX", file=sys.stderr)
        return 2
    try:
        register(Path(sys.argv[1]))
    except (OSError, ValueError, subprocess.CalledProcessError):
        print("error: could not register the Development desktop entry; run mise run doctor:linux", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
