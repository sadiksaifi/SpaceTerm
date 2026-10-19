"""Independent server artifacts authenticated by the existing Ed25519 update trust.

Supported layouts are server/bin/spaceterm-server and server/share/spaceterm on
Apple Silicon macOS 26 and x86_64 glibc Linux (Ubuntu 22.04 or newer). Installation
only retains an exact verified artifact. It never selects an active version or
writes server identity, organization, credentials, stopped intent, or services.
"""

import fcntl
import json
import os
import platform
import re
import shutil
import stat
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
    release_version,
    require_clean_checkout,
    verify_resources,
)

TARGETS = {"darwin": "aarch64-apple-darwin", "linux": "x86_64-unknown-linux-gnu"}
IDENTITIES = ("spaceterm", "spaceterm-preflight", "spaceterm-development")
MAX_ARCHIVE = 256 * 1024 * 1024
MAX_EXPANDED = 512 * 1024 * 1024
MAX_MEMBERS = 128
BUILD_KEYS = {"schema", "identity", "target", "commit", "version"}


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
            or value["identity"] not in IDENTITIES
            or value["target"] not in TARGETS.values()
            or not re.fullmatch(r"[0-9a-f]{40}", value["commit"])
            or len(value["version"]) > 64
            or not re.fullmatch(
                r"(?:dev\.[0-9a-f]{12}|(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*))",
                value["version"],
            )
        ):
            raise ValueError("invalid build")
    except (ValueError, TypeError, UnicodeError):
        raise TaskError("the server build document is malformed or unsupported") from None
    if value["version"].startswith("dev.") and value["version"] != "dev." + value["commit"][:12]:
        raise TaskError("the server version differs from its source commit")
    return value


def build_info(executable):
    try:
        result = subprocess.run(
            [executable, "--build-info"], env={}, capture_output=True, timeout=10
        )
    except (OSError, subprocess.TimeoutExpired):
        raise TaskError("the server build identity query failed") from None
    if result.returncode or result.stderr or len(result.stdout) > 4096:
        raise TaskError("the server build identity query failed")
    return document(result.stdout)


def unpack_verified(archive, signature, destination, expected):
    """Authenticate exact archive bytes before parsing or extracting any member.

    Callers use a private temporary copy, preventing replacement between signature
    verification, extraction, executable checks, and installation.
    """
    if archive.stat().st_size > MAX_ARCHIVE or signature.stat().st_size != 64:
        raise TaskError("the signed server artifact has an invalid size")
    release.verify_ed25519(release.public_key(), archive, signature)
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
                    != (
                        0o755
                        if member.isdir() or member.name == "server/bin/spaceterm-server"
                        else 0o644
                    )
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
            if info != expected:
                raise TaskError("the server artifact differs from the requested exact build")
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


def verify_tree(tree, expected):
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
        "bin/spaceterm-server",
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
    executable = tree / "bin/spaceterm-server"
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
        macos_signing.verify_release_signature(executable, signer(), identifier, "server")
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
    verify_resources(tree / "share/spaceterm", "server")
    try:
        result = subprocess.run(
            [executable, "--check"], env={}, cwd=tree, capture_output=True, timeout=15
        )
    except (OSError, subprocess.TimeoutExpired):
        raise TaskError("the installed server headless check failed") from None
    if (
        result.returncode
        or result.stderr
        or result.stdout != b"Installed server PTY and shell resources verified.\n"
    ):
        raise TaskError("the installed server headless check failed")


def verified_copy(archive, signature, directory, expected):
    archive = Path(archive)
    signature = Path(signature)
    if archive.stat().st_size > MAX_ARCHIVE or signature.stat().st_size != 64:
        raise TaskError("the signed server artifact has an invalid size")
    private = directory / "artifact.tar.gz"
    raw = directory / "artifact.sig"
    shutil.copyfile(archive, private)
    shutil.copyfile(signature, raw)
    unpack_verified(private, raw, directory / "payload", expected)
    tree = directory / "payload/server"
    verify_tree(tree, expected)
    return tree, private, raw


def verify(archive, signature, expected):
    with tempfile.TemporaryDirectory(prefix="spaceterm-server-verify-") as temporary:
        verified_copy(archive, signature, Path(temporary), expected)
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


def package(identity, release_tag=None):
    target = host_target()
    require_clean_checkout()
    if identity not in IDENTITIES or (identity == "spaceterm") != bool(release_tag):
        raise TaskError("only a validated release tag selects the release server identity")
    environment = macos_signing.signing_environment(packaged_environment(release_tag))
    if identity == "spaceterm-development":
        environment.pop("SPACETERM_PACKAGED", None)
    if sys.platform == "darwin":
        environment["MACOSX_DEPLOYMENT_TARGET"] = "26.0"
        macos_signing.signing_credentials(signer(), os.environ)
    secret = os.environ.get(release.SIGNING_SECRET)
    if not secret:
        raise TaskError("the existing update signing key is required")
    seed = release.signing_seed(secret)
    if release.derived_public_key(seed) != release.public_key():
        raise TaskError("the signing key differs from the existing update trust")
    executable = build_executable(
        "--release",
        "--locked",
        "--package",
        "spaceterm-server-host",
        "--no-default-features",
        binary="spaceterm-server",
        env=environment,
        portable_linux=sys.platform == "linux",
    )
    expected = build_info(executable)
    commit = checked(
        ["git", "rev-parse", "HEAD"], "candidate commit unavailable", cwd=ROOT, text=True
    ).strip()
    if (
        expected["identity"] != identity
        or expected["commit"] != commit
        or expected["target"] != target
    ):
        raise TaskError("Cargo returned a different server build")
    DIST.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(dir=DIST, prefix=".server-package-") as temporary:
        directory = Path(temporary)
        tree = directory / "server"
        (tree / "bin").mkdir(parents=True)
        server = tree / "bin/spaceterm-server"
        shutil.copyfile(executable, server)
        server.chmod(0o755)
        resources = tree / "share/spaceterm"
        shutil.copytree(SHELL_INTEGRATION, resources / "shell-integration")
        shutil.copyfile(NOTICES, resources / NOTICES.name)
        checked(
            ["tic", "-x", "-o", str(resources / "terminfo"), str(TERMINFO)],
            "server terminfo compilation failed",
        )
        (tree / "build.json").write_text(json.dumps(expected, sort_keys=True) + "\n")
        if sys.platform == "darwin":
            identifier = f"io.github.sadiksaifi.{identity}.server"
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
        verify_tree(tree, expected)
        name = f"{identity}-server-{commit}-{target}.tar.gz"
        archive = directory / name
        signature = directory / f"{name}.sig"
        write_archive(tree, archive)
        release.sign_ed25519(seed, archive, signature)
        verify(archive, signature, expected)
        archive.replace(DIST / name)
        signature.replace(DIST / signature.name)
    print(f"Created signed server candidate {name}; nothing published")
    return DIST / name, DIST / f"{name}.sig"


def desktop_build(identity, release_tag=None):
    """The exact source revision admitted for a packaged desktop's first server."""
    commit = checked(
        ["git", "rev-parse", "HEAD"], "candidate commit unavailable", cwd=ROOT, text=True
    ).strip()
    return document(
        json.dumps(
            {
                "schema": 1,
                "identity": identity,
                "target": host_target(),
                "commit": commit,
                "version": release_version(release_tag) if release_tag else "dev." + commit[:12],
            }
        )
    )


def stage_desktop_artifact(archive, signature, destination, expected):
    """Embed the verified independent archive and raw signature, never a derived executable."""
    expected = document(json.dumps(expected))
    # Verification and copying use the same private bytes, even if the input path changes.
    with tempfile.TemporaryDirectory(prefix="spaceterm-desktop-artifact-") as temporary:
        _, private, raw = verified_copy(archive, signature, Path(temporary), expected)
        destination.mkdir(parents=True)
        shutil.copyfile(private, destination / "artifact.tar.gz")
        shutil.copyfile(raw, destination / "artifact.sig")
        for path in destination.iterdir():
            path.chmod(0o644)


def prepare_desktop_artifact(destination, identity, release_tag=None):
    """Signing happens during candidate preparation; launch only consumes the resulting pair.

    An explicit pair uses the same inputs as source builds. A candidate without that pair
    builds the independent signed package using the existing candidate signing workflow.
    """
    archive = os.environ.get("SPACETERM_SERVER_ARCHIVE")
    signature = os.environ.get("SPACETERM_SERVER_SIGNATURE")
    if (archive is not None or signature is not None) and (not archive or not signature):
        raise TaskError("supply both SPACETERM_SERVER_ARCHIVE and SPACETERM_SERVER_SIGNATURE")
    require_clean_checkout()
    expected = desktop_build(identity, release_tag)
    if not archive:
        archive, signature = package(identity, release_tag)
    stage_desktop_artifact(archive, signature, destination, expected)


def verify_desktop_artifact(destination, identity, release_tag=None):
    """Verify the pair carried inside the actual app/archive, using the exact candidate build."""
    try:
        if {path.name for path in destination.iterdir()} != {"artifact.tar.gz", "artifact.sig"}:
            raise TaskError("the desktop server artifact layout is invalid")
        verify(
            destination / "artifact.tar.gz",
            destination / "artifact.sig",
            desktop_build(identity, release_tag),
        )
    except OSError:
        raise TaskError("the desktop server artifact is unavailable") from None
