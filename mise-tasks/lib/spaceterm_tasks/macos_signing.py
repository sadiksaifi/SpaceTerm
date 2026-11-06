"""Import release credentials into an isolated Keychain for the duration of signing."""

import base64
import binascii
import re
import secrets
import shlex
import subprocess
import tempfile
from contextlib import contextmanager
from pathlib import Path

from spaceterm_tasks import TaskError

SIGNING_INPUTS = (
    "SPACETERM_RELEASE_DEVID_P12_BASE64",
    "SPACETERM_RELEASE_DEVID_P12_PASSWORD",
    "APPLE_CERTIFICATE",
    "APPLE_CERTIFICATE_PASSWORD",
    "SPACETERM_RELEASE_NOTARY_P8",
    "SPACETERM_RELEASE_NOTARY_KEY_ID",
    "SPACETERM_RELEASE_NOTARY_ISSUER_ID",
    "SPACETERM_RELEASE_UPDATE_ED25519_SEED",
)


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


def signing_credentials(environment):
    """Reject missing or malformed inputs without making a signing key available."""
    encoded = environment.get("SPACETERM_RELEASE_DEVID_P12_BASE64", "")
    password = environment.get("SPACETERM_RELEASE_DEVID_P12_PASSWORD", "")
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
def release_keychain(identity, environment):
    """Require exactly the named private identity; restore the search list and delete imports."""
    certificate, password = signing_credentials(environment)
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
            imported = set(re.findall(rb'\d+\) [0-9A-Fa-f]{40} "([^"]*)"', identities))
            if imported != {identity.encode()}:
                raise TaskError(
                    "release signing Keychain does not contain the named private identity"
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
