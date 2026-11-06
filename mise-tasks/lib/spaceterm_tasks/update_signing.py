"""Ed25519 update signatures and the release trust that verifies them.

`packaging/update-trust.json` holds the two public keys every Release reader trusts: `current`,
whose private key `SPACETERM_RELEASE_UPDATE_ED25519_SEED` holds, and `backup`, whose private key
stays encrypted in 1Password and never enters GitHub or CI. A signature is valid under either key.
Every signature is 64 raw bytes over a file's exact bytes.

A release dry run has no access to the release key. It signs with a generated key and trusts
only that key, so nothing it signs verifies under the release trust.
"""

import base64
import functools
import hashlib
import json
import os
import re
import subprocess
import tempfile
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError

SIGNING_SECRET = "SPACETERM_RELEASE_UPDATE_ED25519_SEED"
DRY_RUN = "SPACETERM_RELEASE_DRY_RUN"
TRUST = Path("packaging/update-trust.json")
SIGNATURE_BYTES = 64
ED25519_PKCS8_PREFIX = bytes.fromhex("302e020100300506032b657004220420")
ED25519_SPKI_PREFIX = bytes.fromhex("302a300506032b6570032100")


class SignatureMismatch(TaskError):
    """The signature does not authenticate the message under the trusted keys."""


def dry_run(environment=os.environ):
    """Whether this process runs a release dry run: generated keys, ad hoc code signatures, and
    no notarization or publication."""
    return environment.get(DRY_RUN) == "1"


@functools.cache
def require_openssl():
    """Ed25519 needs OpenSSL 3 or newer. macOS's /usr/bin/openssl is LibreSSL, which has none."""
    result = subprocess.run(["openssl", "version"], capture_output=True, check=False)
    version = re.match(rb"OpenSSL ([0-9]+)\.", result.stdout)
    if result.returncode or not version or int(version.group(1)) < 3:
        raise TaskError("update signing needs OpenSSL 3 or newer on PATH, not LibreSSL")


def checked(arguments, **kwargs):
    if arguments[0] == "openssl":
        require_openssl()
    result = subprocess.run(arguments, capture_output=True, check=False, **kwargs)
    if result.returncode:
        raise TaskError("update signing or verification failed")
    return result.stdout


def sha256(path):
    with Path(path).open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def public_key_bytes(value):
    """The 32 raw bytes of a standard base64 Ed25519 public key, or None."""
    if not isinstance(value, str) or len(value) != 44:
        return None
    try:
        raw = base64.b64decode(value, validate=True)
    except ValueError:
        return None
    return raw if len(raw) == 32 else None


def signing_seed(secret):
    """The 32-byte Ed25519 seed of a base64 update signing secret."""
    try:
        seed = base64.b64decode(secret.strip(), validate=True)
    except ValueError:
        raise TaskError("the update signing secret is invalid") from None
    if len(seed) != 32:
        raise TaskError("the update signing secret must contain a 32-byte Ed25519 seed")
    return seed


def generated_seed():
    return os.urandom(32)


def encoded_seed(seed):
    return base64.b64encode(seed).decode()


def derived_public_key(seed):
    # Derive the public key through stdin. The private key never enters argv or logs.
    derived = checked(
        ["openssl", "pkey", "-inform", "DER", "-pubout", "-outform", "DER"],
        input=ED25519_PKCS8_PREFIX + seed,
    )
    return base64.b64encode(derived[-32:]).decode()


def release_trust(root=ROOT):
    """The `current` and `backup` keys of packaging/update-trust.json."""

    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate key")
            result[key] = value
        return result

    try:
        document = json.loads((root / TRUST).read_text(), object_pairs_hook=unique)
    except (OSError, ValueError):
        raise TaskError("packaging/update-trust.json is missing or malformed") from None
    if not isinstance(document, dict) or set(document) != {"current", "backup"}:
        raise TaskError("packaging/update-trust.json must hold exactly current and backup")
    if any(public_key_bytes(document[name]) is None for name in ("current", "backup")):
        raise TaskError("packaging/update-trust.json holds no release keys yet")
    if document["current"] == document["backup"]:
        raise TaskError("the current and backup update keys must differ")
    return document["current"], document["backup"]


def trust(environment=os.environ, root=ROOT):
    """The public keys a release reader trusts. A dry run trusts only its generated key."""
    if dry_run(environment):
        return (derived_public_key(seed_from(environment)),)
    return release_trust(root)


def seed_from(environment):
    secret = environment.get(SIGNING_SECRET)
    if not secret:
        raise TaskError(f"{SIGNING_SECRET} must hold the update signing key")
    return signing_seed(secret)


def release_seed(environment=os.environ, root=ROOT):
    """The update signing seed, which must belong to the trusted `current` key."""
    seed = seed_from(environment)
    if derived_public_key(seed) != trust(environment, root)[0]:
        raise TaskError(
            "the update signing key is not the current key in packaging/update-trust.json"
        )
    return seed


def private_key_file(seed, directory):
    key = Path(directory) / "key.der"
    descriptor = os.open(key, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "wb") as output:
        output.write(ED25519_PKCS8_PREFIX + seed)
    return key


def sign(seed, message, signature):
    """Write the 64-byte signature of the file `message` to `signature`."""
    with tempfile.TemporaryDirectory(prefix="spaceterm-signing-") as temporary:
        key = private_key_file(seed, temporary)
        checked(
            [
                "openssl",
                "pkeyutl",
                "-sign",
                "-rawin",
                "-keyform",
                "DER",
                "-inkey",
                key,
                "-in",
                message,
                "-out",
                signature,
            ]
        )
    if Path(signature).stat().st_size != SIGNATURE_BYTES:
        raise TaskError("the update signature is not 64 bytes")


def verifies(public, message, signature):
    raw = public_key_bytes(public)
    if raw is None or Path(signature).stat().st_size != SIGNATURE_BYTES:
        return False
    require_openssl()
    with tempfile.TemporaryDirectory(prefix="spaceterm-verify-") as temporary:
        key = Path(temporary) / "key.der"
        key.write_bytes(ED25519_SPKI_PREFIX + raw)
        result = subprocess.run(
            [
                "openssl",
                "pkeyutl",
                "-verify",
                "-pubin",
                "-rawin",
                "-keyform",
                "DER",
                "-inkey",
                key,
                "-in",
                message,
                "-sigfile",
                signature,
            ],
            capture_output=True,
        )
    return result.returncode == 0


def verify(keys, message, signature):
    """Require a signature of the file `message` under one of `keys`."""
    if not any(verifies(public, message, signature) for public in keys):
        raise SignatureMismatch("the update signature does not verify under the trusted keys")
