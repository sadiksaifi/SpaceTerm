#!/usr/bin/env python3

"""Verify one authenticated native performance workload evidence stream."""

from __future__ import annotations

import argparse
import hashlib
import hmac
import os
import re
import stat
import struct
import sys
from pathlib import Path


AUTH_MAGIC = b"spaceterm.performance.workload-auth/v1\0"
MAX_METADATA_BYTES = 16 * 1024
MAX_EVENTS_BYTES = 64 * 1024 * 1024
MAX_SECRET_BYTES = 4096
METADATA_KEYS = (
    "format_version",
    "scenario",
    "campaign_id",
    "session_id",
    "nonce",
    "subject_identity_sha256",
    "subject_process_pid",
    "subject_process_start_identity",
    "producer_sha256",
    "producer_pid",
    "producer_started_continuous_ns",
    "producer_session_id",
    "producer_process_group",
    "tty_device",
    "tty_inode",
    "tty_rdev",
    "ready_receipt_sha256",
    "events_sha256",
    "auth_algorithm",
    "seed_sha256",
    "seed_bytes",
    "requested_duration_ms",
    "warmup_ms",
    "requested_iterations",
    "requested_seed_rows",
    "emitted_bytes",
    "input_events",
    "plan_start_continuous_ns",
    "started_continuous_ns",
    "ended_continuous_ns",
    "status",
    "events_hmac_sha256",
)
SUBJECT_KEYS = (
    "format_version",
    "subject",
    "app_bundle_path",
    "bundle_identifier",
    "bundle_version",
    "executable_path",
    "executable_sha256",
    "executable_device",
    "executable_inode",
    "executable_fsid",
    "signature_valid",
    "signing_identifier",
    "team_identifier",
    "cdhash",
    "process_pid",
    "process_start_identity",
    "identity_status",
)
READY_KEYS = (
    "format_version", "campaign_id", "session_id", "nonce",
    "subject_identity_sha256", "producer_pid", "producer_started_continuous_ns",
    "producer_session_id", "producer_process_group", "tty_device", "tty_inode",
    "tty_rdev", "events_device", "events_inode", "events_prefix_bytes",
    "events_prefix_sha256", "measurement_ready_continuous_ns",
    "measurement_ready_byte_count", "auth_algorithm", "ready_hmac_sha256",
)
SAFE_LABEL = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,79}\Z")
LOWER_SHA256 = re.compile(r"[0-9a-f]{64}\Z")
UNSIGNED = re.compile(r"0|[1-9][0-9]*\Z")
POSITIVE = re.compile(r"[1-9][0-9]*\Z")


class InvalidEvidence(Exception):
    pass


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Verify authenticated, content-free workload evidence.",
    )
    parser.add_argument("--metadata", required=True)
    parser.add_argument("--events", required=True)
    parser.add_argument("--subject-identity", required=True)
    parser.add_argument("--campaign-secret-file", required=True)
    parser.add_argument("--ready-receipt", required=True)
    parser.add_argument("--campaign-id", required=True)
    parser.add_argument("--session-id", required=True)
    parser.add_argument("--nonce", required=True)
    parser.add_argument("--scenario", required=True)
    parser.add_argument("--requested-warmup-ms", required=True)
    parser.add_argument("--requested-duration-ms", required=True)
    parser.add_argument("--verified-metadata-output")
    parser.add_argument("--verified-events-output")
    parser.add_argument("--verified-subject-identity-output")
    return parser.parse_args()


def publish_snapshot(path_text: str, data: bytes) -> None:
    flags = (
        os.O_WRONLY
        | os.O_CREAT
        | os.O_EXCL
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )
    descriptor = os.open(path_text, flags, 0o400)
    try:
        offset = 0
        while offset < len(data):
            offset += os.write(descriptor, data[offset:])
        os.fchmod(descriptor, 0o400)
        os.fsync(descriptor)
    except BaseException:
        os.close(descriptor)
        os.unlink(path_text)
        raise
    os.close(descriptor)


def read_regular(path_text: str, maximum: int, *, private: bool = False) -> bytes:
    path = Path(path_text)
    before = path.lstat()
    if not stat.S_ISREG(before.st_mode) or stat.S_ISLNK(before.st_mode):
        raise InvalidEvidence("not-regular")
    if before.st_uid != os.geteuid():
        raise InvalidEvidence("wrong-owner")
    if private and before.st_nlink != 1:
        raise InvalidEvidence("private-file-link-count")
    if before.st_mode & 0o022:
        raise InvalidEvidence("group-or-world-writable")
    if private and before.st_mode & 0o077:
        raise InvalidEvidence("secret-not-private")
    if before.st_size <= 0 or before.st_size > maximum:
        raise InvalidEvidence("invalid-size")
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags)
    try:
        opened = os.fstat(descriptor)
        if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
            raise InvalidEvidence("identity-changed-before-read")
        chunks: list[bytes] = []
        remaining = maximum + 1
        while remaining:
            chunk = os.read(descriptor, min(64 * 1024, remaining))
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        data = b"".join(chunks)
        after = os.fstat(descriptor)
    finally:
        os.close(descriptor)
    path_after = os.stat(path, follow_symlinks=False)
    stable_fields = (
        "st_dev",
        "st_ino",
        "st_mode",
        "st_uid",
        "st_size",
        "st_mtime_ns",
        "st_ctime_ns",
    )
    if any(getattr(before, field) != getattr(after, field) for field in stable_fields):
        raise InvalidEvidence("changed-during-read")
    if any(getattr(before, field) != getattr(path_after, field) for field in stable_fields):
        raise InvalidEvidence("path-changed-during-read")
    if len(data) != before.st_size or len(data) > maximum:
        raise InvalidEvidence("read-size-mismatch")
    return data


def parse_exact_tsv(data: bytes, keys: tuple[str, ...]) -> tuple[dict[str, str], list[bytes]]:
    if not data.endswith(b"\n") or b"\0" in data or b"\r" in data:
        raise InvalidEvidence("invalid-encoding")
    raw_lines = data[:-1].split(b"\n")
    if len(raw_lines) != len(keys):
        raise InvalidEvidence("schema-width")
    values: dict[str, str] = {}
    for expected, raw_line in zip(keys, raw_lines):
        fields = raw_line.split(b"\t")
        if len(fields) != 2:
            raise InvalidEvidence("invalid-row")
        try:
            key = fields[0].decode("ascii")
            value = fields[1].decode("utf-8")
        except UnicodeDecodeError as error:
            raise InvalidEvidence("invalid-text") from error
        if key != expected or not value or any(character in value for character in "\t\r\n\0"):
            raise InvalidEvidence("unknown-duplicate-or-empty-key")
        values[key] = value
    return values, raw_lines


def require_unsigned(value: str, *, positive: bool = False) -> int:
    expression = POSITIVE if positive else UNSIGNED
    if expression.fullmatch(value) is None:
        raise InvalidEvidence("invalid-integer")
    parsed = int(value)
    if parsed > (1 << 64) - 1:
        raise InvalidEvidence("integer-overflow")
    return parsed


def parse_process_start(value: str) -> None:
    if len(value.encode()) > 64 or re.fullmatch(
        r"[1-9][0-9]*:(0|[1-9][0-9]{0,5})",
        value,
    ) is None:
        raise InvalidEvidence("invalid-subject-process-start")


def verify() -> None:
    arguments = parse_arguments()
    if SAFE_LABEL.fullmatch(arguments.campaign_id) is None:
        raise InvalidEvidence("invalid-campaign")
    if SAFE_LABEL.fullmatch(arguments.session_id) is None:
        raise InvalidEvidence("invalid-session")
    if LOWER_SHA256.fullmatch(arguments.nonce) is None:
        raise InvalidEvidence("invalid-nonce")
    expected_warmup = require_unsigned(arguments.requested_warmup_ms)
    expected_duration = require_unsigned(arguments.requested_duration_ms)

    metadata = read_regular(arguments.metadata, MAX_METADATA_BYTES)
    events = read_regular(arguments.events, MAX_EVENTS_BYTES)
    subject_data = read_regular(arguments.subject_identity, MAX_METADATA_BYTES)
    secret = read_regular(arguments.campaign_secret_file, MAX_SECRET_BYTES, private=True)
    ready_receipt = read_regular(arguments.ready_receipt, MAX_METADATA_BYTES, private=True)
    if len(secret) < 32:
        raise InvalidEvidence("secret-too-short")

    values, metadata_lines = parse_exact_tsv(metadata, METADATA_KEYS)
    subject, _ = parse_exact_tsv(subject_data, SUBJECT_KEYS)
    ready, _ = parse_exact_tsv(ready_receipt, READY_KEYS)
    if values["format_version"] != "3" or values["auth_algorithm"] != "hmac-sha256":
        raise InvalidEvidence("unsupported-format-or-auth")
    if values["status"] != "complete":
        raise InvalidEvidence("incomplete")
    if values["scenario"] != arguments.scenario:
        raise InvalidEvidence("scenario-mismatch")
    if values["campaign_id"] != arguments.campaign_id:
        raise InvalidEvidence("campaign-mismatch")
    if values["session_id"] != arguments.session_id:
        raise InvalidEvidence("session-mismatch")
    if values["nonce"] != arguments.nonce:
        raise InvalidEvidence("nonce-mismatch")
    if values["subject_identity_sha256"] != hashlib.sha256(subject_data).hexdigest():
        raise InvalidEvidence("subject-hash-mismatch")
    if subject["format_version"] != "1" or subject["identity_status"] != "frozen":
        raise InvalidEvidence("subject-not-frozen")
    if values["subject_process_pid"] != subject["process_pid"]:
        raise InvalidEvidence("subject-pid-mismatch")
    if values["subject_process_start_identity"] != subject["process_start_identity"]:
        raise InvalidEvidence("subject-start-mismatch")
    parse_process_start(values["subject_process_start_identity"])
    if values["events_sha256"] != hashlib.sha256(events).hexdigest():
        raise InvalidEvidence("events-hash-mismatch")
    if values["ready_receipt_sha256"] != hashlib.sha256(ready_receipt).hexdigest():
        raise InvalidEvidence("ready-receipt-hash-mismatch")
    for key in (
        "campaign_id", "session_id", "nonce", "subject_identity_sha256",
        "producer_pid", "producer_started_continuous_ns", "producer_session_id",
        "producer_process_group", "tty_device", "tty_inode", "tty_rdev",
    ):
        if ready[key] != values[key]:
            raise InvalidEvidence("ready-metadata-binding-mismatch")
    if ready["format_version"] != "1" or ready["auth_algorithm"] != "hmac-sha256":
        raise InvalidEvidence("ready-format-mismatch")

    for key in (
        "producer_sha256",
        "seed_sha256",
        "events_sha256",
        "subject_identity_sha256",
        "ready_receipt_sha256",
    ):
        if LOWER_SHA256.fullmatch(values[key]) is None:
            raise InvalidEvidence("invalid-sha256")
    for key in (
        "subject_process_pid",
        "producer_pid",
        "producer_started_continuous_ns",
        "producer_session_id",
        "producer_process_group",
        "tty_device",
        "tty_inode",
        "tty_rdev",
        "emitted_bytes",
        "plan_start_continuous_ns",
        "started_continuous_ns",
        "ended_continuous_ns",
    ):
        require_unsigned(values[key], positive=True)
    for key in (
        "seed_bytes",
        "requested_duration_ms",
        "warmup_ms",
        "requested_iterations",
        "requested_seed_rows",
        "input_events",
    ):
        require_unsigned(values[key])
    if int(values["warmup_ms"]) != expected_warmup:
        raise InvalidEvidence("warmup-mismatch")
    if int(values["requested_duration_ms"]) != expected_duration:
        raise InvalidEvidence("duration-mismatch")
    if int(values["producer_started_continuous_ns"]) >= int(values["started_continuous_ns"]):
        raise InvalidEvidence("producer-start-order")
    plan_start = int(values["plan_start_continuous_ns"])
    measurement_start = plan_start + expected_warmup * 1_000_000
    actual_start = int(values["started_continuous_ns"])
    if actual_start < measurement_start or actual_start - measurement_start > 100_000_000:
        raise InvalidEvidence("measurement-start-gate-mismatch")
    if int(values["started_continuous_ns"]) >= int(values["ended_continuous_ns"]):
        raise InvalidEvidence("measurement-order")
    measured_ns = int(values["ended_continuous_ns"]) - actual_start
    if expected_duration > 0 and not (
        expected_duration * 1_000_000
        <= measured_ns
        <= (expected_duration + 2_000) * 1_000_000
    ):
        raise InvalidEvidence("measurement-duration-mismatch")
    if LOWER_SHA256.fullmatch(values["events_hmac_sha256"]) is None:
        raise InvalidEvidence("invalid-hmac")

    unsigned_metadata = b"\n".join(metadata_lines[:-1]) + b"\n"
    authenticated = (
        AUTH_MAGIC
        + struct.pack(">Q", len(unsigned_metadata))
        + unsigned_metadata
        + struct.pack(">Q", len(events))
        + events
    )
    expected_hmac = hmac.new(secret, authenticated, hashlib.sha256).hexdigest()
    if not hmac.compare_digest(values["events_hmac_sha256"], expected_hmac):
        raise InvalidEvidence("authentication-failed")
    if read_regular(arguments.campaign_secret_file, MAX_SECRET_BYTES, private=True) != secret:
        raise InvalidEvidence("secret-changed-after-verification")
    snapshot_paths = (
        arguments.verified_metadata_output,
        arguments.verified_events_output,
        arguments.verified_subject_identity_output,
    )
    if any(snapshot_paths) and not all(snapshot_paths):
        raise InvalidEvidence("incomplete-snapshot-output-set")
    if all(snapshot_paths):
        if len(set(snapshot_paths)) != 3:
            raise InvalidEvidence("duplicate-snapshot-output")
        published: list[str] = []
        try:
            for path, data in zip(snapshot_paths, (metadata, events, subject_data)):
                publish_snapshot(path, data)
                published.append(path)
        except BaseException:
            for path in published:
                os.unlink(path)
            raise


def main() -> int:
    try:
        verify()
    except (InvalidEvidence, OSError, ValueError) as error:
        print(f"performance workload authentication failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
