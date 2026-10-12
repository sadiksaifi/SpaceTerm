"""Sign a background-only server with no desktop runtime exceptions."""

import os
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, package_server
from spaceterm_tasks import server_artifacts as artifacts
from spaceterm_tasks.macos_signing import (
    MACH_O_MAGIC,
    certificate_requirement,
    release_keychain,
    signing_environment,
    verify_release_signature,
)
from spaceterm_tasks.packaging import DIST, checked, packaged_environment, release_version

TEMPLATE = ROOT / "packaging/server/macos/Info.plist"
ENTITLEMENTS = ROOT / "packaging/server/macos/Entitlements.plist"


def verify_server_signature(path, identifier):
    verify_release_signature(path, artifacts.PINNED_CERTIFICATE, identifier, "server")
    binary = path / "Contents/MacOS/spaceterm-server" if path.is_dir() else path
    if (
        checked(
            ["lipo", "-archs", binary],
            "server architecture query failed",
            text=True,
            env=signing_environment(os.environ),
        ).strip()
        != "arm64"
    ):
        raise TaskError("server executable is not Apple Silicon only")
    try:
        details = subprocess.run(
            ["codesign", "--display", "--verbose=4", path],
            capture_output=True,
            timeout=30,
            env=signing_environment(os.environ),
        )
    except subprocess.TimeoutExpired:
        raise TaskError("server signature observation timed out") from None
    flags = re.search(rb"\bflags=0x([0-9a-fA-F]+)\b", details.stderr)
    if details.returncode or flags is None or not int(flags.group(1), 16) & 0x10000:
        raise TaskError("server executable is not hardened")
    entitlements = checked(
        ["codesign", "--display", "--entitlements", ":-", path],
        "server entitlements could not be inspected",
        env=signing_environment(os.environ),
    )
    try:
        artifacts.validate_entitlements(
            plistlib.loads(entitlements) if entitlements.strip() else {}
        )
    except plistlib.InvalidFileException:
        raise TaskError("server entitlement encoding is unsupported") from None


def verify_macho_closure(root, code):
    """Admit only the three packaged executables and fixed headless OS dependencies.

    Private dylibs and loader-relative paths have no packaging policy yet. Reject
    them rather than letting signatures stand in for dependency resolution.
    """
    system = {
        "/usr/lib/libSystem.B.dylib",
        "/usr/lib/libc++.1.dylib",
        "/usr/lib/libc++abi.dylib",
        "/usr/lib/libobjc.A.dylib",
        "/usr/lib/libresolv.9.dylib",
        *(
            f"/System/Library/Frameworks/{name}.framework/Versions/A/{name}"
            for name in (
                "CoreFoundation",
                "Foundation",
                "Security",
                "SystemConfiguration",
                "DiskArbitration",
                "IOKit",
            )
        ),
    }
    native = set()
    for path in root.rglob("*"):
        if path.is_symlink():
            raise TaskError("server package contains a linked resource")
        if path.is_file():
            with path.open("rb") as source:
                if source.read(4) in MACH_O_MAGIC:
                    native.add(path)
    if native != set(code):
        raise TaskError("server package contains missing or unauthenticated native code")
    for path in sorted(native):
        loads = checked(
            ["otool", "-l", path],
            "server Mach-O dependency metadata is unavailable",
            text=True,
            timeout=30,
            env=signing_environment(os.environ),
        )
        commands = re.split(r"(?m)^Load command [0-9]+\s*$", loads)[1:]
        if not commands or len(commands) > 4096 or len(loads) > 1024 * 1024:
            raise TaskError("server Mach-O dependency metadata is invalid")
        for command in commands:
            kinds = re.findall(r"(?m)^\s*cmd (LC_[A-Z0-9_]+)\s*$", command)
            if len(kinds) != 1:
                raise TaskError("server Mach-O load command is invalid")
            kind = kinds[0]
            if kind in ("LC_RPATH", "LC_DYLD_ENVIRONMENT", "LC_ID_DYLIB"):
                raise TaskError("server Mach-O contains an unapproved loader search path")
            if kind in (
                "LC_LOAD_DYLIB",
                "LC_LOAD_WEAK_DYLIB",
                "LC_REEXPORT_DYLIB",
                "LC_LAZY_LOAD_DYLIB",
                "LC_LOAD_UPWARD_DYLIB",
                "LC_LOAD_DYLINKER",
            ):
                names = re.findall(r"(?m)^\s*name (.*?) \(offset [0-9]+\)\s*$", command)
                allowed = {"/usr/lib/dyld"} if kind == "LC_LOAD_DYLINKER" else system
                if len(names) != 1 or names[0] not in allowed:
                    raise TaskError("server Mach-O dependency is external or not headless")


def verify_info_plist(app, metadata):
    data = (app / "Contents/Info.plist").read_bytes()
    if len(data) > 65536:
        raise TaskError("server bundle metadata exceeds its bound")
    try:
        info = plistlib.loads(data)
    except plistlib.InvalidFileException:
        raise TaskError("server bundle metadata is invalid") from None
    if not isinstance(info, dict):
        raise TaskError("server bundle metadata is invalid")
    if (
        info.get("CFBundleIdentifier") != metadata["native_identifier"]
        or info.get("CFBundleExecutable") != "spaceterm-server"
        or info.get("LSBackgroundOnly") is not True
        or info.get("LSMinimumSystemVersion") != "26.0"
        or any(
            key in info
            for key in ("NSPrincipalClass", "CFBundleIconFile", "SUFeedURL", "SUPublicEDKey")
        )
    ):
        raise TaskError("server bundle contains a desktop identity or unsupported activation")


def package(release_tag=None):
    if sys.platform != "darwin":
        raise TaskError("macOS server packaging requires macOS")
    binaries = package_server.build(release_tag)
    build = "release" if release_tag else "preflight"
    metadata = package_server.executable_metadata(
        binaries["spaceterm-server"],
        "darwin-arm64",
        build,
        release_version(release_tag) if release_tag else None,
    )
    entitlements = plistlib.loads(ENTITLEMENTS.read_bytes())
    artifacts.validate_entitlements(entitlements)
    clean = signing_environment(packaged_environment(release_tag))
    DIST.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="spaceterm-server-macos-", dir=DIST) as temporary:
        root = Path(temporary) / "spaceterm-server"
        app = root / artifacts.bundle_name(build)
        info = plistlib.loads(TEMPLATE.read_bytes())
        version = metadata["version"] if release_tag else "0.0.0"
        for key, value in list(info.items()):
            if isinstance(value, str):
                info[key] = (
                    value.replace("@SERVER_IDENTIFIER@", metadata["native_identifier"])
                    .replace("@SERVER_NAME@", app.stem)
                    .replace("@BUNDLE_VERSION@", version)
                )
        (app / "Contents").mkdir(parents=True)
        (app / "Contents/Info.plist").write_bytes(plistlib.dumps(info))
        package_server.copy_file(
            binaries["spaceterm-server"], app / "Contents/MacOS/spaceterm-server", 0o755
        )
        package_server.copy_resources(app / "Contents/Resources")
        for name in ("spaceterm-server-launcher", "spaceterm-server-manager"):
            package_server.copy_file(binaries[name], root / "libexec" / name, 0o555)
        server_identifier = metadata["native_identifier"]
        sign_targets = [
            (app / "Contents/MacOS/spaceterm-server", server_identifier),
            (root / "libexec/spaceterm-server-launcher", server_identifier + ".launcher"),
            (root / "libexec/spaceterm-server-manager", server_identifier + ".manager"),
        ]
        known_code = {target for target, _ in sign_targets}
        verify_macho_closure(root, known_code)
        with release_keychain(artifacts.PINNED_CERTIFICATE, dict(os.environ)):
            for path, native_identifier in [*sign_targets, (app, server_identifier)]:
                requirement = f'=designated => identifier "{native_identifier}" and {certificate_requirement(artifacts.PINNED_CERTIFICATE)}'
                checked(
                    [
                        "codesign",
                        "--force",
                        "--sign",
                        artifacts.PINNED_CERTIFICATE,
                        "--identifier",
                        native_identifier,
                        "--requirements",
                        requirement,
                        "--options",
                        "runtime",
                        "--entitlements",
                        ENTITLEMENTS,
                        "--timestamp=none",
                        path,
                    ],
                    "server signing failed",
                    env=clean,
                )
            for path, native_identifier in [*sign_targets[1:], (app, server_identifier)]:
                verify_server_signature(path, native_identifier)
        verify_macho_closure(root, known_code)
        verify_info_plist(app, metadata)
        package_server.verify_executable_metadata(root, metadata)
        package_server.write_manifest(root, metadata)
        output = Path(temporary) / "server.tar.gz"
        package_server.write_archive(root, output)
        package_server.verify_archive(output)
        destination = package_server.archive_path("darwin-arm64", build)
        shutil.copyfile(output, destination)
    print("Signed headless server archive and native identities verified.")
