"""Package and verify the Preflight or release application bundle and disk image.

Only a release tag selects the SpaceTerm identity; every other package is SpaceTerm
Preflight. A release embeds its release notes. Outside a dry run it is signed with Developer ID,
and the app and the disk image are each notarized and stapled.
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

from spaceterm_tasks import (
    ROOT,
    TaskError,
    disk_image,
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


def require_apple_silicon():
    if sys.platform != "darwin" or platform.machine() != "arm64":
        raise TaskError("SpaceTerm packages only on Apple Silicon Macs")


def package(release_tag=None, notes=None, *, compiled=None, execute: bool = True):
    """Build, bundle, and verify one identity, then move the app and disk image into dist/.

    A release embeds the release notes file `notes`.
    """
    if execute:
        from spaceterm_tasks.desktop_manifest import require_no_release_secrets

        require_no_release_secrets()
    require_apple_silicon()
    if compiled is None and not execute:
        raise TaskError("static desktop packaging requires compiled artifacts")
    if release_tag:
        require_clean_checkout()
        release_notes_file(release_tag, notes)
        # Refuse missing signing and notarization inputs before the build, not after it.
        if macos_notary.required(os.environ):
            macos_signing.signing_credentials(os.environ)
            macos_notary.credentials(os.environ)
    package_bundle(release_tag, notes, compiled=compiled, execute=execute)


def signer(release_tag):
    """The codesign identity and Team ID: ad hoc and none outside a release."""
    if not release_tag:
        return macos_codesign.ADHOC, None
    return macos_codesign.signing_identity(os.environ), macos_codesign.team_id(os.environ)


def bundle_template(name, release_tag):
    """The Info.plist entries of the identity this package builds.

    Release bundles pin the tracked current public key for Sparkle. Dry-run checkouts track
    their generated public keys before compilation and need no private seed for verification.
    """
    template = identity(name)
    if release_tag:
        template = {**template, "SUPublicEDKey": update_signing.release_trust()[0]}
    return template


def package_bundle(release_tag=None, notes=None, *, compiled=None, execute: bool = True):
    if execute:
        from spaceterm_tasks.desktop_manifest import require_no_release_secrets

        require_no_release_secrets()
    if compiled is None and not execute:
        raise TaskError("static desktop packaging requires compiled artifacts")
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
    from spaceterm_tasks.desktop_manifest import (
        compile_environment,
        compiled_desktop,
        prepare_resources,
        validate_compiled,
    )

    if compiled is None:
        environment = compile_environment(environment, release_tag)
    executable, compiled_info = compiled_desktop(
        compiled,
        environment,
        lambda env: build_executable(
            "--release",
            "--locked",
            "--no-default-features",
            "--features",
            "desktop",
            env=env,
            metadata="desktop-build-info.json",
        ),
    )
    validate_compiled(compiled_info, release_tag)
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
        app = output / f"{app_name}.app"
        dmg = Path(temporary) / f"{app_name}.dmg"
        if compiled is not None:
            assemble_bundle(app, executable, info, config)
        else:
            print(f"Packaging {app_name}.app with cargo-packager")
            packaged = subprocess.run(
                ["cargo", "packager", "--config", json.dumps(config), "--out-dir", str(output)],
                cwd=ROOT,
                env=environment,
            )
            if packaged.returncode or not app.is_dir():
                raise TaskError("cargo-packager did not produce the application")
        prepare_resources(app, compiled_info, release_tag, macos=True, execute=execute)
        signing = (
            macos_signing.release_keychain(identity_name, os.environ)
            if identity_name != macos_codesign.ADHOC
            else nullcontext()
        )
        notarizing = macos_notary.notary() if release_tag else nullcontext()
        with signing, notarizing as notary:
            # cargo-packager signs every native file with the app's entitlements and never
            # reseals nested bundles, so it receives no identity and SpaceTerm signs instead.
            # A release notarizes and staples the app before the image, so the image holds a
            # stapled app.
            sign_app(
                app,
                identity_name,
                identity_directory(name) / "Entitlements.plist",
                team=team,
                release=bool(release_tag),
                notary=notary,
                environment=environment,
            )
            print(f"Building {app_name}.dmg with dmgbuild")
            disk_image.build(
                app, dmg, app / "Contents" / "Resources" / f"{ICON_NAME}.icns", environment
            )
            macos_codesign.sign_disk_image(
                dmg, identity_name, template["CFBundleIdentifier"], environment=environment
            )
            if release_tag:
                macos_notary.notarize(dmg, notary, environment)
        verify(app, dmg, release_tag, execute=execute)
        shutil.rmtree(DIST / app.name, ignore_errors=True)
        (DIST / dmg.name).unlink(missing_ok=True)
        app.rename(DIST / app.name)
        dmg.rename(DIST / dmg.name)
    print(f"Created {app_name}.app and {app_name}.dmg in dist")


def assemble_bundle(app, executable, info, config):
    """Assemble an already-compiled unsigned app without invoking Cargo or cargo-packager."""
    contents = app / "Contents"
    binaries = contents / "MacOS"
    resources = contents / "Resources"
    binaries.mkdir(parents=True)
    resources.mkdir()
    shutil.copyfile(executable, binaries / info["CFBundleExecutable"])
    (binaries / info["CFBundleExecutable"]).chmod(0o755)
    generated = {
        "CFBundlePackageType": "APPL",
        "LSMinimumSystemVersion": MINIMUM_MACOS,
        "LSApplicationCategoryType": "public.app-category.developer-tools",
        **info,
    }
    (contents / "Info.plist").write_bytes(plistlib.dumps(generated))
    shutil.copyfile(STAGE / f"{ICON_NAME}.icns", resources / f"{ICON_NAME}.icns")
    for entry in config["resources"]:
        source = ROOT / entry["src"]
        destination = resources / entry["target"]
        if source.is_dir():
            shutil.copytree(source, destination)
        else:
            shutil.copyfile(source, destination)
    for framework in config["macos"].get("frameworks", []):
        destination = contents / "Frameworks" / Path(framework).name
        destination.parent.mkdir(exist_ok=True)
        shutil.copytree(framework, destination, symlinks=True)


def verify(app=None, dmg=None, release_tag=None, *, execute: bool = True):
    """Verify the invariants SpaceTerm owns in an application bundle and its disk image."""
    require_apple_silicon()
    name = selected_identity(release_tag)
    app_name = identity(name)["CFBundleName"]
    check_app = app is not None or dmg is None
    app = Path(app) if app else DIST / f"{app_name}.app"
    dmg = Path(dmg) if dmg else DIST / f"{app_name}.dmg"
    if check_app:
        verify_bundle(app, name, release_tag, "dist", execute=execute)
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
            background = mount / disk_image.BACKGROUND_IN_IMAGE
            if (
                not background.is_file()
                or background.read_bytes() != disk_image.BACKGROUND.read_bytes()
                or not (mount / ".DS_Store").is_file()
            ):
                raise TaskError("the disk image window must show the brand background")
            result = verify_bundle(
                mount / f"{app_name}.app", name, release_tag, "dmg", execute=execute
            )
        finally:
            subprocess.run(["hdiutil", "detach", str(mount), "-force"], capture_output=True)
    print(f"Verified {app_name}.app and {app_name}.dmg")
    return result


def verify_bundle(app, name, release_tag, label, *, execute: bool = True):
    if execute:
        from spaceterm_tasks.desktop_manifest import require_no_release_secrets

        require_no_release_secrets()
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
    if execute:
        verify_application_identity(
            executable,
            template["CFBundleName"],
            label,
            release_version(release_tag) if release_tag else None,
        )
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
    from spaceterm_tasks.desktop_manifest import verify_embedded_resources

    result = verify_embedded_resources(
        resources,
        template["CFBundleIdentifier"].removeprefix("com.peblon."),
        execute=execute,
        executable=executable,
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

    return result


def sign_app(app, identity_name, entitlements, *, team, release, notary, environment):
    """Sign the finished app. A release then notarizes and staples it; a dry run's `notary` is
    None, which logs the skipped step."""
    # Extended attributes such as Finder information make codesign refuse a bundle.
    checked(["xattr", "-cr", str(app)], "the app's extended attributes could not be cleared")
    macos_codesign.sign_desktop(
        app,
        identity_name,
        entitlements,
        team=team,
        environment=macos_signing.signing_environment(environment),
    )
    if release:
        macos_notary.notarize(app, notary, environment)
