"""Pinned Ubuntu native libraries for Zig's glibc-baseline package builds.

Packages are extracted into a private build sysroot. Their maintainer scripts
never run, and the host's package database and libraries are left untouched.
"""

import fcntl
import hashlib
import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, main

MANIFEST = ROOT / "packaging/linux/toolchain.json"


def sysroot_path(manifest, cache):
    return cache / hashlib.sha256(manifest.read_bytes()).hexdigest()


def prepare(manifest, cache):
    """Publish a complete checksum-verified, relocatable sysroot, or leave none."""
    cache.mkdir(parents=True, exist_ok=True)
    destination = sysroot_path(manifest, cache)
    with (cache / ".lock").open("w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if destination.is_dir():
            return destination
        configuration = json.loads(manifest.read_text())
        with tempfile.TemporaryDirectory(dir=cache, prefix=".prepare-") as temporary:
            directory = Path(temporary)
            tree = directory / "sysroot"
            tree.mkdir()
            archive = directory / "package.deb"
            for package in configuration["packages"]:
                print(f"Preparing Linux build input {package['name']} {package['version']}")
                try:
                    with (
                        urllib.request.urlopen(
                            configuration["snapshot"] + package["path"], timeout=60
                        ) as source,
                        archive.open("wb") as output,
                    ):
                        shutil.copyfileobj(source, output)
                except (urllib.error.URLError, TimeoutError):
                    raise TaskError("a pinned Linux build input could not be downloaded") from None
                with archive.open("rb") as source:
                    if hashlib.file_digest(source, "sha256").hexdigest() != package["sha256"]:
                        raise TaskError("a pinned Linux build input has a different checksum")
                result = subprocess.run(
                    ["dpkg-deb", "--extract", str(archive), str(tree)], capture_output=True
                )
                if result.returncode:
                    raise TaskError("a pinned Linux build input could not be extracted")
            # Debian's loader symlink is absolute. Keep all package links inside
            # the sysroot, including when it moves out of the temporary directory.
            for path in tree.rglob("*"):
                if path.is_symlink() and path.readlink().is_absolute():
                    target = tree / str(path.readlink()).lstrip("/")
                    path.unlink()
                    path.symlink_to(os.path.relpath(target, path.parent))
            tree.rename(destination)
    return destination


def build_environment(environment):
    """Select baseline native headers/libraries alongside cargo-zigbuild's libc."""
    result = dict(environment)
    target = result.get("SPACETERM_LINUX_PACKAGE_TARGET")
    cache = result.get("SPACETERM_LINUX_PACKAGE_SYSROOT")
    if not target or not cache:
        raise TaskError("the Linux package toolchain is required; run packaging through mise")
    sysroot = sysroot_path(MANIFEST, Path(cache))
    # pkg-config's host paths must not supply newer native APIs or libc inputs.
    for key in tuple(result):
        if key.startswith(("PKG_CONFIG_PATH", "PKG_CONFIG_LIBDIR", "PKG_CONFIG_SYSROOT_DIR")):
            result.pop(key)
    libraries = [sysroot / "usr/lib/x86_64-linux-gnu", sysroot / "lib/x86_64-linux-gnu"]
    result["PKG_CONFIG_SYSROOT_DIR"] = str(sysroot)
    result["PKG_CONFIG_LIBDIR"] = os.pathsep.join(
        str(path) for path in (libraries[0] / "pkgconfig", sysroot / "usr/share/pkgconfig")
    )
    flags = (
        result["CARGO_ENCODED_RUSTFLAGS"].split("\x1f")
        if "CARGO_ENCODED_RUSTFLAGS" in result
        else shlex.split(result.get("RUSTFLAGS", ""))
    )
    result.pop("RUSTFLAGS", None)
    result["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join(
        [*flags, *(f"-Lnative={path}" for path in libraries)]
    )
    headers = shlex.join(["-isystem", str(sysroot / "usr/include")])
    for variable in ("CFLAGS", "CXXFLAGS"):
        result[variable] = (result.get(variable, "") + " " + headers).strip()
    return target, result


def setup():
    if sys.platform == "linux":
        cache = os.environ.get("SPACETERM_LINUX_PACKAGE_SYSROOT")
        if not cache:
            raise TaskError("the Linux package sysroot is required; run setup through mise")
        print(f"Prepared Linux package sysroot {prepare(MANIFEST, Path(cache))}")


if __name__ == "__main__":
    main(setup)
