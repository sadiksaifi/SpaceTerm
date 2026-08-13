#!/usr/bin/env python3

"""Create or verify the authenticated performance tail-complete receipt."""

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


MAGIC = b"spaceterm.performance.tail-complete/v1\0"
SAFE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,79}\Z")
HEX = re.compile(r"[0-9a-f]{64}\Z")
POSITIVE = re.compile(r"[1-9][0-9]*\Z")
START = re.compile(r"[1-9][0-9]*:[0-9]+\Z")
KEYS = (
    "format_version", "campaign_id", "session_id", "nonce", "quit_token",
    "run_intent_sha256", "subject_identity_sha256", "subject_process_pid",
    "subject_process_start_identity", "driver_receipt_sha256", "driver_events_sha256",
    "workload_metadata_sha256", "workload_events_sha256", "rss_samples_sha256",
    "trace_provisional_receipt_sha256", "tail_completed_continuous_ns",
    "terminal_status", "auth_algorithm", "tail_hmac_sha256",
)
INTENT_KEYS = (
    "format_version", "subject", "subject_identity_sha256", "scenario",
    "scenario_plan_sha256", "workload_sha256", "command_sha256", "environment_sha256",
    "font_sha256", "initial_grid_sha256", "measured_duration_ms", "process_pid",
    "process_start_identity", "campaign_id", "session_id", "nonce",
    "native_provisional_observation_sha256", "status",
)
TRACE_KEYS = (
    "format_version", "subject_identity_sha256", "run_intent_sha256",
    "workload_metadata_sha256", "workload_ready_receipt_sha256",
    "supplemental_evidence_sha256", "capture_status", "requested_duration_ms",
    "actual_duration_ms", "capture_started_continuous_ns", "capture_ended_continuous_ns",
    "trace_bundle_tree_sha256", "toc_sha256", "time_profile_export_sha256",
    "allocations_export_sha256", "hangs_export_sha256", "trace_verification_sha256",
    "verifier_sha256", "status", "auth_algorithm", "provisional_hmac_sha256",
)
TRACE_MAGIC = b"spaceterm.performance.trace-provisional/v1\0"
WORKLOAD_MAGIC = b"spaceterm.performance.workload-auth/v1\0"
WORKLOAD_KEYS = (
    "format_version", "scenario", "campaign_id", "session_id", "nonce",
    "subject_identity_sha256", "subject_process_pid", "subject_process_start_identity",
    "producer_sha256", "producer_pid", "producer_started_continuous_ns",
    "producer_session_id", "producer_process_group", "tty_device", "tty_inode", "tty_rdev",
    "ready_receipt_sha256", "events_sha256", "auth_algorithm", "seed_sha256", "seed_bytes",
    "requested_duration_ms", "warmup_ms", "requested_iterations", "requested_seed_rows",
    "emitted_bytes", "input_events", "plan_start_continuous_ns", "started_continuous_ns",
    "ended_continuous_ns", "status", "events_hmac_sha256",
)
READY_MAGIC = b"spaceterm.performance.workload-ready/v1\0"
READY_KEYS = (
    "format_version", "campaign_id", "session_id", "nonce", "subject_identity_sha256",
    "producer_pid", "producer_started_continuous_ns", "producer_session_id",
    "producer_process_group", "tty_device", "tty_inode", "tty_rdev", "events_device",
    "events_inode", "events_prefix_bytes", "events_prefix_sha256",
    "measurement_ready_continuous_ns", "measurement_ready_byte_count", "auth_algorithm",
    "ready_hmac_sha256",
)


class Invalid(Exception):
    pass


def stable_read(path_text: str, maximum: int, *, private: bool = False) -> bytes:
    path = Path(path_text)
    before = path.lstat()
    if not stat.S_ISREG(before.st_mode) or stat.S_ISLNK(before.st_mode) \
            or before.st_uid != os.geteuid() or before.st_mode & 0o022 \
            or before.st_size <= 0 or before.st_size > maximum \
            or (private and (before.st_mode & 0o077 or before.st_nlink != 1)):
        raise Invalid("unsafe-input")
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(fd)
        data = b""
        while len(data) <= maximum:
            chunk = os.read(fd, min(65536, maximum + 1 - len(data)))
            if not chunk:
                break
            data += chunk
        after = os.fstat(fd)
    finally:
        os.close(fd)
    current = path.lstat()
    fields = ("st_dev", "st_ino", "st_mode", "st_uid", "st_size", "st_mtime_ns", "st_ctime_ns")
    if any(getattr(before, key) != getattr(opened, key) for key in fields) \
            or any(getattr(before, key) != getattr(after, key) for key in fields) \
            or any(getattr(before, key) != getattr(current, key) for key in fields) \
            or len(data) != before.st_size:
        raise Invalid("input-changed")
    return data


def parse(data: bytes, keys: tuple[str, ...]) -> tuple[dict[str, str], bytes]:
    if not data.endswith(b"\n") or b"\0" in data or b"\r" in data:
        raise Invalid("record-encoding")
    lines = data[:-1].split(b"\n")
    if len(lines) != len(keys):
        raise Invalid("record-count")
    values: dict[str, str] = {}
    for expected, line in zip(keys, lines):
        fields = line.split(b"\t")
        if len(fields) != 2 or fields[0].decode("ascii") != expected or not fields[1]:
            raise Invalid("record-order")
        values[expected] = fields[1].decode("utf-8")
    return values, b"\n".join(lines[:-1]) + b"\n"


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def add_common(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--campaign-secret-file", required=True)
    parser.add_argument("--campaign-id", required=True)
    parser.add_argument("--session-id", required=True)
    parser.add_argument("--nonce", required=True)
    parser.add_argument("--quit-token", required=True)
    parser.add_argument("--run-intent", required=True)
    parser.add_argument("--subject-identity", required=True)
    parser.add_argument("--driver-receipt", required=True)
    parser.add_argument("--driver-events", required=True)
    parser.add_argument("--workload-metadata", required=True)
    parser.add_argument("--workload-events", required=True)
    parser.add_argument("--workload-ready-receipt", required=True)
    parser.add_argument("--rss-samples", required=True)
    parser.add_argument("--trace-provisional-receipt", required=True)
    parser.add_argument("--tail-completed-continuous-ns", required=True)


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    create = commands.add_parser("create")
    add_common(create)
    create.add_argument("--output", required=True)
    verify = commands.add_parser("verify")
    add_common(verify)
    verify.add_argument("--receipt", required=True)
    return parser.parse_args()


def build(args: argparse.Namespace) -> tuple[bytes, bytes]:
    if SAFE.fullmatch(args.campaign_id) is None or SAFE.fullmatch(args.session_id) is None \
            or HEX.fullmatch(args.nonce) is None or HEX.fullmatch(args.quit_token) is None \
            or POSITIVE.fullmatch(args.tail_completed_continuous_ns) is None:
        raise Invalid("argument-binding")
    secret = stable_read(args.campaign_secret_file, 4096, private=True)
    if len(secret) < 32:
        raise Invalid("secret-too-short")
    intent_data = stable_read(args.run_intent, 64 * 1024)
    subject_data = stable_read(args.subject_identity, 64 * 1024)
    intent, _ = parse(intent_data, INTENT_KEYS)
    subject_lines = subject_data.splitlines()
    subject = dict(line.split(b"\t", 1) for line in subject_lines)
    pid = subject.get(b"process_pid", b"").decode()
    start = subject.get(b"process_start_identity", b"").decode()
    if intent["format_version"] != "1" or intent["status"] != "prepared" \
            or intent["campaign_id"] != args.campaign_id \
            or intent["session_id"] != args.session_id or intent["nonce"] != args.nonce \
            or intent["subject_identity_sha256"] != digest(subject_data) \
            or intent["process_pid"] != pid or intent["process_start_identity"] != start \
            or POSITIVE.fullmatch(pid) is None or START.fullmatch(start) is None:
        raise Invalid("intent-subject-binding")
    artifact_names = (
        "driver_receipt", "driver_events", "workload_metadata", "workload_events",
        "workload_ready_receipt",
        "rss_samples", "trace_provisional_receipt",
    )
    artifacts = {
        name: stable_read(getattr(args, name), 64 * 1024 * 1024) for name in artifact_names
    }
    trace, trace_unsigned = parse(artifacts["trace_provisional_receipt"], TRACE_KEYS)
    workload, workload_unsigned = parse(artifacts["workload_metadata"], WORKLOAD_KEYS)
    ready, ready_unsigned = parse(artifacts["workload_ready_receipt"], READY_KEYS)
    if trace["format_version"] != "1" or trace["capture_status"] != "CAPTURED" \
            or trace["status"] != "complete" or trace["auth_algorithm"] != "hmac-sha256" \
            or trace["subject_identity_sha256"] != digest(subject_data) \
            or trace["run_intent_sha256"] != digest(intent_data) \
            or trace["workload_metadata_sha256"] != digest(artifacts["workload_metadata"]) \
            or HEX.fullmatch(trace["provisional_hmac_sha256"]) is None:
        raise Invalid("trace-provisional-binding")
    expected_trace_hmac = hmac.new(
        secret, TRACE_MAGIC + struct.pack(">Q", len(trace_unsigned)) + trace_unsigned,
        hashlib.sha256,
    ).hexdigest()
    if not hmac.compare_digest(trace["provisional_hmac_sha256"], expected_trace_hmac):
        raise Invalid("trace-provisional-authentication")
    if workload["format_version"] != "3" or workload["status"] != "complete" \
            or workload["auth_algorithm"] != "hmac-sha256" \
            or workload["campaign_id"] != args.campaign_id \
            or workload["session_id"] != args.session_id or workload["nonce"] != args.nonce \
            or workload["subject_identity_sha256"] != digest(subject_data) \
            or workload["events_sha256"] != digest(artifacts["workload_events"]) \
            or HEX.fullmatch(workload["events_hmac_sha256"]) is None \
            or POSITIVE.fullmatch(workload["ended_continuous_ns"]) is None:
        raise Invalid("workload-binding")
    expected_workload_hmac = hmac.new(
        secret,
        WORKLOAD_MAGIC + struct.pack(">Q", len(workload_unsigned)) + workload_unsigned
        + struct.pack(">Q", len(artifacts["workload_events"])) + artifacts["workload_events"],
        hashlib.sha256,
    ).hexdigest()
    if not hmac.compare_digest(workload["events_hmac_sha256"], expected_workload_hmac):
        raise Invalid("workload-authentication")
    if ready["format_version"] != "1" or ready["auth_algorithm"] != "hmac-sha256" \
            or ready["campaign_id"] != args.campaign_id or ready["session_id"] != args.session_id \
            or ready["nonce"] != args.nonce or ready["subject_identity_sha256"] != digest(subject_data) \
            or workload["ready_receipt_sha256"] != digest(artifacts["workload_ready_receipt"]) \
            or trace["workload_ready_receipt_sha256"] != digest(artifacts["workload_ready_receipt"]) \
            or HEX.fullmatch(ready["ready_hmac_sha256"]) is None:
        raise Invalid("workload-ready-binding")
    for key in ("producer_pid", "producer_started_continuous_ns", "producer_session_id",
                "producer_process_group", "tty_device", "tty_inode", "tty_rdev"):
        if ready[key] != workload[key]:
            raise Invalid("workload-ready-producer-binding")
    expected_ready_hmac = hmac.new(
        secret, READY_MAGIC + struct.pack(">Q", len(ready_unsigned)) + ready_unsigned,
        hashlib.sha256,
    ).hexdigest()
    if not hmac.compare_digest(ready["ready_hmac_sha256"], expected_ready_hmac):
        raise Invalid("workload-ready-authentication")
    events_stat = Path(args.workload_events).stat()
    prefix_bytes = int(ready["events_prefix_bytes"])
    if prefix_bytes <= 0 or prefix_bytes > len(artifacts["workload_events"]) \
            or int(ready["events_device"]) != events_stat.st_dev \
            or int(ready["events_inode"]) != events_stat.st_ino \
            or ready["events_prefix_sha256"] \
                != digest(artifacts["workload_events"][:prefix_bytes]):
        raise Invalid("workload-ready-prefix-binding")
    tail_delta = int(args.tail_completed_continuous_ns) - int(workload["ended_continuous_ns"])
    if tail_delta < 5_000_000_000 or tail_delta > 15_000_000_000:
        raise Invalid("tail-duration")
    values = {
        "format_version": "1", "campaign_id": args.campaign_id,
        "session_id": args.session_id, "nonce": args.nonce, "quit_token": args.quit_token,
        "run_intent_sha256": digest(intent_data), "subject_identity_sha256": digest(subject_data),
        "subject_process_pid": pid, "subject_process_start_identity": start,
        "driver_receipt_sha256": digest(artifacts["driver_receipt"]),
        "driver_events_sha256": digest(artifacts["driver_events"]),
        "workload_metadata_sha256": digest(artifacts["workload_metadata"]),
        "workload_events_sha256": digest(artifacts["workload_events"]),
        "rss_samples_sha256": digest(artifacts["rss_samples"]),
        "trace_provisional_receipt_sha256": digest(artifacts["trace_provisional_receipt"]),
        "tail_completed_continuous_ns": args.tail_completed_continuous_ns,
        "terminal_status": "tail-complete", "auth_algorithm": "hmac-sha256",
    }
    unsigned = b"".join(f"{key}\t{values[key]}\n".encode() for key in KEYS[:-1])
    values["tail_hmac_sha256"] = hmac.new(
        secret, MAGIC + struct.pack(">Q", len(unsigned)) + unsigned, hashlib.sha256,
    ).hexdigest()
    return b"".join(f"{key}\t{values[key]}\n".encode() for key in KEYS), secret


def publish(path_text: str, data: bytes) -> None:
    path = Path(path_text)
    if not path.is_absolute() or path.exists() or path.is_symlink():
        raise Invalid("output-exists-or-relative")
    parent = path.parent.resolve(strict=True)
    parent_stat = parent.stat()
    if parent_stat.st_uid != os.geteuid() or parent_stat.st_mode & 0o022:
        raise Invalid("unsafe-output-parent")
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), 0o400)
    try:
        if os.write(fd, data) != len(data):
            raise OSError("short-write")
        os.fchmod(fd, 0o400)
        os.fsync(fd)
    except BaseException:
        os.close(fd)
        os.unlink(path)
        raise
    os.close(fd)


def main() -> int:
    args = arguments()
    try:
        expected, _ = build(args)
        if args.command == "create":
            publish(args.output, expected)
        else:
            actual = stable_read(args.receipt, 64 * 1024, private=True)
            actual_values, _ = parse(actual, KEYS)
            expected_values, _ = parse(expected, KEYS)
            for key in KEYS:
                if key == "tail_hmac_sha256":
                    if not hmac.compare_digest(actual_values[key], expected_values[key]):
                        raise Invalid("tail-authentication")
                elif actual_values[key] != expected_values[key]:
                    raise Invalid(f"tail-binding-{key}")
    except (Invalid, OSError, UnicodeError, ValueError) as error:
        print(f"performance tail receipt failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
