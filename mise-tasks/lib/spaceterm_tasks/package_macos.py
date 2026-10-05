"""Package and verify the Preflight or release application bundle and disk image.

Only a release tag selects the SpaceTerm identity; every other package is SpaceTerm
Preflight. See ADR 0012.
"""

import filecmp
import json
import os
import platform
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, sparkle
from spaceterm_tasks.cargo import build_executable
from spaceterm_tasks.macos_bundle import ICON_NAME, compile_icon, identity, identity_directory

STAGE = ROOT / "target" / "package-macos"
DIST = ROOT / "dist"
TERMINFO = ROOT / "assets" / "terminfo" / "xterm-spaceterm.terminfo"
TERMINFO_IDENTITY = "xterm-spaceterm|SpaceTerm truthful xterm-compatible terminal"
MINIMUM_MACOS = "26.0"
PREFLIGHT_VERSION = "0.0.0"
STABLE_TAG = re.compile(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")
ASKPASS_HELPER_MODE = "broker-v1"


def release_version(tag):
    """Return a release tag's version. build.rs proves the tag is annotated, on HEAD, and clean."""
    if not STABLE_TAG.fullmatch(tag):
        raise TaskError("a release tag is v followed by a canonical stable SemVer")
    return tag[1:]


def selected_identity(release_tag):
    return "spaceterm" if release_tag else "preflight"


def checked(arguments, failure, **kwargs):
    result = subprocess.run(arguments, capture_output=True, **kwargs)
    if result.returncode:
        raise TaskError(failure)
    return result.stdout


def require_apple_silicon():
    if sys.platform != "darwin" or platform.machine() != "arm64":
        raise TaskError("SpaceTerm packages only on Apple Silicon Macs")


def require_clean_checkout():
    """Packaging reads files Cargo never sees, and Cargo can reuse a build script's last check."""
    status = checked(
        ["git", "--no-optional-locks", "status", "--porcelain", "--untracked-files=normal"],
        "Git could not report the checkout state",
        cwd=ROOT,
    )
    if status.strip():
        raise TaskError("a release package requires a clean checkout")


def package(release_tag=None):
    """Build, bundle, and verify one identity, then move the app and disk image into dist/."""
    require_apple_silicon()
    if release_tag:
        require_clean_checkout()
    name = selected_identity(release_tag)
    template = identity(name)
    version = release_version(release_tag) if release_tag else PREFLIGHT_VERSION
    shutil.rmtree(STAGE, ignore_errors=True)
    STAGE.mkdir(parents=True)
    compile_icon(name, STAGE)
    checked(
        ["tic", "-x", "-o", str(STAGE / "terminfo"), str(TERMINFO)],
        "tic could not compile the SpaceTerm terminfo entry",
    )

    environment = {
        key: value
        for key, value in os.environ.items()
        if key not in ("SPACETERM_RELEASE_TAG", "SPACETERM_SPARKLE_DIR")
    }
    # Leaves the SpaceTerm Development identity; see ADR 0012.
    environment.update(MACOSX_DEPLOYMENT_TARGET=MINIMUM_MACOS, SPACETERM_PACKAGED="1")
    config = tomllib.loads((identity_directory(name) / "Packager.toml").read_text())
    if release_tag:
        updater = sparkle.directory()
        environment.update(SPACETERM_RELEASE_TAG=release_tag, SPACETERM_SPARKLE_DIR=str(updater))
        config["macos"]["frameworks"] = [str(updater / "Sparkle.framework")]
    print(f"Building the Apple Silicon {template['CFBundleName']} executable")
    executable = build_executable("--release", "--locked", "--no-default-features", env=environment)
    binaries = STAGE / "binaries"
    binaries.mkdir()
    shutil.copyfile(executable, binaries / template["CFBundleExecutable"])
    (binaries / template["CFBundleExecutable"]).chmod(0o755)
    config["version"] = version
    # cargo-packager stamps CFBundleVersion with the build time; the identity's Info.plist
    # entries override its generated ones, so add the tag-derived versions to them.
    info = {**template, "CFBundleShortVersionString": version, "CFBundleVersion": version}
    (STAGE / "Info.plist").write_bytes(plistlib.dumps(info))
    config["macos"]["info-plist-path"] = str(STAGE / "Info.plist")
    config["binaries-dir"] = str(binaries)

    DIST.mkdir(exist_ok=True)
    app_name = template["CFBundleName"]
    with tempfile.TemporaryDirectory(dir=DIST, prefix=".package.") as temporary:
        output = Path(temporary) / "output"
        print(f"Packaging {app_name}.app and {app_name}.dmg with cargo-packager")
        packaged = subprocess.run(
            ["cargo", "packager", "--config", json.dumps(config), "--out-dir", str(output)],
            cwd=ROOT,
            env={"CI": "true", **environment},
        )
        app = output / f"{app_name}.app"
        dmg = output / f"{app_name}_{version}_aarch64.dmg"
        if packaged.returncode or not app.is_dir() or not dmg.is_file():
            raise TaskError("cargo-packager did not produce the application and disk image")
        staged_dmg = Path(temporary) / f"{app_name}.dmg"
        dmg.rename(staged_dmg)
        verify(app, staged_dmg, release_tag)
        shutil.rmtree(DIST / app.name, ignore_errors=True)
        (DIST / staged_dmg.name).unlink(missing_ok=True)
        app.rename(DIST / app.name)
        staged_dmg.rename(DIST / staged_dmg.name)
    print(f"Created {app_name}.app and {app_name}.dmg in dist")


def verify(app=None, dmg=None, release_tag=None):
    """Verify the invariants SpaceTerm owns in an application bundle and its disk image."""
    require_apple_silicon()
    name = selected_identity(release_tag)
    app_name = identity(name)["CFBundleName"]
    app = Path(app) if app else DIST / f"{app_name}.app"
    dmg = Path(dmg) if dmg else DIST / f"{app_name}.dmg"
    verify_bundle(app, name, release_tag, "dist")
    checked(["hdiutil", "verify", str(dmg)], "disk image checksum verification failed")
    with tempfile.TemporaryDirectory(prefix="spaceterm-verify-") as temporary:
        mount = Path(temporary) / "mount"
        mount.mkdir()
        try:
            # Inside try: termination during attach can still leave the image mounted.
            checked(
                [
                    "hdiutil",
                    "attach",
                    "-nobrowse",
                    "-readonly",
                    "-mountpoint",
                    str(mount),
                    str(dmg),
                ],
                "the disk image could not be mounted",
            )
            applications = mount / "Applications"
            if not applications.is_symlink() or os.readlink(applications) != "/Applications":
                raise TaskError("the disk image must link Applications to /Applications")
            verify_bundle(mount / f"{app_name}.app", name, release_tag, "dmg")
        finally:
            subprocess.run(["hdiutil", "detach", str(mount), "-force"], capture_output=True)
    print(f"Verified {app_name}.app and {app_name}.dmg")


def verify_bundle(app, name, release_tag, label):
    template = identity(name)
    contents = app / "Contents"
    try:
        info = plistlib.loads((contents / "Info.plist").read_bytes())
    except (OSError, plistlib.InvalidFileException):
        raise TaskError(f"{label} Info.plist is missing or invalid") from None
    for key, value in template.items():
        if info.get(key) != value:
            raise TaskError(f"{label} Info.plist {key} differs from the {name} identity")
    version = release_version(release_tag) if release_tag else PREFLIGHT_VERSION
    if (info.get("CFBundleShortVersionString"), info.get("CFBundleVersion")) != (version, version):
        raise TaskError(f"{label} bundle version must be {version}")

    executable = contents / "MacOS" / template["CFBundleExecutable"]
    if not os.access(executable, os.X_OK):
        raise TaskError(f"{label} executable is missing")
    architectures = checked(
        ["lipo", "-archs", str(executable)],
        f"{label} executable architecture could not be read",
        text=True,
    )
    if architectures.split() != ["arm64"]:
        raise TaskError(f"{label} executable must contain only Apple Silicon code")
    build = checked(
        ["xcrun", "vtool", "-show-build", str(executable)],
        f"{label} executable deployment target could not be read",
        text=True,
    )
    if f"minos {MINIMUM_MACOS}" not in " ".join(build.split()):
        raise TaskError(f"{label} executable deployment target differs from its bundle minimum")
    verify_application_identity(executable, template["CFBundleName"], label)
    verify_askpass_helper_mode(executable, label)

    resources = contents / "Resources"
    if not (resources / f"{ICON_NAME}.icns").is_file():
        raise TaskError(f"{label} app icon is missing")
    assets = checked(
        ["assetutil", "--info", str(resources / "Assets.car")],
        f"{label} layered icon asset catalog is invalid",
    )
    if not any(
        entry.get("Name") == ICON_NAME
        and entry.get("AssetType") == "Icon Image"
        and entry.get("PixelWidth") == 1024
        for entry in json.loads(assets)
    ):
        raise TaskError(f"{label} asset catalog has no 1024-pixel {ICON_NAME} icon")
    if not filecmp.cmp(
        ROOT / "assets/THIRD-PARTY-NOTICES.txt",
        resources / "THIRD-PARTY-NOTICES.txt",
        shallow=False,
    ):
        raise TaskError(f"{label} third-party notices differ from the tracked resource")
    if not same_tree(ROOT / "assets/shell-integration", resources / "shell-integration"):
        raise TaskError(f"{label} shell integration differs from the tracked resources")
    terminfo = checked(
        ["infocmp", "-x", "-1", "xterm-spaceterm"],
        f"{label} xterm-spaceterm entry is not discoverable",
        env={**os.environ, "TERMINFO": str(resources / "terminfo")},
        text=True,
    )
    if TERMINFO_IDENTITY not in terminfo:
        raise TaskError(f"{label} terminfo entry has the wrong identity")

    signature = ["codesign", "--verify", "--strict", *(["--deep"] if release_tag else []), str(app)]
    checked(signature, f"{label} app signature verification failed")
    details = subprocess.run(
        ["codesign", "--display", "--verbose=4", str(app)], capture_output=True, text=True
    ).stderr
    if "Signature=adhoc" not in details:
        raise TaskError(f"{label} app is not ad hoc signed")
    entitlements = checked(
        ["codesign", "--display", "--entitlements", ":-", str(app)],
        f"{label} app entitlements could not be read",
    )
    expected = plistlib.loads((identity_directory(name) / "Entitlements.plist").read_bytes())
    if plistlib.loads(entitlements) != expected:
        raise TaskError(f"{label} app entitlements differ from the {name} identity")
    if release_tag and not (contents / "Frameworks" / "Sparkle.framework").is_dir():
        raise TaskError(f"{label} updater framework is missing")


def verify_application_identity(executable, expected, label):
    # An old executable may launch the application instead of answering --version.
    # Clear helper inputs and bound the query before inspecting its compiled name.
    try:
        result = subprocess.run([executable, "--version"], env={}, capture_output=True, timeout=5)
    except (OSError, subprocess.TimeoutExpired):
        raise TaskError(f"{label} executable application identity query failed") from None
    name, _, version = result.stdout.removesuffix(b"\n").rpartition(b" ")
    if (
        result.returncode
        or result.stderr
        or name != expected.encode()
        or not version
        or any(byte in b" \t\r\n" for byte in version)
    ):
        raise TaskError(f"{label} executable application identity must be {expected}")


def verify_askpass_helper_mode(executable, label):
    """The packaged executable must enter AskPass helper mode before anything else."""
    for prompt in ("SpaceTerm package verifier prompt", "--version"):
        try:
            result = subprocess.run(
                [executable, prompt],
                capture_output=True,
                timeout=5,
                env={"SPACETERM_SSH_ASKPASS_MODE": ASKPASS_HELPER_MODE},
            )
        except (OSError, subprocess.TimeoutExpired):
            raise TaskError(f"{label} AskPass helper mode did not finish") from None
        if result.returncode != 2 or result.stdout or result.stderr:
            raise TaskError(
                f"{label} AskPass helper mode must reject missing transport "
                "inputs silently with exit 2"
            )


def same_tree(expected, actual):
    comparison = filecmp.dircmp(expected, actual, ignore=[])
    if comparison.left_only or comparison.right_only or comparison.funny_files:
        return False
    _, mismatch, errors = filecmp.cmpfiles(expected, actual, comparison.common_files, shallow=False)
    return (
        not mismatch
        and not errors
        and all(
            same_tree(expected / directory, actual / directory)
            for directory in comparison.common_dirs
        )
    )
