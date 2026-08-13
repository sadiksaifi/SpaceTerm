#!/usr/bin/env python3

"""Verify the authenticated producer readiness prefix before native actions."""

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

MAGIC = b"spaceterm.performance.workload-ready/v1\0"
KEYS = (
    "format_version", "campaign_id", "session_id", "nonce",
    "subject_identity_sha256", "producer_pid", "producer_started_continuous_ns",
    "producer_session_id", "producer_process_group", "tty_device", "tty_inode",
    "tty_rdev", "events_device", "events_inode", "events_prefix_bytes",
    "events_prefix_sha256", "measurement_ready_continuous_ns",
    "measurement_ready_byte_count", "auth_algorithm", "ready_hmac_sha256",
)
GATE_KEYS = (
    "format_version", "campaign_id", "session_id", "nonce",
    "ready_receipt_sha256", "plan_start_continuous_ns",
    "start_gate_hmac_sha256",
)
SAFE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,79}\Z")
HEX = re.compile(r"[0-9a-f]{64}\Z")
POSITIVE = re.compile(r"[1-9][0-9]*\Z")
EVENT_HEADER = (
    b"sequence\tcontinuous_ns\tkind\tevent_id\tbyte_count\trows\tcolumns\t"
    b"pixel_width\tpixel_height\tstatus\n"
)


class Invalid(Exception):
    pass


def read_file(path_text: str, maximum: int, *, private: bool = False) -> tuple[bytes, os.stat_result]:
    path = Path(path_text)
    before = path.lstat()
    if not stat.S_ISREG(before.st_mode) or stat.S_ISLNK(before.st_mode):
        raise Invalid("not-regular")
    if before.st_uid != os.geteuid() or before.st_mode & 0o022:
        raise Invalid("unsafe-owner-or-mode")
    if private and (before.st_mode & 0o077 or before.st_nlink != 1):
        raise Invalid("private-file-policy")
    if before.st_size <= 0 or before.st_size > maximum:
        raise Invalid("invalid-size")
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags)
    try:
        opened = os.fstat(descriptor)
        data = b""
        while len(data) <= maximum:
            chunk = os.read(descriptor, min(65536, maximum + 1 - len(data)))
            if not chunk:
                break
            data += chunk
        after = os.fstat(descriptor)
    finally:
        os.close(descriptor)
    fields = ("st_dev", "st_ino", "st_mode", "st_uid", "st_nlink", "st_size", "st_mtime_ns", "st_ctime_ns")
    if any(getattr(before, key) != getattr(opened, key) for key in fields) or any(
        getattr(before, key) != getattr(after, key) for key in fields
    ) or len(data) != before.st_size:
        raise Invalid("file-changed")
    return data, before


def parse_exact(data: bytes, keys: tuple[str, ...] = KEYS) -> tuple[dict[str, str], list[bytes]]:
    if not data.endswith(b"\n") or b"\0" in data or b"\r" in data:
        raise Invalid("encoding")
    lines = data[:-1].split(b"\n")
    if len(lines) != len(keys):
        raise Invalid("schema-width")
    values: dict[str, str] = {}
    for wanted, line in zip(keys, lines):
        fields = line.split(b"\t")
        if len(fields) != 2 or fields[0].decode("ascii") != wanted:
            raise Invalid("schema")
        value = fields[1].decode("utf-8")
        if not value:
            raise Invalid("empty")
        values[wanted] = value
    return values, lines


def positive(value: str) -> int:
    if POSITIVE.fullmatch(value) is None:
        raise Invalid("integer")
    result = int(value)
    if result > (1 << 64) - 1:
        raise Invalid("overflow")
    return result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--ready-receipt", required=True)
    parser.add_argument("--events", required=True)
    parser.add_argument("--subject-identity", required=True)
    parser.add_argument("--campaign-secret-file", required=True)
    parser.add_argument("--campaign-id", required=True)
    parser.add_argument("--session-id", required=True)
    parser.add_argument("--nonce", required=True)
    parser.add_argument("--plan-start-gate")
    parser.add_argument("--expected-plan-start-continuous-ns")
    parser.add_argument("--ignore-events-file-identity", action="store_true")
    arguments = parser.parse_args()
    try:
        receipt, _ = read_file(arguments.ready_receipt, 16 * 1024, private=True)
        events, events_stat = read_file(arguments.events, 64 * 1024 * 1024)
        subject, _ = read_file(arguments.subject_identity, 64 * 1024)
        secret, _ = read_file(arguments.campaign_secret_file, 4096, private=True)
        values, lines = parse_exact(receipt)
        if len(secret) < 32 or values["format_version"] != "1" \
                or values["auth_algorithm"] != "hmac-sha256":
            raise Invalid("format")
        if SAFE.fullmatch(arguments.campaign_id) is None or SAFE.fullmatch(arguments.session_id) is None \
                or HEX.fullmatch(arguments.nonce) is None:
            raise Invalid("argument")
        if values["campaign_id"] != arguments.campaign_id \
                or values["session_id"] != arguments.session_id \
                or values["nonce"] != arguments.nonce:
            raise Invalid("run-binding")
        if values["subject_identity_sha256"] != hashlib.sha256(subject).hexdigest():
            raise Invalid("subject-binding")
        for key in ("producer_pid", "producer_started_continuous_ns", "producer_session_id",
                    "producer_process_group", "tty_device", "tty_inode", "tty_rdev",
                    "events_device", "events_inode", "events_prefix_bytes",
                    "measurement_ready_continuous_ns", "measurement_ready_byte_count"):
            positive(values[key])
        prefix_bytes = positive(values["events_prefix_bytes"])
        if prefix_bytes > len(events) or (not arguments.ignore_events_file_identity and (
                int(values["events_device"]) != events_stat.st_dev
                or int(values["events_inode"]) != events_stat.st_ino)):
            raise Invalid("events-identity")
        prefix = events[:prefix_bytes]
        if values["events_prefix_sha256"] != hashlib.sha256(prefix).hexdigest() \
                or HEX.fullmatch(values["events_prefix_sha256"]) is None:
            raise Invalid("events-prefix-hash")
        rows = prefix.splitlines()
        if not prefix.startswith(EVENT_HEADER) or not rows:
            raise Invalid("events-prefix-header")
        fields = rows[-1].split(b"\t")
        if len(fields) != 10 or fields[2] != b"measurement-ready" or fields[3] != b"none" \
                or fields[9] != b"ok" or fields[1].decode() != values["measurement_ready_continuous_ns"] \
                or fields[4].decode() != values["measurement_ready_byte_count"]:
            raise Invalid("ready-row-binding")
        unsigned = b"\n".join(lines[:-1]) + b"\n"
        expected = hmac.new(secret, MAGIC + struct.pack(">Q", len(unsigned)) + unsigned, hashlib.sha256).hexdigest()
        if HEX.fullmatch(values["ready_hmac_sha256"]) is None \
                or not hmac.compare_digest(values["ready_hmac_sha256"], expected):
            raise Invalid("authentication")
        if bool(arguments.plan_start_gate) != bool(arguments.expected_plan_start_continuous_ns):
            raise Invalid("incomplete-gate-arguments")
        if arguments.plan_start_gate:
            gate, _ = read_file(arguments.plan_start_gate, 16 * 1024, private=True)
            gate_values, gate_lines = parse_exact(gate, GATE_KEYS)
            if gate_values["format_version"] != "1" \
                    or gate_values["campaign_id"] != arguments.campaign_id \
                    or gate_values["session_id"] != arguments.session_id \
                    or gate_values["nonce"] != arguments.nonce \
                    or gate_values["ready_receipt_sha256"] != hashlib.sha256(receipt).hexdigest() \
                    or gate_values["plan_start_continuous_ns"] != arguments.expected_plan_start_continuous_ns:
                raise Invalid("gate-binding")
            positive(gate_values["plan_start_continuous_ns"])
            unsigned_gate = b"\n".join(gate_lines[:-1]) + b"\n"
            gate_expected = hmac.new(
                secret,
                b"spaceterm.performance.plan-start-gate/v1\0"
                + struct.pack(">Q", len(unsigned_gate)) + unsigned_gate,
                hashlib.sha256,
            ).hexdigest()
            if HEX.fullmatch(gate_values["start_gate_hmac_sha256"]) is None \
                    or not hmac.compare_digest(gate_values["start_gate_hmac_sha256"], gate_expected):
                raise Invalid("gate-authentication")
    except (Invalid, OSError, UnicodeError, ValueError) as error:
        print(f"performance workload readiness failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
