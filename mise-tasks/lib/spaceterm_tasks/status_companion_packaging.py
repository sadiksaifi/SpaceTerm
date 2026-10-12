"""Stage, finalize, and verify the optional status companion in desktop packages.

The companion is a second client executable in the desktop package, never in the headless
server package. macOS nests `<Desktop> Status.app` in the desktop bundle's Contents/Helpers.
Linux installs `bin/spaceterm-status` beside `bin/spaceterm`. Its identifier is the desktop
identifier followed by `.status`, so it shares no bundle identifier, application ID, or bus
name with the desktop. Packaging registers no login item, launch agent, autostart entry, or
service: the user launches the companion explicitly.
"""

import os
import plistlib
import re
import shutil
import subprocess
import tempfile
import tomllib
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from xml.sax.saxutils import escape

from spaceterm_tasks import ROOT, TaskError, macos_bundle, macos_signing
from spaceterm_tasks import server_artifacts as artifacts
from spaceterm_tasks.cargo import build_executable
from spaceterm_tasks.macos_bundle import ICON_NAME
from spaceterm_tasks.macos_signing import certificate_requirement, signing_environment
from spaceterm_tasks.package_linux import verify_glibc_baseline
from spaceterm_tasks.packaging import selected_identity, verify_application_identity

BINARY = "spaceterm-status"
FEATURE = "status-companion"
IDENTITIES = ("spaceterm", "preflight", "development")
INFO_TEMPLATE = ROOT / "packaging" / "companion" / "macos" / "Info.plist.in"
ENTRY_TEMPLATE = ROOT / "packaging" / "companion" / "linux" / "status.desktop.in"
# The installer reads the desktop identity from the one entry in share/applications, and
# adding the companion to a menu is the user's choice, so its entry stays beside the resources.
ENTRY_DIRECTORY = PurePosixPath("share/spaceterm/companion")
# The desktop's compiled icon outputs, which the nested bundle shares.
ICON_RESOURCES = (f"{ICON_NAME}.icns", "Assets.car")
MINIMUM_MACOS = "26.0"
VERSION = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")
PLACEHOLDER = re.compile(r"@([A-Z_]+)@")
# Codesign timestamps and disk image conversion can take minutes but must not hang a release.
TOOL_TIMEOUT = 600
# Names that would register background launch or supervision. A desktop package has none.
REGISTRATIONS = frozenset({"loginitems", "launchagents", "launchdaemons", "autostart", "systemd"})
UNIT_SUFFIXES = (".service", ".socket", ".timer")
# Every server entry point and bundle name. Replacing the client never replaces the server.
SERVER_NAMES = frozenset(
    {
        PurePosixPath(path).name
        for platform in artifacts.PLATFORMS
        for build in artifacts.BUILDS
        for path in artifacts.entry_points(platform, build).values()
        if path
    }
    | {artifacts.bundle_name(build) for build in artifacts.BUILDS}
)


@dataclass(frozen=True)
class Companion:
    """The companion names that one desktop identity determines."""

    desktop_identifier: str
    desktop_name: str

    @property
    def identifier(self):
        return f"{self.desktop_identifier}.status"

    @property
    def name(self):
        return f"{self.desktop_name} Status"

    @property
    def bundle(self):
        return f"{self.name}.app"

    @property
    def entry(self):
        return ENTRY_DIRECTORY / f"{self.identifier}.desktop"


def companion(identity):
    """The companion of the spaceterm, preflight, or development desktop identity."""
    if identity not in IDENTITIES:
        raise TaskError("the status companion has no such desktop identity")
    template = macos_bundle.identity(identity)
    return Companion(template["CFBundleIdentifier"], template["CFBundleName"])


def render(template, values):
    """Fill every placeholder, and reject a template whose placeholders differ from `values`."""
    text = template.read_text(encoding="utf-8")
    if set(PLACEHOLDER.findall(text)) != set(values):
        raise TaskError("a companion template's placeholders differ from its inputs")
    return PLACEHOLDER.sub(lambda match: values[match[1]], text)


def info_plist(identity, version):
    if not isinstance(version, str) or not VERSION.fullmatch(version):
        raise TaskError("a companion bundle version is a canonical stable version")
    names = companion(identity)
    document = render(
        INFO_TEMPLATE,
        {"IDENTIFIER": escape(names.identifier), "NAME": escape(names.name), "VERSION": version},
    ).encode()
    try:
        plistlib.loads(document)
    except plistlib.InvalidFileException:
        raise TaskError("the companion Info.plist template is invalid") from None
    return document


def desktop_entry(identity):
    names = companion(identity)
    return render(
        ENTRY_TEMPLATE,
        {"NAME": names.name, "DESKTOP_NAME": names.desktop_name, "ICON": names.desktop_identifier},
    )


def run_tool(arguments, failure, *, env=None, timeout=TOOL_TIMEOUT, check=True):
    """Run one native tool without signing inputs. Its failure reports only `failure`."""
    try:
        result = subprocess.run(
            [str(argument) for argument in arguments],
            capture_output=True,
            timeout=timeout,
            env=signing_environment(os.environ if env is None else env),
        )
    except (OSError, subprocess.TimeoutExpired):
        raise TaskError(failure) from None
    if check and result.returncode:
        raise TaskError(failure)
    return result


def build_companion(*cargo_args, env=None):
    """Build the feature-gated companion with the desktop build's identity environment."""
    return build_executable(
        *cargo_args, "-p", "spaceterm", "--features", FEATURE, binary=BINARY, env=env
    )


def stage_macos(stage, executable, identity, version):
    """Stage `<Desktop> Status.app` in `stage`, which holds the desktop's compiled icon.

    Returns the unsigned bundle. Its executable takes the bundle name, as the desktop's does.
    """
    names = companion(identity)
    stage = Path(stage)
    bundle = stage / names.bundle
    if bundle.exists() or bundle.is_symlink():
        raise TaskError("a companion bundle is already staged")
    for resource in ICON_RESOURCES:
        if not (stage / resource).is_file():
            raise TaskError("the companion is staged after the desktop icon is compiled")
    contents = bundle / "Contents"
    for directory in (bundle, contents, contents / "MacOS", contents / "Resources"):
        directory.mkdir(mode=0o755)
        directory.chmod(0o755)
    (contents / "Info.plist").write_bytes(info_plist(identity, version))
    (contents / "Info.plist").chmod(0o644)
    binary = contents / "MacOS" / names.name
    shutil.copyfile(executable, binary)
    binary.chmod(0o755)
    for resource in ICON_RESOURCES:
        shutil.copyfile(stage / resource, contents / "Resources" / resource)
        (contents / "Resources" / resource).chmod(0o644)
    return bundle


def stage_linux(tree, executable, identity):
    """Install the companion beside a staged desktop executable, with its optional entry."""
    names = companion(identity)
    tree = Path(tree)
    if not (tree / "bin" / "spaceterm").is_file():
        raise TaskError("the companion is staged beside a staged desktop executable")
    binary = tree / "bin" / BINARY
    entry = tree / names.entry
    if binary.exists() or binary.is_symlink() or entry.parent.exists():
        raise TaskError("a companion is already staged")
    shutil.copyfile(executable, binary)
    binary.chmod(0o755)
    entry.parent.mkdir(mode=0o755, parents=True)
    entry.parent.chmod(0o755)
    entry.write_text(desktop_entry(identity), encoding="utf-8")
    entry.chmod(0o644)


def finalize_macos_package(output, executable, identity, version, config, environment, release_tag):
    """Nest the companion in a packaged desktop bundle, re-sign it, and rebuild its disk image.

    Runs inside the packager's signing scope after cargo-packager writes `output`. The nested
    bundle is signed before the outer bundle. The outer bundle is re-signed as cargo-packager
    signed it, with its own identifier, entitlements, and signer, so its designated requirement
    does not change. The disk image is rebuilt from the final bundle.
    """
    if selected_identity(release_tag) != identity:
        raise TaskError("the companion identity differs from the package identity")
    signer = config["macos"]["signing-identity"]
    if release_tag:
        certificate_requirement(signer)
    elif signer != "-":
        raise TaskError("a Preflight companion is ad hoc signed")
    template = macos_bundle.identity(identity)
    output = Path(output)
    app = output / f"{template['CFBundleName']}.app"
    dmg = output / f"{template['CFBundleName']}_{version}_aarch64.dmg"
    if not app.is_dir() or not dmg.is_file():
        raise TaskError("the desktop package to finalize is missing")
    helpers = app / "Contents" / "Helpers"
    if helpers.exists():
        raise TaskError("the desktop bundle already holds helpers")
    environment = signing_environment(environment)
    with tempfile.TemporaryDirectory(dir=output, prefix=".companion.") as temporary:
        work = Path(temporary)
        for resource in ICON_RESOURCES:
            source = app / "Contents" / "Resources" / resource
            if not source.is_file():
                raise TaskError("the desktop bundle has no compiled icon")
            shutil.copyfile(source, work / resource)
        staged = stage_macos(work, executable, identity, version)
        helpers.mkdir(mode=0o755)
        nested = helpers / staged.name
        staged.rename(nested)
        run_tool(
            ["xattr", "-crs", nested],
            "companion extended attributes could not be cleared",
            env=environment,
        )
        run_tool(
            [
                "codesign",
                "--force",
                "--sign",
                signer,
                "--options",
                "runtime",
                "--timestamp",
                nested,
            ],
            "the companion bundle could not be signed",
            env=environment,
        )
        run_tool(
            [
                "codesign",
                "--force",
                "--sign",
                signer,
                "--entitlements",
                ROOT / config["macos"]["entitlements"],
                "--options",
                "runtime",
                "--timestamp",
                app,
            ],
            "the desktop bundle could not be signed with its companion",
            env=environment,
        )
        rebuild_disk_image(app, dmg, template["CFBundleName"], signer, work, environment)


def rebuild_disk_image(app, dmg, volume_name, signer, work, environment):
    """Lay out the disk image as cargo-packager's create-dmg does in CI, from the final bundle."""
    volume = work / "volume"
    volume.mkdir()
    run_tool(
        ["ditto", app, volume / app.name], "the disk image could not be staged", env=environment
    )
    (volume / "Applications").symlink_to("/Applications")
    icon = volume / ".VolumeIcon.icns"
    shutil.copyfile(app / "Contents" / "Resources" / f"{ICON_NAME}.icns", icon)
    run_tool(
        ["xcrun", "SetFile", "-c", "icnC", icon],
        "the disk image icon could not be staged",
        env=environment,
    )
    writable = work / "volume.dmg"
    run_tool(
        [
            "hdiutil",
            "create",
            "-quiet",
            "-srcfolder",
            volume,
            "-volname",
            volume_name,
            "-fs",
            "HFS+",
            "-fsargs",
            "-c c=64,a=16,e=16",
            "-format",
            "UDRW",
            writable,
        ],
        "the disk image could not be created",
        env=environment,
    )
    mount = work / "mount"
    mount.mkdir()
    run_tool(
        [
            "hdiutil",
            "attach",
            "-quiet",
            "-nobrowse",
            "-readwrite",
            "-noverify",
            "-noautoopen",
            "-mountpoint",
            mount,
            writable,
        ],
        "the disk image could not be mounted",
        env=environment,
    )
    try:
        # The volume shows its custom icon only when its root carries the custom-icon flag.
        run_tool(
            ["xcrun", "SetFile", "-a", "C", mount],
            "the disk image icon could not be applied",
            env=environment,
        )
        shutil.rmtree(mount / ".fseventsd", ignore_errors=True)
    except BaseException:
        run_tool(["hdiutil", "detach", mount, "-force"], "", env=environment, check=False)
        raise
    run_tool(
        ["hdiutil", "detach", "-quiet", mount],
        "the disk image could not be detached",
        env=environment,
    )
    # Codesign derives the image's identifier from its file name, as for cargo-packager's image.
    final = work / dmg.name
    run_tool(
        [
            "hdiutil",
            "convert",
            "-quiet",
            writable,
            "-format",
            "UDZO",
            "-imagekey",
            "zlib-level=9",
            "-o",
            final,
        ],
        "the disk image could not be compressed",
        env=environment,
    )
    run_tool(
        ["codesign", "--force", "--sign", signer, "--timestamp", final],
        "the disk image could not be signed",
        env=environment,
    )
    final.replace(dmg)


def verify_companion(package, identity):
    """Verify the companion in a desktop `.app` bundle or an extracted Linux installation tree.

    The package must also carry no server artifact and nothing that registers a login item,
    launch agent, autostart entry, or service.
    """
    package = Path(package)
    names = companion(identity)
    reject_background_registration(package)
    if package.suffix == ".app":
        verify_macos_companion(package, identity, names)
    else:
        verify_linux_companion(package, identity, names)


def reject_background_registration(package):
    for _, directories, files in os.walk(package):
        for name in (*directories, *files):
            if name in SERVER_NAMES:
                raise TaskError("the desktop package carries a server artifact")
            if name.lower() in REGISTRATIONS or name.lower().endswith(UNIT_SUFFIXES):
                raise TaskError("the desktop package registers background launch or a service")


def reported_version(executable, expected, label):
    """The version an executable reports after its compiled name, as verify checks it."""
    try:
        result = subprocess.run([executable, "--version"], env={}, capture_output=True, timeout=5)
    except (OSError, subprocess.TimeoutExpired):
        raise TaskError(f"{label} executable application identity query failed") from None
    name, _, reported = result.stdout.removesuffix(b"\n").rpartition(b" ")
    if result.returncode or name != expected.encode() or not reported:
        raise TaskError(f"{label} executable application identity must be {expected}")
    try:
        return reported.decode("ascii")
    except UnicodeDecodeError:
        raise TaskError(f"{label} executable version is invalid") from None


def entries(directory):
    return sorted(path.name for path in directory.iterdir()) if directory.is_dir() else []


def read_plist(path, label):
    try:
        return plistlib.loads(path.read_bytes())
    except (OSError, plistlib.InvalidFileException):
        raise TaskError(f"{label} Info.plist is missing or invalid") from None


def verify_macos_companion(app, identity, names):
    template = macos_bundle.identity(identity)
    contents = app / "Contents"
    desktop = read_plist(contents / "Info.plist", "desktop")
    if any(
        desktop.get(key) != template[key] for key in ("CFBundleIdentifier", "CFBundleExecutable")
    ):
        raise TaskError("the desktop bundle identity changed")
    if entries(contents / "Helpers") != [names.bundle]:
        raise TaskError("the desktop bundle must hold only the companion in Contents/Helpers")
    nested = contents / "Helpers" / names.bundle
    expected = plistlib.loads(info_plist(identity, desktop.get("CFBundleShortVersionString")))
    if read_plist(nested / "Contents" / "Info.plist", "companion") != expected:
        raise TaskError(f"companion Info.plist differs from the {identity} identity")
    if entries(nested / "Contents" / "MacOS") != [names.name]:
        raise TaskError("the companion bundle must hold only its executable")
    executable = nested / "Contents" / "MacOS" / names.name
    if not os.access(executable, os.X_OK):
        raise TaskError("companion executable is missing")
    architectures = run_tool(
        ["lipo", "-archs", executable], "companion executable architecture could not be read"
    ).stdout
    if architectures.split() != [b"arm64"]:
        raise TaskError("companion executable must contain only Apple Silicon code")
    build = run_tool(
        ["xcrun", "vtool", "-show-build", executable],
        "companion executable deployment target could not be read",
    ).stdout
    if f"minos {MINIMUM_MACOS}".encode() not in b" ".join(build.split()):
        raise TaskError("companion executable deployment target differs from its bundle minimum")
    version = reported_version(
        contents / "MacOS" / template["CFBundleExecutable"], template["CFBundleName"], "desktop"
    )
    verify_application_identity(executable, names.name, "companion", version)

    run_tool(["codesign", "--verify", "--strict", nested], "companion signature is invalid")
    run_tool(
        ["codesign", "--verify", "--strict", app],
        "the desktop signature does not seal its companion",
    )
    details = run_tool(
        ["codesign", "--display", "--verbose=4", nested], "companion signature could not be read"
    ).stderr
    flags = re.search(rb"\bflags=0x([0-9a-fA-F]+)\b", details)
    if flags is None or not int(flags.group(1), 16) & 0x10000:
        raise TaskError("companion executable is not hardened")
    if identity == "spaceterm":
        fingerprint = tomllib.loads(
            (macos_bundle.identity_directory(identity) / "Packager.toml").read_text()
        )["macos"]["signing-identity"]
        macos_signing.verify_release_signature(nested, fingerprint, names.identifier, "companion")
    elif b"Signature=adhoc" not in details:
        raise TaskError("companion is not ad hoc signed")
    entitlements = run_tool(
        ["codesign", "--display", "--entitlements", ":-", nested],
        "companion entitlements could not be read",
    ).stdout
    try:
        granted = plistlib.loads(entitlements) if entitlements.strip() else {}
    except plistlib.InvalidFileException:
        raise TaskError("companion entitlements are invalid") from None
    if granted != {}:
        raise TaskError("the companion carries no entitlements")


def verify_linux_companion(tree, identity, names):
    if entries(tree / "bin") != ["spaceterm", BINARY]:
        raise TaskError("the desktop tree must hold only the desktop and companion executables")
    executable = tree / "bin" / BINARY
    if not os.access(executable, os.X_OK):
        raise TaskError("companion executable is missing")
    header = run_tool(["objdump", "-f", executable], "companion executable is unreadable").stdout
    if b"elf64-x86-64" not in header:
        raise TaskError("companion executable must contain only x86_64 code")
    # Preflight runs only where it was built. Releases must run on every supported host.
    if identity == "spaceterm":
        verify_glibc_baseline(executable, "companion")
    version = reported_version(tree / "bin" / "spaceterm", names.desktop_name, "desktop")
    verify_application_identity(executable, names.name, "companion", version)
    if entries(tree / "share" / "applications") != [f"{names.desktop_identifier}.desktop"]:
        raise TaskError("the desktop tree must keep exactly its own desktop entry")
    entry = tree / names.entry
    if entries(entry.parent) != [entry.name] or entry.read_text(encoding="utf-8") != desktop_entry(
        identity
    ):
        raise TaskError(f"companion desktop entry differs from the {identity} identity")
    run_tool(["desktop-file-validate", entry], "companion desktop entry is invalid")
