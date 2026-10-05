"""Run source-built SpaceTerm Development from a signed application bundle."""

import hashlib
import os
import plistlib
import shutil
import subprocess
import tempfile
from pathlib import Path

from spaceterm_tasks import TaskError, developer_environment
from spaceterm_tasks.cargo import build_executable
from spaceterm_tasks.macos_bundle import ICON_NAME, compile_icon, identity, identity_directory

IDENTITY = "development"
# Development builds are never releases; see ADR 0012.
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


def stage_bundle(executable):
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
    bundle = stage_bundle(build_executable("--locked"))
    scenario = os.environ.get("SPACETERM_UPDATE_PREVIEW")
    if scenario:
        print(f"Update preview: {scenario}. Choose SpaceTerm Development > Check for Updates…")
        print("Downloads and installation are simulated. Nothing will be installed or restarted.")
    # LaunchServices makes the bundle the responsible process for its own privacy checks, as it
    # is for an installed SpaceTerm. Executing the binary from here would attribute those checks
    # to the terminal that ran this task, so System Permissions would follow that terminal.
    output = os.ttyname(0) if os.isatty(0) else os.devnull
    # open passes its environment to the application.
    os.execvpe(
        "open",
        ["open", "-W", "-n", "--stdout", output, "--stderr", output, str(bundle)],
        developer_environment(os.environ),
    )
