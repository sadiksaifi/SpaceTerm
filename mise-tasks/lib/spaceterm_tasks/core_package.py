"""Independent server artifacts authenticated by identity-scoped Ed25519 trust.

On x86_64 glibc Linux (Ubuntu 22.04 or newer) the layout is core/bin/spaceterm beside
core/share/spaceterm. On Apple Silicon macOS 26 it is an app-like bundle named for the
identity, core/<bundle>.app/Contents/{Info.plist,MacOS/spaceterm,Resources}, because macOS
attributes System Permissions to a bundle's identifier and signature. Installation
only retains an exact verified artifact. It never selects an active version or
writes server identity, organization, credentials, stopped intent, or services.
"""

import base64
import fcntl
import json
import os
import platform
import plistlib
import re
import shutil
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
from contextlib import nullcontext
from enum import Enum
from pathlib import Path, PurePosixPath

from spaceterm_tasks import (
    ROOT,
    TaskError,
    macos_codesign,
    macos_notary,
    macos_signing,
    update_signing,
)
from spaceterm_tasks.cargo import build_executable
from spaceterm_tasks.macos_bundle import ICON_NAME, compile_icon
from spaceterm_tasks.package_linux import verify_glibc_baseline, write_archive
from spaceterm_tasks.packaging import (
    DIST,
    NOTICES,
    RELEASE_FLOOR,
    SHELL_INTEGRATION,
    TERMINFO,
    checked,
    packaged_environment,
    release_version,
    require_clean_checkout,
)

TARGETS = {"darwin": "aarch64-apple-darwin", "linux": "x86_64-unknown-linux-gnu"}
IDENTITIES = ("spaceterm", "spaceterm-preflight", "spaceterm-development")
# The product name a person sees for each identity's server bundle. Privacy prompts and System
# Settings show the bundle's Launch Services display name, which is its directory name.
BUNDLES = {
    "spaceterm": "SpaceTerm Core",
    "spaceterm-preflight": "SpaceTerm Core Preflight",
    "spaceterm-development": "SpaceTerm Core Development",
}
CORE_INFO = ROOT / "packaging/macos/core/Info.plist"
# The compiled brand icon, which the Info.plist template names.
ICON_RESOURCES = (f"{ICON_NAME}.icns", "Assets.car")
# The release asset platform of each target.
PLATFORMS = {TARGETS["darwin"]: "macos-arm64", TARGETS["linux"]: "linux-x86_64"}
SHARED_RESOURCES = "share/spaceterm"
MAX_ARCHIVE = 512 * 1024 * 1024
MAX_EXPANDED = 512 * 1024 * 1024
MAX_MEMBERS = 128
# The file that owns the protocol versions. `crates/spaceterm-cli/build.rs` stamps the same
# versions into Core build documents as `protocols`.
PROTOCOL_SOURCE = "crates/spaceterm-core/src/protocol/version.rs"
PROTOCOL_NAMES = {"work": "WORK", "service": "SERVICE", "desktop_channel": "DESKTOP_CHANNEL"}


class ArtifactCause(Enum):
    MISSING = "missing"
    UNREADABLE = "unreadable"
    TIMED_OUT = "timed out"
    SIGNATURE_MISMATCH = "signature mismatch"
    DIGEST_MISMATCH = "digest mismatch"
    OS_ERROR = "operating-system operation failed"


class ArtifactVerificationError(TaskError):
    """A bounded cause, without native error details or artifact contents."""

    def __init__(self, cause: ArtifactCause):
        self.cause = cause
        super().__init__(f"SpaceTerm Core artifact verification failed: {cause.value}")


def protocol_line(constant):
    """Matches the one-line declaration of the protocol version `constant`."""
    return re.compile(
        rf"^pub const {constant}: ProtocolVersion = ProtocolVersion "
        r"\{ major: ([1-9][0-9]*), minor: (0|[1-9][0-9]*) \};$",
        re.MULTILINE,
    )


def protocols(root=ROOT):
    """Returns the `protocols` object of the source tree at `root`."""
    text = (root / PROTOCOL_SOURCE).read_text()
    result = {}
    for key, constant in PROTOCOL_NAMES.items():
        found = protocol_line(constant).findall(text)
        if len(found) != 1:
            raise TaskError("the protocol source must declare each protocol version once")
        result[key] = {"major": int(found[0][0]), "minor": int(found[0][1])}
    return result


def protocol_major(root=ROOT):
    """Returns the work protocol major of the source tree at `root`."""
    return protocols(root)["work"]["major"]


PROTOCOLS = protocols()
PROTOCOL_MAJOR = PROTOCOLS["work"]["major"]
BUILD_KEYS = {
    "schema",
    "stapled",
    "identity",
    "target",
    "commit",
    "version",
    "floor",
    "protocols",
    "source_sha256",
}
RELEASE_VERSION = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")


def core_identifier(identity):
    return f"com.peblon.{identity}.core"


def bundle_directory(identity):
    return f"{BUNDLES[identity]}.app"


def executable_path(target, identity):
    """The server executable, relative to the package tree."""
    if target == TARGETS["darwin"]:
        return f"{bundle_directory(identity)}/Contents/MacOS/spaceterm"
    return "bin/spaceterm"


def resource_directory(target, identity):
    if target == TARGETS["darwin"]:
        return f"{bundle_directory(identity)}/Contents/Resources"
    return SHARED_RESOURCES


def bundle_files(target, identity, stapled):
    """The bundle's Info.plist, its icon, the seal codesign writes and a stapled ticket; none
    without a bundle."""
    if target != TARGETS["darwin"]:
        return set()
    contents = f"{bundle_directory(identity)}/Contents"
    files = {
        f"{contents}/Info.plist",
        f"{contents}/_CodeSignature/CodeResources",
        *(f"{contents}/Resources/{name}" for name in ICON_RESOURCES),
    }
    if stapled:
        files.add(f"{contents}/CodeResources")
    return files


def signed_path(target, identity):
    """What code signing covers: the bundle on macOS."""
    return bundle_directory(identity) if target == TARGETS["darwin"] else "bin/spaceterm"


def core_info(identity):
    """The tracked server Info.plist with this identity's bundle identifier and name."""
    info = plistlib.loads(CORE_INFO.read_bytes())
    if set(info) & {"CFBundleIdentifier", "CFBundleName", "CFBundleDisplayName"}:
        raise TaskError("the server Info.plist template must not name an identity")
    return {
        **info,
        "CFBundleIdentifier": core_identifier(identity),
        "CFBundleName": BUNDLES[identity],
        "CFBundleDisplayName": BUNDLES[identity],
    }


def artifact_trust(identity):
    """The public keys that authenticate an identity's Core archives."""
    if identity == "spaceterm":
        return update_signing.release_trust()
    if identity in ("spaceterm-preflight", "spaceterm-development"):
        return (
            json.loads((ROOT / "packaging/core-nonproduction-trust.json").read_text())[
                "public_key"
            ],
        )
    raise TaskError("the server artifact application identity is unsupported")


def artifact_signing_seed(identity):
    if identity == "spaceterm":
        return update_signing.release_seed()
    if identity not in ("spaceterm-preflight", "spaceterm-development"):
        raise TaskError("the server artifact application identity is unsupported")
    # This tracked seed is intentionally public. It grants no Release authority.
    seed = base64.b64decode(
        json.loads((ROOT / "packaging/core-nonproduction-trust.json").read_text())["seed"],
        validate=True,
    )
    if update_signing.derived_public_key(seed) not in artifact_trust(identity):
        raise TaskError("the signing key differs from the server artifact identity's trust")
    return seed


def archive_name(identity, version, target):
    """`spaceterm-core-<version>-<platform>.tar.gz` for Release, the release asset name."""
    return f"{identity}-core-{version}-{PLATFORMS[target]}.tar.gz"


def host_target():
    target = TARGETS.get(sys.platform)
    if (sys.platform, platform.machine()) not in (("darwin", "arm64"), ("linux", "x86_64")):
        raise TaskError("server packages support Apple Silicon macOS and x86_64 glibc Linux")
    return target


def valid_protocols(value):
    """Whether `value` is a build document's `protocols` object."""

    def version(item):
        return (
            isinstance(item, dict)
            and set(item) == {"major", "minor"}
            and type(item["major"]) is int
            and type(item["minor"]) is int
            and 0 < item["major"] <= 65535
            and 0 <= item["minor"] <= 65535
        )

    return (
        isinstance(value, dict)
        and set(value) == set(PROTOCOL_NAMES)
        and all(version(item) for item in value.values())
    )


def document(data):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate key")
            result[key] = value
        return result

    try:
        value = json.loads(data, object_pairs_hook=unique)
        if (
            not isinstance(value, dict)
            or set(value) != BUILD_KEYS
            or type(value["schema"]) is not int
            or value["schema"] != 1
            or type(value["stapled"]) is not bool
            or (
                value["stapled"]
                and (value["identity"] != "spaceterm" or value["target"] != TARGETS["darwin"])
            )
            or not valid_protocols(value["protocols"])
            or (
                value["source_sha256"] is not None
                and not re.fullmatch(r"[0-9a-f]{64}", value["source_sha256"])
            )
            or value["identity"] not in IDENTITIES
            or value["target"] not in TARGETS.values()
            or not re.fullmatch(r"[0-9a-f]{40}", value["commit"])
            or len(value["version"]) > 64
            or not re.fullmatch(
                r"(?:dev\.[0-9a-f]{12}(?:\.dirty)?|(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*))",
                value["version"],
            )
        ):
            raise ValueError("invalid build")
    except (ValueError, TypeError, UnicodeError):
        raise TaskError("the server build document is malformed or unsupported") from None
    if value["version"].startswith("dev.") and (
        value["version"].removesuffix(".dirty") != "dev." + value["commit"][:12]
        or (value["identity"] == "spaceterm" and value["version"].endswith(".dirty"))
    ):
        raise TaskError("the server version differs from its source commit")
    if not valid_floor(value["floor"], value["version"]):
        raise TaskError("the server build names an Update Floor its version cannot have")
    return value


def valid_floor(floor, version):
    """A release build's floor is a release version at or below its own. A build that names a
    commit has no order, so its floor is null."""
    release = RELEASE_VERSION.fullmatch(version)
    if release is None:
        return floor is None
    named = RELEASE_VERSION.fullmatch(floor) if isinstance(floor, str) else None
    return named is not None and tuple(map(int, named.groups())) <= tuple(
        map(int, release.groups())
    )


def build_info(executable):
    try:
        result = subprocess.run(
            [executable, "internal", "build-info"],
            env={},
            capture_output=True,
            timeout=10,
        )
    except (OSError, subprocess.TimeoutExpired):
        raise TaskError("the server build identity query failed") from None
    if result.returncode or result.stderr or len(result.stdout) > 4096:
        raise TaskError("the server build identity query failed")
    return document(result.stdout)


# Every executable a supported layout holds. The tree check then requires the declared target's.
EXECUTABLES = {
    f"core/{executable_path(target, identity)}"
    for target in TARGETS.values()
    for identity in IDENTITIES
}


def unpack_verified(
    archive, signature, destination, expected, *, identity=None, target=None, expected_sha256=None
):
    """Authenticate exact archive bytes before parsing or extracting any member.

    Callers use a private temporary copy, preventing replacement between signature
    verification, extraction, executable checks, and installation.
    """
    if archive.stat().st_size > MAX_ARCHIVE or signature.stat().st_size != 64:
        raise TaskError("the signed server artifact has an invalid size")
    trust_identity = identity or (expected["identity"] if expected is not None else None)
    try:
        update_signing.verify(artifact_trust(trust_identity), archive, signature)
    except update_signing.SignatureMismatch:
        raise ArtifactVerificationError(ArtifactCause.SIGNATURE_MISMATCH) from None
    if expected_sha256 is not None and update_signing.sha256(archive) != expected_sha256:
        raise ArtifactVerificationError(ArtifactCause.DIGEST_MISMATCH) from None
    try:
        from spaceterm_tasks.bounded_tar import open_archive

        with open_archive(archive, maximum=MAX_EXPANDED, members=MAX_MEMBERS) as source:
            members = []
            names = set()
            expanded = 0
            for member in source:
                path = PurePosixPath(member.name)
                expanded += member.size
                if (
                    len(members) >= MAX_MEMBERS
                    or expanded > MAX_EXPANDED
                    or member.size < 0
                    or member.name != str(path)
                    or path.is_absolute()
                    or ".." in path.parts
                    or not path.parts
                    or path.parts[0] != "core"
                    or member.name in names
                    or not (member.isfile() or member.isdir())
                    or member.mode
                    != (0o755 if member.isdir() or member.name in EXECUTABLES else 0o644)
                ):
                    raise TaskError("the server archive layout is invalid")
                names.add(member.name)
                members.append(member)
            manifest = source.getmember("core/build.json")
            if not manifest.isfile() or manifest.size > 4096:
                raise TaskError("the server build document is invalid")
            stream = source.extractfile(manifest)
            if stream is None:
                raise TaskError("the server build document is missing")
            info = document(stream.read())
            if expected is not None and info != expected:
                raise TaskError("the server artifact differs from the requested exact build")
            if identity is not None:
                compatible_candidate(info, identity, target=target)
            # A failure above cannot create any destination entry.
            source.extractall(destination, members=members, filter="data")
            # Archives may omit directory entries. Tarfile creates those parents using the
            # account's umask, which must not grant group write access to a retained runtime.
            tree = destination / "core"
            for directory in [tree, *(path for path in tree.rglob("*") if path.is_dir())]:
                directory.chmod(0o755)
    except (OSError, EOFError, OverflowError, ValueError, tarfile.TarError, KeyError):
        raise TaskError("the server archive could not be read") from None
    return info


def signer(identity):
    """The codesign identity and Team ID: ad hoc and none outside the release identity."""
    if identity != "spaceterm":
        return macos_codesign.ADHOC, None
    return macos_codesign.signing_identity(os.environ), macos_codesign.team_id(os.environ)


def compile_terminfo(destination, target):
    """Compile target-compatible data with tic, including its extended capabilities."""
    with tempfile.TemporaryDirectory(prefix="spaceterm-core-terminfo-") as temporary:
        source = TERMINFO
        if target == TARGETS["darwin"] and sys.platform != "darwin":
            # The inherited Linux entry advertises 65536 pairs, requiring ncurses 6's
            # 32-bit format. Advertise the legacy capacity that macOS can represent.
            source = Path(temporary) / "server.terminfo"
            source.write_text(TERMINFO.read_text().replace("\n", "\n\tpairs#32767,\n", 1))
        checked(
            ["tic", "-x", "-o", str(destination), str(source)],
            "server terminfo compilation failed",
        )
    if target == TARGETS["darwin"]:
        hexadecimal = destination / "78/xterm-spaceterm"
        alphabetic = destination / "x/xterm-spaceterm"
        if alphabetic.exists():
            hexadecimal.parent.mkdir(exist_ok=True)
            alphabetic.replace(hexadecimal)
            alphabetic.parent.rmdir()
        verify_foreign_terminfo(hexadecimal)
        if hexadecimal.read_bytes()[:2] != b"\x1a\x01":
            raise TaskError("the macOS server terminfo requires the legacy 16-bit format")
    elif not (destination / "x/xterm-spaceterm").exists():
        # Linux ncurses uses the first character, while macOS tic emits its hex value.
        alphabetic = destination / "x/xterm-spaceterm"
        alphabetic.parent.mkdir(exist_ok=True)
        shutil.copyfile(destination / "78/xterm-spaceterm", alphabetic)


def verify_foreign_terminfo(path):
    """Check portable table bounds; the native producer owns full terminfo decoding."""
    # ncurses term(5): 0x011a has 16-bit numbers, 0x021e has 32-bit numbers.
    # macOS's older infocmp cannot decode the latter Linux-produced format.
    with path.open("rb") as source:
        header = source.read(12)
        if len(header) != 12:
            raise TaskError("the foreign server terminfo header is truncated")
        magic, names, booleans, numbers, strings, table = struct.unpack("<6h", header)
        if (
            magic not in (0x011A, 0x021E)
            or names <= 0
            or min(booleans, numbers, strings, table) < 0
        ):
            raise TaskError("the foreign server terminfo header is invalid")
        aliases = source.read(names)
        if not aliases.endswith(b"\0") or b"xterm-spaceterm" not in aliases[:-1].split(b"|"):
            raise TaskError("the foreign server terminfo identity is invalid")
        end = 12 + names + booleans
        end += end % 2
        end += numbers * (2 if magic == 0x011A else 4) + strings * 2 + table
        if path.stat().st_size < end:
            raise TaskError("the foreign server terminfo tables are truncated")


def verify_cross_build(tree, expected):
    """Inspect a local cross-build; native execution checks belong to its target host."""
    executable = tree / executable_path(expected["target"], expected["identity"])
    library = checked(
        ["rustc", "--print", "target-libdir"], "Rust host tools are unavailable", text=True
    ).strip()
    objdump = Path(library).parent / "bin/llvm-objdump"
    if expected["target"] != TARGETS["linux"]:
        raise TaskError("only the Linux Core cross-builds; macOS builds natively")
    header = checked([str(objdump), "-f", str(executable)], "invalid server architecture")
    if b"elf64-x86-64" not in header:
        raise TaskError("the server must contain only x86_64 Linux code")
    dependencies = checked(
        [str(objdump), "-p", str(executable)], "server dependencies unavailable"
    ).lower()
    if any(
        name in dependencies
        for name in (
            b"libx11",
            b"libxcb",
            b"libwayland",
            b"libvulkan",
            b"libegl",
            b"libgl.",
            b"libgtk",
        )
    ):
        raise TaskError("the server links a graphical or GPU dependency")
    symbols = checked(
        [str(objdump), "-T", str(executable)], "server executable symbols could not be read"
    )
    required = max(
        ((int(major), int(minor)) for major, minor in re.findall(rb"GLIBC_(\d+)\.(\d+)", symbols)),
        default=(0, 0),
    )
    if required > (2, 35):
        raise TaskError("the server executable needs glibc newer than 2.35")


def verify_tree(tree, expected, *, native=True, execute=True):
    target, identity = expected["target"], expected["identity"]
    files = {str(path.relative_to(tree)) for path in tree.rglob("*") if path.is_file()}
    shell = {
        f"{SHARED_RESOURCES}/shell-integration/" + str(path.relative_to(SHELL_INTEGRATION))
        for path in SHELL_INTEGRATION.rglob("*")
        if path.is_file()
    }
    listed = set(json.loads((ROOT / "packaging/core-resources.json").read_text()))
    if listed != shell | {f"{SHARED_RESOURCES}/THIRD-PARTY-NOTICES.txt"}:
        raise TaskError("the server resource policy differs from the tracked resources")
    directory = resource_directory(target, identity)
    resources = {directory + name.removeprefix(SHARED_RESOURCES) for name in listed}
    terminfo = {
        name
        for name in files
        if re.fullmatch(re.escape(directory) + r"/terminfo/(?:78|x)/xterm-spaceterm", name)
    }
    if not terminfo or files != resources | terminfo | bundle_files(
        target, identity, expected["stapled"]
    ) | {
        executable_path(target, identity),
        "build.json",
    }:
        raise TaskError("the server resources do not match the supported layout")
    directories = {str(path.relative_to(tree)) for path in tree.rglob("*") if path.is_dir()}
    required_directories = {
        str(parent)
        for name in files
        for parent in PurePosixPath(name).parents
        if str(parent) != "."
    }
    if directories != required_directories:
        raise TaskError("the server directories do not match the supported layout")
    executable = tree / executable_path(target, identity)
    if not native:
        # Catalogs authenticate foreign bytes without executing them. Local cross-builds
        # receive the additional static producer checks in verify_cross_build.
        with executable.open("rb") as source:
            header = source.read(64)
        if expected["target"] == TARGETS["darwin"]:
            valid = (
                header[:8] == bytes.fromhex("cffaedfe0c000001") and header[12:16] == b"\x02\0\0\0"
            )
        else:
            valid = (
                header[:7] == b"\x7fELF\x02\x01\x01"
                and header[16:18] in (b"\x02\0", b"\x03\0")
                and header[18:20] == b"\x3e\0"
            )
        if not valid:
            raise TaskError("the signed server executable differs from its declared native target")
        for name in terminfo:
            verify_foreign_terminfo(tree / name)
        return
    if expected["target"] != host_target():
        raise TaskError("the server target cannot run on this host")
    if sys.platform == "darwin":
        if checked(
            ["lipo", "-archs", str(executable)], "invalid server architecture", text=True
        ).split() != ["arm64"]:
            raise TaskError("the server must contain only Apple Silicon code")
        build = checked(
            ["xcrun", "vtool", "-show-build", str(executable)],
            "invalid server deployment target",
            text=True,
        )
        if "minos 26.0" not in " ".join(build.split()):
            raise TaskError("the server requires the macOS 26 deployment target")
        identifier = core_identifier(identity)
        bundle = tree / signed_path(target, identity)
        info = plistlib.loads((bundle / "Contents/Info.plist").read_bytes())
        if info != core_info(identity):
            raise TaskError("the server Info.plist differs from its identity's")
        codesign_identity, team = signer(identity)
        macos_codesign.verify(
            bundle,
            identifier,
            plistlib.loads(macos_codesign.CORE_ENTITLEMENTS.read_bytes()),
            team=team,
            identity=codesign_identity,
            label="server",
        )
        if identity == "spaceterm":
            macos_notary.assess(bundle)
        dependencies = checked(
            ["otool", "-L", str(executable)], "server dependencies unavailable", text=True
        ).lower()
        forbidden = ("appkit", "metal", "sparkle", "quartz", "opengl")
    else:
        header = checked(["objdump", "-f", str(executable)], "invalid server architecture")
        if b"elf64-x86-64" not in header:
            raise TaskError("the server must contain only x86_64 Linux code")
        verify_glibc_baseline(executable, "server")
        dependencies = checked(
            ["objdump", "-p", str(executable)], "server dependencies unavailable", text=True
        ).lower()
        forbidden = ("libx11", "libxcb", "libwayland", "libvulkan", "libegl", "libgl.", "libgtk")
    if any(name in dependencies for name in forbidden):
        raise TaskError("the server links a graphical or GPU dependency")
    # Signed resources belong to this server artifact, independently of client source assets.
    checked(
        ["infocmp", "-x", "-1", "xterm-spaceterm"],
        "the server terminfo resource is invalid",
        env={**os.environ, "TERMINFO": str(tree / directory / "terminfo")},
    )
    if not execute:
        return
    # Inspect native metadata and signatures before executing the candidate.
    # Stapling happens after compilation. Every compiled fact must still match exactly.
    if build_info(executable) != {**expected, "stapled": False}:
        raise TaskError("the executable differs from the authenticated server build")
    try:
        result = subprocess.run(
            [executable, "internal", "check"],
            env={},
            cwd=tree,
            capture_output=True,
            timeout=15,
        )
    except (OSError, subprocess.TimeoutExpired):
        raise TaskError("the installed server headless check failed") from None
    if result.returncode or result.stderr or result.stdout != b"SpaceTerm Core checks passed.\n":
        raise TaskError("the installed server headless check failed")


def verified_copy(
    archive,
    signature,
    directory,
    expected,
    *,
    identity=None,
    target=None,
    expected_sha256=None,
    native=True,
    execute=True,
):
    archive = Path(archive)
    signature = Path(signature)
    if archive.stat().st_size > MAX_ARCHIVE or signature.stat().st_size != 64:
        raise TaskError("the signed server artifact has an invalid size")
    private = directory / "artifact.tar.gz"
    raw = directory / "artifact.sig"
    shutil.copyfile(archive, private)
    shutil.copyfile(signature, raw)
    info = unpack_verified(
        private,
        raw,
        directory / "payload",
        expected,
        identity=identity,
        target=target,
        expected_sha256=expected_sha256,
    )
    tree = directory / "payload/core"
    verify_tree(tree, info, native=native, execute=execute)
    return tree, private, raw


def verify(archive, signature, expected, *, native=True, execute=True):
    with tempfile.TemporaryDirectory(prefix="spaceterm-core-verify-") as temporary:
        verified_copy(archive, signature, Path(temporary), expected, native=native, execute=execute)
    print(json.dumps(expected, sort_keys=True))


def install(archive, signature, expected):
    """Retain a verified immutable runtime under the account identity, without activation."""
    import pwd

    expected = document(json.dumps(expected))
    home = Path(pwd.getpwuid(os.geteuid()).pw_dir)
    parent = home / ".local/share" / expected["identity"] / "core/installations"
    # Existing inaccessible state is a failure, never permission to recreate identity.
    current = Path(home.anchor)
    for part in parent.parts[1:]:
        current /= part
        try:
            current.mkdir(mode=0o700)
        except FileExistsError:
            pass
        entry = current.lstat()
        if not stat.S_ISDIR(entry.st_mode) or (
            current != home and home in current.parents and entry.st_uid != os.geteuid()
        ):
            raise TaskError("the server installation parent is unsafe")
    descriptor = os.open(parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        fcntl.flock(descriptor, fcntl.LOCK_EX)
        with tempfile.TemporaryDirectory(dir=parent, prefix=".prepare-") as temporary:
            directory = Path(temporary)
            tree, private, raw = verified_copy(archive, signature, directory, expected)
            digest = update_signing.sha256(private)
            destination = parent / digest
            if destination.exists() or destination.is_symlink():
                raise TaskError("this server installation already exists; it was left unchanged")
            shutil.copyfile(private, tree / "artifact.tar.gz")
            shutil.copyfile(raw, tree / "artifact.sig")
            (tree / "artifact.tar.gz").chmod(0o644)
            (tree / "artifact.sig").chmod(0o644)
            # Only a fully verified new directory is published. Existing installations
            # and active-version selection are never replaced or cleaned up here.
            for path in tree.rglob("*"):
                if path.is_file():
                    with path.open("rb") as source:
                        os.fsync(source.fileno())
            for path in sorted(
                [tree, *(p for p in tree.rglob("*") if p.is_dir())],
                key=lambda p: len(p.parts),
                reverse=True,
            ):
                nested = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
                try:
                    os.fsync(nested)
                finally:
                    os.close(nested)
            os.rename(tree, destination)
            os.fsync(descriptor)
    finally:
        os.close(descriptor)
    print(f"Retained verified {expected['identity']} server installation {digest}; no activation")
    return destination


def compile_unsigned(identity, release_tag=None, *, target=None, destination=None):
    native_target = host_target()
    target = native_target if target is None else target
    if target not in TARGETS.values():
        raise TaskError("the server package target is unsupported")
    native = target == native_target
    if target == TARGETS["darwin"] and not native:
        raise TaskError("macOS Core packages require a native macOS build")
    # A dry run on a Mac cross-builds the Linux Core it embeds. A release builds natively.
    if release_tag and not native and not update_signing.dry_run():
        raise TaskError("release Core packages require their native target host")
    if identity not in IDENTITIES or (identity == "spaceterm") != bool(release_tag):
        raise TaskError("only a validated release tag selects the release server identity")
    if release_tag:
        require_clean_checkout()
    environment = macos_signing.signing_environment(packaged_environment(release_tag))
    floor = core_floor(release_tag)
    if floor:
        environment[RELEASE_FLOOR] = floor
    if identity == "spaceterm-development":
        environment.pop("SPACETERM_PACKAGED", None)
    if target == TARGETS["darwin"]:
        environment["MACOSX_DEPLOYMENT_TARGET"] = "26.0"
    from spaceterm_tasks.core_builds import _source

    source = _source(ROOT)[2]
    environment["SPACETERM_CLI_SOURCE_STATE"] = source
    executable, metadata = build_executable(
        "--release",
        "--locked",
        "--package",
        "spaceterm-cli",
        "--no-default-features",
        binary="spaceterm",
        env=environment,
        target=(
            "x86_64-unknown-linux-gnu.2.35"
            if target == TARGETS["linux"]
            else target
            if not native
            else None
        ),
        metadata="build-info.json",
    )
    expected = document(metadata)
    commit = checked(
        ["git", "rev-parse", "HEAD"], "candidate commit unavailable", cwd=ROOT, text=True
    ).strip()
    if (
        expected["identity"] != identity
        or expected["commit"] != commit
        or expected["target"] != target
        or expected["source_sha256"] != source
        or expected["floor"] != floor
        or expected["stapled"]
        or _source(ROOT)[2] != source
    ):
        raise TaskError("Cargo returned a different server build")
    if native and build_info(executable) != expected:
        raise TaskError("the executable differs from Cargo's server build metadata")
    DIST.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(dir=DIST, prefix=".unsigned-core-") as temporary:
        directory = Path(temporary)
        tree = directory / "core"
        core = tree / executable_path(target, identity)
        core.parent.mkdir(parents=True)
        shutil.copyfile(executable, core)
        core.chmod(0o755)
        resources = tree / resource_directory(target, identity)
        shutil.copytree(SHELL_INTEGRATION, resources / "shell-integration")
        shutil.copyfile(NOTICES, resources / NOTICES.name)
        compile_terminfo(resources / "terminfo", target)
        if target == TARGETS["darwin"]:
            identifier = core_identifier(identity)
            bundle = tree / signed_path(target, identity)
            (bundle / "Contents/Info.plist").write_bytes(plistlib.dumps(core_info(identity)))
            (bundle / "Contents/Info.plist").chmod(0o644)
            # System Settings shows this icon beside Core's permissions and background item.
            compile_icon(resources, identifier)
            (resources / "IconPartialInfo.plist").unlink()
        (tree / "build.json").write_text(json.dumps(expected, sort_keys=True) + "\n")
        destination = Path(destination) if destination else DIST / "unsigned-core" / target
        shutil.rmtree(destination, ignore_errors=True)
        destination.parent.mkdir(parents=True, exist_ok=True)
        tree.rename(destination)
    return destination


def sign_unsigned(tree, *, identity, release_tag=None, target=None):
    """Sign already compiled Core inputs without invoking Cargo or regenerating resources."""
    from spaceterm_tasks.core_builds import _source

    tree = Path(tree)
    target = target or host_target()
    expected = document((tree / "build.json").read_bytes())
    source = _source(ROOT)[2]
    if (
        expected["identity"] != identity
        or expected["target"] != target
        or expected["commit"]
        != checked(["git", "rev-parse", "HEAD"], "Git unavailable", cwd=ROOT, text=True).strip()
        or expected["source_sha256"] != source
        or expected["floor"] != core_floor(release_tag)
        or (release_tag and expected["version"] != release_version(release_tag))
    ):
        raise TaskError("the unsigned Core inputs differ from the release source")
    native = target == host_target()
    environment = macos_signing.signing_environment(packaged_environment(release_tag))
    seed = artifact_signing_seed(identity)
    # Work on a private copy so reruns never mutate retained unsigned inputs.
    with tempfile.TemporaryDirectory(dir=DIST, prefix=".server-package-") as temporary:
        directory = Path(temporary)
        shutil.copytree(tree, directory / "core")
        tree = directory / "core"
        if target == TARGETS["darwin"]:
            core = tree / signed_path(target, identity)
            identifier = core_identifier(identity)
            # A macOS Core always builds natively; package() refuses a cross-build.
            codesign_identity, team = signer(identity)
            signing = (
                macos_signing.release_keychain(codesign_identity, os.environ)
                if codesign_identity != macos_codesign.ADHOC
                else nullcontext()
            )
            with signing:
                macos_codesign.sign_core(
                    core, codesign_identity, identifier, team=team, environment=environment
                )
            if release_tag:
                with macos_notary.notary() as notary:
                    from spaceterm_tasks import release_notary

                    release_notary.notarize(core, notary)
            expected["stapled"] = (core / "Contents/CodeResources").is_file()
        # build.json is outside the native bundle seal, and inside the signed archive.
        (tree / "build.json").write_text(json.dumps(expected, sort_keys=True) + "\n")

        verify_tree(tree, expected, native=native)
        if not native:
            verify_cross_build(tree, expected)
        name = archive_name(identity, expected["version"], target)
        archive = directory / name
        signature = directory / f"{name}.sig"
        write_archive(tree, archive)
        update_signing.sign(seed, archive, signature)
        verify(archive, signature, expected, native=native)
        if _source(ROOT)[2] != source:
            raise TaskError("source changed during server packaging; retry the command")
        archive.replace(DIST / name)
        signature.replace(DIST / signature.name)
    print(f"Created signed server candidate {name}; nothing published")
    return DIST / name, DIST / f"{name}.sig"


def package(identity, release_tag=None, *, target=None):
    """Compile and sign a Core package for local packaging tasks."""
    target = target or host_target()
    tree = compile_unsigned(identity, release_tag, target=target)
    return sign_unsigned(tree, identity=identity, release_tag=release_tag, target=target)


def core_floor(release_tag):
    """The Update Floor a release Core build carries; None for a build that names a commit."""
    if not release_tag:
        return None
    from spaceterm_tasks import release_floor

    return release_floor.floor("core", release_version(release_tag))


def stage_desktop_artifact(archive, signature, destination, expected=None, *, identity=None):
    """Embed the verified independent archive and raw signature, never a derived executable."""
    expected = document(json.dumps(expected)) if expected is not None else None
    # Verification and copying use the same private bytes, even if the input path changes.
    with tempfile.TemporaryDirectory(prefix="spaceterm-desktop-artifact-") as temporary:
        tree, private, raw = verified_copy(
            archive, signature, Path(temporary), expected, identity=identity
        )
        info = document((tree / "build.json").read_bytes())
        compatible_candidate(info, identity or info["identity"])
        destination.mkdir(parents=True)
        shutil.copyfile(private, destination / "artifact.tar.gz")
        shutil.copyfile(raw, destination / "artifact.sig")
        for path in destination.iterdir():
            path.chmod(0o644)
    return {
        "SPACETERM_CLI_ARCHIVE": str((destination / "artifact.tar.gz").resolve()),
        "SPACETERM_CLI_SIGNATURE": str((destination / "artifact.sig").resolve()),
    }


def compatible_candidate(info, identity, *, target=None):
    if info["identity"] != identity or info["target"] != (target or host_target()):
        raise TaskError("the server candidate has a different identity or target")
    if info["protocols"]["work"]["major"] != PROTOCOL_MAJOR:
        raise TaskError("the server candidate has an incompatible protocol major")


def prepare_desktop_artifact(destination, identity, release_tag=None, *, catalog_destination=None):
    """Signing happens during candidate preparation; launch only consumes the resulting pair.

    An explicit pair reuses its signed bytes across client releases. A release embeds the two
    Core builds in SPACETERM_CLI_ARTIFACTS. Without a pair, a nonproduction build packages its
    own Core.
    """
    archive = os.environ.get("SPACETERM_CLI_ARCHIVE")
    signature = os.environ.get("SPACETERM_CLI_SIGNATURE")
    if (archive is not None or signature is not None) and (not archive or not signature):
        raise TaskError("supply both SPACETERM_CLI_ARCHIVE and SPACETERM_CLI_SIGNATURE")
    source = os.environ.get("SPACETERM_CLI_ARTIFACTS")
    if release_tag:
        require_clean_checkout()
        # A release embeds the Core builds release:core signed, never a build of its own.
        if not source:
            raise TaskError(
                "release desktop packaging needs both Core builds in SPACETERM_CLI_ARTIFACTS"
            )
        if not archive:
            local = Path(source) / host_target()
            archive, signature = str(local / "artifact.tar.gz"), str(local / "artifact.sig")
    if source is not None and not release_tag and host_target() == TARGETS["linux"]:
        from spaceterm_tasks.core_builds import _source
        from spaceterm_tasks.core_catalog import collect_target

        digest = _source(ROOT)[2]
        with tempfile.TemporaryDirectory(prefix="spaceterm-native-catalog-") as temporary:
            collect_target(source, Path(temporary) / "mac", identity, TARGETS["darwin"], digest)
        if _source(ROOT)[2] != digest:
            raise TaskError("source changed while reading the Mac-built server catalog")
    automatic = catalog_destination is not None and not release_tag and source is None
    if automatic:
        from spaceterm_tasks.core_builds import prepare

        prepare(catalog_destination, identity)
        if not archive:
            local = Path(catalog_destination) / host_target()
            archive, signature = local / "artifact.tar.gz", local / "artifact.sig"
    if not archive:
        archive, signature = package(identity)
    environment = stage_desktop_artifact(archive, signature, destination, identity=identity)
    if catalog_destination is not None and not automatic:
        from spaceterm_tasks.core_catalog import stage_catalog

        stage_catalog(
            environment["SPACETERM_CLI_ARCHIVE"],
            environment["SPACETERM_CLI_SIGNATURE"],
            catalog_destination,
            identity,
            source=source,
            require_all=bool(release_tag),
        )
    return environment


def verify_desktop_artifact(
    destination, identity, release_tag=None, *, executable=None, execute=True
):
    """Verify signed candidate compatibility and the final client's compiled whole-archive pin."""
    try:
        entries = {path.name for path in destination.iterdir()}
        required = {"artifact.tar.gz", "artifact.sig"}
        if not required <= entries:
            raise ArtifactVerificationError(ArtifactCause.MISSING) from None
        if entries != required:
            raise TaskError("the desktop server artifact layout is invalid")
        pin = None
        if executable is not None and execute:
            result = subprocess.run(
                [executable, "--build-info"], env={}, capture_output=True, timeout=10
            )
            try:
                client = json.loads(result.stdout)
                valid = (
                    not result.returncode
                    and not result.stderr
                    and len(result.stdout) <= 4096
                    and client["schema"] == 1
                    and client["identity"] == identity
                    and client["target"] == host_target()
                    and re.fullmatch(r"[0-9a-f]{64}", client["core_candidate_sha256"])
                )
                pin = client["core_candidate_sha256"] if valid else None
            except (ValueError, KeyError, TypeError):
                valid = False
            if not valid:
                raise TaskError("the client's compiled server candidate pin is unavailable")
        with tempfile.TemporaryDirectory(prefix="spaceterm-desktop-verify-") as temporary:
            verified_copy(
                destination / "artifact.tar.gz",
                destination / "artifact.sig",
                Path(temporary),
                None,
                identity=identity,
                expected_sha256=pin,
                execute=execute,
            )
    except FileNotFoundError:
        raise ArtifactVerificationError(ArtifactCause.MISSING) from None
    except PermissionError:
        raise ArtifactVerificationError(ArtifactCause.UNREADABLE) from None
    except subprocess.TimeoutExpired:
        raise ArtifactVerificationError(ArtifactCause.TIMED_OUT) from None
    except OSError:
        raise ArtifactVerificationError(ArtifactCause.OS_ERROR) from None
