"""Package and verify the Preflight or release x86_64 Linux archive.

Only a release tag selects the SpaceTerm identity; every other package is SpaceTerm
Preflight. The archive holds one directory in the installed layout, so the executable at
bin/spaceterm reads its resources from share/spaceterm beside it.
"""

import os
import platform
import re
import shutil
import sys
import tarfile
import tempfile
import xml.etree.ElementTree as ElementTree
from pathlib import Path, PurePosixPath

from spaceterm_tasks import ROOT, TaskError
from spaceterm_tasks.cargo import build_executable
from spaceterm_tasks.development_linux import icon_svg
from spaceterm_tasks.macos_bundle import identity, identity_directory
from spaceterm_tasks.packaging import (
    DIST,
    NOTICES,
    SHELL_INTEGRATION,
    TERMINFO,
    checked,
    packaged_environment,
    release_version,
    require_clean_checkout,
    selected_identity,
    verify_application_identity,
    verify_askpass_helper_mode,
    verify_resources,
)

STAGE = ROOT / "target" / "package-linux"
PLATFORM = "linux-x86_64"
# Ubuntu 22.04. The release builds there, and every symbol the executable needs must exist there.
GLIBC_BASELINE = (2, 35)
GLIBC_SYMBOL = re.compile(rb"\bGLIBC_([0-9]+)\.([0-9]+)(?:\.[0-9]+)?\b")
DIRECTORIES = {"spaceterm": "spaceterm", "preflight": "spaceterm-preflight"}


def require_x86_64_linux():
    if sys.platform != "linux" or platform.machine() != "x86_64":
        raise TaskError("SpaceTerm packages Linux releases only on x86_64 Linux")


def archive_path(name):
    """dist/<directory>-linux-x86_64.tar.gz for the spaceterm or preflight identity."""
    return DIST / f"{DIRECTORIES[name]}-{PLATFORM}.tar.gz"


def application_id(name):
    return identity(name)["CFBundleIdentifier"]


def desktop_entry(name):
    """The installer replaces Exec with the absolute path of the installed executable."""
    return ROOT / "packaging" / "linux" / name / f"{application_id(name)}.desktop"


def icon_document(name):
    return identity_directory(name) / f"{identity(name)['CFBundleName']}.icon"


def stage(name, executable, tree):
    """Lay out one identity's installed directory at `tree`."""
    (tree / "bin").mkdir(parents=True)
    shutil.copyfile(executable, tree / "bin" / "spaceterm")
    (tree / "bin" / "spaceterm").chmod(0o755)
    resources = tree / "share" / "spaceterm"
    shutil.copytree(SHELL_INTEGRATION, resources / "shell-integration")
    shutil.copyfile(NOTICES, resources / NOTICES.name)
    checked(
        ["tic", "-x", "-o", str(resources / "terminfo"), str(TERMINFO)],
        "tic could not compile the SpaceTerm terminfo entry",
    )
    identifier = application_id(name)
    applications = tree / "share" / "applications"
    applications.mkdir()
    shutil.copyfile(desktop_entry(name), applications / f"{identifier}.desktop")
    icons = tree / "share" / "icons" / "hicolor" / "scalable" / "apps"
    icons.mkdir(parents=True)
    try:
        (icons / f"{identifier}.svg").write_text(icon_svg(icon_document(name)), encoding="utf-8")
    except ValueError:
        raise TaskError(f"the {name} icon document could not be flattened") from None


def normalized(member):
    """Archive entries carry no builder identity, and only regular files and directories."""
    if not (member.isfile() or member.isdir()):
        raise TaskError("a Linux package holds only regular files and directories")
    member.uid = member.gid = 0
    member.uname = member.gname = ""
    member.mode = 0o755 if member.isdir() or member.mode & 0o111 else 0o644
    return member


def write_archive(tree, archive):
    with tarfile.open(archive, "w:gz", format=tarfile.PAX_FORMAT) as output:
        output.add(tree, arcname=tree.name, filter=normalized)


def package(release_tag=None):
    """Build, stage, archive, and verify one identity, then move the archive into dist/."""
    require_x86_64_linux()
    if release_tag:
        require_clean_checkout()
    name = selected_identity(release_tag)
    display_name = identity(name)["CFBundleName"]
    print(f"Building the x86_64 Linux {display_name} executable")
    executable = build_executable(
        "--release", "--locked", "--no-default-features", env=packaged_environment(release_tag)
    )
    shutil.rmtree(STAGE, ignore_errors=True)
    STAGE.mkdir(parents=True)
    stage(name, executable, STAGE / DIRECTORIES[name])
    DIST.mkdir(exist_ok=True)
    destination = archive_path(name)
    with tempfile.TemporaryDirectory(dir=DIST, prefix=".package.") as temporary:
        archive = Path(temporary) / destination.name
        write_archive(STAGE / DIRECTORIES[name], archive)
        verify(archive, release_tag)
        archive.replace(destination)
    print(f"Created {destination.name} in dist")


def verify(archive=None, release_tag=None):
    """Verify the invariants SpaceTerm owns in a Linux archive."""
    require_x86_64_linux()
    name = selected_identity(release_tag)
    archive = Path(archive) if archive else archive_path(name)
    directory = DIRECTORIES[name]
    with tempfile.TemporaryDirectory(prefix="spaceterm-verify-") as temporary:
        try:
            with tarfile.open(archive, "r:gz") as source:
                members = source.getmembers()
                for member in members:
                    path = PurePosixPath(member.name)
                    if (
                        path.is_absolute()
                        or ".." in path.parts
                        or path.parts[0] != directory
                        or not (member.isfile() or member.isdir())
                    ):
                        raise TaskError(f"the archive may hold only the {directory} directory")
                source.extractall(temporary, filter="data")
        except (OSError, tarfile.TarError):
            raise TaskError("the archive could not be read") from None
        verify_tree(Path(temporary) / directory, name, release_tag)
    print(f"Verified {archive.name}")


def verify_tree(tree, name, release_tag):
    label = "archive"
    executable = tree / "bin" / "spaceterm"
    if not os.access(executable, os.X_OK):
        raise TaskError(f"{label} executable is missing")
    header = checked(["objdump", "-f", str(executable)], f"{label} executable is unreadable")
    if b"elf64-x86-64" not in header:
        raise TaskError(f"{label} executable must contain only x86_64 code")
    # Preflight runs only where it was built. Releases must run on every supported host.
    if release_tag:
        verify_glibc_baseline(executable, label)
    version = release_version(release_tag) if release_tag else None
    verify_application_identity(executable, identity(name)["CFBundleName"], label, version)
    verify_askpass_helper_mode(executable, label)
    verify_resources(tree / "share" / "spaceterm", label)

    identifier = application_id(name)
    entry = tree / "share" / "applications" / f"{identifier}.desktop"
    if not entry.is_file() or entry.read_bytes() != desktop_entry(name).read_bytes():
        raise TaskError(f"{label} desktop entry differs from the {name} identity")
    checked(["desktop-file-validate", str(entry)], f"{label} desktop entry is invalid")
    icon = tree / "share" / "icons" / "hicolor" / "scalable" / "apps" / f"{identifier}.svg"
    try:
        root = ElementTree.parse(icon).getroot()
    except (OSError, ElementTree.ParseError):
        raise TaskError(f"{label} icon is missing or invalid") from None
    if root.tag != "{http://www.w3.org/2000/svg}svg":
        raise TaskError(f"{label} icon is not an SVG document")


def verify_glibc_baseline(executable, label):
    """Every versioned glibc symbol the executable imports must exist in the baseline."""
    symbols = checked(
        ["objdump", "-T", str(executable)], f"{label} executable symbols could not be read"
    )
    required = max(
        ((int(major), int(minor)) for major, minor in GLIBC_SYMBOL.findall(symbols)),
        default=(0, 0),
    )
    if required > GLIBC_BASELINE:
        baseline = ".".join(map(str, GLIBC_BASELINE))
        needed = ".".join(map(str, required))
        raise TaskError(f"{label} executable needs glibc {needed}, newer than {baseline}")
