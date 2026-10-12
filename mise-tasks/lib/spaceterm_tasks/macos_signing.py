"""Import release credentials into an isolated Keychain and verify stable code identity."""

import base64
import binascii
import fcntl
import os
import re
import secrets
import shlex
import stat
import subprocess
import sys
import tempfile
import time
from contextlib import contextmanager
from pathlib import Path

from spaceterm_tasks import TaskError
from spaceterm_tasks.packaging import checked

SIGNING_INPUTS = (
    "MACOS_SIGNING_CERTIFICATE_P12",
    "MACOS_SIGNING_CERTIFICATE_PASSWORD",
    "APPLE_CERTIFICATE",
    "APPLE_CERTIFICATE_PASSWORD",
    "UPDATE_SIGNING_KEY",
)
MACH_O_MAGIC = {
    bytes.fromhex(magic)
    for magic in (
        "feedface",
        "cefaedfe",
        "feedfacf",
        "cffaedfe",
        "cafebabe",
        "bebafeca",
        "cafebabf",
        "bfbafeca",
    )
}


def certificate_requirement(fingerprint):
    if not re.fullmatch(r"[0-9A-Fa-f]{40}", fingerprint):
        raise TaskError("release signing requires a pinned certificate fingerprint")
    return f'certificate leaf = H"{fingerprint.lower()}"'


def signing_environment(environment):
    """Do not pass private credentials to builds, packagers, or native tool logs."""
    return {key: value for key, value in environment.items() if key not in SIGNING_INPUTS}


def security(arguments, environment):
    try:
        result = subprocess.run(
            ["security", *arguments], capture_output=True, env=environment, timeout=30
        )
    except subprocess.TimeoutExpired:
        raise TaskError("release signing Keychain operation timed out") from None
    if result.returncode:
        raise TaskError("release signing Keychain operation failed")
    return result.stdout


def signing_credentials(fingerprint, environment):
    """Reject missing or malformed inputs without making a signing key available."""
    certificate_requirement(fingerprint)
    encoded = environment.get("MACOS_SIGNING_CERTIFICATE_P12", "")
    password = environment.get("MACOS_SIGNING_CERTIFICATE_PASSWORD", "")
    if not encoded or not password:
        raise TaskError("release signing certificate and password are required")
    try:
        certificate = base64.b64decode("".join(encoded.split()), validate=True)
    except (ValueError, binascii.Error):
        raise TaskError("release signing certificate must be valid Base64") from None
    if not certificate:
        raise TaskError("release signing certificate is empty")
    return certificate, password


@contextmanager
def _isolated_release_keychain(fingerprint, environment):
    """Require the pinned private identity; restore the search list and delete all imports."""
    certificate, password = signing_credentials(fingerprint, environment)
    clean = signing_environment(environment)
    original = shlex.split(security(["list-keychains", "-d", "user"], clean).decode())
    with tempfile.TemporaryDirectory(prefix="spaceterm-release-signing-") as temporary:
        root = Path(temporary)
        keychain = root / "signing.keychain-db"
        exported = root / "certificate.p12"
        exported.write_bytes(certificate)
        exported.chmod(0o600)
        keychain_password = secrets.token_hex(32)
        try:
            security(["create-keychain", "-p", keychain_password, str(keychain)], clean)
            security(["unlock-keychain", "-p", keychain_password, str(keychain)], clean)
            security(["set-keychain-settings", "-lut", "7200", str(keychain)], clean)
            security(
                [
                    "import",
                    str(exported),
                    "-k",
                    str(keychain),
                    "-P",
                    password,
                    "-x",
                    "-T",
                    "/usr/bin/codesign",
                ],
                clean,
            )
            exported.unlink()
            identities = security(["find-identity", "-p", "codesigning", str(keychain)], clean)
            # Untrusted self-signed certificates are usable by codesign. Do not filter
            # with -v, which would exclude them. A certificate without its private key
            # does not appear in this list.
            imported = {
                value.upper() for value in re.findall(rb"\d+\) ([0-9A-Fa-f]{40}) ", identities)
            }
            if imported != {fingerprint.upper().encode()}:
                raise TaskError(
                    "release signing Keychain does not contain the pinned private identity"
                )
            if any(
                error != b"CSSMERR_TP_NOT_TRUSTED"
                for error in re.findall(rb"\((CSSMERR_[A-Z_]+)\)", identities)
            ):
                raise TaskError("release signing private identity is invalid")
            security(
                [
                    "set-key-partition-list",
                    "-S",
                    "apple-tool:,apple:,codesign:",
                    "-s",
                    "-k",
                    keychain_password,
                    str(keychain),
                ],
                clean,
            )
            security(["list-keychains", "-d", "user", "-s", str(keychain), *original], clean)
            yield
        finally:
            try:
                security(["list-keychains", "-d", "user", "-s", *original], clean)
            finally:
                if keychain.exists():
                    security(["delete-keychain", str(keychain)], clean)


def verify_release_signature(app, fingerprint, identifier, label):
    """Require the pinned signer throughout the bundle and an invariant app requirement."""
    requirement = certificate_requirement(fingerprint)
    clean = signing_environment(os.environ)
    checked(
        ["codesign", "--verify", "--strict", "--deep", "-R", f"={requirement}", str(app)],
        f"{label} release signature does not match the pinned certificate",
        env=clean,
    )
    designated = checked(
        ["codesign", "--display", "--requirements", "-", str(app)],
        f"{label} release designated requirement could not be read",
        env=clean,
        text=True,
    ).strip()
    if designated != f'designated => identifier "{identifier}" and {requirement}':
        raise TaskError(f"{label} release designated requirement is not stable")
    for path in app.rglob("*"):
        if path.is_symlink() or not path.is_file():
            continue
        with path.open("rb") as binary:
            native = binary.read(4) in MACH_O_MAGIC
        if native:
            checked(
                ["codesign", "--verify", "--strict", "-R", f"={requirement}", str(path)],
                f"{label} bundled executable does not match the pinned certificate",
                env=clean,
            )


@contextmanager
def signing_search_list_lock():
    """Serialize the entire user search-list snapshot/mutation/restore window."""
    root = (
        Path("/private/tmp" if sys.platform == "darwin" else "/tmp")
        / f"spaceterm-signing-{os.getuid()}"
    )
    try:
        root.mkdir(mode=0o700)
    except FileExistsError:
        pass
    directory = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
    descriptor = None
    try:
        info = os.fstat(directory)
        if info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o700:
            raise TaskError("signing serialization storage is unsafe")
        descriptor = os.open(
            "search-list.lock",
            os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_CLOEXEC,
            0o600,
            dir_fd=directory,
        )
        info = os.fstat(descriptor)
        if (
            not stat.S_ISREG(info.st_mode)
            or info.st_uid != os.getuid()
            or info.st_nlink != 1
            or stat.S_IMODE(info.st_mode) != 0o600
        ):
            raise TaskError("signing serialization file is unsafe")
        deadline = time.monotonic() + 30
        while True:
            try:
                fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if time.monotonic() >= deadline:
                    raise TaskError("signing serialization timed out") from None
                time.sleep(0.02)
        yield
    finally:
        if descriptor is not None:
            os.close(descriptor)
        os.close(directory)


@contextmanager
def release_keychain(fingerprint, environment):
    """Import only within the per-account, per-host serialized search-list window."""
    signing_credentials(fingerprint, environment)
    with signing_search_list_lock(), _isolated_release_keychain(fingerprint, environment):
        yield
