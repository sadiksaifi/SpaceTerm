"""Package and verify the Preflight or release application bundle and disk image.

Only a release tag selects the SpaceTerm identity; every other package is SpaceTerm
Preflight.
"""

import json
import os
import platform
import plistlib
import shutil
import subprocess
import sys
import tempfile
import tomllib
from contextlib import nullcontext
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, macos_signing, sparkle
from spaceterm_tasks.cargo import build_executable
from spaceterm_tasks.macos_bundle import ICON_NAME, compile_icon, identity, identity_directory
from spaceterm_tasks.packaging import (
    DIST,
    PREFLIGHT_VERSION,
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

STAGE = ROOT / "target" / "package-macos"
MINIMUM_MACOS = "26.0"


def require_apple_silicon():
    if sys.platform != "darwin" or platform.machine() != "arm64":
        raise TaskError("SpaceTerm packages only on Apple Silicon Macs")


def package(release_tag=None):
    """Build, bundle, and verify one identity, then move the app and disk image into dist/."""
    require_apple_silicon()
    if release_tag:
        require_clean_checkout()
        fingerprint = tomllib.loads(
            (identity_directory("spaceterm") / "Packager.toml").read_text()
        )["macos"]["signing-identity"]
        macos_signing.signing_credentials(fingerprint, os.environ)
    package_bundle(release_tag)


def package_bundle(release_tag=None):
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

    environment = macos_signing.signing_environment(
        packaged_environment(release_tag, removed=("SPACETERM_SPARKLE_DIR",))
    )
    environment["MACOSX_DEPLOYMENT_TARGET"] = MINIMUM_MACOS
    config = tomllib.loads((identity_directory(name) / "Packager.toml").read_text())
    if release_tag:
        updater = sparkle.directory()
        environment["SPACETERM_SPARKLE_DIR"] = str(updater)
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
        signing = (
            macos_signing.release_keychain(config["macos"]["signing-identity"], os.environ)
            if release_tag
            else nullcontext()
        )
        with signing:
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
    verify_resources(resources, label)

    signature = ["codesign", "--verify", "--strict", *(["--deep"] if release_tag else []), str(app)]
    checked(signature, f"{label} app signature verification failed")
    details = subprocess.run(
        ["codesign", "--display", "--verbose=4", str(app)], capture_output=True, text=True
    ).stderr
    if release_tag:
        config = tomllib.loads((identity_directory(name) / "Packager.toml").read_text())
        macos_signing.verify_release_signature(
            app, config["macos"]["signing-identity"], template["CFBundleIdentifier"], label
        )
    elif "Signature=adhoc" not in details:
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
