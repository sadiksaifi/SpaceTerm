"""Independent server artifacts authenticated by identity-scoped Ed25519 trust.

Supported layouts are server/bin/spaceterm and server/share/spaceterm on
Apple Silicon macOS 26 and x86_64 glibc Linux (Ubuntu 22.04 or newer). Installation
only retains an exact verified artifact. It never selects an active version or
writes server identity, organization, credentials, stopped intent, or services.
"""

import base64
import fcntl
import json
import os
import platform
import re
import shutil
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import tomllib
from pathlib import Path, PurePosixPath

from spaceterm_tasks import ROOT, TaskError, macos_signing, release
from spaceterm_tasks.cargo import build_executable
from spaceterm_tasks.package_linux import verify_glibc_baseline, write_archive
from spaceterm_tasks.packaging import (
    DIST,
    NOTICES,
    SHELL_INTEGRATION,
    TERMINFO,
    checked,
    packaged_environment,
    require_clean_checkout,
)

TARGETS = {"darwin": "aarch64-apple-darwin", "linux": "x86_64-unknown-linux-gnu"}
IDENTITIES = ("spaceterm", "spaceterm-preflight", "spaceterm-development")
MAX_ARCHIVE = 512 * 1024 * 1024
MAX_EXPANDED = 512 * 1024 * 1024
MAX_MEMBERS = 128
# The SpaceTerm Protocol major that `crates/spaceterm-cli/build.rs` stamps into server builds.
PROTOCOL_MAJOR = 4
BUILD_KEYS = {
    "schema",
    "identity",
    "target",
    "commit",
    "version",
    "protocol_major",
    "capabilities",
    "source_sha256",
}


def artifact_public_key(identity):
    if identity == "spaceterm":
        return release.public_key()
    if identity in ("spaceterm-preflight", "spaceterm-development"):
        return json.loads((ROOT / "packaging/server-nonproduction-trust.json").read_text())[
            "public_key"
        ]
    raise TaskError("the server artifact application identity is unsupported")


def artifact_signing_seed(identity):
    if identity == "spaceterm":
        secret = os.environ.get(release.SIGNING_SECRET)
        if not secret:
            raise TaskError("the existing update signing key is required")
        seed = release.signing_seed(secret)
    elif identity in ("spaceterm-preflight", "spaceterm-development"):
        # This tracked seed is intentionally public. It grants no Release authority.
        seed = base64.b64decode(
            json.loads((ROOT / "packaging/server-nonproduction-trust.json").read_text())["seed"],
            validate=True,
        )
    else:
        raise TaskError("the server artifact application identity is unsupported")
    if release.derived_public_key(seed) != artifact_public_key(identity):
        raise TaskError("the signing key differs from the server artifact identity's trust")
    return seed


def host_target():
    target = TARGETS.get(sys.platform)
    if (sys.platform, platform.machine()) not in (("darwin", "arm64"), ("linux", "x86_64")):
        raise TaskError("server packages support Apple Silicon macOS and x86_64 glibc Linux")
    return target


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
            or type(value["protocol_major"]) is not int
            or not 0 < value["protocol_major"] <= 65535
            or not isinstance(value["capabilities"], list)
            or len(value["capabilities"]) > 32
            or any(
                not isinstance(capability, str) or not re.fullmatch(r"[a-z0-9_]{1,64}", capability)
                for capability in value["capabilities"]
            )
            or len(set(value["capabilities"])) != len(value["capabilities"])
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
    return value


def build_info(executable):
    try:
        result = subprocess.run(
            [executable, "server", "provision", "build-info"],
            env={},
            capture_output=True,
            timeout=10,
        )
    except (OSError, subprocess.TimeoutExpired):
        raise TaskError("the server build identity query failed") from None
    if result.returncode or result.stderr or len(result.stdout) > 4096:
        raise TaskError("the server build identity query failed")
    return document(result.stdout)


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
    release.verify_ed25519(artifact_public_key(trust_identity), archive, signature)
    if expected_sha256 is not None and release.sha256(archive) != expected_sha256:
        raise TaskError(
            "the embedded server archive differs from the client's compiled candidate pin"
        )
    try:
        with tarfile.open(archive, "r:gz") as source:
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
                    or path.parts[0] != "server"
                    or member.name in names
                    or not (member.isfile() or member.isdir())
                    or member.mode
                    != (0o755 if member.isdir() or member.name == "server/bin/spaceterm" else 0o644)
                ):
                    raise TaskError("the server archive layout is invalid")
                names.add(member.name)
                members.append(member)
            manifest = source.getmember("server/build.json")
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
            tree = destination / "server"
            for directory in [tree, *(path for path in tree.rglob("*") if path.is_dir())]:
                directory.chmod(0o755)
    except (OSError, tarfile.TarError, KeyError):
        raise TaskError("the server archive could not be read") from None
    return info


def signer():
    config = tomllib.loads((ROOT / "packaging/macos/spaceterm/Packager.toml").read_text())
    return config["macos"]["signing-identity"]


def compile_terminfo(destination, target):
    """Compile target-compatible data with tic, including its extended capabilities."""
    with tempfile.TemporaryDirectory(prefix="spaceterm-server-terminfo-") as temporary:
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


def verify_macho_output(output):
    """Require the pinned LLVM tool's complete Mach-O header and load-command output."""
    if len(output) > 4 * 1024 * 1024:
        raise TaskError("the macOS server inspection output is too large")
    lines = output.splitlines()
    rows = [line.split() for line in lines if line.startswith("MH_MAGIC")]
    if len(rows) != 1 or rows[0][:5] != ["MH_MAGIC_64", "ARM64", "ALL", "0x00", "EXECUTE"]:
        raise TaskError("the server must contain only Apple Silicon executable code")
    try:
        count = int(rows[0][5])
    except (IndexError, ValueError):
        raise TaskError("the macOS server load command count is invalid") from None
    blocks = re.split(r"(?m)^Load command ([0-9]+)\n", output)
    if not 0 < count <= 4096 or blocks[1::2] != [str(index) for index in range(count)]:
        raise TaskError("the macOS server load commands are incomplete")
    versions = []
    dependencies = []
    for block in blocks[2::2]:
        commands = re.findall(r"(?m)^\s+cmd (LC_[A-Z0-9_]+)$", block)
        if len(commands) != 1:
            raise TaskError("the macOS server load command is invalid")
        command = commands[0]
        if command == "LC_BUILD_VERSION":
            versions.append(
                (
                    re.findall(r"(?m)^\s+platform (\S+)$", block),
                    re.findall(r"(?m)^\s+minos (\S+)$", block),
                )
            )
        elif command in (
            "LC_LOAD_DYLIB",
            "LC_LOAD_WEAK_DYLIB",
            "LC_REEXPORT_DYLIB",
            "LC_LAZY_LOAD_DYLIB",
            "LC_LOAD_UPWARD_DYLIB",
        ):
            names = re.findall(r"(?m)^\s+name (.+) \(offset [0-9]+\)$", block)
            if len(names) != 1 or not names[0].startswith(("/usr/lib/", "/System/Library/")):
                raise TaskError("the macOS server dependency is not a system library")
            dependencies.append(names[0].lower())
    if versions != [(["macos"], ["26.0"])]:
        raise TaskError("the server requires the macOS 26 deployment target")
    if not dependencies or any(
        forbidden in name
        for name in dependencies
        for forbidden in ("appkit", "metal", "sparkle", "quartz", "opengl")
    ):
        raise TaskError("the server links a graphical or GPU dependency")


def verify_macho_identity(output, identifier):
    """Require the exact identity and ad-hoc flags emitted by pinned rcodesign."""
    if (
        len(output) > 4 * 1024 * 1024
        or not output.startswith("CodeDirectoryBlob {\n")
        or not output.endswith("}\n")
        or re.findall(r'(?m)^    ident: "([^"\n]*)",$', output) != [identifier]
        or output.count("    flags: CodeSignatureFlags(\n        ADHOC,\n    ),\n") != 1
    ):
        raise TaskError("the macOS server ad-hoc identity differs from its application identity")


def verify_cross_build(tree, expected):
    """Inspect a local cross-build; native execution checks belong to its target host."""
    executable = tree / "bin/spaceterm"
    library = checked(
        ["rustc", "--print", "target-libdir"], "Rust host tools are unavailable", text=True
    ).strip()
    objdump = Path(library).parent / "bin/llvm-objdump"
    if expected["target"] == TARGETS["darwin"]:
        headers = checked(
            [str(objdump), "--macho", "--private-headers", str(executable)],
            "the macOS server load commands are unavailable",
            text=True,
        )
        verify_macho_output(headers)
        # rcodesign 0.29.0 verify rejects valid ad-hoc signatures because they have no
        # CMS payload. Extract the producer's identity; native codesign owns verification.
        identity = checked(
            ["rcodesign", "-C", "/dev/null", "extract", "code-directory", str(executable)],
            "the macOS server code identity is unavailable",
            text=True,
        )
        verify_macho_identity(identity, f"io.github.sadiksaifi.{expected['identity']}.server")
    else:
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
            (
                (int(major), int(minor))
                for major, minor in re.findall(rb"GLIBC_(\d+)\.(\d+)", symbols)
            ),
            default=(0, 0),
        )
        if required > (2, 35):
            raise TaskError("the server executable needs glibc newer than 2.35")


def verify_tree(tree, expected, *, native=True):
    files = {str(path.relative_to(tree)) for path in tree.rglob("*") if path.is_file()}
    shell = {
        "share/spaceterm/shell-integration/" + str(path.relative_to(SHELL_INTEGRATION))
        for path in SHELL_INTEGRATION.rglob("*")
        if path.is_file()
    }
    terminfo = {
        name
        for name in files
        if re.fullmatch(r"share/spaceterm/terminfo/(?:78|x)/xterm-spaceterm", name)
    }
    resources = set(json.loads((ROOT / "packaging/server-resources.json").read_text()))
    if resources != shell | {"share/spaceterm/THIRD-PARTY-NOTICES.txt"}:
        raise TaskError("the server resource policy differs from the tracked resources")
    if not terminfo or files != resources | terminfo | {
        "bin/spaceterm",
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
    executable = tree / "bin/spaceterm"
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
        identifier = f"io.github.sadiksaifi.{expected['identity']}.server"
        if expected["identity"] == "spaceterm":
            macos_signing.verify_release_signature(executable, signer(), identifier, "server")
        else:
            macos_signing.verify_adhoc_signature(executable, identifier, "server")
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
    # Inspect native metadata and signatures before executing the candidate.
    if build_info(executable) != expected:
        raise TaskError("the executable differs from the authenticated server build")
    # Signed resources belong to this server artifact, independently of client source assets.
    checked(
        ["infocmp", "-x", "-1", "xterm-spaceterm"],
        "the server terminfo resource is invalid",
        env={**os.environ, "TERMINFO": str(tree / "share/spaceterm/terminfo")},
    )
    try:
        result = subprocess.run(
            [executable, "server", "provision", "check"],
            env={},
            cwd=tree,
            capture_output=True,
            timeout=15,
        )
    except (OSError, subprocess.TimeoutExpired):
        raise TaskError("the installed server headless check failed") from None
    if result.returncode or result.stderr or result.stdout != b"Server checks passed.\n":
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
    tree = directory / "payload/server"
    verify_tree(tree, info, native=native)
    return tree, private, raw


def verify(archive, signature, expected, *, native=True):
    with tempfile.TemporaryDirectory(prefix="spaceterm-server-verify-") as temporary:
        verified_copy(archive, signature, Path(temporary), expected, native=native)
    print(json.dumps(expected, sort_keys=True))


def install(archive, signature, expected):
    """Retain a verified immutable runtime under the account identity, without activation."""
    import pwd

    expected = document(json.dumps(expected))
    home = Path(pwd.getpwuid(os.geteuid()).pw_dir)
    parent = home / ".local/share" / expected["identity"] / "server/installations"
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
            digest = release.sha256(private)
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


def package(identity, release_tag=None, *, target=None):
    native_target = host_target()
    target = native_target if target is None else target
    if target not in TARGETS.values():
        raise TaskError("the server package target is unsupported")
    native = target == native_target
    if target == TARGETS["darwin"] and not native:
        raise TaskError("macOS CLI packages require a native macOS build for Keychain access")
    if release_tag and not native:
        raise TaskError("release server packages require their native target host")
    if identity not in IDENTITIES or (identity == "spaceterm") != bool(release_tag):
        raise TaskError("only a validated release tag selects the release server identity")
    if release_tag:
        require_clean_checkout()
    environment = macos_signing.signing_environment(packaged_environment(release_tag))
    if identity == "spaceterm-development":
        environment.pop("SPACETERM_PACKAGED", None)
    if target == TARGETS["darwin"]:
        environment["MACOSX_DEPLOYMENT_TARGET"] = "26.0"
        if release_tag:
            macos_signing.signing_credentials(signer(), os.environ)
    from spaceterm_tasks.server_builds import _source

    source = _source(ROOT)[2]
    environment["SPACETERM_SERVER_SOURCE_STATE"] = source
    seed = artifact_signing_seed(identity)
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
        or _source(ROOT)[2] != source
    ):
        raise TaskError("Cargo returned a different server build")
    if native and build_info(executable) != expected:
        raise TaskError("the executable differs from Cargo's server build metadata")
    DIST.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(dir=DIST, prefix=".server-package-") as temporary:
        directory = Path(temporary)
        tree = directory / "server"
        (tree / "bin").mkdir(parents=True)
        server = tree / "bin/spaceterm"
        shutil.copyfile(executable, server)
        server.chmod(0o755)
        resources = tree / "share/spaceterm"
        shutil.copytree(SHELL_INTEGRATION, resources / "shell-integration")
        shutil.copyfile(NOTICES, resources / NOTICES.name)
        compile_terminfo(resources / "terminfo", target)
        (tree / "build.json").write_text(json.dumps(expected, sort_keys=True) + "\n")
        if target == TARGETS["darwin"]:
            identifier = f"io.github.sadiksaifi.{identity}.server"
            if release_tag:
                requirement = f'=designated => identifier "{identifier}" and {macos_signing.certificate_requirement(signer())}'
                with macos_signing.release_keychain(signer(), os.environ):
                    checked(
                        [
                            "codesign",
                            "--force",
                            "--sign",
                            signer(),
                            "--identifier",
                            identifier,
                            "--requirements",
                            requirement,
                            "--timestamp=none",
                            str(server),
                        ],
                        "server code signing failed",
                        env=environment,
                    )
            elif native:
                checked(
                    [
                        "codesign",
                        "--force",
                        "--sign",
                        "-",
                        "--identifier",
                        identifier,
                        "--timestamp=none",
                        str(server),
                    ],
                    "server code signing failed",
                    env=environment,
                )
            else:
                signed = directory / "signed-server"
                checked(
                    [
                        "rcodesign",
                        "-C",
                        "/dev/null",
                        "sign",
                        "--binary-identifier",
                        identifier,
                        str(server),
                        str(signed),
                    ],
                    "server code signing failed",
                    env=environment,
                )
                signed.replace(server)
                server.chmod(0o755)
        verify_tree(tree, expected, native=native)
        if not native:
            verify_cross_build(tree, expected)
        platform = "darwin-arm64" if target == TARGETS["darwin"] else "linux-x86_64"
        name = f"{identity}-{expected['version']}-{platform}.tar.gz"
        archive = directory / name
        signature = directory / f"{name}.sig"
        write_archive(tree, archive)
        release.sign_ed25519(seed, archive, signature)
        verify(archive, signature, expected, native=native)
        if _source(ROOT)[2] != source:
            raise TaskError("source changed during server packaging; retry the command")
        archive.replace(DIST / name)
        signature.replace(DIST / signature.name)
    print(f"Created signed server candidate {name}; nothing published")
    return DIST / name, DIST / f"{name}.sig"


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
        "SPACETERM_SERVER_ARCHIVE": str((destination / "artifact.tar.gz").resolve()),
        "SPACETERM_SERVER_SIGNATURE": str((destination / "artifact.sig").resolve()),
    }


def compatible_candidate(info, identity, *, target=None):
    if info["identity"] != identity or info["target"] != (target or host_target()):
        raise TaskError("the server candidate has a different identity or target")
    if info["protocol_major"] != PROTOCOL_MAJOR:
        raise TaskError("the server candidate has an incompatible protocol major")
    if "workspace_policy" not in info["capabilities"]:
        raise TaskError("the server candidate is missing a required capability")


def prepare_desktop_artifact(destination, identity, release_tag=None, *, catalog_destination=None):
    """Signing happens during candidate preparation; launch only consumes the resulting pair.

    An explicit pair reuses its signed bytes across client releases. Without that pair,
    build the independent package using the existing candidate signing workflow.
    """
    archive = os.environ.get("SPACETERM_SERVER_ARCHIVE")
    signature = os.environ.get("SPACETERM_SERVER_SIGNATURE")
    if (archive is not None or signature is not None) and (not archive or not signature):
        raise TaskError("supply both SPACETERM_SERVER_ARCHIVE and SPACETERM_SERVER_SIGNATURE")
    if release_tag:
        require_clean_checkout()
    source = os.environ.get("SPACETERM_SERVER_ARTIFACTS")
    automatic = catalog_destination is not None and not release_tag and source is None
    if automatic:
        from spaceterm_tasks.server_builds import prepare

        prepare(catalog_destination, identity)
        if not archive:
            local = Path(catalog_destination) / host_target()
            archive, signature = local / "artifact.tar.gz", local / "artifact.sig"
    if not archive:
        archive, signature = package(identity, release_tag)
    environment = stage_desktop_artifact(archive, signature, destination, identity=identity)
    if catalog_destination is not None and not automatic:
        from spaceterm_tasks.server_catalog import stage_catalog

        stage_catalog(
            environment["SPACETERM_SERVER_ARCHIVE"],
            environment["SPACETERM_SERVER_SIGNATURE"],
            catalog_destination,
            identity,
            source=source,
            require_all=bool(release_tag),
        )
    return environment


def verify_desktop_artifact(destination, identity, release_tag=None, *, executable=None):
    """Verify signed candidate compatibility and the final client's compiled whole-archive pin."""
    try:
        if {path.name for path in destination.iterdir()} != {"artifact.tar.gz", "artifact.sig"}:
            raise TaskError("the desktop server artifact layout is invalid")
        pin = None
        if executable is not None:
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
                    and re.fullmatch(r"[0-9a-f]{64}", client["server_candidate_sha256"])
                )
                pin = client["server_candidate_sha256"] if valid else None
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
            )
    except (OSError, subprocess.TimeoutExpired):
        raise TaskError("the desktop server artifact is unavailable") from None
