"""Read application identities and compile their icons."""

import plistlib
import shutil
import subprocess
import tempfile
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError

# Every identity compiles under the one internal icon name that the bundle templates expect.
ICON_NAME = "SpaceTerm"
ICON_OUTPUTS = (f"{ICON_NAME}.icns", "Assets.car", "IconPartialInfo.plist")


def identity_directory(name, root=ROOT):
    return root / "packaging" / "macos" / name


def identity(name, root=ROOT):
    """Return the Info.plist template of the spaceterm, preflight, or development identity."""
    return plistlib.loads((identity_directory(name, root) / "Info.plist").read_bytes())


def compile_icon(name, output, root=ROOT):
    """Compile one identity's Icon Composer document into icns, Assets.car, and a partial plist."""
    template = identity(name, root)
    source = identity_directory(name, root) / f"{template['CFBundleName']}.icon"
    if not (source / "icon.json").is_file():
        raise TaskError(f"the {name} identity has no Icon Composer document")
    output = Path(output)
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=output, prefix=".icon.") as staging:
        document = Path(staging) / f"{ICON_NAME}.icon"
        shutil.copytree(source, document)
        print(f"Compiling layered {ICON_NAME}.icon for {template['CFBundleName']}")
        result = subprocess.run(
            [
                "xcrun",
                "actool",
                str(document),
                "--compile",
                str(output),
                "--platform",
                "macosx",
                "--minimum-deployment-target",
                "26.0",
                "--app-icon",
                ICON_NAME,
                "--output-partial-info-plist",
                str(output / "IconPartialInfo.plist"),
                "--enable-on-demand-resources",
                "NO",
                "--development-region",
                "en",
                "--target-device",
                "mac",
                "--bundle-identifier",
                template["CFBundleIdentifier"],
            ],
            stdout=subprocess.DEVNULL,
        )
    if result.returncode:
        raise TaskError("actool could not compile the application icon")
    for generated in ICON_OUTPUTS:
        if not (output / generated).is_file():
            raise TaskError(f"actool did not produce {generated}")
    partial = plistlib.loads((output / "IconPartialInfo.plist").read_bytes())
    for key in ("CFBundleIconFile", "CFBundleIconName"):
        if partial.get(key) != ICON_NAME:
            raise TaskError(f"actool emitted an unexpected {key}")
