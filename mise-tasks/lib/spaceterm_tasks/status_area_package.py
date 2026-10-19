"""Build and package the independent status-area helper without registering or launching it."""

import argparse
import filecmp
import os
import plistlib
import re
import shutil
import stat
import subprocess
from pathlib import Path

from spaceterm_tasks import TaskError, macos_signing, main
from spaceterm_tasks.cargo import build_executable
from spaceterm_tasks.macos_bundle import ICON_NAME
from spaceterm_tasks.packaging import checked, verify_application_identity

BINARY = "spaceterm-status-area"
MINIMUM_MACOS = "26.0"


def build(*arguments, env=None):
    return build_executable("--package", BINARY, *arguments, "--locked", binary=BINARY, env=env)


def stage_executable(executable, directory):
    directory.mkdir(parents=True, exist_ok=True)
    destination = directory / BINARY
    shutil.copyfile(executable, destination)
    destination.chmod(0o755)
    return destination


def macos_app(app, outer):
    return app / "Contents/Library/LoginItems" / f"{outer['CFBundleName']} Server Status.app"


def macos_info(outer):
    name = f"{outer['CFBundleName']} Server Status"
    return {
        "CFBundleIdentifier": outer["CFBundleIdentifier"] + ".status-area",
        "CFBundleName": name,
        "CFBundleDisplayName": name,
        "CFBundleExecutable": BINARY,
        "CFBundlePackageType": "APPL",
        "CFBundleInfoDictionaryVersion": "6.0",
        "CFBundleShortVersionString": outer["CFBundleShortVersionString"],
        "CFBundleVersion": outer["CFBundleVersion"],
        "CFBundleIconFile": ICON_NAME,
        "CFBundleIconName": ICON_NAME,
        "LSUIElement": True,
        "LSMinimumSystemVersion": MINIMUM_MACOS,
        "NSHighResolutionCapable": True,
    }


def stage_macos(app, executable, outer):
    nested = macos_app(app, outer)
    contents = nested / "Contents"
    stage_executable(executable, contents / "MacOS")
    resources = contents / "Resources"
    resources.mkdir()
    for name in (f"{ICON_NAME}.icns", "Assets.car"):
        shutil.copyfile(app / "Contents/Resources" / name, resources / name)
    (contents / "Info.plist").write_bytes(plistlib.dumps(macos_info(outer)))
    return nested


def sign_macos(app, signing_identity, release=False, entitlements=None):
    arguments = ["codesign", "--force", "--sign", signing_identity]
    if release:
        arguments.extend(["--options", "runtime", "--timestamp"])
        info = plistlib.loads((app / "Contents/Info.plist").read_bytes())
        requirement = macos_signing.certificate_requirement(signing_identity)
        arguments.extend(
            [
                "--identifier",
                info["CFBundleIdentifier"],
                "--requirements",
                f'=designated => identifier "{info["CFBundleIdentifier"]}" and {requirement}',
            ]
        )
    else:
        arguments.append("--timestamp=none")
    if entitlements:
        arguments.extend(["--entitlements", str(entitlements)])
    checked([*arguments, str(app)], "status-area application signing failed")


def verify_macos(app, outer, label, signing_identity="-"):
    label = f"{label} status-area"
    nested = macos_app(app, outer)
    contents = nested / "Contents"
    expected = macos_info(outer)
    try:
        info = plistlib.loads((contents / "Info.plist").read_bytes())
    except (OSError, plistlib.InvalidFileException):
        raise TaskError(f"{label} Info.plist is missing or invalid") from None
    if info != expected:
        raise TaskError(f"{label} Info.plist differs from its outer application")
    executable = contents / "MacOS" / BINARY
    verify_executable(executable, label)
    architectures = checked(
        ["lipo", "-archs", str(executable)], f"{label} architecture could not be read", text=True
    )
    if architectures.split() != ["arm64"]:
        raise TaskError(f"{label} executable must contain only Apple Silicon code")
    build = checked(
        ["xcrun", "vtool", "-show-build", str(executable)],
        f"{label} deployment target could not be read",
        text=True,
    )
    if f"minos {MINIMUM_MACOS}" not in " ".join(build.split()):
        raise TaskError(f"{label} executable deployment target differs from its bundle minimum")
    outer_version = verify_application_identity(
        app / "Contents/MacOS" / outer["CFBundleExecutable"],
        outer["CFBundleName"],
        f"{label} outer",
    )
    verify_application_identity(executable, expected["CFBundleName"], label, outer_version)
    for name in (f"{ICON_NAME}.icns", "Assets.car"):
        copied = contents / "Resources" / name
        if not copied.is_file() or not filecmp.cmp(
            app / "Contents/Resources" / name, copied, shallow=False
        ):
            raise TaskError(f"{label} icon differs from its outer application")
    checked(["codesign", "--verify", "--strict", str(nested)], f"{label} signature is invalid")
    details = checked(
        ["codesign", "--display", "--entitlements", ":-", str(nested)],
        f"{label} entitlements could not be read",
    )
    if details.strip() and plistlib.loads(details):
        raise TaskError(f"{label} must have no extra entitlements")
    signature = subprocess.run(
        ["codesign", "--display", "--verbose=4", str(nested)], capture_output=True, text=True
    )
    if signature.returncode:
        raise TaskError(f"{label} signature details could not be read")
    if signing_identity == "-":
        if "Signature=adhoc" not in signature.stderr:
            raise TaskError(f"{label} must be ad hoc signed")
    else:
        if not re.search(r"flags=.*\bruntime\b", signature.stderr):
            raise TaskError(f"{label} release signature requires the hardened runtime")
        macos_signing.verify_release_signature(
            nested, signing_identity, expected["CFBundleIdentifier"], label
        )


def verify_executable(executable, label):
    try:
        mode = executable.lstat().st_mode
    except OSError:
        raise TaskError(f"{label} executable is missing") from None
    if not stat.S_ISREG(mode) or stat.S_IMODE(mode) != 0o755:
        raise TaskError(f"{label} executable must be a regular file with mode 0755")


def finalize_macos(app, executable, signing_identity, entitlements, release=False):
    outer = plistlib.loads((app / "Contents/Info.plist").read_bytes())
    nested = stage_macos(app, executable, outer)
    sign_macos(nested, signing_identity, release)
    sign_macos(app, signing_identity, release, entitlements)
    checked(
        ["codesign", "--verify", "--strict", "--deep", str(app)],
        "the application signature does not seal its status-area helper",
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("app", type=Path)
    parser.add_argument("executable", type=Path)
    parser.add_argument("signing_identity")
    parser.add_argument("entitlements", type=Path)
    parser.add_argument("--release", action="store_true")
    args = parser.parse_args()
    # The packager invokes the same hook before app and dmg. Only the final app is resealed.
    main(
        lambda: (
            finalize_macos(
                args.app, args.executable, args.signing_identity, args.entitlements, args.release
            )
            if os.environ.get("CARGO_PACKAGER_FORMAT") == "dmg"
            else None
        )
    )
