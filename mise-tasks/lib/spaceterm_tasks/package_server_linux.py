"""Build a headless account server for the Ubuntu 22.04 glibc baseline."""

import os
import re
import shutil
import sys
import tempfile
from pathlib import Path

from spaceterm_tasks import TaskError, package_server
from spaceterm_tasks.macos_signing import signing_environment
from spaceterm_tasks.packaging import DIST, checked, release_version


def verify_elf(path):
    header = checked(
        ["readelf", "-hW", path],
        "server ELF header is unavailable",
        text=True,
        env=signing_environment(os.environ),
        timeout=30,
    )
    if "Advanced Micro Devices X86-64" not in header or "ELF64" not in header:
        raise TaskError("server ELF target is unsupported")
    versions = checked(
        ["readelf", "--version-info", "-W", path],
        "server ELF versions are unavailable",
        text=True,
        env=signing_environment(os.environ),
        timeout=30,
    )
    if (
        any(
            tuple(map(int, match.split("."))) > (2, 35)
            for match in re.findall(r"\bGLIBC_([0-9.]+)\b", versions)
        )
        or "GLIBC_PRIVATE" in versions
    ):
        raise TaskError("server ELF requires a newer or private glibc ABI")
    dynamic = checked(
        ["readelf", "-dW", path],
        "server ELF dependency metadata is unavailable",
        text=True,
        env=signing_environment(os.environ),
        timeout=30,
    )
    if "(RPATH)" in dynamic:
        raise TaskError("server ELF contains a legacy loader search path")
    if "(RUNPATH)" in dynamic:
        raise TaskError("account server ELF contains an unapproved library search path")
    needed = re.findall(r"\(NEEDED\).*\[(.*?)\]", dynamic)
    if any("/" in name or not name for name in needed):
        raise TaskError("server ELF dependency name is unsafe")
    program = checked(
        ["readelf", "-lW", path],
        "server ELF interpreter metadata is unavailable",
        text=True,
        env=signing_environment(os.environ),
        timeout=30,
    )
    interpreters = re.findall(r"Requesting program interpreter: (.*?)\]", program)
    if interpreters and interpreters != ["/lib64/ld-linux-x86-64.so.2"]:
        raise TaskError("server ELF interpreter is unsupported")
    return needed


def verify_elf_closure(root):
    system = {
        "libc.so.6",
        "libm.so.6",
        "libpthread.so.0",
        "libdl.so.2",
        "librt.so.1",
        "libgcc_s.so.1",
        "ld-linux-x86-64.so.2",
    }
    expected = {root / "bin/spaceterm-server"}
    if (root / "bin/spaceterm-cli").is_file():
        expected.add(root / "bin/spaceterm-cli")
    found = set()
    for path in root.rglob("*"):
        if path.is_symlink():
            raise TaskError("server ELF closure contains a linked file")
        if not path.is_file():
            continue
        with path.open("rb") as source:
            if source.read(4) != b"\x7fELF":
                continue
        if path not in expected:
            raise TaskError("server package contains unrecorded native code")
        found.add(path)
        if any(name not in system for name in verify_elf(path)):
            raise TaskError("server ELF links a non-headless or unapproved dependency")
    if found != expected:
        raise TaskError("server ELF closure is missing an account executable")


def package(release_tag=None):
    if sys.platform != "linux":
        raise TaskError("Linux server packaging requires Linux")
    binaries = package_server.build(release_tag)
    build = "release" if release_tag else "preflight"
    metadata = package_server.executable_metadata(
        binaries["spaceterm-server"],
        "linux-x86_64",
        build,
        release_version(release_tag) if release_tag else None,
    )
    DIST.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="spaceterm-server-linux-", dir=DIST) as temporary:
        root = Path(temporary) / "spaceterm-server"
        root.mkdir()
        package_server.copy_built_executables(binaries, root, metadata)
        package_server.copy_resources(root / "share/spaceterm-server")
        verify_elf_closure(root)
        package_server.verify_executable_metadata(root, metadata)
        package_server.write_manifest(root, metadata)
        output = Path(temporary) / "server.tar.gz"
        package_server.write_archive(root, output)
        package_server.verify_archive(output)
        destination = package_server.archive_path("linux-x86_64", build)
        shutil.copyfile(output, destination)
    print("Headless account server archive and glibc 2.35 compatibility verified.")
    return destination
