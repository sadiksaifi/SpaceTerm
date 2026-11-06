"""Read application identities and compile the brand icon into their bundles."""

import plistlib
import subprocess
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError

# Every bundle, Core's included, compiles the brand's one icon document under its own name.
ICON_DOCUMENT = Path("assets/brand/apple/SpaceTerm.icon")
ICON_NAME = ICON_DOCUMENT.stem
ICON_OUTPUTS = (f"{ICON_NAME}.icns", "Assets.car", "IconPartialInfo.plist")


def identity_directory(name, root=ROOT):
    return root / "packaging" / "macos" / name


def identity(name, root=ROOT):
    """Return the Info.plist template of the spaceterm, preflight, or development identity."""
    return plistlib.loads((identity_directory(name, root) / "Info.plist").read_bytes())


def compile_icon(output, bundle_identifier, root=ROOT):
    """Compile the brand's Icon Composer document into icns, Assets.car, and a partial plist."""
    document = root / ICON_DOCUMENT
    if not (document / "icon.json").is_file():
        raise TaskError("the brand Icon Composer document is missing")
    output = Path(output)
    output.mkdir(parents=True, exist_ok=True)
    print(f"Compiling layered {ICON_NAME}.icon for {bundle_identifier}")
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
            bundle_identifier,
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
