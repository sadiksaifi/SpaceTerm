"""Notarize and staple Release code with an App Store Connect API key, then assess it.

A release signs with Developer ID and notarizes. A dry run signs ad hoc, which Apple's notary
service rejects, so it skips notarization with a log line and has no ticket to assess.

The API key reaches notarytool only as a 0600 file in a temporary directory.
"""

import fcntl
import hashlib
import json
import os
import re
import shutil
import subprocess
import tempfile
from contextlib import contextmanager
from dataclasses import dataclass
from datetime import UTC, datetime
from enum import StrEnum
from pathlib import Path

from spaceterm_tasks import TaskError, macos_codesign
from spaceterm_tasks.packaging import DIST, checked
from spaceterm_tasks.update_signing import DRY_RUN, dry_run

API_KEY = "APP_STORE_CONNECT_API_KEY"
KEY_ID = "APP_STORE_CONNECT_KEY_ID"
ISSUER = "APP_STORE_CONNECT_ISSUER_ID"
INPUTS = (API_KEY, KEY_ID, ISSUER)
# The enclosing step must leave time for upload and cleanup after this bounded wait.
WAIT_SECONDS = 15 * 60
MAX_OUTPUT = 1024 * 1024
MAX_ISSUES = 100
UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")


class Failure(StrEnum):
    MALFORMED = "malformed_output"
    TOOL = "tool_failed"
    TIMEOUT = "wait_timeout"
    REJECTED = "not_accepted"
    RECEIPT = "invalid_receipt"
    MISMATCH = "receipt_mismatch"
    UNCERTAIN = "submission_unconfirmed"
    STORAGE = "artifact_storage_failed"
    BUSY = "submission_busy"
    ARCHIVE = "archive_failed"
    STAPLE = "staple_failed"
    TICKET = "ticket_validation_failed"


class NotaryError(TaskError):
    def __init__(self, classification):
        self.classification = classification
        super().__init__(f"notarization failed ({classification.value})")


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


def _write_json(path, value):
    """Replace and fsync both the artifact and its directory before continuing."""
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as output:
        temporary = Path(output.name)
        try:
            output.write((json.dumps(value, sort_keys=True) + "\n").encode())
            output.flush()
            os.fsync(output.fileno())
            os.replace(temporary, path)
            descriptor = os.open(path.parent, os.O_RDONLY)
            try:
                os.fsync(descriptor)
            finally:
                os.close(descriptor)
        finally:
            temporary.unlink(missing_ok=True)


def _checked(arguments, failure, environment):
    try:
        checked(arguments, "notarization operation failed", env=environment)
    except TaskError:
        raise NotaryError(failure) from None


def _digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def _input_digest(path):
    if path.is_file():
        return _digest(path)
    digest = hashlib.sha256()
    for child in sorted(path.rglob("*")):
        # Symlinks, executable modes and relative names are part of the submitted bundle.
        metadata = [child.relative_to(path).as_posix(), child.lstat().st_mode]
        if child.is_symlink():
            metadata.append(os.readlink(child))
        elif child.is_file():
            metadata.append(_digest(child))
        digest.update(json.dumps(metadata).encode() + b"\n")
    return digest.hexdigest()


def _tool(command, value, notary, environment):
    arguments = [
        "xcrun",
        "notarytool",
        command,
        str(value),
        "--key",
        notary.key_file,
        "--key-id",
        notary.key_id,
        "--issuer",
        notary.issuer,
        "--output-format",
        "json",
    ]
    if command == "wait":
        arguments += ["--timeout", f"{WAIT_SECONDS}s"]
    # Capture raw native output privately; never attach stderr or the unfiltered log.
    with tempfile.TemporaryFile() as output:
        try:
            result = subprocess.run(
                arguments,
                stdout=output,
                stderr=subprocess.DEVNULL,
                env=environment,
                timeout=WAIT_SECONDS + 30 if command == "wait" else 300,
            )
        except subprocess.TimeoutExpired:
            raise NotaryError(Failure.TIMEOUT) from None
        except OSError:
            raise NotaryError(Failure.TOOL) from None
        output.seek(0)
        raw = output.read(MAX_OUTPUT + 1)
    if len(raw) > MAX_OUTPUT:
        raise NotaryError(Failure.MALFORMED)
    return result.returncode, raw


def _object(raw):
    try:
        value = json.loads(raw)
    except (ValueError, UnicodeError, RecursionError):
        raise NotaryError(Failure.MALFORMED) from None
    if not isinstance(value, dict):
        raise NotaryError(Failure.MALFORMED)
    return value


def _identifier(value):
    return isinstance(value, str) and UUID.fullmatch(value) is not None


def _status(code, raw, identifier):
    value = _object(raw)
    if value.get("id") != identifier or value.get("status") not in (
        "Accepted",
        "In Progress",
        "Invalid",
        "Rejected",
    ):
        raise NotaryError(Failure.MALFORMED)
    if value["status"] == "In Progress" and code:
        raise NotaryError(Failure.TIMEOUT)
    if code:
        raise NotaryError(Failure.TOOL)
    return value["status"]


def _diagnostic(directory, identifier, failure, notary, environment):
    evidence = {"failure": failure.value, "id": identifier, "log": "unavailable"}
    if identifier is not None:
        try:
            code, raw = _tool("log", identifier, notary, environment)
            log = _object(raw)
            if code or log.get("jobId") != identifier:
                raise NotaryError(Failure.MALFORMED)
            # Allowlist typed facts. Apple's messages, filenames, paths, URLs and ticket
            # contents can contain private values and are never copied to the artifact.
            evidence["log"] = "retrieved"
            if type(log.get("statusCode")) is int and 0 <= log["statusCode"] <= 99999:
                evidence["status_code"] = log["statusCode"]
            issues = log.get("issues")
            if isinstance(issues, list):
                evidence["issues"] = [
                    {
                        "classification": _issue_classification(issue.get("message")),
                        "severity": issue.get("severity")
                        if issue.get("severity") in ("error", "warning")
                        else "unknown",
                        "code": issue.get("code")
                        if type(issue.get("code")) is int and 0 <= issue["code"] <= 99999
                        else None,
                        "architecture": issue.get("architecture")
                        if issue.get("architecture") in ("arm64", "x86_64", "i386")
                        else "unknown",
                    }
                    for issue in issues[:MAX_ISSUES]
                    if isinstance(issue, dict)
                ]
                evidence["truncated"] = len(issues) > MAX_ISSUES
        except NotaryError as error:
            evidence["log_failure"] = error.classification.value
    _write_json(directory / "diagnostic.json", evidence)


def _issue_classification(message):
    if isinstance(message, str):
        for text, classification in (
            ("timestamp", "timestamp"),
            ("hardened runtime", "hardened_runtime"),
            ("entitlement", "entitlements"),
            ("signed", "signature"),
            ("SDK", "sdk"),
            ("bundle", "bundle"),
        ):
            if text in message:
                return classification
    return "other"


def _record_status(directory, status):
    path = directory / "receipt.json"
    receipt = _object(path.read_bytes())
    receipt["status"] = status
    _write_json(path, receipt)


def _submission(path, directory, notary, environment, plan_sha256):
    receipt_path = directory / "receipt.json"
    payload = directory / ("submission.zip" if path.suffix == ".app" else "submission.dmg")
    binding = {
        "source_sha": environment.get("GITHUB_SHA", ""),
        "plan_sha256": plan_sha256,
        "product": path.name,
        "input_sha256": _input_digest(path),
    }
    if receipt_path.exists():
        try:
            receipt = _object(receipt_path.read_bytes())
        except NotaryError:
            raise NotaryError(Failure.RECEIPT) from None
        if any(receipt.get(key) != value for key, value in binding.items()):
            raise NotaryError(Failure.MISMATCH)
        if not payload.is_file() or _digest(payload) != receipt.get("submitted_sha256"):
            raise NotaryError(Failure.MISMATCH)
        if not _identifier(receipt.get("id")):
            # Submission may have reached Apple before interruption. Never resubmit it.
            raise NotaryError(Failure.UNCERTAIN)
        return receipt["id"]
    if path.suffix == ".app":
        _checked(
            ["ditto", "-c", "-k", "--keepParent", str(path), str(payload)],
            Failure.ARCHIVE,
            environment,
        )
    else:
        shutil.copyfile(path, payload)
    with payload.open("rb") as source:
        os.fsync(source.fileno())
    receipt = {
        **binding,
        "submitted_sha256": _digest(payload),
        "id": None,
        "status": "unconfirmed",
        "run_id": environment.get("GITHUB_RUN_ID", ""),
        "run_attempt": environment.get("GITHUB_RUN_ATTEMPT", ""),
        "submitted_at": datetime.now(UTC).isoformat(),
    }
    _write_json(receipt_path, receipt)
    identifier = None
    try:
        code, raw = _tool("submit", payload, notary, environment)
        # Save a recoverable UUID even if the rest of the response is malformed.
        try:
            candidate = _object(raw).get("id")
            if _identifier(candidate):
                identifier = candidate
        except NotaryError:
            matches = set(
                re.findall(
                    r'"id"\s*:\s*"(' + UUID.pattern + r')"', raw.decode("utf-8", errors="replace")
                )
            )
            if len(matches) == 1:
                identifier = matches.pop()
        if identifier is not None:
            receipt["id"] = identifier
            _write_json(receipt_path, receipt)
        if not _identifier(identifier):
            raise NotaryError(Failure.MALFORMED)
        status = _status(code, raw, identifier)
        _record_status(directory, status)
        if status not in ("Accepted", "In Progress"):
            raise NotaryError(Failure.REJECTED)
    except NotaryError as error:
        _diagnostic(directory, identifier, error.classification, notary, environment)
        raise
    return identifier


def notarize(path, notary, environment=os.environ, *, artifact_directory=None, plan_sha256=None):
    """Submit once, persist the UUID, then resume info/wait and staple only Accepted code.

    Upload the entire artifact_directory (default dist/notarization/<product>), including
    receipt.json and the exact submitted ZIP/DMG. Restore it at the same directory on retry
    and supply the unchanged signed input and source/plan. An uncertain submit fails closed.
    WAIT_SECONDS is the wait budget; enclosing steps must also allow upload and cleanup time.
    """
    path = Path(path)
    if notary is None:
        print(
            f"Dry run: notarization and stapling skipped for {path.name}; "
            "no App Store Connect key is used."
        )
        return
    directory = (
        Path(artifact_directory) if artifact_directory else DIST / "notarization" / path.name
    )
    clean = clean_environment(environment)
    try:
        directory.mkdir(parents=True, exist_ok=True)
        with (directory / "lock").open("w") as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                raise NotaryError(Failure.BUSY) from None
            identifier = _submission(path, directory, notary, clean, plan_sha256)
            try:
                code, raw = _tool("info", identifier, notary, clean)
                status = _status(code, raw, identifier)
                _record_status(directory, status)
                if status == "In Progress":
                    code, raw = _tool("wait", identifier, notary, clean)
                    status = _status(code, raw, identifier)
                    _record_status(directory, status)
                if status != "Accepted":
                    raise NotaryError(
                        Failure.TIMEOUT if status == "In Progress" else Failure.REJECTED
                    )
            except NotaryError as error:
                _diagnostic(directory, identifier, error.classification, notary, clean)
                raise
            _checked(["xcrun", "stapler", "staple", str(path)], Failure.STAPLE, clean)
            _checked(
                ["xcrun", "stapler", "validate", str(path)],
                Failure.TICKET,
                clean,
            )
    except OSError:
        raise NotaryError(Failure.STORAGE) from None
    print("Notarization accepted; ticket stapled and validated")


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
