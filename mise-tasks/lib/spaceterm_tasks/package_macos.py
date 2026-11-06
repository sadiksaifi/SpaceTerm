"""Package and verify the Preflight or release application bundle and disk image.

Only a release tag selects the SpaceTerm identity; every other package is SpaceTerm
Preflight. A release embeds its release notes. Outside a dry run it is signed with Developer ID,
and the app and the disk image are each notarized and stapled.
"""

import json
import os
import platform
import plistlib
import shlex
import shutil
import subprocess
import sys
import tempfile
import tomllib
from contextlib import nullcontext
from pathlib import Path

from spaceterm_tasks import (
    ROOT,
    TaskError,
    macos_codesign,
    macos_notary,
    macos_signing,
    sparkle,
    update_signing,
)
from spaceterm_tasks.cargo import build_executable
from spaceterm_tasks.macos_bundle import ICON_NAME, compile_icon, identity, identity_directory
from spaceterm_tasks.packaging import (
    DESKTOP_FLOOR,
    DIST,
    PREFLIGHT_VERSION,
    RELEASE_NOTES,
    TERMINFO,
    checked,
    desktop_floor,
    packaged_environment,
    release_notes_file,
    release_version,
    require_clean_checkout,
    selected_identity,
    verify_application_identity,
    verify_askpass_helper_mode,
    verify_resources,
)

STAGE = ROOT / "target" / "package-macos"
MINIMUM_MACOS = "26.0"
# The disk image window art, at 1x and 2x, drawn for the Packager.toml window and icon positions.
DMG_BACKGROUND = ROOT / "assets" / "brand" / "macos" / "dmg" / "background.tiff"


def require_apple_silicon():
    if sys.platform != "darwin" or platform.machine() != "arm64":
        raise TaskError("SpaceTerm packages only on Apple Silicon Macs")


def package(release_tag=None, notes=None):
    """Build, bundle, and verify one identity, then move the app and disk image into dist/.

    A release embeds the release notes file `notes`.
    """
    require_apple_silicon()
    if release_tag:
        require_clean_checkout()
        release_notes_file(release_tag, notes)
        # Refuse missing signing and notarization inputs before the build, not after it.
        if macos_notary.required(os.environ):
            macos_signing.signing_credentials(os.environ)
            macos_notary.credentials(os.environ)
    package_bundle(release_tag, notes)


def signer(release_tag):
    """The codesign identity and Team ID: ad hoc and none outside a release."""
    if not release_tag:
        return macos_codesign.ADHOC, None
    return macos_codesign.signing_identity(os.environ), macos_codesign.team_id(os.environ)


def bundle_template(name, release_tag):
    """The Info.plist entries of the identity this package builds.

    A dry-run release's appcast is signed with the dry run's generated update key, and Sparkle
    signs an appcast only for an app that trusts the signing key. So a dry-run app pins that key
    as `SUPublicEDKey`; every other package keeps the tracked entries.
    """
    template = identity(name)
    if release_tag and update_signing.dry_run():
        template = {**template, "SUPublicEDKey": update_signing.trust()[0]}
    return template


def package_bundle(release_tag=None, notes=None):
    name = selected_identity(release_tag)
    template = bundle_template(name, release_tag)
    version = release_version(release_tag) if release_tag else PREFLIGHT_VERSION
    shutil.rmtree(STAGE, ignore_errors=True)
    STAGE.mkdir(parents=True)
    compile_icon(STAGE, template["CFBundleIdentifier"])
    checked(
        ["tic", "-x", "-o", str(STAGE / "terminfo"), str(TERMINFO)],
        "tic could not compile the SpaceTerm terminfo entry",
    )

    environment = macos_signing.signing_environment(
        packaged_environment(release_tag, removed=("SPACETERM_SPARKLE_DIR",))
    )
    environment["MACOSX_DEPLOYMENT_TARGET"] = MINIMUM_MACOS
    config = tomllib.loads((identity_directory(name) / "Packager.toml").read_text())
    identity_name, team = signer(release_tag)
    if release_tag:
        environment[RELEASE_NOTES] = str(release_notes_file(release_tag, notes))
        environment[DESKTOP_FLOOR] = desktop_floor(release_tag)
        updater = sparkle.directory()
        environment["SPACETERM_SPARKLE_DIR"] = str(updater)
        config["macos"]["frameworks"] = [str(updater / "Sparkle.framework")]
    from spaceterm_tasks.core_package import prepare_desktop_artifact

    environment.update(
        prepare_desktop_artifact(
            STAGE / "core-artifact",
            template["CFBundleIdentifier"].removeprefix("io.github.sadiksaifi."),
            release_tag,
            catalog_destination=STAGE / "core-artifacts",
        )
    )
    print(f"Building the Apple Silicon {template['CFBundleName']} executable")
    executable = build_executable(
        "--release", "--locked", "--no-default-features", "--features", "desktop", env=environment
    )
    binaries = STAGE / "binaries"
    binaries.mkdir()
    shutil.copyfile(executable, binaries / template["CFBundleExecutable"])
    (binaries / template["CFBundleExecutable"]).chmod(0o755)
    config["resources"].append({"src": str(STAGE / "core-artifact"), "target": "core-artifact"})
    config["resources"].append({"src": str(STAGE / "core-artifacts"), "target": "core-artifacts"})
    config["version"] = version
    # cargo-packager stamps CFBundleVersion with the build time; the identity's Info.plist
    # entries override its generated ones, so add the tag-derived versions to them.
    info = {**template, "CFBundleShortVersionString": version, "CFBundleVersion": version}
    (STAGE / "Info.plist").write_bytes(plistlib.dumps(info))
    config["macos"]["info-plist-path"] = str(STAGE / "Info.plist")
    config["dmg"]["background"] = str(DMG_BACKGROUND)
    config["binaries-dir"] = str(binaries)

    DIST.mkdir(exist_ok=True)
    app_name = template["CFBundleName"]
    with tempfile.TemporaryDirectory(dir=DIST, prefix=".package.") as temporary:
        output = Path(temporary) / "output"
        app = output / f"{app_name}.app"
        dmg = output / f"{app_name}_{version}_aarch64.dmg"
        # cargo-packager signs every native file with the app's entitlements and never
        # reseals nested bundles. SpaceTerm signs instead, after the app and before the image.
        # A release notarizes and staples the app there too, so the image holds a stapled app.
        config["before-each-package-command"] = (
            f"{shlex.quote(sys.executable)} -m spaceterm_tasks.package_macos sign-app"
        )
        hook = {
            "SPACETERM_PACKAGER_APP": str(app),
            "SPACETERM_PACKAGER_IDENTITY": identity_name,
            "SPACETERM_PACKAGER_ENTITLEMENTS": str(identity_directory(name) / "Entitlements.plist"),
            **({"SPACETERM_PACKAGER_TEAM": team} if team else {}),
            **({"SPACETERM_PACKAGER_RELEASE": "1"} if release_tag else {}),
        }
        print(f"Packaging {app_name}.app and {app_name}.dmg with cargo-packager")
        signing = (
            macos_signing.release_keychain(identity_name, os.environ)
            if identity_name != macos_codesign.ADHOC
            else nullcontext()
        )
        notarizing = macos_notary.notary() if release_tag else nullcontext()
        with signing, notarizing as notary:
            if notary is not None:
                hook.update(notary.environment())
            packaged = subprocess.run(
                ["cargo", "packager", "--config", json.dumps(config), "--out-dir", str(output)],
                cwd=ROOT,
                env={"CI": "true", **environment, **hook},
            )
            if packaged.returncode or not app.is_dir() or not dmg.is_file():
                raise TaskError("cargo-packager did not produce the application and disk image")
            macos_codesign.sign_disk_image(
                dmg, identity_name, template["CFBundleIdentifier"], environment=environment
            )
            if release_tag:
                macos_notary.notarize(dmg, notary, environment)
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
    identity_name, team = signer(release_tag)
    if identity_name != macos_codesign.ADHOC:
        macos_codesign.verify_disk_image(dmg, identity(name)["CFBundleIdentifier"], team)
    if release_tag:
        macos_notary.assess(dmg)
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
            background = mount / ".background" / DMG_BACKGROUND.name
            if not background.is_file() or background.read_bytes() != DMG_BACKGROUND.read_bytes():
                raise TaskError("the disk image window must show the brand background")
            verify_bundle(mount / f"{app_name}.app", name, release_tag, "dmg")
        finally:
            subprocess.run(["hdiutil", "detach", str(mount), "-force"], capture_output=True)
    print(f"Verified {app_name}.app and {app_name}.dmg")


def verify_bundle(app, name, release_tag, label):
    template = bundle_template(name, release_tag)
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
    from spaceterm_tasks.core_package import verify_desktop_artifact

    verify_desktop_artifact(
        resources / "core-artifact",
        template["CFBundleIdentifier"].removeprefix("io.github.sadiksaifi."),
        release_tag,
        executable=executable,
    )
    from spaceterm_tasks.core_catalog import verify_catalog

    verify_catalog(
        resources / "core-artifacts",
        template["CFBundleIdentifier"].removeprefix("io.github.sadiksaifi."),
        require_all=bool(release_tag),
    )

    identity_name, team = signer(release_tag)
    macos_codesign.verify(
        app,
        template["CFBundleIdentifier"],
        plistlib.loads((identity_directory(name) / "Entitlements.plist").read_bytes()),
        team=team,
        identity=identity_name,
        label=f"{label} app",
    )
    if release_tag:
        macos_notary.assess(app)
    framework = contents / "Frameworks" / "Sparkle.framework"
    if framework.exists() != bool(release_tag):
        raise TaskError(f"{label} updater framework must ship with releases only")
    if release_tag:
        if (framework / "Versions/B/XPCServices").exists():
            raise TaskError(f"{label} updater framework still holds its XPC Services")
        for code in (framework / "Versions/B/Autoupdate", framework / "Versions/B/Updater.app"):
            macos_codesign.verify(
                code,
                macos_codesign.signing_identifier(code),
                {},
                team=team,
                identity=identity_name,
                label=f"{label} updater {code.name}",
            )


def sign_app(environment):
    """cargo-packager runs this before each format; sign the finished app before its image."""
    if environment.get("CARGO_PACKAGER_FORMAT") != "dmg":
        return
    app = Path(environment["SPACETERM_PACKAGER_APP"])
    # Extended attributes such as Finder information make codesign refuse a bundle.
    checked(["xattr", "-cr", str(app)], "the app's extended attributes could not be cleared")
    macos_codesign.sign_desktop(
        app,
        environment["SPACETERM_PACKAGER_IDENTITY"],
        environment["SPACETERM_PACKAGER_ENTITLEMENTS"],
        team=environment.get("SPACETERM_PACKAGER_TEAM"),
        environment=macos_signing.signing_environment(environment),
    )
    if environment.get("SPACETERM_PACKAGER_RELEASE") == "1":
        macos_notary.notarize(app, macos_notary.from_environment(environment), environment)


if __name__ == "__main__" and sys.argv[1:] == ["sign-app"]:
    from spaceterm_tasks import main

    main(lambda: sign_app(os.environ))
