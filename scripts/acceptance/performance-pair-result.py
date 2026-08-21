#!/usr/bin/env python3

"""Create or verify the authenticated closure for one paired performance case."""

from __future__ import annotations

import argparse
import hashlib
import hmac
import os
import re
import stat
import struct
import subprocess
import sys
import tempfile
import unicodedata
from pathlib import Path


MAGIC = b"spaceterm.performance.pair-result/v3\0"
READY_MAGIC = b"spaceterm.acceptance.performance-lifecycle-ready/v1\0"
REGISTRATION_MAGIC = b"spaceterm.acceptance.performance-lifecycle-registration/v1\0"
SAFE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,79}\Z")
HEX = re.compile(r"[0-9a-f]{64}\Z")
POSITIVE = re.compile(r"[1-9][0-9]*\Z")
START = re.compile(r"[1-9][0-9]*:[0-9]+\Z")
DRIVER_EVENT_ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9._:-]{0,95}\Z")
PAIR_KEYS = (
    "format_version", "pair_id", "scenario", "plan_sha256", "workload_sha256",
    "command_sha256", "environment_sha256", "font_sha256", "initial_grid_sha256",
    "duration_ms", "spaceterm_subject_identity_sha256",
    "ghostty_subject_identity_sha256",
)
INTENT_KEYS = (
    "format_version", "subject", "subject_identity_sha256", "scenario",
    "scenario_plan_sha256", "workload_sha256", "command_sha256", "environment_sha256",
    "font_sha256", "initial_grid_sha256", "measured_duration_ms", "process_pid",
    "process_start_identity", "campaign_id", "session_id", "nonce",
    "native_provisional_observation_sha256", "evidence_mode", "status",
)
SUBJECT_KEYS = (
    "format_version", "subject", "app_bundle_path", "bundle_identifier",
    "bundle_version", "executable_path", "executable_sha256", "executable_device",
    "executable_inode", "executable_fsid", "signature_valid", "signing_identifier",
    "team_identifier", "cdhash", "process_pid", "process_start_identity",
    "identity_status",
)
RUN_KEYS = (
    "format_version", "subject", "subject_identity_sha256", "scenario",
    "scenario_plan_sha256", "workload_sha256", "command_sha256", "environment_sha256",
    "font_sha256", "initial_grid_sha256", "measured_duration_ms", "process_pid",
    "process_start_identity", "run_intent_sha256", "native_observation_sha256",
    "native_runtime_metadata_sha256", "native_failure_actions_sha256",
    "native_failure_action_enabled", "native_failure_request_count",
    "native_failure_result_count", "native_failure_resource_staged_count",
    "native_failure_resource_staged_bytes", "native_failure_resource_rolled_back_count",
    "native_failure_resource_rolled_back_bytes", "trace_provisional_receipt_sha256",
    "performance_tail_receipt_sha256", "performance_quit_receipt_sha256",
    "subject_exit_receipt_sha256", "lifecycle_ready_receipt_sha256",
    "lifecycle_registration_receipt_sha256", "lifecycle_helper_sha256",
    "terminator_source_sha256", "terminator_binary_sha256", "evidence_mode", "status",
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
GATE_KEYS = (
    "format_version", "campaign_id", "session_id", "nonce",
    "ready_receipt_sha256", "plan_start_continuous_ns", "start_gate_hmac_sha256",
)
TRACE_KEYS = (
    "format_version", "subject_identity_sha256", "run_intent_sha256",
    "workload_metadata_sha256", "workload_ready_receipt_sha256",
    "supplemental_evidence_sha256", "capture_status", "requested_duration_ms",
    "actual_duration_ms", "capture_started_continuous_ns", "capture_ended_continuous_ns",
    "trace_bundle_tree_sha256", "toc_sha256", "time_profile_export_sha256",
    "allocations_export_sha256", "hangs_export_sha256", "trace_verification_sha256",
    "verifier_sha256", "evidence_mode", "status", "auth_algorithm",
    "provisional_hmac_sha256",
)
FINAL_TRACE_KEYS = (
    "format_version", "capture_status", "incomplete_reason", "subject_identity_sha256",
    "run_metadata_sha256", "workload_metadata_sha256", "workload_ready_receipt_sha256",
    "supplemental_evidence_sha256", "requested_duration_ms", "actual_duration_ms",
    "capture_started_continuous_ns", "capture_ended_continuous_ns",
    "target_identity_verified", "trace_target_pid_verified", "time_profiler_instrument",
    "allocations_instrument", "hangs_instrument", "time_profiler_target_verified",
    "allocations_target_verified", "hangs_target_verified", "time_profiler_rows",
    "allocations_rows", "hangs_rows", "maximum_main_thread_hang_ms", "status",
)
DRIVER_INTENT_KEYS = (
    "format_version", "campaign_id", "session_id", "nonce",
    "driver_output_path", "driver_output_parent_device", "driver_output_parent_inode",
    "driver_binary_path", "driver_binary_device", "driver_binary_inode",
    "driver_binary_size", "driver_binary_sha256", "driver_source_sha256",
    "controller_sha256", "scenario_plan_sha256", "plan_start_continuous_ns",
    "subject_identity_sha256", "subject_process_pid", "subject_process_start_identity",
    "window_identity_sha256", "window_number", "auth_algorithm", "intent_hmac_sha256",
)
DRIVER_RECEIPT_KEYS = (
    "format_version", "campaign_id", "session_id", "nonce", "intent_sha256",
    "driver_output_device", "driver_output_inode", "driver_output_size",
    "driver_events_sha256", "event_row_count", "first_continuous_ns",
    "last_continuous_ns", "terminal_event_id", "terminal_action", "terminal_result",
    "auth_algorithm", "receipt_hmac_sha256",
)
WINDOW_KEYS = (
    "format_version", "subject_identity_sha256", "subject", "process_pid",
    "process_start_identity", "bundle_identifier", "executable_sha256", "window_number",
    "window_owner_pid_verified", "window_layer", "window_onscreen", "window_minimized",
    "window_x", "window_y", "window_width", "window_height", "resolved_continuous_ns",
    "selector_kind", "status",
)
DRIVER_PLAN_HEADER = b"event_id\toffset_ms\taction\targ0\targ1"
DRIVER_EVENT_HEADER = (
    b"sequence\tcontinuous_ns\tevent_id\taction\ttarget_pid\twindow_number\t"
    b"requested_a\trequested_b\tobserved_a\tobserved_b\tresult"
)
CASE_REPORT_KEYS = (
    "format_version", "subject", "scenario", "session_id", "nonce",
    "run_intent_sha256", "run_metadata_sha256", "trace_metadata_sha256",
    "trace_archive_sha256", "manual_artifacts_sha256", "manual_screenshot_sha256",
    "manual_video_sha256", "result", "reason",
)
MANUAL_KEYS = (
    "format_version", "screenshot_sha256", "video_sha256", "final_content_review",
    "anchor_review", "restoration_review", "geometry_review", "reviewer", "result",
)
READY_KEYS = (
    "schema", "subject", "campaign_id", "session_id", "nonce",
    "subject_identity_sha256", "process_pid", "process_start_identity",
    "executable_sha256", "ready_continuous_ns", "registration_control_device",
    "registration_control_inode", "lifecycle_helper_device", "lifecycle_helper_inode",
    "lifecycle_helper_sha256", "process_inspector_device", "process_inspector_inode",
    "process_inspector_sha256", "appkit_terminator_process_pid",
    "appkit_terminator_process_start_identity", "appkit_terminator_source_device",
    "appkit_terminator_source_inode", "appkit_terminator_source_sha256",
    "appkit_terminator_binary_device", "appkit_terminator_binary_inode",
    "appkit_terminator_binary_sha256", "evidence_mode", "auth_algorithm",
    "receipt_hmac_sha256", "status",
)
REGISTRATION_KEYS = (
    "format_version", "campaign_id", "session_id", "nonce", "registration_token",
    "subject_identity_sha256", "process_pid", "process_start_identity", "run_intent_path",
    "run_intent_sha256", "tail_receipt_path", "workload_metadata_path",
    "workload_events_path", "workload_ready_receipt_path", "quit_receipt_path",
    "subject_exit_receipt_path", "native_observation_path",
    "lifecycle_helper_device", "lifecycle_helper_inode", "lifecycle_helper_sha256",
    "process_inspector_device", "process_inspector_inode", "process_inspector_sha256",
    "appkit_terminator_process_pid", "appkit_terminator_process_start_identity",
    "appkit_terminator_source_device", "appkit_terminator_source_inode",
    "appkit_terminator_source_sha256", "appkit_terminator_binary_device",
    "appkit_terminator_binary_inode", "appkit_terminator_binary_sha256",
    "evidence_mode", "auth_algorithm", "registration_hmac_sha256", "status",
)
RESULT_KEYS = (
    "format_version", "campaign_id", "pair_metadata_sha256",
    "scenario_plan_sha256", "workload_sha256", "command_sha256",
    "environment_sha256", "font_sha256", "initial_grid_sha256",
    "spaceterm_session_id", "spaceterm_nonce", "spaceterm_run_intent_sha256",
    "spaceterm_run_metadata_sha256", "spaceterm_driver_intent_sha256",
    "spaceterm_driver_events_sha256", "spaceterm_driver_receipt_sha256",
    "spaceterm_window_identity_sha256", "spaceterm_driver_binary_sha256",
    "spaceterm_driver_source_sha256", "spaceterm_driver_controller_sha256",
    "spaceterm_plan_start_gate_sha256", "spaceterm_tail_receipt_sha256",
    "spaceterm_quit_receipt_sha256", "spaceterm_exit_receipt_sha256",
    "spaceterm_case_report_sha256", "spaceterm_trace_metadata_sha256",
    "spaceterm_trace_archive_sha256", "spaceterm_manual_artifacts_sha256",
    "spaceterm_manual_screenshot_sha256", "spaceterm_manual_video_sha256",
    "ghostty_session_id", "ghostty_nonce", "ghostty_run_intent_sha256",
    "ghostty_run_metadata_sha256", "ghostty_driver_intent_sha256",
    "ghostty_driver_events_sha256", "ghostty_driver_receipt_sha256",
    "ghostty_window_identity_sha256", "ghostty_driver_binary_sha256",
    "ghostty_driver_source_sha256", "ghostty_driver_controller_sha256",
    "ghostty_plan_start_gate_sha256", "ghostty_tail_receipt_sha256",
    "ghostty_quit_receipt_sha256", "ghostty_exit_receipt_sha256",
    "ghostty_case_report_sha256", "ghostty_trace_metadata_sha256",
    "ghostty_trace_archive_sha256", "ghostty_manual_artifacts_sha256",
    "ghostty_manual_screenshot_sha256", "ghostty_manual_video_sha256",
    "spaceterm_lifecycle_ready_receipt_sha256",
    "spaceterm_lifecycle_registration_receipt_sha256",
    "ghostty_lifecycle_ready_receipt_sha256",
    "ghostty_lifecycle_registration_receipt_sha256",
    "lifecycle_helper_sha256", "terminator_source_sha256",
    "terminator_binary_sha256",
    "evidence_mode", "status", "auth_algorithm", "pair_result_hmac_sha256",
)


class Invalid(Exception):
    pass


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def stable_read_material(
    path_text: str, maximum: int = 64 * 1024, *, private: bool = False,
    secret: bool = False, mutable: bool = False,
) -> tuple[bytes, os.stat_result, str]:
    path = Path(path_text)
    before = path.lstat()
    if not stat.S_ISREG(before.st_mode) or stat.S_ISLNK(before.st_mode) \
            or before.st_uid != os.geteuid() or before.st_mode & 0o022 \
            or before.st_size <= 0 or before.st_size > maximum:
        raise Invalid("unsafe-input")
    if not secret and not mutable and before.st_mode & 0o200:
        raise Invalid("mutable-input")
    if private and (before.st_mode & 0o077 or before.st_nlink != 1):
        raise Invalid("non-private-input")
    if secret and (before.st_mode & 0o077 or before.st_nlink != 1 \
                   or before.st_size < 32 or before.st_size > 4096):
        raise Invalid("unsafe-secret")
    descriptor = os.open(
        path, os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    )
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
    current = path.lstat()
    fields = (
        "st_dev", "st_ino", "st_mode", "st_uid", "st_nlink", "st_size",
        "st_mtime_ns", "st_ctime_ns",
    )
    if any(getattr(before, key) != getattr(other, key)
           for other in (opened, after, current) for key in fields) \
            or len(data) != before.st_size or len(data) > maximum:
        raise Invalid("input-changed")
    return data, before, str(path.resolve(strict=True))


def stable_read(
    path_text: str, maximum: int = 64 * 1024, *, private: bool = False,
    secret: bool = False, mutable: bool = False,
) -> bytes:
    return stable_read_material(
        path_text, maximum, private=private, secret=secret, mutable=mutable,
    )[0]


def stable_file_digest(path_text: str) -> str:
    path = Path(path_text)
    before = path.lstat()
    if not stat.S_ISREG(before.st_mode) or stat.S_ISLNK(before.st_mode) \
            or before.st_uid != os.geteuid() or before.st_mode & 0o222:
        raise Invalid("unsafe-media")
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor); hasher = hashlib.sha256()
        while True:
            chunk = os.read(descriptor, 1024 * 1024)
            if not chunk: break
            hasher.update(chunk)
        after = os.fstat(descriptor)
    finally:
        os.close(descriptor)
    current = path.lstat()
    fields = ("st_dev", "st_ino", "st_mode", "st_uid", "st_size", "st_mtime_ns", "st_ctime_ns")
    if any(getattr(before, key) != getattr(other, key)
           for other in (opened, after, current) for key in fields):
        raise Invalid("media-changed")
    return hasher.hexdigest()


def stable_trace_tree(path_text: str) -> str:
    root = Path(path_text)
    before = root.lstat()
    if not stat.S_ISDIR(before.st_mode) or stat.S_ISLNK(before.st_mode) \
            or before.st_uid != os.geteuid() or before.st_mode & 0o022:
        raise Invalid("unsafe-trace-archive")
    digestor = hashlib.sha256(b"spaceterm.performance.trace-tree/v1\0")
    entries: list[tuple[bytes, Path]] = []
    directories: dict[Path, os.stat_result] = {root: before}
    observed: list[tuple[str, str]] = []
    for path in root.rglob("*"):
        info = path.lstat()
        if stat.S_ISLNK(info.st_mode) or not (stat.S_ISREG(info.st_mode) or stat.S_ISDIR(info.st_mode)):
            raise Invalid("unsafe-trace-entry")
        relative_text = path.relative_to(root).as_posix()
        relative = unicodedata.normalize("NFC", relative_text)
        if relative != relative_text:
            raise Invalid("noncanonical-trace-entry")
        entry_type = "file" if stat.S_ISREG(info.st_mode) else "directory"
        observed.append((relative, entry_type))
        if stat.S_ISREG(info.st_mode):
            entries.append((relative.encode(), path))
        else:
            directories[path] = info
    for encoded, path in sorted(entries):
        file_before = path.lstat()
        digestor.update(struct.pack(">Q", len(encoded))); digestor.update(encoded)
        digestor.update(struct.pack(">Q", file_before.st_size))
        descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
        try:
            opened = os.fstat(descriptor)
            while True:
                chunk = os.read(descriptor, 1024 * 1024)
                if not chunk: break
                digestor.update(chunk)
            file_after = os.fstat(descriptor)
        finally:
            os.close(descriptor)
        current = path.lstat()
        fields = ("st_dev", "st_ino", "st_mode", "st_uid", "st_size", "st_mtime_ns", "st_ctime_ns")
        if any(getattr(file_before, key) != getattr(other, key)
               for other in (opened, file_after, current) for key in fields):
            raise Invalid("trace-entry-changed")
    rescanned: list[tuple[str, str]] = []
    for path in root.rglob("*"):
        info = path.lstat()
        if stat.S_ISLNK(info.st_mode) or not (stat.S_ISREG(info.st_mode) or stat.S_ISDIR(info.st_mode)):
            raise Invalid("unsafe-trace-entry")
        relative = path.relative_to(root).as_posix()
        rescanned.append((relative, "file" if stat.S_ISREG(info.st_mode) else "directory"))
    if sorted(observed) != sorted(rescanned):
        raise Invalid("trace-entry-set-changed")
    directory_fields = (
        "st_dev", "st_ino", "st_mode", "st_uid", "st_nlink", "st_size",
        "st_mtime_ns", "st_ctime_ns",
    )
    for directory, directory_before in directories.items():
        directory_after = directory.lstat()
        if any(getattr(directory_before, key) != getattr(directory_after, key)
               for key in directory_fields):
            raise Invalid("trace-directory-changed")
    return digestor.hexdigest()


def parse(data: bytes, keys: tuple[str, ...], *, hmac_key: str | None = None) \
        -> tuple[dict[str, str], bytes]:
    if not data.endswith(b"\n") or b"\0" in data or b"\r" in data:
        raise Invalid("record-encoding")
    lines = data[:-1].split(b"\n")
    if len(lines) != len(keys):
        raise Invalid("record-count")
    values: dict[str, str] = {}
    unsigned = bytearray()
    for expected, line in zip(keys, lines):
        fields = line.split(b"\t")
        try:
            key = fields[0].decode("ascii")
            value = fields[1].decode("utf-8")
        except (IndexError, UnicodeDecodeError) as error:
            raise Invalid("record-text") from error
        if len(fields) != 2 or key != expected or not value:
            raise Invalid("record-order")
        values[key] = value
        if key != hmac_key:
            unsigned.extend(line + b"\n")
    return values, bytes(unsigned)


def add_common(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--campaign-secret-file", required=True)
    parser.add_argument("--campaign-id", required=True)
    parser.add_argument("--pair-metadata", required=True)
    parser.add_argument("--scenario-plan", required=True)
    for subject in ("spaceterm", "ghostty"):
        parser.add_argument(f"--{subject}-subject-identity", required=True)
        parser.add_argument(f"--{subject}-run-intent", required=True)
        parser.add_argument(f"--{subject}-run-metadata", required=True)
        parser.add_argument(f"--{subject}-window-identity", required=True)
        parser.add_argument(f"--{subject}-driver-intent", required=True)
        parser.add_argument(f"--{subject}-driver-events", required=True)
        parser.add_argument(f"--{subject}-driver-receipt", required=True)
        parser.add_argument(f"--{subject}-driver-binary", required=True)
        parser.add_argument(f"--{subject}-driver-source", required=True)
        parser.add_argument(f"--{subject}-driver-controller", required=True)
        parser.add_argument(f"--{subject}-plan-start-gate", required=True)
        parser.add_argument(f"--{subject}-trace-provisional-receipt", required=True)
        parser.add_argument(f"--{subject}-workload-metadata", required=True)
        parser.add_argument(f"--{subject}-workload-events", required=True)
        parser.add_argument(f"--{subject}-workload-ready-receipt", required=True)
        parser.add_argument(f"--{subject}-lifecycle-ready-receipt", required=True)
        parser.add_argument(f"--{subject}-lifecycle-registration", required=True)
        parser.add_argument(f"--{subject}-lifecycle-helper", required=True)
        parser.add_argument(f"--{subject}-tail-receipt", required=True)
        parser.add_argument(f"--{subject}-quit-receipt", required=True)
        parser.add_argument(f"--{subject}-exit-receipt", required=True)
        parser.add_argument(f"--{subject}-case-report", required=True)
        parser.add_argument(f"--{subject}-trace-metadata", required=True)
        parser.add_argument(f"--{subject}-trace-archive", required=True)
        parser.add_argument(f"--{subject}-manual-artifacts", required=True)
        parser.add_argument(f"--{subject}-manual-screenshot", required=True)
        parser.add_argument(f"--{subject}-manual-video", required=True)
    parser.add_argument("--spaceterm-native-provisional-observation", required=True)
    parser.add_argument("--spaceterm-native-observation", required=True)
    parser.add_argument("--spaceterm-native-runtime-metadata", required=True)
    parser.add_argument("--spaceterm-native-runtime-samples", required=True)
    parser.add_argument("--spaceterm-native-runtime-events", required=True)
    parser.add_argument("--spaceterm-native-failure-actions", required=True)
    parser.add_argument("--common-lifecycle-helper", required=True)
    parser.add_argument("--process-inspector", default=str(
        Path(__file__).resolve().parent.parent / "inspect-release-performance-process.py"
    ))
    parser.add_argument("--appkit-terminator-source", required=True)
    parser.add_argument("--appkit-terminator-binary", required=True)


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Authenticate a content-free completed SpaceTerm/Ghostty performance pair."
    )
    commands = parser.add_subparsers(dest="command", required=True)
    create = commands.add_parser("create")
    add_common(create)
    create.add_argument("--output", required=True)
    verify = commands.add_parser("verify")
    add_common(verify)
    verify.add_argument("--receipt", required=True)
    return parser.parse_args()


def unsigned_integer(value: str, *, positive: bool = False) -> int:
    pattern = POSITIVE if positive else re.compile(r"0|[1-9][0-9]*\Z")
    if pattern.fullmatch(value) is None:
        raise Invalid("driver-invalid-integer")
    result = int(value)
    if result > (1 << 64) - 1:
        raise Invalid("driver-integer-overflow")
    return result


def driver_plan(data: bytes) -> list[tuple[str, int, str]]:
    if not data.endswith(b"\n") or b"\0" in data or b"\r" in data:
        raise Invalid("driver-plan-encoding")
    lines = data[:-1].split(b"\n")
    if len(lines) < 2 or lines[0] != DRIVER_PLAN_HEADER or len(lines) > 4097:
        raise Invalid("driver-plan-schema")
    result: list[tuple[str, int, str]] = []
    seen: set[str] = set()
    prior = -1
    for raw in lines[1:]:
        fields = raw.split(b"\t")
        if len(fields) != 5:
            raise Invalid("driver-plan-row")
        try:
            event_id, offset_text, action = (field.decode("ascii") for field in fields[:3])
        except UnicodeDecodeError as error:
            raise Invalid("driver-plan-text") from error
        offset = unsigned_integer(offset_text)
        if DRIVER_EVENT_ID.fullmatch(event_id) is None or event_id in seen \
                or offset > 720000 or offset < prior:
            raise Invalid("driver-plan-order")
        result.append((event_id, offset, action)); seen.add(event_id); prior = offset
    if result[-1][2] != "stop":
        raise Invalid("driver-plan-terminal")
    return result


def verify_driver_snapshot(
    *, secret: bytes, campaign_id: str, session_id: str, nonce: str,
    subject_data: bytes, window_data: bytes, plan_data: bytes, plan_start: str,
    intent_data: bytes, receipt_data: bytes, events_data: bytes,
    events_stat: os.stat_result, events_path: str, binary_data: bytes,
    binary_stat: os.stat_result, binary_path: str, source_data: bytes,
    controller_data: bytes,
) -> None:
    intent, intent_unsigned = parse(
        intent_data, DRIVER_INTENT_KEYS, hmac_key="intent_hmac_sha256",
    )
    receipt, receipt_unsigned = parse(
        receipt_data, DRIVER_RECEIPT_KEYS, hmac_key="receipt_hmac_sha256",
    )
    identity, _ = parse(subject_data, SUBJECT_KEYS)
    window, _ = parse(window_data, WINDOW_KEYS)
    events_parent = Path(events_path).parent.stat()
    intent_expected = {
        "format_version": "1", "campaign_id": campaign_id,
        "session_id": session_id, "nonce": nonce,
        "driver_output_path": events_path,
        "driver_output_parent_device": str(events_parent.st_dev),
        "driver_output_parent_inode": str(events_parent.st_ino),
        "driver_binary_path": binary_path,
        "driver_binary_device": str(binary_stat.st_dev),
        "driver_binary_inode": str(binary_stat.st_ino),
        "driver_binary_size": str(binary_stat.st_size),
        "driver_binary_sha256": digest(binary_data),
        "driver_source_sha256": digest(source_data),
        "controller_sha256": digest(controller_data),
        "scenario_plan_sha256": digest(plan_data),
        "plan_start_continuous_ns": plan_start,
        "subject_identity_sha256": digest(subject_data),
        "subject_process_pid": identity["process_pid"],
        "subject_process_start_identity": identity["process_start_identity"],
        "window_identity_sha256": digest(window_data),
        "window_number": window["window_number"], "auth_algorithm": "hmac-sha256",
    }
    if any(intent[key] != value for key, value in intent_expected.items()) \
            or window["subject_identity_sha256"] != digest(subject_data) \
            or window["process_pid"] != identity["process_pid"] \
            or window["process_start_identity"] != identity["process_start_identity"] \
            or window["window_owner_pid_verified"] != "true" \
            or window["window_layer"] != "0" or window["window_onscreen"] != "true" \
            or window["window_minimized"] != "false" or window["status"] != "frozen":
        raise Invalid("driver-intent-binding")
    expected_intent_hmac = hmac.new(
        secret, b"spaceterm.performance.driver-intent/v1\0"
        + struct.pack(">Q", len(intent_unsigned)) + intent_unsigned, hashlib.sha256,
    ).hexdigest()
    if not hmac.compare_digest(intent["intent_hmac_sha256"], expected_intent_hmac):
        raise Invalid("driver-intent-authentication")
    plan = driver_plan(plan_data)
    if not events_data.endswith(b"\n") or b"\0" in events_data or b"\r" in events_data:
        raise Invalid("driver-events-encoding")
    lines = events_data[:-1].split(b"\n")
    if len(lines) != len(plan) + 1 or lines[0] != DRIVER_EVENT_HEADER:
        raise Invalid("driver-events-schema")
    first = last = 0
    terminal: list[str] = []
    plan_start_ns = unsigned_integer(plan_start, positive=True)
    for sequence, (raw, expected) in enumerate(zip(lines[1:], plan)):
        fields = raw.split(b"\t")
        if len(fields) != 11:
            raise Invalid("driver-event-width")
        try:
            decoded = [field.decode("utf-8") for field in fields]
        except UnicodeDecodeError as error:
            raise Invalid("driver-event-text") from error
        timestamp = unsigned_integer(decoded[1], positive=True)
        event_id, offset_ms, action = expected
        tolerance = 2_000_000_000 if sequence == 0 else 250_000_000
        deadline = plan_start_ns + offset_ms * 1_000_000
        if decoded[0] != str(sequence) or decoded[2] != event_id \
                or decoded[3] != action or decoded[4] != identity["process_pid"] \
                or decoded[5] != window["window_number"] or decoded[10] != "verified" \
                or timestamp < deadline or timestamp > deadline + tolerance \
                or (sequence > 0 and timestamp <= last):
            raise Invalid("driver-event-binding")
        if sequence == 0:
            first = timestamp
        last = timestamp; terminal = decoded
    receipt_expected = {
        "format_version": "1", "campaign_id": campaign_id,
        "session_id": session_id, "nonce": nonce, "intent_sha256": digest(intent_data),
        "driver_output_device": str(events_stat.st_dev),
        "driver_output_inode": str(events_stat.st_ino),
        "driver_output_size": str(events_stat.st_size),
        "driver_events_sha256": digest(events_data), "event_row_count": str(len(plan)),
        "first_continuous_ns": str(first), "last_continuous_ns": str(last),
        "terminal_event_id": terminal[2], "terminal_action": terminal[3],
        "terminal_result": terminal[10], "auth_algorithm": "hmac-sha256",
    }
    if terminal[3] != "stop" or any(receipt[key] != value
                                      for key, value in receipt_expected.items()):
        raise Invalid("driver-receipt-binding")
    authenticated = (
        b"spaceterm.performance.driver-events/v1\0"
        + struct.pack(">Q", len(intent_data)) + intent_data
        + struct.pack(">Q", len(events_data)) + events_data
        + struct.pack(">Q", len(receipt_unsigned)) + receipt_unsigned
    )
    expected_receipt_hmac = hmac.new(secret, authenticated, hashlib.sha256).hexdigest()
    if not hmac.compare_digest(receipt["receipt_hmac_sha256"], expected_receipt_hmac):
        raise Invalid("driver-receipt-authentication")


def verify_run(
    args: argparse.Namespace, secret: bytes, pair: dict[str, str], subject: str,
    plan_data: bytes, expected_helper_hash: str,
    inspector_data: bytes, terminator_source_data: bytes,
    terminator_binary_data: bytes, helper_stat: os.stat_result,
    inspector_stat: os.stat_result, terminator_source_stat: os.stat_result,
    terminator_binary_stat: os.stat_result,
) -> dict[str, str]:
    prefix = subject.replace("-", "_")
    helper_data = stable_read(
        getattr(args, f"{prefix}_lifecycle_helper"), 4 * 1024 * 1024, mutable=True,
    )
    if digest(helper_data) != expected_helper_hash:
        raise Invalid(f"{subject}-lifecycle-helper-code-mismatch")
    intent_data = stable_read(getattr(args, f"{prefix}_run_intent"))
    subject_data = stable_read(getattr(args, f"{prefix}_subject_identity"))
    run_data = stable_read(getattr(args, f"{prefix}_run_metadata"))
    window_data = stable_read(getattr(args, f"{prefix}_window_identity"))
    driver_intent_data = stable_read(
        getattr(args, f"{prefix}_driver_intent"), private=True
    )
    driver_events_data, driver_events_stat, driver_events_path = stable_read_material(
        getattr(args, f"{prefix}_driver_events"), 64 * 1024 * 1024, private=True
    )
    driver_receipt_data = stable_read(
        getattr(args, f"{prefix}_driver_receipt"), private=True
    )
    driver_binary_data, driver_binary_stat, driver_binary_path = stable_read_material(
        getattr(args, f"{prefix}_driver_binary"), 512 * 1024 * 1024
    )
    driver_source_data = stable_read(
        getattr(args, f"{prefix}_driver_source"), 4 * 1024 * 1024, mutable=True
    )
    driver_controller_data = stable_read(
        getattr(args, f"{prefix}_driver_controller"), 4 * 1024 * 1024, mutable=True
    )
    gate_data = stable_read(getattr(args, f"{prefix}_plan_start_gate"), private=True)
    trace_data = stable_read(
        getattr(args, f"{prefix}_trace_provisional_receipt"), private=True
    )
    workload_metadata_data = stable_read(
        getattr(args, f"{prefix}_workload_metadata"), private=True
    )
    workload_events_data = stable_read(
        getattr(args, f"{prefix}_workload_events"), 64 * 1024 * 1024, private=True
    )
    workload_ready_data = stable_read(
        getattr(args, f"{prefix}_workload_ready_receipt"), private=True
    )
    lifecycle_ready_data = stable_read(
        getattr(args, f"{prefix}_lifecycle_ready_receipt"), private=True
    )
    lifecycle_registration_data = stable_read(
        getattr(args, f"{prefix}_lifecycle_registration"), private=True
    )
    tail_data = stable_read(getattr(args, f"{prefix}_tail_receipt"), private=True)
    quit_data = stable_read(getattr(args, f"{prefix}_quit_receipt"), private=True)
    exit_data = stable_read(getattr(args, f"{prefix}_exit_receipt"), private=True)
    case_report_data = stable_read(getattr(args, f"{prefix}_case_report"), private=True)
    final_trace_data = stable_read(getattr(args, f"{prefix}_trace_metadata"), private=True)
    manual_data = stable_read(getattr(args, f"{prefix}_manual_artifacts"), private=True)
    trace_archive_hash = stable_trace_tree(getattr(args, f"{prefix}_trace_archive"))
    screenshot_hash = stable_file_digest(getattr(args, f"{prefix}_manual_screenshot"))
    video_hash = stable_file_digest(getattr(args, f"{prefix}_manual_video"))
    intent, _ = parse(intent_data, INTENT_KEYS)
    identity, _ = parse(subject_data, SUBJECT_KEYS)
    run, _ = parse(run_data, RUN_KEYS)
    trace, trace_unsigned = parse(
        trace_data, TRACE_KEYS, hmac_key="provisional_hmac_sha256"
    )
    lifecycle_ready, lifecycle_ready_unsigned = parse(
        lifecycle_ready_data, READY_KEYS, hmac_key="receipt_hmac_sha256"
    )
    lifecycle_registration, lifecycle_registration_unsigned = parse(
        lifecycle_registration_data, REGISTRATION_KEYS,
        hmac_key="registration_hmac_sha256",
    )
    tail, tail_unsigned = parse(tail_data, TAIL_KEYS, hmac_key="tail_hmac_sha256")
    quit_receipt, _ = parse(quit_data, QUIT_KEYS)
    exit_receipt, exit_unsigned = parse(
        exit_data, EXIT_KEYS, hmac_key="receipt_hmac_sha256"
    )
    case_report, _ = parse(case_report_data, CASE_REPORT_KEYS)
    final_trace, _ = parse(final_trace_data, FINAL_TRACE_KEYS)
    manual, _ = parse(manual_data, MANUAL_KEYS)
    gate, gate_unsigned = parse(gate_data, GATE_KEYS, hmac_key="start_gate_hmac_sha256")
    intent_hash = digest(intent_data)
    identity_key = f"{subject}_subject_identity_sha256"
    shared = {
        "scenario": "scenario", "scenario_plan_sha256": "plan_sha256",
        "workload_sha256": "workload_sha256", "command_sha256": "command_sha256",
        "environment_sha256": "environment_sha256", "font_sha256": "font_sha256",
        "initial_grid_sha256": "initial_grid_sha256",
        "measured_duration_ms": "duration_ms",
    }
    if case_report["format_version"] != "2" or case_report["subject"] != subject \
            or case_report["scenario"] != pair["scenario"] \
            or case_report["session_id"] != intent["session_id"] \
            or case_report["nonce"] != intent["nonce"] \
            or case_report["run_intent_sha256"] != intent_hash \
            or case_report["run_metadata_sha256"] != digest(run_data) \
            or case_report["trace_metadata_sha256"] != digest(final_trace_data) \
            or case_report["trace_archive_sha256"] != trace_archive_hash \
            or case_report["manual_artifacts_sha256"] != digest(manual_data) \
            or case_report["manual_screenshot_sha256"] != screenshot_hash \
            or case_report["manual_video_sha256"] != video_hash \
            or case_report["result"] != "CASE-COMPLETE" \
            or case_report["reason"] != "all-required-evidence-complete":
        raise Invalid(f"{subject}-case-report-binding")
    if final_trace["format_version"] != "3" or final_trace["capture_status"] != "CAPTURED" \
            or final_trace["incomplete_reason"] != "none" \
            or final_trace["subject_identity_sha256"] != digest(subject_data) \
            or final_trace["run_metadata_sha256"] != digest(run_data) \
            or final_trace["workload_metadata_sha256"] != digest(workload_metadata_data) \
            or final_trace["workload_ready_receipt_sha256"] != digest(workload_ready_data) \
            or final_trace["supplemental_evidence_sha256"] != digest(gate_data) \
            or final_trace["status"] != "complete" \
            or final_trace["requested_duration_ms"] != pair["duration_ms"] \
            or any(final_trace[key] != "true" for key in (
                "target_identity_verified", "trace_target_pid_verified",
                "time_profiler_instrument", "allocations_instrument", "hangs_instrument",
                "time_profiler_target_verified", "allocations_target_verified",
                "hangs_target_verified",
            )) \
            or any(re.fullmatch(r"0|[1-9][0-9]*", final_trace[key]) is None for key in (
                "requested_duration_ms", "actual_duration_ms",
                "capture_started_continuous_ns", "capture_ended_continuous_ns",
                "time_profiler_rows", "allocations_rows", "hangs_rows",
            )) \
            or re.fullmatch(r"[0-9]+(?:\.[0-9]+)?",
                            final_trace["maximum_main_thread_hang_ms"]) is None \
            or float(final_trace["maximum_main_thread_hang_ms"]) > 250 \
            or int(final_trace["actual_duration_ms"]) < int(pair["duration_ms"]) \
            or int(final_trace["actual_duration_ms"]) > int(pair["duration_ms"]) + 3250 \
            or int(final_trace["capture_ended_continuous_ns"]) \
                <= int(final_trace["capture_started_continuous_ns"]):
        raise Invalid(f"{subject}-final-trace-binding")
    if manual["format_version"] != "1" or manual["screenshot_sha256"] != screenshot_hash \
            or manual["video_sha256"] != video_hash or manual["result"] != "PASS" \
            or any(manual[key] != "PASS" for key in (
                "final_content_review", "anchor_review", "restoration_review", "geometry_review",
            )):
        raise Invalid(f"{subject}-manual-binding")
    if intent["format_version"] != "1" or intent["status"] != "prepared" \
            or intent["evidence_mode"] != "production" \
            or intent["subject"] != subject or intent["campaign_id"] != args.campaign_id \
            or intent["subject_identity_sha256"] != pair[identity_key] \
            or intent["subject_identity_sha256"] != digest(subject_data) \
            or identity["format_version"] != "1" or identity["subject"] != subject \
            or identity["signature_valid"] != "true" \
            or identity["identity_status"] != "frozen" \
            or identity["process_pid"] != intent["process_pid"] \
            or identity["process_start_identity"] != intent["process_start_identity"] \
            or HEX.fullmatch(identity["executable_sha256"]) is None \
            or any(POSITIVE.fullmatch(identity[key]) is None for key in (
                "executable_device", "executable_inode", "executable_fsid",
            )) \
            or not identity["app_bundle_path"].startswith("/") \
            or not identity["executable_path"].startswith("/") \
            or SAFE.fullmatch(intent["session_id"]) is None \
            or HEX.fullmatch(intent["nonce"]) is None \
            or POSITIVE.fullmatch(intent["process_pid"]) is None \
            or START.fullmatch(intent["process_start_identity"]) is None:
        raise Invalid(f"{subject}-intent-binding")
    if any(intent[key] != pair[pair_key] for key, pair_key in shared.items()):
        raise Invalid(f"{subject}-pair-input-binding")
    tool_values = {
        "lifecycle_helper_device": str(helper_stat.st_dev),
        "lifecycle_helper_inode": str(helper_stat.st_ino),
        "lifecycle_helper_sha256": digest(helper_data),
        "process_inspector_device": str(inspector_stat.st_dev),
        "process_inspector_inode": str(inspector_stat.st_ino),
        "process_inspector_sha256": digest(inspector_data),
        "appkit_terminator_source_device": str(terminator_source_stat.st_dev),
        "appkit_terminator_source_inode": str(terminator_source_stat.st_ino),
        "appkit_terminator_source_sha256": digest(terminator_source_data),
        "appkit_terminator_binary_device": str(terminator_binary_stat.st_dev),
        "appkit_terminator_binary_inode": str(terminator_binary_stat.st_ino),
        "appkit_terminator_binary_sha256": digest(terminator_binary_data),
    }
    bridge_values = {
        "appkit_terminator_process_pid": lifecycle_ready["appkit_terminator_process_pid"],
        "appkit_terminator_process_start_identity":
            lifecycle_ready["appkit_terminator_process_start_identity"],
    }
    tool_values.update(bridge_values)
    if lifecycle_ready["schema"] \
            != "spaceterm.acceptance.performance-lifecycle-ready/v1" \
            or lifecycle_ready["subject"] != subject \
            or lifecycle_ready["campaign_id"] != args.campaign_id \
            or lifecycle_ready["session_id"] != intent["session_id"] \
            or lifecycle_ready["nonce"] != intent["nonce"] \
            or lifecycle_ready["subject_identity_sha256"] != digest(subject_data) \
            or lifecycle_ready["process_pid"] != intent["process_pid"] \
            or lifecycle_ready["process_start_identity"] \
                != intent["process_start_identity"] \
            or lifecycle_ready["executable_sha256"] != identity["executable_sha256"] \
            or POSITIVE.fullmatch(lifecycle_ready["ready_continuous_ns"]) is None \
            or any(POSITIVE.fullmatch(lifecycle_ready[key]) is None for key in (
                "registration_control_device", "registration_control_inode"
            )) \
            or any(lifecycle_ready[key] != value for key, value in tool_values.items()) \
            or lifecycle_ready["evidence_mode"] != "production" \
            or lifecycle_ready["auth_algorithm"] != "hmac-sha256" \
            or lifecycle_ready["status"] != "ready":
        raise Invalid(f"{subject}-lifecycle-ready-binding")
    expected_lifecycle_ready = hmac.new(
        secret, READY_MAGIC + struct.pack(">Q", len(lifecycle_ready_unsigned))
        + lifecycle_ready_unsigned, hashlib.sha256,
    ).hexdigest()
    if not hmac.compare_digest(
        lifecycle_ready["receipt_hmac_sha256"], expected_lifecycle_ready
    ):
        raise Invalid(f"{subject}-lifecycle-ready-authentication")
    registration_paths = {
        "run_intent_path": getattr(args, f"{prefix}_run_intent"),
        "tail_receipt_path": getattr(args, f"{prefix}_tail_receipt"),
        "workload_metadata_path": getattr(args, f"{prefix}_workload_metadata"),
        "workload_events_path": getattr(args, f"{prefix}_workload_events"),
        "workload_ready_receipt_path": getattr(
            args, f"{prefix}_workload_ready_receipt"
        ),
        "quit_receipt_path": getattr(args, f"{prefix}_quit_receipt"),
        "subject_exit_receipt_path": getattr(args, f"{prefix}_exit_receipt"),
    }
    if lifecycle_registration["format_version"] != "1" \
            or lifecycle_registration["campaign_id"] != args.campaign_id \
            or lifecycle_registration["session_id"] != intent["session_id"] \
            or lifecycle_registration["nonce"] != intent["nonce"] \
            or HEX.fullmatch(lifecycle_registration["registration_token"]) is None \
            or lifecycle_registration["subject_identity_sha256"] != digest(subject_data) \
            or lifecycle_registration["process_pid"] != intent["process_pid"] \
            or lifecycle_registration["process_start_identity"] \
                != intent["process_start_identity"] \
            or lifecycle_registration["run_intent_sha256"] != intent_hash \
            or any(Path(lifecycle_registration[key]).resolve(strict=True)
                   != Path(path).resolve(strict=True)
                   for key, path in registration_paths.items()) \
            or any(lifecycle_registration[key] != value
                   for key, value in tool_values.items()) \
            or lifecycle_registration["evidence_mode"] != "production" \
            or lifecycle_registration["auth_algorithm"] != "hmac-sha256" \
            or lifecycle_registration["status"] != "registered":
        raise Invalid(f"{subject}-lifecycle-registration-binding")
    expected_registration = hmac.new(
        secret, REGISTRATION_MAGIC
        + struct.pack(">Q", len(lifecycle_registration_unsigned))
        + lifecycle_registration_unsigned, hashlib.sha256,
    ).hexdigest()
    if not hmac.compare_digest(
        lifecycle_registration["registration_hmac_sha256"], expected_registration
    ):
        raise Invalid(f"{subject}-lifecycle-registration-authentication")
    expected_native_path = (
        str(Path(args.spaceterm_native_observation).resolve(strict=True))
        if subject == "spaceterm" else "not-applicable"
    )
    if lifecycle_registration["native_observation_path"] != expected_native_path:
        raise Invalid(f"{subject}-lifecycle-native-path-binding")
    canonical_directory = Path(__file__).resolve(strict=True).parent
    canonical_source = canonical_directory / "performance-driver.m"
    canonical_controller = canonical_directory / "run-native-performance-scenario.sh"
    if Path(getattr(args, f"{prefix}_driver_source")).resolve(strict=True) != canonical_source \
            or Path(getattr(args, f"{prefix}_driver_controller")).resolve(strict=True) \
            != canonical_controller:
        raise Invalid(f"{subject}-noncanonical-driver-toolchain")
    if gate["format_version"] != "1" or gate["campaign_id"] != args.campaign_id \
            or gate["session_id"] != intent["session_id"] \
            or gate["nonce"] != intent["nonce"] \
            or HEX.fullmatch(gate["ready_receipt_sha256"]) is None \
            or POSITIVE.fullmatch(gate["plan_start_continuous_ns"]) is None \
            or HEX.fullmatch(gate["start_gate_hmac_sha256"]) is None:
        raise Invalid(f"{subject}-plan-start-gate-binding")
    expected_gate = hmac.new(
        secret,
        b"spaceterm.performance.plan-start-gate/v1\0"
        + struct.pack(">Q", len(gate_unsigned)) + gate_unsigned,
        hashlib.sha256,
    ).hexdigest()
    if not hmac.compare_digest(gate["start_gate_hmac_sha256"], expected_gate):
        raise Invalid(f"{subject}-plan-start-gate-authentication")
    trace_hash_fields = (
        "workload_metadata_sha256", "workload_ready_receipt_sha256",
        "supplemental_evidence_sha256", "trace_bundle_tree_sha256", "toc_sha256",
        "time_profile_export_sha256", "allocations_export_sha256",
        "hangs_export_sha256", "trace_verification_sha256", "verifier_sha256",
        "provisional_hmac_sha256",
    )
    if trace["format_version"] != "1" or trace["capture_status"] != "CAPTURED" \
            or trace["status"] != "complete" or trace["auth_algorithm"] != "hmac-sha256" \
            or trace["subject_identity_sha256"] != digest(subject_data) \
            or trace["run_intent_sha256"] != intent_hash \
            or trace["workload_metadata_sha256"] != digest(workload_metadata_data) \
            or trace["workload_ready_receipt_sha256"] != digest(workload_ready_data) \
            or trace["supplemental_evidence_sha256"] != digest(gate_data) \
            or trace["requested_duration_ms"] != pair["duration_ms"] \
            or trace["evidence_mode"] != "production" \
            or any(HEX.fullmatch(trace[key]) is None for key in trace_hash_fields) \
            or any(POSITIVE.fullmatch(trace[key]) is None for key in (
                "actual_duration_ms", "capture_started_continuous_ns",
                "capture_ended_continuous_ns",
            )) \
            or int(trace["capture_ended_continuous_ns"]) \
                <= int(trace["capture_started_continuous_ns"]) \
            or int(trace["actual_duration_ms"]) < int(pair["duration_ms"]) \
            or int(trace["actual_duration_ms"]) > int(pair["duration_ms"]) + 2000:
        raise Invalid(f"{subject}-trace-provisional-binding")
    if trace["trace_bundle_tree_sha256"] != trace_archive_hash:
        raise Invalid(f"{subject}-trace-archive-binding")
    expected_trace = hmac.new(
        secret,
        b"spaceterm.performance.trace-provisional/v1\0"
        + struct.pack(">Q", len(trace_unsigned)) + trace_unsigned,
        hashlib.sha256,
    ).hexdigest()
    if not hmac.compare_digest(trace["provisional_hmac_sha256"], expected_trace):
        raise Invalid(f"{subject}-trace-provisional-authentication")
    verify_driver_snapshot(
        secret=secret, campaign_id=args.campaign_id, session_id=intent["session_id"],
        nonce=intent["nonce"], subject_data=subject_data, window_data=window_data,
        plan_data=plan_data, plan_start=gate["plan_start_continuous_ns"],
        intent_data=driver_intent_data, receipt_data=driver_receipt_data,
        events_data=driver_events_data, events_stat=driver_events_stat,
        events_path=driver_events_path, binary_data=driver_binary_data,
        binary_stat=driver_binary_stat, binary_path=driver_binary_path,
        source_data=driver_source_data, controller_data=driver_controller_data,
    )
    common = (
        "subject", "subject_identity_sha256", "scenario", "scenario_plan_sha256",
        "workload_sha256", "command_sha256", "environment_sha256", "font_sha256",
        "initial_grid_sha256", "measured_duration_ms", "process_pid",
        "process_start_identity",
    )
    if run["format_version"] != "4" or run["status"] != "complete" \
            or run["evidence_mode"] != "production" \
            or run["run_intent_sha256"] != intent_hash \
            or any(run[key] != intent[key] for key in common) \
            or run["trace_provisional_receipt_sha256"] != digest(trace_data) \
            or run["performance_tail_receipt_sha256"] != digest(tail_data) \
            or run["performance_quit_receipt_sha256"] != digest(quit_data) \
            or run["subject_exit_receipt_sha256"] != digest(exit_data) \
            or run["lifecycle_ready_receipt_sha256"] \
                != digest(lifecycle_ready_data) \
            or run["lifecycle_registration_receipt_sha256"] \
                != digest(lifecycle_registration_data) \
            or run["lifecycle_helper_sha256"] != digest(helper_data) \
            or run["terminator_source_sha256"] != digest(terminator_source_data) \
            or run["terminator_binary_sha256"] != digest(terminator_binary_data):
        raise Invalid(f"{subject}-final-run-binding")
    native_keys = (
        "native_observation_sha256", "native_runtime_metadata_sha256",
        "native_failure_actions_sha256", "native_failure_action_enabled",
        "native_failure_request_count", "native_failure_result_count",
        "native_failure_resource_staged_count", "native_failure_resource_staged_bytes",
        "native_failure_resource_rolled_back_count",
        "native_failure_resource_rolled_back_bytes",
    )
    if subject == "spaceterm":
        native_provisional_data = stable_read(
            args.spaceterm_native_provisional_observation, private=True
        )
        native_observation_data = stable_read(
            args.spaceterm_native_observation, private=True
        )
        native_metadata_data = stable_read(
            args.spaceterm_native_runtime_metadata, private=True
        )
        native_samples_data = stable_read(
            args.spaceterm_native_runtime_samples, 64 * 1024 * 1024, private=True
        )
        native_events_data = stable_read(
            args.spaceterm_native_runtime_events, 64 * 1024 * 1024, private=True
        )
        native_failures_data = stable_read(
            args.spaceterm_native_failure_actions, 64 * 1024 * 1024, private=True
        )
        if intent["native_provisional_observation_sha256"] \
                != digest(native_provisional_data) \
                or run["native_observation_sha256"] != digest(native_observation_data) \
                or run["native_runtime_metadata_sha256"] != digest(native_metadata_data) \
                or run["native_failure_actions_sha256"] != digest(native_failures_data) \
                or run["native_failure_action_enabled"] != "false" \
                or any(run[key] != "0" for key in native_keys[4:]) \
                or exit_receipt["native_observation_sha256"] \
                    != digest(native_observation_data):
            raise Invalid("spaceterm-native-final-binding")
        native_verifier = canonical_directory / "verify-performance-native-closure.py"
        snapshot_material = {
            "subject.tsv": subject_data,
            "provisional.tsv": native_provisional_data,
            "native-observation.tsv": native_observation_data,
            "runtime-metadata.tsv": native_metadata_data,
            "runtime-samples.tsv": native_samples_data,
            "runtime-events.tsv": native_events_data,
            "failure-actions.tsv": native_failures_data,
        }
        with tempfile.TemporaryDirectory(prefix="spaceterm-native-replay-") as temporary:
            snapshot_root = Path(temporary); snapshot_root.chmod(0o700)
            for name, data in snapshot_material.items():
                snapshot = snapshot_root / name
                descriptor = os.open(
                    snapshot, os.O_WRONLY | os.O_CREAT | os.O_EXCL
                    | getattr(os, "O_NOFOLLOW", 0), 0o400,
                )
                try:
                    offset = 0
                    while offset < len(data):
                        written = os.write(descriptor, data[offset:])
                        if written <= 0:
                            raise OSError("snapshot-short-write")
                        offset += written
                    os.fchmod(descriptor, 0o400); os.fsync(descriptor)
                finally:
                    os.close(descriptor)
            native_command = [
                sys.executable, str(native_verifier),
                "--subject-identity", str(snapshot_root / "subject.tsv"),
                "--provisional-observation", str(snapshot_root / "provisional.tsv"),
                "--native-observation", str(snapshot_root / "native-observation.tsv"),
                "--runtime-metadata", str(snapshot_root / "runtime-metadata.tsv"),
                "--runtime-samples", str(snapshot_root / "runtime-samples.tsv"),
                "--runtime-events", str(snapshot_root / "runtime-events.tsv"),
                "--failure-actions", str(snapshot_root / "failure-actions.tsv"),
            ]
            completed = subprocess.run(
                native_command, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL, close_fds=True, check=False,
            )
        if completed.returncode != 0:
            raise Invalid("spaceterm-native-closure-invalid")
        native_postflight = (
            (args.spaceterm_subject_identity, subject_data, 64 * 1024, False),
            (args.spaceterm_native_provisional_observation, native_provisional_data,
             64 * 1024, True),
            (args.spaceterm_native_observation, native_observation_data, 64 * 1024, True),
            (args.spaceterm_native_runtime_metadata, native_metadata_data, 64 * 1024, True),
            (args.spaceterm_native_runtime_samples, native_samples_data,
             64 * 1024 * 1024, True),
            (args.spaceterm_native_runtime_events, native_events_data,
             64 * 1024 * 1024, True),
            (args.spaceterm_native_failure_actions, native_failures_data,
             64 * 1024 * 1024, True),
        )
        if any(stable_read(path, maximum, private=private) != expected
               for path, expected, maximum, private in native_postflight):
            raise Invalid("spaceterm-native-closure-changed-after-replay")
    elif intent["native_provisional_observation_sha256"] != "not-applicable" \
            or any(run[key] != "not-applicable" for key in native_keys) \
            or exit_receipt["native_observation_sha256"] != "not-applicable":
        raise Invalid("ghostty-native-closure-not-applicable")
    for receipt in (tail, quit_receipt, exit_receipt):
        if receipt["campaign_id"] != args.campaign_id \
                or receipt["session_id"] != intent["session_id"] \
                or receipt["nonce"] != intent["nonce"] \
                or receipt["run_intent_sha256"] != intent_hash:
            raise Invalid(f"{subject}-receipt-replay")
    if tail["format_version"] != "1" or tail["terminal_status"] != "tail-complete" \
            or tail["auth_algorithm"] != "hmac-sha256" \
            or tail["evidence_mode"] != "production" \
            or tail["subject_identity_sha256"] != intent["subject_identity_sha256"] \
            or tail["subject_process_pid"] != intent["process_pid"] \
            or tail["subject_process_start_identity"] != intent["process_start_identity"] \
            or tail["quit_token"] != lifecycle_registration["registration_token"] \
            or tail["driver_receipt_sha256"] != digest(driver_receipt_data) \
            or tail["driver_events_sha256"] != digest(driver_events_data) \
            or tail["trace_provisional_receipt_sha256"] != digest(trace_data) \
            or tail["workload_metadata_sha256"] != trace["workload_metadata_sha256"] \
            or tail["workload_events_sha256"] != digest(workload_events_data) \
            or any(tail[key] != value for key, value in tool_values.items()) \
            or HEX.fullmatch(tail["tail_hmac_sha256"]) is None:
        raise Invalid(f"{subject}-tail-binding")
    expected_tail = hmac.new(
        secret,
        b"spaceterm.performance.tail-complete/v1\0"
        + struct.pack(">Q", len(tail_unsigned)) + tail_unsigned,
        hashlib.sha256,
    ).hexdigest()
    if not hmac.compare_digest(tail["tail_hmac_sha256"], expected_tail):
        raise Invalid(f"{subject}-tail-authentication")
    if quit_receipt["format_version"] != "1" \
            or quit_receipt["subject_process_pid"] != intent["process_pid"] \
            or quit_receipt["subject_process_start_identity"] != intent["process_start_identity"] \
            or quit_receipt["quit_token"] != tail["quit_token"] \
            or quit_receipt["termination_method"] != "appkit-terminate" \
            or quit_receipt["runtime_closure_status"] != "confirmed" \
            or quit_receipt["evidence_mode"] != "production" \
            or any(quit_receipt[key] != value for key, value in tool_values.items()) \
            or quit_receipt["status"] != "completed":
        raise Invalid(f"{subject}-quit-binding")
    if exit_receipt["schema"] != "spaceterm.acceptance.performance-subject-exit/v1" \
            or exit_receipt["subject"] != subject \
            or exit_receipt["subject_identity_sha256"] != intent["subject_identity_sha256"] \
            or exit_receipt["process_pid"] != intent["process_pid"] \
            or exit_receipt["process_start_identity"] != intent["process_start_identity"] \
            or exit_receipt["tail_receipt_sha256"] != digest(tail_data) \
            or exit_receipt["quit_receipt_sha256"] != digest(quit_data) \
            or exit_receipt["exit_status"] != "normal" \
            or exit_receipt["evidence_mode"] != "production" \
            or any(exit_receipt[key] != value for key, value in tool_values.items()) \
            or exit_receipt["auth_algorithm"] != "hmac-sha256" \
            or exit_receipt["status"] != "complete" \
            or HEX.fullmatch(exit_receipt["receipt_hmac_sha256"]) is None:
        raise Invalid(f"{subject}-exit-binding")
    expected_exit = hmac.new(
        secret,
        b"spaceterm.acceptance.performance-subject-exit/v1\0"
        + struct.pack(">Q", len(exit_unsigned)) + exit_unsigned,
        hashlib.sha256,
    ).hexdigest()
    if not hmac.compare_digest(exit_receipt["receipt_hmac_sha256"], expected_exit):
        raise Invalid(f"{subject}-exit-authentication")
    times = (
        tail["tail_completed_continuous_ns"], quit_receipt["request_continuous_ns"],
        quit_receipt["exit_continuous_ns"], exit_receipt["exit_requested_continuous_ns"],
        exit_receipt["process_exited_continuous_ns"],
    )
    if any(POSITIVE.fullmatch(value) is None for value in times) \
            or times[1] != times[3] or times[2] != times[4] \
            or not int(times[2]) >= int(times[1]) >= int(times[0]):
        raise Invalid(f"{subject}-exit-timing")
    postflight = (
        (getattr(args, f"{prefix}_run_intent"), intent_data, 64 * 1024, False, False),
        (getattr(args, f"{prefix}_subject_identity"), subject_data, 64 * 1024, False, False),
        (getattr(args, f"{prefix}_run_metadata"), run_data, 64 * 1024, False, False),
        (getattr(args, f"{prefix}_window_identity"), window_data, 64 * 1024, False, False),
        (getattr(args, f"{prefix}_driver_intent"), driver_intent_data, 64 * 1024, True, False),
        (getattr(args, f"{prefix}_driver_events"), driver_events_data,
         64 * 1024 * 1024, True, False),
        (getattr(args, f"{prefix}_driver_receipt"), driver_receipt_data, 64 * 1024, True, False),
        (getattr(args, f"{prefix}_driver_binary"), driver_binary_data,
         512 * 1024 * 1024, False, False),
        (getattr(args, f"{prefix}_driver_source"), driver_source_data,
         4 * 1024 * 1024, False, True),
        (getattr(args, f"{prefix}_driver_controller"), driver_controller_data,
         4 * 1024 * 1024, False, True),
        (getattr(args, f"{prefix}_plan_start_gate"), gate_data, 64 * 1024, True, False),
        (getattr(args, f"{prefix}_trace_provisional_receipt"), trace_data,
         64 * 1024, True, False),
        (getattr(args, f"{prefix}_workload_metadata"), workload_metadata_data,
         64 * 1024, True, False),
        (getattr(args, f"{prefix}_workload_events"), workload_events_data,
         64 * 1024 * 1024, True, False),
        (getattr(args, f"{prefix}_workload_ready_receipt"), workload_ready_data,
         64 * 1024, True, False),
        (getattr(args, f"{prefix}_lifecycle_ready_receipt"), lifecycle_ready_data,
         64 * 1024, True, False),
        (getattr(args, f"{prefix}_lifecycle_registration"), lifecycle_registration_data,
         64 * 1024, True, False),
        (getattr(args, f"{prefix}_tail_receipt"), tail_data, 64 * 1024, True, False),
        (getattr(args, f"{prefix}_quit_receipt"), quit_data, 64 * 1024, True, False),
        (getattr(args, f"{prefix}_exit_receipt"), exit_data, 64 * 1024, True, False),
        (getattr(args, f"{prefix}_case_report"), case_report_data, 64 * 1024, True, False),
        (getattr(args, f"{prefix}_trace_metadata"), final_trace_data, 64 * 1024, True, False),
        (getattr(args, f"{prefix}_manual_artifacts"), manual_data, 64 * 1024, True, False),
    )
    if any(stable_read(path, maximum, private=private, mutable=mutable) != expected
           for path, expected, maximum, private, mutable in postflight) \
            or stable_trace_tree(getattr(args, f"{prefix}_trace_archive")) != trace_archive_hash \
            or stable_file_digest(getattr(args, f"{prefix}_manual_screenshot")) != screenshot_hash \
            or stable_file_digest(getattr(args, f"{prefix}_manual_video")) != video_hash:
        raise Invalid(f"{subject}-evidence-changed-after-replay")
    return {
        "session_id": intent["session_id"], "nonce": intent["nonce"],
        "run_intent_sha256": intent_hash, "run_metadata_sha256": digest(run_data),
        "driver_intent_sha256": digest(driver_intent_data),
        "driver_events_sha256": digest(driver_events_data),
        "driver_receipt_sha256": digest(driver_receipt_data),
        "window_identity_sha256": digest(window_data),
        "driver_binary_sha256": digest(driver_binary_data),
        "driver_source_sha256": digest(driver_source_data),
        "driver_controller_sha256": digest(driver_controller_data),
        "plan_start_gate_sha256": digest(gate_data),
        "tail_receipt_sha256": digest(tail_data), "quit_receipt_sha256": digest(quit_data),
        "exit_receipt_sha256": digest(exit_data),
        "case_report_sha256": digest(case_report_data),
        "trace_metadata_sha256": digest(final_trace_data),
        "trace_archive_sha256": trace_archive_hash,
        "manual_artifacts_sha256": digest(manual_data),
        "manual_screenshot_sha256": screenshot_hash,
        "manual_video_sha256": video_hash,
        "lifecycle_ready_receipt_sha256": digest(lifecycle_ready_data),
        "lifecycle_registration_receipt_sha256": digest(lifecycle_registration_data),
    }


def build(args: argparse.Namespace) -> bytes:
    if SAFE.fullmatch(args.campaign_id) is None:
        raise Invalid("invalid-campaign")
    secret = stable_read(args.campaign_secret_file, 4096, private=True, secret=True)
    pair_data = stable_read(args.pair_metadata)
    plan_data = stable_read(args.scenario_plan, 4 * 1024 * 1024)
    helper_data = stable_read(args.common_lifecycle_helper, 4 * 1024 * 1024, mutable=True)
    inspector_data = stable_read(args.process_inspector, 4 * 1024 * 1024, mutable=True)
    terminator_source_data = stable_read(
        args.appkit_terminator_source, 4 * 1024 * 1024, mutable=True
    )
    terminator_binary_data = stable_read(
        args.appkit_terminator_binary, 512 * 1024 * 1024
    )
    inspector_stat = Path(args.process_inspector).lstat()
    canonical_directory = Path(__file__).resolve(strict=True).parent
    if Path(args.common_lifecycle_helper).resolve(strict=True) \
            != canonical_directory / "performance-subject-lifecycle.py" \
            or Path(args.appkit_terminator_source).resolve(strict=True) \
            != canonical_directory / "performance-appkit-terminate.m":
        raise Invalid("noncanonical-lifecycle-toolchain")
    terminator_source_stat = Path(args.appkit_terminator_source).lstat()
    terminator_binary_stat = Path(args.appkit_terminator_binary).lstat()
    pair, _ = parse(pair_data, PAIR_KEYS)
    if pair["format_version"] != "1" or SAFE.fullmatch(pair["pair_id"]) is None \
            or pair["scenario"] not in (
                "ascii", "unicode-styles", "scrolled", "hidden-occluded", "resize"
            ) or POSITIVE.fullmatch(pair["duration_ms"]) is None \
            or any(HEX.fullmatch(pair[key]) is None for key in PAIR_KEYS[3:9]) \
            or any(HEX.fullmatch(pair[key]) is None for key in PAIR_KEYS[10:12]):
        raise Invalid("pair-metadata-binding")
    if digest(plan_data) != pair["plan_sha256"]:
        raise Invalid("scenario-plan-binding")
    spaceterm = verify_run(
        args, secret, pair, "spaceterm", plan_data, digest(helper_data), inspector_data,
        terminator_source_data, terminator_binary_data,
        Path(args.spaceterm_lifecycle_helper).lstat(), inspector_stat,
        terminator_source_stat, terminator_binary_stat,
    )
    ghostty = verify_run(
        args, secret, pair, "ghostty", plan_data, digest(helper_data), inspector_data,
        terminator_source_data, terminator_binary_data,
        Path(args.ghostty_lifecycle_helper).lstat(), inspector_stat,
        terminator_source_stat, terminator_binary_stat,
    )
    if spaceterm["session_id"] == ghostty["session_id"] \
            or spaceterm["nonce"] == ghostty["nonce"]:
        raise Invalid("paired-run-replay")
    values = {
        "format_version": "3", "campaign_id": args.campaign_id,
        "pair_metadata_sha256": digest(pair_data),
        "scenario_plan_sha256": pair["plan_sha256"],
        "workload_sha256": pair["workload_sha256"],
        "command_sha256": pair["command_sha256"],
        "environment_sha256": pair["environment_sha256"],
        "font_sha256": pair["font_sha256"],
        "initial_grid_sha256": pair["initial_grid_sha256"],
        "evidence_mode": "production", "status": "complete",
        "auth_algorithm": "hmac-sha256",
        "lifecycle_helper_sha256": digest(helper_data),
        "terminator_source_sha256": digest(terminator_source_data),
        "terminator_binary_sha256": digest(terminator_binary_data),
    }
    for subject, run in (("spaceterm", spaceterm), ("ghostty", ghostty)):
        for key, value in run.items():
            values[f"{subject}_{key}"] = value
    unsigned = b"".join(f"{key}\t{values[key]}\n".encode() for key in RESULT_KEYS[:-1])
    values["pair_result_hmac_sha256"] = hmac.new(
        secret, MAGIC + struct.pack(">Q", len(unsigned)) + unsigned, hashlib.sha256,
    ).hexdigest()
    return b"".join(f"{key}\t{values[key]}\n".encode() for key in RESULT_KEYS)


def publish(path_text: str, data: bytes) -> None:
    path = Path(path_text)
    if not path.is_absolute() or path.exists() or path.is_symlink():
        raise Invalid("output-exists-or-relative")
    parent = path.parent.resolve(strict=True)
    parent_stat = parent.stat()
    if not stat.S_ISDIR(parent_stat.st_mode) or parent_stat.st_uid != os.geteuid() \
            or parent_stat.st_mode & 0o022:
        raise Invalid("unsafe-output-parent")
    descriptor = os.open(
        path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), 0o400
    )
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


def main() -> int:
    args = arguments()
    try:
        expected = build(args)
        if args.command == "create":
            publish(args.output, expected)
        else:
            actual = stable_read(args.receipt, private=True)
            actual_values, _ = parse(actual, RESULT_KEYS)
            expected_values, _ = parse(expected, RESULT_KEYS)
            for key in RESULT_KEYS:
                if key == "pair_result_hmac_sha256":
                    if not hmac.compare_digest(actual_values[key], expected_values[key]):
                        raise Invalid("pair-result-authentication")
                elif actual_values[key] != expected_values[key]:
                    raise Invalid(f"pair-result-binding-{key}")
    except (Invalid, OSError, UnicodeError, ValueError) as error:
        print(f"performance pair result failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
