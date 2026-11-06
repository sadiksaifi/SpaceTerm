"""Run source-built SpaceTerm Development from a private Linux prefix.

The prefix mirrors an installed layout, so resources resolve from <prefix>/share/spaceterm.
Desktop metadata belongs in XDG_DATA_HOME/applications, outside SpaceTerm's private
AppDirectories. The desktop owns discovery; each source launch replaces only the
Development identity's entry so its Exec follows the current source prefix.
"""

import os
import shutil
import subprocess
import tempfile
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError
from spaceterm_tasks.cargo import build_executable
from spaceterm_tasks.development_supervision import development_run, source_environment, supervise
from spaceterm_tasks.packaging import install_linux_icons, linux_icon_path

APPLICATION_ID = "com.peblon.spaceterm-development"
DESKTOP_ENTRY = Path("packaging/linux/development") / (APPLICATION_ID + ".desktop")
TERMINFO = Path("assets/terminfo/xterm-spaceterm.terminfo")
SHELL_INTEGRATION = Path("assets/shell-integration")
REQUIRED_TOOLS = ("tic", "desktop-file-validate")


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
    return " ".join(
        exec_argument(argument)
        for argument in (
            env,
            "--",
            "python3",
            "-I",
            "-c",
            "import os, sys; os.execv(sys.argv[1], sys.argv[1:])",
            str(executable),
        )
    )


def icon_value(icon: Path) -> str:
    # An absolute path starts with "/", so only backslashes need string value escaping.
    return str(icon).replace("\\", "\\\\")


def atomic_write(path: Path, contents: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w", encoding="utf-8", dir=path.parent, delete=False
        ) as file:
            temporary = Path(file.name)
            file.write(contents)
            file.flush()
            os.fchmod(file.fileno(), 0o644)
            os.fsync(file.fileno())
        os.replace(temporary, path)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def register(prefix: Path, root: Path = ROOT) -> None:
    executable = prefix / "bin" / "spaceterm-desktop"
    if not prefix.is_absolute() or not executable.is_file() or not os.access(executable, os.X_OK):
        raise ValueError("a staged Development executable is required")
    destination = desktop_registry() / DESKTOP_ENTRY.name
    entry = prefix / "share" / "applications" / DESKTOP_ENTRY.name
    # The source prefix holds the icon, so the entry follows it like Exec does.
    icon = prefix / "share" / linux_icon_path(APPLICATION_ID)
    if any(ord(character) < 32 or ord(character) == 127 for character in str(icon)):
        raise ValueError("the source path contains characters unsupported by desktop launchers")
    if not icon.is_file():
        raise ValueError("a staged Development icon is required")
    template = (root / DESKTOP_ENTRY).read_text(encoding="utf-8")
    contents = template.replace("{exec}", exec_value(executable)).replace(
        "{icon}", icon_value(icon)
    )
    atomic_write(entry, contents)
    subprocess.run(
        ["desktop-file-validate", str(entry)],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    atomic_write(destination, contents)


def stage_prefix(
    executable: Path, core: Path, root: Path = ROOT, *, catalog: Path | None = None
) -> Path:
    """Stage an installed layout beside Cargo's artifact and rename it into place."""
    parent = executable.parent.parent / "development-apps"
    parent.mkdir(parents=True, exist_ok=True)
    staging = Path(tempfile.mkdtemp(prefix=".prefix.", dir=parent))
    try:
        (staging / "bin").mkdir()
        shutil.copyfile(executable, staging / "bin" / "spaceterm-desktop")
        (staging / "bin" / "spaceterm-desktop").chmod(0o755)
        shutil.copyfile(core, staging / "bin" / "spaceterm")
        (staging / "bin" / "spaceterm").chmod(0o755)
        resources = staging / "share" / "spaceterm"
        shutil.copytree(root / SHELL_INTEGRATION, resources / "shell-integration")
        compiled = subprocess.run(
            ["tic", "-x", "-o", str(resources / "terminfo"), str(root / TERMINFO)]
        )
        if compiled.returncode or not (resources / "terminfo" / "x" / "xterm-spaceterm").is_file():
            raise TaskError("tic did not produce the SpaceTerm terminfo entry")
        install_linux_icons(APPLICATION_ID, staging / "share", root)
        if catalog is not None:
            shutil.copytree(catalog, resources / "core-artifacts")
        prefix = parent / "development"
        shutil.rmtree(prefix, ignore_errors=True)
        staging.rename(prefix)
    except BaseException:
        shutil.rmtree(staging, ignore_errors=True)
        raise
    return prefix


def launch(backend: str | None = None) -> int | None:
    """Build and own the current client and foreground Server for this Development run."""
    from spaceterm_tasks.core_builds import prepare

    with (
        development_run(),
        tempfile.TemporaryDirectory(prefix="spaceterm-development-catalog-") as temporary,
    ):
        environment = source_environment(os.environ)
        if backend == "wayland" and not environment.get("WAYLAND_DISPLAY"):
            raise TaskError("Wayland requires WAYLAND_DISPLAY")
        if backend == "x11":
            if not environment.get("DISPLAY"):
                raise TaskError("X11 requires DISPLAY")
            environment.pop("WAYLAND_DISPLAY", None)
        for tool in REQUIRED_TOOLS:
            if shutil.which(tool) is None:
                raise TaskError(f"{tool} is required; run mise doctor project")
        catalog = prepare(Path(temporary) / "catalog", "spaceterm-development")
        core = build_executable(
            "--package",
            "spaceterm-cli",
            "--no-default-features",
            "--locked",
            binary="spaceterm",
            env=environment,
        )
        prefix = stage_prefix(build_executable("--locked", env=environment), core, catalog=catalog)
        try:
            register(prefix)
        except (OSError, ValueError, subprocess.CalledProcessError):
            raise TaskError(
                "could not register the Development desktop entry; run mise doctor project"
            ) from None
        return supervise(
            [str(prefix / "bin/spaceterm"), "internal", "serve", "--owned"],
            [str(prefix / "bin/spaceterm-desktop"), "--new-instance"],
            environment,
        )
