#!/usr/bin/env python3

"""Verify the authenticated lifecycle ready and registration receipts."""

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


READY_MAGIC = b"spaceterm.acceptance.performance-lifecycle-ready/v1\0"
REGISTER_MAGIC = b"spaceterm.acceptance.performance-lifecycle-registration/v1\0"
READY_KEYS = (
    "schema", "subject", "campaign_id", "session_id", "nonce",
    "subject_identity_sha256", "process_pid", "process_start_identity",
    "executable_sha256", "ready_continuous_ns", "registration_control_device",
    "registration_control_inode", "appkit_terminator_source_device",
    "appkit_terminator_source_inode", "appkit_terminator_source_sha256",
    "appkit_terminator_binary_device", "appkit_terminator_binary_inode",
    "appkit_terminator_binary_sha256", "evidence_mode", "auth_algorithm",
    "receipt_hmac_sha256", "status",
)
REGISTER_KEYS = (
    "format_version", "campaign_id", "session_id", "nonce", "registration_token",
    "subject_identity_sha256", "process_pid", "process_start_identity", "run_intent_path",
    "run_intent_sha256", "tail_receipt_path", "workload_metadata_path",
    "workload_events_path", "workload_ready_receipt_path", "quit_receipt_path",
    "subject_exit_receipt_path", "native_observation_path",
    "appkit_terminator_source_device", "appkit_terminator_source_inode",
    "appkit_terminator_source_sha256", "appkit_terminator_binary_device",
    "appkit_terminator_binary_inode", "appkit_terminator_binary_sha256",
    "evidence_mode", "auth_algorithm", "registration_hmac_sha256", "status",
)
HEX = re.compile(r"[0-9a-f]{64}\Z")
POSITIVE = re.compile(r"[1-9][0-9]*\Z")


class Invalid(Exception):
    pass


def read(path_text: str, *, private: bool = False, maximum: int = 65536) -> bytes:
    path = Path(path_text)
    before = path.lstat()
    if not stat.S_ISREG(before.st_mode) or stat.S_ISLNK(before.st_mode) \
            or before.st_uid != os.geteuid() or before.st_mode & 0o022 \
            or before.st_nlink != 1 or before.st_size <= 0 or before.st_size > maximum \
            or (private and before.st_mode & 0o077):
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
    fields = ("st_dev", "st_ino", "st_mode", "st_uid", "st_nlink", "st_size",
              "st_mtime_ns", "st_ctime_ns")
    if len(data) != before.st_size or any(
        getattr(before, key) != getattr(other, key)
        for other in (opened, after, current) for key in fields
    ):
        raise Invalid("input-changed")
    return data


def parse(data: bytes, keys: tuple[str, ...], signature: str) -> tuple[dict[str, str], bytes]:
    if not data.endswith(b"\n") or b"\0" in data or b"\r" in data:
        raise Invalid("encoding")
    lines = data[:-1].split(b"\n")
    if len(lines) != len(keys):
        raise Invalid("record-count")
    values: dict[str, str] = {}
    unsigned = bytearray()
    for key, line in zip(keys, lines):
        fields = line.split(b"\t")
        if len(fields) != 2 or fields[0].decode("ascii") != key or not fields[1]:
            raise Invalid("record-order")
        values[key] = fields[1].decode("utf-8")
        if key != signature:
            unsigned.extend(line + b"\n")
    return values, bytes(unsigned)


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def canonical(path_text: str) -> str:
    return str(Path(path_text).resolve(strict=True))


def tool_values(path_text: str, prefix: str, executable: bool) -> dict[str, str]:
    path = Path(path_text)
    before = path.lstat()
    if not path.is_absolute() or not stat.S_ISREG(before.st_mode) or stat.S_ISLNK(before.st_mode) \
            or before.st_uid != os.geteuid() or before.st_nlink != 1 or before.st_mode & 0o022 \
            or (executable and before.st_mode & 0o111 == 0):
        raise Invalid("unsafe-tool")
    return {f"{prefix}_device": str(before.st_dev), f"{prefix}_inode": str(before.st_ino),
            f"{prefix}_sha256": sha(read(path_text, maximum=16 * 1024 * 1024))}


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--campaign-secret-file", required=True)
    parser.add_argument("--ready-receipt", required=True)
    parser.add_argument("--registration-receipt", required=True)
    parser.add_argument("--run-intent", required=True)
    parser.add_argument("--subject-identity", required=True)
    parser.add_argument("--tail-receipt", required=True)
    parser.add_argument("--workload-metadata", required=True)
    parser.add_argument("--workload-events", required=True)
    parser.add_argument("--workload-ready-receipt", required=True)
    parser.add_argument("--quit-receipt", required=True)
    parser.add_argument("--subject-exit-receipt", required=True)
    parser.add_argument("--native-observation")
    parser.add_argument("--appkit-terminator-source", required=True)
    parser.add_argument("--appkit-terminator-binary", required=True)
    return parser.parse_args()


def verify(args: argparse.Namespace) -> None:
    secret = read(args.campaign_secret_file, private=True, maximum=4096)
    if len(secret) < 32:
        raise Invalid("secret-too-short")
    ready_data = read(args.ready_receipt, private=True)
    registration_data = read(args.registration_receipt, private=True)
    intent_data = read(args.run_intent)
    subject_data = read(args.subject_identity)
    ready, ready_unsigned = parse(ready_data, READY_KEYS, "receipt_hmac_sha256")
    registration, registration_unsigned = parse(
        registration_data, REGISTER_KEYS, "registration_hmac_sha256",
    )
    subject = dict(line.split(b"\t", 1) for line in subject_data.splitlines())
    common = ("campaign_id", "session_id", "nonce", "subject_identity_sha256",
              "process_pid", "process_start_identity")
    if ready["schema"] != "spaceterm.acceptance.performance-lifecycle-ready/v1" \
            or ready["status"] != "ready" or ready["evidence_mode"] != "production" \
            or ready["auth_algorithm"] != "hmac-sha256" \
            or registration["format_version"] != "1" or registration["status"] != "registered" \
            or registration["evidence_mode"] != "production" \
            or registration["auth_algorithm"] != "hmac-sha256" \
            or any(ready[key] != registration[key] for key in common) \
            or ready["subject_identity_sha256"] != sha(subject_data) \
            or ready["subject"] != subject.get(b"subject", b"").decode() \
            or ready["process_pid"] != subject.get(b"process_pid", b"").decode() \
            or ready["process_start_identity"] != subject.get(b"process_start_identity", b"").decode() \
            or ready["executable_sha256"] != subject.get(b"executable_sha256", b"").decode() \
            or any(POSITIVE.fullmatch(ready[key]) is None for key in
                   ("ready_continuous_ns", "registration_control_device",
                    "registration_control_inode")) \
            or HEX.fullmatch(registration["registration_token"]) is None \
            or registration["run_intent_sha256"] != sha(intent_data):
        raise Invalid("receipt-binding")
    expected_paths = {
        "run_intent_path": args.run_intent, "tail_receipt_path": args.tail_receipt,
        "workload_metadata_path": args.workload_metadata,
        "workload_events_path": args.workload_events,
        "workload_ready_receipt_path": args.workload_ready_receipt,
        "quit_receipt_path": args.quit_receipt,
        "subject_exit_receipt_path": args.subject_exit_receipt,
    }
    if any(registration[key] != canonical(path) for key, path in expected_paths.items()):
        raise Invalid("registration-path-binding")
    native = "not-applicable" if args.native_observation is None else canonical(args.native_observation)
    if registration["native_observation_path"] != native:
        raise Invalid("native-path-binding")
    tools = {**tool_values(args.appkit_terminator_source, "appkit_terminator_source", False),
             **tool_values(args.appkit_terminator_binary, "appkit_terminator_binary", True)}
    if any(ready[key] != value or registration[key] != value for key, value in tools.items()):
        raise Invalid("terminator-binding")
    for values, unsigned, field, magic in (
        (ready, ready_unsigned, "receipt_hmac_sha256", READY_MAGIC),
        (registration, registration_unsigned, "registration_hmac_sha256", REGISTER_MAGIC),
    ):
        expected = hmac.new(
            secret, magic + struct.pack(">Q", len(unsigned)) + unsigned, hashlib.sha256,
        ).hexdigest()
        if HEX.fullmatch(values[field]) is None or not hmac.compare_digest(values[field], expected):
            raise Invalid("receipt-authentication")


def main() -> int:
    try:
        verify(arguments())
    except (Invalid, OSError, UnicodeError, ValueError) as error:
        print(f"performance lifecycle receipt verification failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
