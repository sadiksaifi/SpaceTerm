"""Notarize and staple Release code with an App Store Connect API key, then assess it.

A release signs with Developer ID and notarizes. A dry run signs ad hoc, which Apple's notary
service rejects, so it skips notarization with a log line and has no ticket to assess.

The API key reaches notarytool only as a 0600 file in a temporary directory.
"""

import json
import os
import re
import subprocess
import tempfile
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path

from spaceterm_tasks import TaskError, macos_codesign
from spaceterm_tasks.packaging import checked
from spaceterm_tasks.update_signing import DRY_RUN, dry_run

API_KEY = "APP_STORE_CONNECT_API_KEY"
KEY_ID = "APP_STORE_CONNECT_KEY_ID"
ISSUER = "APP_STORE_CONNECT_ISSUER_ID"
INPUTS = (API_KEY, KEY_ID, ISSUER)
TIMEOUT = "2h"


@dataclass(frozen=True)
class Notary:
    key_file: str
    key_id: str
    issuer: str


def required(environment=os.environ):
    """Whether this release package is notarized: Developer ID outside a dry run.

    A dry run must sign ad hoc and a release must sign with Developer ID, so neither can
    publish code the other would.
    """
    adhoc = macos_codesign.signing_identity(environment) == macos_codesign.ADHOC
    if dry_run(environment):
        if not adhoc:
            raise TaskError(f"a dry run signs ad hoc; set {macos_codesign.SIGNING_IDENTITY} to -")
        return False
    if adhoc:
        raise TaskError(
            f"a release signs with Developer ID; {macos_codesign.SIGNING_IDENTITY} must name "
            f"the certificate, and only {DRY_RUN}=1 signs ad hoc"
        )
    return True


def credentials(environment):
    key = environment.get(API_KEY, "")
    key_id = environment.get(KEY_ID, "")
    issuer = environment.get(ISSUER, "")
    if not key or not key_id or not issuer:
        raise TaskError(f"notarization requires {API_KEY}, {KEY_ID} and {ISSUER}")
    if "-----BEGIN PRIVATE KEY-----" not in key:
        raise TaskError(f"{API_KEY} must hold the App Store Connect .p8 key")
    if not re.fullmatch(r"[0-9A-Z]{10}", key_id):
        raise TaskError(f"{KEY_ID} must be a 10-character App Store Connect key ID")
    if not re.fullmatch(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", issuer):
        raise TaskError(f"{ISSUER} must be the App Store Connect issuer UUID")
    return key, key_id, issuer


@contextmanager
def notary(environment=os.environ):
    """Yield the release Notary, or None in a dry run; delete the key file on exit."""
    if not required(environment):
        yield None
        return
    key, key_id, issuer = credentials(environment)
    with tempfile.TemporaryDirectory(prefix="spaceterm-notary-") as temporary:
        key_file = Path(temporary) / f"AuthKey_{key_id}.p8"
        descriptor = os.open(key_file, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "w") as output:
            output.write(key)
        yield Notary(str(key_file), key_id, issuer)


def clean_environment(environment):
    return {key: value for key, value in environment.items() if key not in INPUTS}


def notarize(path, notary, environment=os.environ):
    """Notarize an app-like bundle or disk image, then staple and validate its ticket.

    `notary` is None in a dry run, which skips this step with a log line.
    """
    path = Path(path)
    if notary is None:
        print(
            f"Dry run: notarization and stapling skipped for {path.name}; "
            "no App Store Connect key is used."
        )
        return
    clean = clean_environment(environment)
    with tempfile.TemporaryDirectory(prefix="spaceterm-notarization-") as temporary:
        submission = path
        if path.suffix == ".app":
            submission = Path(temporary) / f"{path.stem}.zip"
            checked(
                ["ditto", "-c", "-k", "--keepParent", str(path), str(submission)],
                f"{path.name} could not be archived for notarization",
                env=clean,
            )
        print(f"Submitting {path.name} for notarization")
        result = subprocess.run(
            [
                "xcrun",
                "notarytool",
                "submit",
                str(submission),
                "--key",
                notary.key_file,
                "--key-id",
                notary.key_id,
                "--issuer",
                notary.issuer,
                "--wait",
                "--timeout",
                TIMEOUT,
                "--output-format",
                "json",
            ],
            capture_output=True,
            env=clean,
        )
    try:
        outcome = json.loads(result.stdout)
        status, identifier = outcome["status"], outcome["id"]
    except (ValueError, KeyError, TypeError):
        raise TaskError(f"notarytool could not submit {path.name}") from None
    if not re.fullmatch(r"[0-9a-f-]{36}", str(identifier)):
        raise TaskError(f"notarytool returned no submission ID for {path.name}")
    if status != "Accepted":
        raise TaskError(
            f"Apple did not accept notarization submission {identifier} of {path.name}; "
            f"read its log with xcrun notarytool log {identifier}"
        )
    checked(["xcrun", "stapler", "staple", str(path)], f"stapling {path.name} failed", env=clean)
    checked(
        ["xcrun", "stapler", "validate", str(path)], f"{path.name} has no stapled ticket", env=clean
    )
    print(f"Notarized and stapled {path.name}")


def assess(path, environment=os.environ):
    """Require a stapled ticket and Gatekeeper's approval. A dry run has no ticket."""
    path = Path(path)
    if dry_run(environment):
        return
    if path.suffix == ".dmg":
        arguments = ["--type", "open", "--context", "context:primary-signature"]
    else:
        arguments = ["--type", "execute"]
    checked(["xcrun", "stapler", "validate", str(path)], f"{path.name} has no stapled ticket")
    checked(["spctl", "--assess", *arguments, str(path)], f"Gatekeeper refuses {path.name}")
