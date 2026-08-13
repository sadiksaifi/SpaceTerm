#!/usr/bin/env python3

"""Verify the authenticated normal-exit closure for one performance subject."""

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


MAGIC = b"spaceterm.acceptance.performance-subject-exit/v1\0"
HEX = re.compile(r"[0-9a-f]{64}\Z")
UINT = re.compile(r"(?:0|[1-9][0-9]*)\Z")
START = re.compile(r"[1-9][0-9]*:[0-9]+\Z")
INTENT_KEYS = (
    "format_version", "subject", "subject_identity_sha256", "scenario",
    "scenario_plan_sha256", "workload_sha256", "command_sha256", "environment_sha256",
    "font_sha256", "initial_grid_sha256", "measured_duration_ms", "process_pid",
    "process_start_identity", "campaign_id", "session_id", "nonce",
    "native_provisional_observation_sha256", "evidence_mode", "status",
)
TAIL_KEYS = (
    "format_version", "campaign_id", "session_id", "nonce", "quit_token",
    "run_intent_sha256", "subject_identity_sha256", "subject_process_pid",
    "subject_process_start_identity", "driver_receipt_sha256", "driver_events_sha256",
    "workload_metadata_sha256", "workload_events_sha256", "rss_samples_sha256",
    "trace_provisional_receipt_sha256", "tail_completed_continuous_ns",
    "lifecycle_helper_device", "lifecycle_helper_inode", "lifecycle_helper_sha256",
    "process_inspector_device", "process_inspector_inode", "process_inspector_sha256",
    "appkit_terminator_process_pid", "appkit_terminator_process_start_identity",
    "appkit_terminator_source_device", "appkit_terminator_source_inode",
    "appkit_terminator_source_sha256", "appkit_terminator_binary_device",
    "appkit_terminator_binary_inode", "appkit_terminator_binary_sha256",
    "evidence_mode", "terminal_status", "auth_algorithm", "tail_hmac_sha256",
)
QUIT_KEYS = (
    "format_version", "campaign_id", "session_id", "nonce", "run_intent_sha256",
    "subject_process_pid", "subject_process_start_identity", "quit_token",
    "request_continuous_ns", "exit_continuous_ns", "termination_method",
    "runtime_closure_status", "lifecycle_helper_device", "lifecycle_helper_inode",
    "lifecycle_helper_sha256", "process_inspector_device", "process_inspector_inode",
    "process_inspector_sha256", "appkit_terminator_process_pid",
    "appkit_terminator_process_start_identity", "appkit_terminator_source_device",
    "appkit_terminator_source_inode", "appkit_terminator_source_sha256",
    "appkit_terminator_binary_device", "appkit_terminator_binary_inode",
    "appkit_terminator_binary_sha256", "evidence_mode", "status",
)
EXIT_KEYS = (
    "schema", "subject", "campaign_id", "session_id", "nonce", "run_intent_sha256",
    "subject_identity_sha256", "process_pid", "process_start_identity",
    "tail_receipt_sha256", "quit_receipt_sha256", "exit_requested_continuous_ns",
    "process_exited_continuous_ns", "exit_status", "native_observation_sha256",
    "lifecycle_helper_device", "lifecycle_helper_inode", "lifecycle_helper_sha256",
    "process_inspector_device", "process_inspector_inode", "process_inspector_sha256",
    "appkit_terminator_process_pid", "appkit_terminator_process_start_identity",
    "appkit_terminator_source_device", "appkit_terminator_source_inode",
    "appkit_terminator_source_sha256", "appkit_terminator_binary_device",
    "appkit_terminator_binary_inode", "appkit_terminator_binary_sha256",
    "evidence_mode", "auth_algorithm", "receipt_hmac_sha256", "status",
)


class Invalid(Exception):
    pass


def stable(
    path_text: str, maximum: int = 65536, *, secret: bool = False, private: bool = False,
) -> bytes:
    path = Path(path_text)
    before = path.lstat()
    if not stat.S_ISREG(before.st_mode) or stat.S_ISLNK(before.st_mode) \
            or before.st_uid != os.geteuid() or before.st_mode & 0o022 \
            or before.st_nlink != 1 or before.st_size <= 0 or before.st_size > maximum:
        raise Invalid("unsafe-input")
    if private and before.st_mode & 0o077:
        raise Invalid("non-private-input")
    if not secret and before.st_mode & 0o200:
        raise Invalid("mutable-input")
    if secret and before.st_size < 32:
        raise Invalid("secret-too-short")
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
    if any(getattr(before, key) != getattr(other, key)
           for other in (opened, after, current) for key in fields) or len(data) != before.st_size:
        raise Invalid("input-changed")
    return data


def parse(data: bytes, keys: tuple[str, ...]) -> tuple[dict[str, str], bytes]:
    if not data.endswith(b"\n") or b"\0" in data or b"\r" in data:
        raise Invalid("encoding")
    lines = data[:-1].split(b"\n")
    if len(lines) != len(keys):
        raise Invalid("record-count")
    result: dict[str, str] = {}
    unsigned = bytearray()
    for expected, line in zip(keys, lines):
        fields = line.split(b"\t")
        if len(fields) != 2 or fields[0].decode("ascii") != expected or not fields[1]:
            raise Invalid("record-order")
        result[expected] = fields[1].decode("utf-8")
        if expected != "receipt_hmac_sha256":
            unsigned.extend(line + b"\n")
    return result, bytes(unsigned)


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--campaign-secret-file", required=True)
    parser.add_argument("--run-intent", required=True)
    parser.add_argument("--subject-identity", required=True)
    parser.add_argument("--tail-receipt", required=True)
    parser.add_argument("--quit-receipt", required=True)
    parser.add_argument("--subject-exit-receipt", required=True)
    parser.add_argument("--native-observation")
    parser.add_argument("--appkit-terminator-source")
    parser.add_argument("--appkit-terminator-binary")
    return parser.parse_args()


def tool_identity(path_text: str, *, executable: bool) -> dict[str, str]:
    path = Path(path_text)
    before = path.lstat()
    if not path.is_absolute() or not stat.S_ISREG(before.st_mode) \
            or stat.S_ISLNK(before.st_mode) or before.st_uid != os.geteuid() \
            or before.st_nlink != 1 or before.st_mode & 0o022 \
            or (executable and before.st_mode & 0o111 == 0):
        raise Invalid("unsafe-terminator-tool")
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(fd)
        data = b""
        while len(data) <= 16 * 1024 * 1024:
            chunk = os.read(fd, min(65536, 16 * 1024 * 1024 + 1 - len(data)))
            if not chunk:
                break
            data += chunk
        after = os.fstat(fd)
    finally:
        os.close(fd)
    current = path.lstat()
    fields = ("st_dev", "st_ino", "st_mode", "st_uid", "st_nlink", "st_size",
              "st_mtime_ns", "st_ctime_ns")
    if len(data) != before.st_size or len(data) > 16 * 1024 * 1024 \
            or any(getattr(before, key) != getattr(other, key)
                   for other in (opened, after, current) for key in fields):
        raise Invalid("terminator-tool-changed")
    prefix = "appkit_terminator_binary" if executable else "appkit_terminator_source"
    return {
        f"{prefix}_device": str(before.st_dev),
        f"{prefix}_inode": str(before.st_ino),
        f"{prefix}_sha256": sha(data),
    }


def verify(args: argparse.Namespace) -> None:
    evidence_mode = "production"
    secret = stable(args.campaign_secret_file, 4096, secret=True, private=True)
    intent_data = stable(args.run_intent)
    subject_data = stable(args.subject_identity)
    tail_data = stable(args.tail_receipt, private=True)
    quit_data = stable(args.quit_receipt, private=True)
    exit_data = stable(args.subject_exit_receipt, private=True)
    intent, _ = parse(intent_data, INTENT_KEYS)
    tail, _ = parse(tail_data, TAIL_KEYS)
    quit_receipt, _ = parse(quit_data, QUIT_KEYS)
    receipt, unsigned = parse(exit_data, EXIT_KEYS)
    tool_keys = (
        "lifecycle_helper_device", "lifecycle_helper_inode", "lifecycle_helper_sha256",
        "process_inspector_device", "process_inspector_inode", "process_inspector_sha256",
        "appkit_terminator_process_pid", "appkit_terminator_process_start_identity",
        "appkit_terminator_source_device", "appkit_terminator_source_inode",
        "appkit_terminator_source_sha256", "appkit_terminator_binary_device",
        "appkit_terminator_binary_inode", "appkit_terminator_binary_sha256",
    )
    subject = dict(line.split(b"\t", 1) for line in subject_data.splitlines())
    pid = subject.get(b"process_pid", b"").decode()
    start = subject.get(b"process_start_identity", b"").decode()
    requested = receipt["exit_requested_continuous_ns"]
    exited = receipt["process_exited_continuous_ns"]
    tail_time = tail["tail_completed_continuous_ns"]
    if intent["format_version"] != "1" or intent["status"] != "prepared" \
            or intent["subject"] not in ("spaceterm", "ghostty") \
            or intent["evidence_mode"] != evidence_mode \
            or intent["subject_identity_sha256"] != sha(subject_data) \
            or intent["process_pid"] != pid or intent["process_start_identity"] != start \
            or START.fullmatch(start) is None or UINT.fullmatch(pid) is None or pid == "0" \
            or tail["evidence_mode"] != evidence_mode \
            or tail["terminal_status"] != "tail-complete" \
            or quit_receipt["termination_method"] != "appkit-terminate" \
            or quit_receipt["runtime_closure_status"] != "confirmed" \
            or quit_receipt["evidence_mode"] != evidence_mode \
            or quit_receipt["status"] != "completed" \
            or receipt["schema"] != "spaceterm.acceptance.performance-subject-exit/v1" \
            or receipt["subject"] != intent["subject"] \
            or receipt["exit_status"] != "normal" or receipt["evidence_mode"] != evidence_mode \
            or receipt["status"] != "complete" \
            or receipt["auth_algorithm"] != "hmac-sha256" \
            or receipt["run_intent_sha256"] != sha(intent_data) \
            or receipt["subject_identity_sha256"] != sha(subject_data) \
            or receipt["process_pid"] != pid or receipt["process_start_identity"] != start \
            or receipt["tail_receipt_sha256"] != sha(tail_data) \
            or receipt["quit_receipt_sha256"] != sha(quit_data):
        raise Invalid("closure-binding")
    if any(tail[key] != quit_receipt[key] or tail[key] != receipt[key]
           for key in tool_keys):
        raise Invalid("terminator-provenance-binding")
    if UINT.fullmatch(receipt["appkit_terminator_process_pid"]) is None \
            or receipt["appkit_terminator_process_pid"] == "0" \
            or START.fullmatch(receipt["appkit_terminator_process_start_identity"]) is None:
        raise Invalid("terminator-process-identity")
    if (args.appkit_terminator_source is None) != (args.appkit_terminator_binary is None):
        raise Invalid("incomplete-terminator-tool")
    if args.appkit_terminator_source is not None:
        actual_tool = {
            **tool_identity(args.appkit_terminator_source, executable=False),
            **tool_identity(args.appkit_terminator_binary, executable=True),
        }
        if any(receipt[key] != value for key, value in actual_tool.items()):
            raise Invalid("terminator-provenance-file-binding")
    elif evidence_mode == "production":
        raise Invalid("missing-production-terminator-tool")
    for key in ("campaign_id", "session_id", "nonce"):
        if receipt[key] != intent[key] or tail[key] != intent[key] or quit_receipt[key] != intent[key]:
            raise Invalid(f"campaign-binding-{key}")
    if tail["run_intent_sha256"] != sha(intent_data) \
            or quit_receipt["run_intent_sha256"] != sha(intent_data) \
            or tail["subject_process_pid"] != pid \
            or tail["subject_process_start_identity"] != start \
            or quit_receipt["subject_process_pid"] != pid \
            or quit_receipt["subject_process_start_identity"] != start \
            or tail["quit_token"] != quit_receipt["quit_token"] \
            or quit_receipt["request_continuous_ns"] != requested \
            or quit_receipt["exit_continuous_ns"] != exited:
        raise Invalid("tail-quit-binding")
    if any(UINT.fullmatch(value) is None for value in (tail_time, requested, exited)) \
            or not int(exited) >= int(requested) >= int(tail_time) > 0:
        raise Invalid("exit-timing")
    native_hash = "not-applicable"
    if intent["subject"] == "spaceterm":
        if args.native_observation is None:
            raise Invalid("missing-native-observation")
        native_hash = sha(stable(args.native_observation))
    elif args.native_observation is not None:
        raise Invalid("ghostty-native-observation")
    if receipt["native_observation_sha256"] != native_hash:
        raise Invalid("native-observation-binding")
    if HEX.fullmatch(receipt["receipt_hmac_sha256"]) is None:
        raise Invalid("exit-authentication")
    expected = hmac.new(
        secret, MAGIC + struct.pack(">Q", len(unsigned)) + unsigned, hashlib.sha256,
    ).hexdigest()
    if not hmac.compare_digest(expected, receipt["receipt_hmac_sha256"]):
        raise Invalid("exit-authentication")


def main() -> int:
    try:
        verify(arguments())
    except (Invalid, OSError, UnicodeError, ValueError) as error:
        print(f"performance subject exit receipt failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
