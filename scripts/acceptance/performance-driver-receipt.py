#!/usr/bin/env python3

"""Create and verify authenticated native performance driver evidence."""

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


INTENT_MAGIC = b"spaceterm.performance.driver-intent/v1\0"
RECEIPT_MAGIC = b"spaceterm.performance.driver-events/v1\0"
MAX_RECORD_BYTES = 64 * 1024
MAX_EVENTS_BYTES = 64 * 1024 * 1024
MAX_PLAN_BYTES = 4 * 1024 * 1024
MAX_BINARY_BYTES = 512 * 1024 * 1024
MAX_SECRET_BYTES = 4096
SAFE_LABEL = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,79}\Z")
EVENT_ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9._:-]{0,95}\Z")
SHA256 = re.compile(r"[0-9a-f]{64}\Z")
UNSIGNED = re.compile(r"0|[1-9][0-9]*\Z")
POSITIVE = re.compile(r"[1-9][0-9]*\Z")
START_IDENTITY = re.compile(r"[1-9][0-9]*:[0-9]+\Z")
PLAN_HEADER = b"event_id\toffset_ms\taction\targ0\targ1"
EVENT_HEADER = (
    b"sequence\tcontinuous_ns\tevent_id\taction\ttarget_pid\twindow_number\t"
    b"requested_a\trequested_b\tobserved_a\tobserved_b\tresult"
)
INTENT_KEYS = (
    "format_version", "campaign_id", "session_id", "nonce",
    "driver_output_path", "driver_output_parent_device", "driver_output_parent_inode",
    "driver_binary_path", "driver_binary_device", "driver_binary_inode",
    "driver_binary_size", "driver_binary_sha256", "driver_source_sha256",
    "controller_sha256", "scenario_plan_sha256", "plan_start_continuous_ns",
    "subject_identity_sha256", "subject_process_pid", "subject_process_start_identity",
    "window_identity_sha256", "window_number", "auth_algorithm", "intent_hmac_sha256",
)
RECEIPT_KEYS = (
    "format_version", "campaign_id", "session_id", "nonce", "intent_sha256",
    "driver_output_device", "driver_output_inode", "driver_output_size",
    "driver_events_sha256", "event_row_count", "first_continuous_ns",
    "last_continuous_ns", "terminal_event_id", "terminal_action", "terminal_result",
    "auth_algorithm", "receipt_hmac_sha256",
)
SUBJECT_KEYS = (
    "format_version", "subject", "app_bundle_path", "bundle_identifier", "bundle_version",
    "executable_path", "executable_sha256", "executable_device", "executable_inode",
    "executable_fsid", "signature_valid", "signing_identifier", "team_identifier",
    "cdhash", "process_pid", "process_start_identity", "identity_status",
)
WINDOW_KEYS = (
    "format_version", "subject_identity_sha256", "subject", "process_pid",
    "process_start_identity", "bundle_identifier", "executable_sha256", "window_number",
    "window_owner_pid_verified", "window_layer", "window_onscreen", "window_minimized",
    "window_x", "window_y", "window_width", "window_height", "resolved_continuous_ns",
    "selector_kind", "status",
)


class Invalid(Exception):
    pass


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def require_unsigned(value: str, *, positive: bool = False) -> int:
    if (POSITIVE if positive else UNSIGNED).fullmatch(value) is None:
        raise Invalid("invalid-integer")
    result = int(value)
    if result > (1 << 64) - 1:
        raise Invalid("integer-overflow")
    return result


def stable_read(
    path_text: str,
    maximum: int,
    *,
    private: bool = False,
    immutable: bool = True,
    executable: bool = False,
) -> tuple[bytes, os.stat_result, str]:
    path = Path(path_text)
    before = path.lstat()
    if not stat.S_ISREG(before.st_mode) or stat.S_ISLNK(before.st_mode):
        raise Invalid("not-regular")
    if before.st_uid != os.geteuid() or before.st_mode & 0o022:
        raise Invalid("unsafe-owner-or-mode")
    if immutable and before.st_mode & 0o200:
        raise Invalid("file-is-mutable")
    if private and (before.st_mode & 0o077 or before.st_nlink != 1):
        raise Invalid("private-file-policy")
    if executable and before.st_mode & 0o111 == 0:
        raise Invalid("not-executable")
    if before.st_size <= 0 or before.st_size > maximum:
        raise Invalid("invalid-size")
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags)
    try:
        opened = os.fstat(descriptor)
        chunks: list[bytes] = []
        remaining = maximum + 1
        while remaining:
            chunk = os.read(descriptor, min(65536, remaining))
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        data = b"".join(chunks)
        after = os.fstat(descriptor)
    finally:
        os.close(descriptor)
    path_after = path.lstat()
    fields = (
        "st_dev", "st_ino", "st_mode", "st_uid", "st_nlink", "st_size",
        "st_mtime_ns", "st_ctime_ns",
    )
    if any(getattr(before, key) != getattr(opened, key) for key in fields) \
            or any(getattr(before, key) != getattr(after, key) for key in fields) \
            or any(getattr(before, key) != getattr(path_after, key) for key in fields) \
            or len(data) != before.st_size or len(data) > maximum:
        raise Invalid("file-changed-during-read")
    return data, before, str(path.resolve(strict=True))


def parse_exact(data: bytes, keys: tuple[str, ...]) -> tuple[dict[str, str], bytes]:
    if not data.endswith(b"\n") or b"\0" in data or b"\r" in data:
        raise Invalid("invalid-record-encoding")
    lines = data[:-1].split(b"\n")
    if len(lines) != len(keys):
        raise Invalid("record-schema-width")
    values: dict[str, str] = {}
    for expected, line in zip(keys, lines):
        fields = line.split(b"\t")
        if len(fields) != 2:
            raise Invalid("invalid-record-row")
        try:
            key = fields[0].decode("ascii")
            value = fields[1].decode("utf-8")
        except UnicodeDecodeError as error:
            raise Invalid("invalid-record-text") from error
        if key != expected or not value:
            raise Invalid("record-key-or-value")
        values[key] = value
    return values, b"\n".join(lines[:-1]) + b"\n"


def parse_kv_input(data: bytes, keys: tuple[str, ...]) -> dict[str, str]:
    values, _ = parse_exact(data, keys)
    return values


def output_identity(path_text: str) -> tuple[str, os.stat_result]:
    path = Path(path_text)
    if not path.is_absolute() or path.name in ("", ".", ".."):
        raise Invalid("driver-output-path-not-absolute")
    if path.exists() or path.is_symlink():
        raise Invalid("driver-output-already-exists")
    parent = path.parent.resolve(strict=True)
    parent_stat = parent.stat()
    if not stat.S_ISDIR(parent_stat.st_mode) or parent_stat.st_uid != os.geteuid() \
            or parent_stat.st_mode & 0o022:
        raise Invalid("unsafe-driver-output-parent")
    return str(parent / path.name), parent_stat


def publish(path_text: str, data: bytes) -> None:
    path = Path(path_text)
    if not path.is_absolute():
        raise Invalid("output-path-not-absolute")
    parent = path.parent.resolve(strict=True)
    parent_stat = parent.stat()
    if not stat.S_ISDIR(parent_stat.st_mode) or parent_stat.st_uid != os.geteuid() \
            or parent_stat.st_mode & 0o022:
        raise Invalid("unsafe-output-parent")
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags, 0o400)
    try:
        offset = 0
        while offset < len(data):
            written = os.write(descriptor, data[offset:])
            if written <= 0:
                raise OSError("short-write")
            offset += written
        os.fchmod(descriptor, 0o400)
        os.fsync(descriptor)
    except BaseException:
        os.close(descriptor)
        os.unlink(path)
        raise
    os.close(descriptor)


def add_binding_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--campaign-secret-file", required=True)
    parser.add_argument("--campaign-id", required=True)
    parser.add_argument("--session-id", required=True)
    parser.add_argument("--nonce", required=True)
    parser.add_argument("--driver-output", required=True)
    parser.add_argument("--driver-binary", required=True)
    parser.add_argument("--driver-source", required=True)
    parser.add_argument("--controller", required=True)
    parser.add_argument("--scenario-plan", required=True)
    parser.add_argument("--plan-start-continuous-ns", required=True)
    parser.add_argument("--subject-identity", required=True)
    parser.add_argument("--window-identity", required=True)


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Create or verify a signed native performance driver stream.",
    )
    commands = parser.add_subparsers(dest="command", required=True)
    intent = commands.add_parser("intent")
    add_binding_arguments(intent)
    intent.add_argument("--output", required=True)
    finalize = commands.add_parser("finalize")
    add_binding_arguments(finalize)
    finalize.add_argument("--intent", required=True)
    finalize.add_argument("--receipt-output", required=True)
    verify = commands.add_parser("verify")
    add_binding_arguments(verify)
    verify.add_argument("--intent", required=True)
    verify.add_argument("--receipt", required=True)
    return parser.parse_args()


def load_bindings(args: argparse.Namespace, *, output_must_be_absent: bool) -> tuple[dict[str, str], bytes]:
    if SAFE_LABEL.fullmatch(args.campaign_id) is None \
            or SAFE_LABEL.fullmatch(args.session_id) is None \
            or SHA256.fullmatch(args.nonce) is None:
        raise Invalid("invalid-run-binding")
    plan_start = require_unsigned(args.plan_start_continuous_ns, positive=True)
    secret, _, _ = stable_read(args.campaign_secret_file, MAX_SECRET_BYTES, private=True, immutable=False)
    if len(secret) < 32:
        raise Invalid("secret-too-short")
    binary, binary_stat, binary_path = stable_read(
        args.driver_binary, MAX_BINARY_BYTES, executable=True,
    )
    source, _, _ = stable_read(args.driver_source, 4 * 1024 * 1024, immutable=False)
    controller, _, _ = stable_read(
        args.controller, 4 * 1024 * 1024, immutable=False, executable=True,
    )
    plan, _, _ = stable_read(args.scenario_plan, MAX_PLAN_BYTES)
    subject_data, _, _ = stable_read(args.subject_identity, MAX_RECORD_BYTES)
    window_data, _, _ = stable_read(args.window_identity, MAX_RECORD_BYTES)
    subject = parse_kv_input(subject_data, SUBJECT_KEYS)
    window = parse_kv_input(window_data, WINDOW_KEYS)
    pid = require_unsigned(subject["process_pid"], positive=True)
    window_number = require_unsigned(window["window_number"], positive=True)
    if subject["format_version"] != "1" or subject["identity_status"] != "frozen" \
            or START_IDENTITY.fullmatch(subject["process_start_identity"]) is None:
        raise Invalid("invalid-subject-identity")
    if window["format_version"] != "1" or window["status"] != "frozen" \
            or window["subject_identity_sha256"] != sha256(subject_data) \
            or window["process_pid"] != str(pid) \
            or window["process_start_identity"] != subject["process_start_identity"] \
            or window["window_owner_pid_verified"] != "true" \
            or window["window_layer"] != "0" or window["window_onscreen"] != "true" \
            or window["window_minimized"] != "false":
        raise Invalid("invalid-window-identity")
    if output_must_be_absent:
        output_path, output_parent = output_identity(args.driver_output)
    else:
        output_path = str(Path(args.driver_output).resolve(strict=True))
        output_parent = Path(output_path).parent.stat()
    values = {
        "format_version": "1",
        "campaign_id": args.campaign_id,
        "session_id": args.session_id,
        "nonce": args.nonce,
        "driver_output_path": output_path,
        "driver_output_parent_device": str(output_parent.st_dev),
        "driver_output_parent_inode": str(output_parent.st_ino),
        "driver_binary_path": binary_path,
        "driver_binary_device": str(binary_stat.st_dev),
        "driver_binary_inode": str(binary_stat.st_ino),
        "driver_binary_size": str(binary_stat.st_size),
        "driver_binary_sha256": sha256(binary),
        "driver_source_sha256": sha256(source),
        "controller_sha256": sha256(controller),
        "scenario_plan_sha256": sha256(plan),
        "plan_start_continuous_ns": str(plan_start),
        "subject_identity_sha256": sha256(subject_data),
        "subject_process_pid": str(pid),
        "subject_process_start_identity": subject["process_start_identity"],
        "window_identity_sha256": sha256(window_data),
        "window_number": str(window_number),
        "auth_algorithm": "hmac-sha256",
    }
    return values, secret


def encoded_rows(keys: tuple[str, ...], values: dict[str, str]) -> bytes:
    return b"".join(f"{key}\t{values[key]}\n".encode("utf-8") for key in keys)


def intent_unsigned(values: dict[str, str]) -> bytes:
    return encoded_rows(INTENT_KEYS[:-1], values)


def authenticate_intent(values: dict[str, str], secret: bytes) -> bytes:
    unsigned = intent_unsigned(values)
    values["intent_hmac_sha256"] = hmac.new(
        secret, INTENT_MAGIC + struct.pack(">Q", len(unsigned)) + unsigned, hashlib.sha256,
    ).hexdigest()
    return encoded_rows(INTENT_KEYS, values)


def verify_intent(args: argparse.Namespace, secret: bytes) -> tuple[bytes, dict[str, str]]:
    data, _, _ = stable_read(args.intent, MAX_RECORD_BYTES, private=True)
    actual, unsigned = parse_exact(data, INTENT_KEYS)
    expected, expected_secret = load_bindings(args, output_must_be_absent=False)
    if not hmac.compare_digest(secret, expected_secret):
        raise Invalid("secret-changed")
    for key in INTENT_KEYS[:-1]:
        if actual[key] != expected[key]:
            raise Invalid(f"intent-binding-{key}")
    signature = hmac.new(
        secret, INTENT_MAGIC + struct.pack(">Q", len(unsigned)) + unsigned, hashlib.sha256,
    ).hexdigest()
    if SHA256.fullmatch(actual["intent_hmac_sha256"]) is None \
            or not hmac.compare_digest(actual["intent_hmac_sha256"], signature):
        raise Invalid("intent-authentication")
    return data, actual


def parse_plan(path_text: str) -> list[tuple[str, int, str]]:
    data, _, _ = stable_read(path_text, MAX_PLAN_BYTES)
    if not data.endswith(b"\n") or b"\0" in data or b"\r" in data:
        raise Invalid("invalid-plan-encoding")
    rows = data[:-1].split(b"\n")
    if len(rows) < 2 or rows[0] != PLAN_HEADER:
        raise Invalid("invalid-plan-header")
    if len(rows) - 1 > 4096:
        raise Invalid("plan-too-many-events")
    result: list[tuple[str, int, str]] = []
    seen: set[str] = set()
    prior = -1
    for raw in rows[1:]:
        fields = raw.split(b"\t")
        if len(fields) != 5:
            raise Invalid("invalid-plan-row")
        try:
            event_id = fields[0].decode("ascii")
            offset_text = fields[1].decode("ascii")
            action = fields[2].decode("ascii")
        except UnicodeDecodeError as error:
            raise Invalid("invalid-plan-text") from error
        if EVENT_ID.fullmatch(event_id) is None or event_id in seen:
            raise Invalid("invalid-plan-event-id")
        offset = require_unsigned(offset_text)
        if offset > 720000 or offset < prior:
            raise Invalid("plan-offset-decreased")
        seen.add(event_id)
        prior = offset
        result.append((event_id, offset, action))
    if result[-1][2] != "stop":
        raise Invalid("plan-terminal-stop-missing")
    return result


def validate_events(
    path_text: str,
    plan: list[tuple[str, int, str]],
    intent: dict[str, str],
) -> tuple[bytes, os.stat_result, dict[str, str]]:
    data, file_stat, canonical = stable_read(path_text, MAX_EVENTS_BYTES, private=True)
    if canonical != intent["driver_output_path"]:
        raise Invalid("driver-output-path-binding")
    if not data.endswith(b"\n") or b"\0" in data or b"\r" in data:
        raise Invalid("invalid-driver-events-encoding")
    rows = data[:-1].split(b"\n")
    if len(rows) != len(plan) + 1 or rows[0] != EVENT_HEADER:
        raise Invalid("driver-events-row-count-or-header")
    pid = intent["subject_process_pid"]
    window = intent["window_number"]
    plan_start = int(intent["plan_start_continuous_ns"])
    first = 0
    last = 0
    terminal_fields: list[str] = []
    for sequence, (raw, expected) in enumerate(zip(rows[1:], plan)):
        fields = raw.split(b"\t")
        if len(fields) != 11:
            raise Invalid("invalid-driver-event-width")
        try:
            decoded = [field.decode("utf-8") for field in fields]
        except UnicodeDecodeError as error:
            raise Invalid("invalid-driver-event-text") from error
        timestamp = require_unsigned(decoded[1], positive=True)
        event_id, offset_ms, action = expected
        if decoded[0] != str(sequence) or decoded[2] != event_id or decoded[3] != action:
            raise Invalid("driver-plan-one-to-one-mismatch")
        if decoded[4] != pid or decoded[5] != window or decoded[10] != "verified":
            raise Invalid("driver-identity-or-result-mismatch")
        deadline = plan_start + offset_ms * 1_000_000
        tolerance = 2_000_000_000 if sequence == 0 else 250_000_000
        if timestamp < deadline or timestamp > deadline + tolerance or timestamp <= last:
            raise Invalid("driver-cadence-mismatch")
        if sequence == 0:
            first = timestamp
        last = timestamp
        terminal_fields = decoded
    if terminal_fields[3] != "stop" or terminal_fields[10] != "verified":
        raise Invalid("driver-terminal-outcome")
    receipt_values = {
        "driver_output_device": str(file_stat.st_dev),
        "driver_output_inode": str(file_stat.st_ino),
        "driver_output_size": str(file_stat.st_size),
        "driver_events_sha256": sha256(data),
        "event_row_count": str(len(plan)),
        "first_continuous_ns": str(first),
        "last_continuous_ns": str(last),
        "terminal_event_id": terminal_fields[2],
        "terminal_action": terminal_fields[3],
        "terminal_result": terminal_fields[10],
    }
    return data, file_stat, receipt_values


def authenticate_receipt(values: dict[str, str], intent: bytes, events: bytes, secret: bytes) -> bytes:
    unsigned = encoded_rows(RECEIPT_KEYS[:-1], values)
    authenticated = (
        RECEIPT_MAGIC
        + struct.pack(">Q", len(intent)) + intent
        + struct.pack(">Q", len(events)) + events
        + struct.pack(">Q", len(unsigned)) + unsigned
    )
    values["receipt_hmac_sha256"] = hmac.new(secret, authenticated, hashlib.sha256).hexdigest()
    return encoded_rows(RECEIPT_KEYS, values)


def make_receipt(args: argparse.Namespace, secret: bytes) -> tuple[bytes, dict[str, str]]:
    intent_data, intent = verify_intent(args, secret)
    plan = parse_plan(args.scenario_plan)
    events, _, event_values = validate_events(args.driver_output, plan, intent)
    values = {
        "format_version": "1",
        "campaign_id": args.campaign_id,
        "session_id": args.session_id,
        "nonce": args.nonce,
        "intent_sha256": sha256(intent_data),
        **event_values,
        "auth_algorithm": "hmac-sha256",
    }
    return authenticate_receipt(values, intent_data, events, secret), values


def verify_receipt(args: argparse.Namespace, secret: bytes) -> None:
    expected_data, expected_values = make_receipt(args, secret)
    actual_data, _, _ = stable_read(args.receipt, MAX_RECORD_BYTES, private=True)
    actual, _ = parse_exact(actual_data, RECEIPT_KEYS)
    expected, _ = parse_exact(expected_data, RECEIPT_KEYS)
    for key in RECEIPT_KEYS:
        if key == "receipt_hmac_sha256":
            if SHA256.fullmatch(actual[key]) is None or not hmac.compare_digest(actual[key], expected[key]):
                raise Invalid("receipt-authentication")
        elif actual[key] != expected_values.get(key, expected[key]):
            raise Invalid(f"receipt-binding-{key}")


def main() -> int:
    args = arguments()
    try:
        if args.command == "intent":
            values, secret = load_bindings(args, output_must_be_absent=True)
            publish(args.output, authenticate_intent(values, secret))
        else:
            secret, _, _ = stable_read(
                args.campaign_secret_file, MAX_SECRET_BYTES, private=True, immutable=False,
            )
            if len(secret) < 32:
                raise Invalid("secret-too-short")
            if args.command == "finalize":
                data, _ = make_receipt(args, secret)
                publish(args.receipt_output, data)
            else:
                verify_receipt(args, secret)
    except (Invalid, OSError, UnicodeError, ValueError) as error:
        print(f"performance driver evidence failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
