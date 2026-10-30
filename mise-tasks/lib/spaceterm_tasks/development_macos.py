"""Run source-built SpaceTerm Development from a signed application bundle."""

import hashlib
import os
import plistlib
import shutil
import subprocess
import tempfile
from pathlib import Path

from spaceterm_tasks import TaskError
from spaceterm_tasks.cargo import build_executable
from spaceterm_tasks.development_supervision import development_run, source_environment, supervise
from spaceterm_tasks.macos_bundle import ICON_NAME, compile_icon, identity, identity_directory
from spaceterm_tasks.packaging import (
    NOTICES,
    SHELL_INTEGRATION,
    TERMINFO,
    checked,
    verify_resources,
)

IDENTITY = "development"
# Development builds are never releases.
BUNDLE_VERSION = "0.0.0"


def icon_digest(document):
    digest = hashlib.sha256()
    for path in sorted(path for path in document.rglob("*") if path.is_file()):
        digest.update(path.relative_to(document).as_posix().encode() + b"\0")
        digest.update(path.read_bytes())
    return digest.hexdigest()[:16]


def cached_icon(parent, name):
    """Compile each version of the icon document once; actool takes seconds."""
    document = identity_directory(IDENTITY) / f"{name}.icon"
    cache = parent / "icons" / icon_digest(document)
    if not cache.is_dir():
        partial = cache.with_name(cache.name + ".partial")
        shutil.rmtree(partial, ignore_errors=True)
        compile_icon(IDENTITY, partial)
        partial.rename(cache)
    return cache


def stage_bundle(executable, server, *, catalog=None):
    template = identity(IDENTITY)
    name = template["CFBundleName"]
    if name != template["CFBundleExecutable"]:
        raise TaskError("the Development bundle name and executable name must match")
    parent = executable.parent.parent / "development-apps"
    parent.mkdir(parents=True, exist_ok=True)
    icon = cached_icon(parent, name)
    staging = Path(tempfile.mkdtemp(prefix=".bundle.", dir=parent))
    try:
        contents = staging / f"{name}.app" / "Contents"
        (contents / "MacOS").mkdir(parents=True)
        (contents / "Resources").mkdir()
        for resource in (f"{ICON_NAME}.icns", "Assets.car"):
            shutil.copyfile(icon / resource, contents / "Resources" / resource)
        info = {
            **template,
            "CFBundleShortVersionString": BUNDLE_VERSION,
            "CFBundleVersion": BUNDLE_VERSION,
        }
        (contents / "Info.plist").write_bytes(plistlib.dumps(info))
        shutil.copyfile(executable, contents / "MacOS" / name)
        (contents / "MacOS" / name).chmod(0o755)
        shutil.copyfile(server, contents / "MacOS" / "spaceterm")
        (contents / "MacOS" / "spaceterm").chmod(0o755)
        resources = contents / "Resources"
        shutil.copytree(SHELL_INTEGRATION, resources / "shell-integration")
        shutil.copyfile(NOTICES, resources / NOTICES.name)
        checked(
            ["tic", "-x", "-o", str(resources / "terminfo"), str(TERMINFO)],
            "Development terminfo compilation failed",
        )
        if catalog is not None:
            shutil.copytree(catalog, resources / "server-artifacts")
        verify_resources(resources, "Development bundle")
        checked(
            [
                "codesign",
                "--force",
                "--sign",
                "-",
                "--timestamp=none",
                str(contents / "MacOS/spaceterm"),
            ],
            "codesign could not sign the Development Server",
        )
        signed = subprocess.run(
            ["codesign", "--force", "--sign", "-", "--timestamp=none", str(contents.parent)],
            stdout=subprocess.DEVNULL,
        )
        if signed.returncode:
            raise TaskError("codesign could not sign the Development bundle")
        bundle = parent / f"{name}.app"
        shutil.rmtree(bundle, ignore_errors=True)
        contents.parent.rename(bundle)
    finally:
        shutil.rmtree(staging, ignore_errors=True)
    return bundle


def launch():
    from spaceterm_tasks.server_builds import prepare

    with (
        development_run(),
        tempfile.TemporaryDirectory(prefix="spaceterm-development-catalog-") as temporary,
    ):
        environment = source_environment(os.environ)
        catalog = prepare(Path(temporary) / "catalog", "spaceterm-development")
        server = build_executable(
            "--package",
            "spaceterm-cli",
            "--no-default-features",
            "--locked",
            binary="spaceterm",
            env=environment,
        )
        bundle = stage_bundle(
            build_executable("--locked", env=environment), server, catalog=catalog
        )
        scenario = environment.get("SPACETERM_UPDATE_PREVIEW")
        if scenario:
            print(f"Update preview: {scenario}. Choose SpaceTerm Development > Check for Updates…")
            print(
                "Downloads and installation are simulated. Nothing will be installed or restarted."
            )
        # LaunchServices attributes System Permissions to this bundle. The private owner
        # connection ends this exact client without signaling an unrelated Development process.
        output = os.ttyname(0) if os.isatty(0) else os.devnull
        return supervise(
            [
                str(bundle / "Contents/MacOS/spaceterm"),
                "server",
                "run",
                "--owned",
            ],
            ["open", "-W", "-n", "--stdout", output, "--stderr", output, str(bundle)],
            environment,
        )
