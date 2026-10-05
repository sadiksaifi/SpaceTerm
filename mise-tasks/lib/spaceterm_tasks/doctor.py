"""Development environment checks that `mise doctor project` runs; see [doctor.checks]."""

import ctypes
import shutil
import subprocess
import sys

MINIMUM_XCODE = 26
XCODE_TOOLS = ("codesign", "actool", "assetutil", "hdiutil", "metal")


def xcode():
    result = subprocess.run(["xcodebuild", "-version"], capture_output=True, text=True)
    version = result.stdout.split()[1] if result.returncode == 0 else ""
    if not version or int(version.split(".")[0]) < MINIMUM_XCODE:
        return f"Xcode {MINIMUM_XCODE} or newer is required"
    for tool in XCODE_TOOLS:
        if subprocess.run(["xcrun", "--find", tool], capture_output=True).returncode:
            return f"Xcode does not provide {tool}"
    if subprocess.run(["xcrun", "metal", "--version"], capture_output=True).returncode:
        return "the Metal toolchain is missing"
    return None


def vulkan():
    try:
        ctypes.CDLL("libvulkan.so.1")
    except OSError:
        return "the Vulkan loader is missing"
    return None


def gio():
    # Desktop entry tests parse launchers with the system Python's GIO bindings.
    probe = ["/usr/bin/python3", "-c", "from gi.repository import Gio; assert Gio.DesktopAppInfo"]
    if subprocess.run(probe, capture_output=True).returncode:
        return "the system Python cannot import GIO"
    return None


def tools(*names):
    missing = [name for name in names if shutil.which(name) is None]
    return f"missing: {', '.join(missing)}" if missing else None


CHECKS = {"xcode": xcode, "vulkan": vulkan, "gio": gio, "tools": tools}

if __name__ == "__main__":
    failure = CHECKS[sys.argv[1]](*sys.argv[2:])
    if failure:
        print(failure, file=sys.stderr)
        raise SystemExit(1)
