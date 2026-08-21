#!/usr/bin/python3
"""Freeze and verify authenticated render trace replay anchors and receipts."""

from __future__ import annotations

import argparse
import hashlib
import hmac
import os
import pathlib
import re
import stat
import struct
import subprocess
import sys
from collections.abc import Sequence


MANIFEST_DOMAIN = b"SPACETERM_RENDER_CAMPAIGN_CASE_MANIFEST_V1\0"
ANCHOR_DOMAIN = b"SPACETERM_RENDER_TRACE_ANCHOR_V1\0"
RECEIPT_DOMAIN = b"SPACETERM_RENDER_TRACE_RECEIPT_V1\0"
INTENT_DOMAIN = b"SPACETERM_RENDER_PROFILE_INTENT_V1\0"
EVIDENCE_DOMAIN = b"SPACETERM_RENDER_PROFILE_EVIDENCE_V1\0"
CANONICALIZATION = "utf8-lf-tab-kv-fixed-order-domain-nul-v1"
ZERO_HASH = "0" * 64
HASH = re.compile(r"[0-9a-f]{64}")
LABEL = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*")
START_IDENTITY = re.compile(r"([0-9]+):([0-9]+)")
COMMIT = re.compile(r"[0-9a-f]{40}")

MANIFEST_KEYS = (
    "format_version", "canonicalization", "auth_domain", "campaign_id",
    "session_id", "nonce", "scenario", "subject", "subject_identity_sha256",
    "render_intent_path", "render_evidence_path", "driver_intent_path",
    "driver_receipt_path", "trace_anchor_receipt_path", "trace_anchor_receipt_parent_device",
    "trace_anchor_receipt_parent_inode", "trace_receipt_path", "trace_receipt_parent_device",
    "trace_receipt_parent_inode", "campaign_secret_device", "campaign_secret_inode",
    "render_tool_bundle_manifest_path", "render_tool_bundle_manifest_device",
    "render_tool_bundle_manifest_inode", "render_tool_bundle_manifest_sha256",
    "render_tool_bundle_source_commit",
    "render_profile_hmac_path", "render_profile_hmac_device",
    "render_profile_hmac_inode", "render_profile_hmac_sha256",
    "render_trace_receipt_helper_path", "render_trace_receipt_helper_device",
    "render_trace_receipt_helper_inode", "render_trace_receipt_helper_sha256",
    "process_inspector_path", "process_inspector_device", "process_inspector_inode",
    "process_inspector_sha256", "trace_verifier_path", "trace_verifier_device",
    "trace_verifier_inode", "trace_verifier_sha256", "command_runner_path",
    "command_runner_device", "command_runner_inode", "command_runner_sha256",
    "hmac_key_identifier_sha256",
    "manifest_hmac_sha256",
)

ANCHOR_KEYS = (
    "format_version", "canonicalization", "auth_domain", "campaign_id", "session_id",
    "nonce", "scenario", "subject", "campaign_manifest_sha256",
    "subject_identity_sha256", "run_metadata_sha256", "render_intent_sha256",
    "render_evidence_sha256", "trace_metadata_sha256", "capture_started_continuous_ns",
    "capture_ended_continuous_ns", "trace_started_epoch_ns", "trace_ended_epoch_ns",
    "start_anchor_continuous_ns", "start_anchor_epoch_ns", "start_anchor_width_ns",
    "end_anchor_continuous_ns", "end_anchor_epoch_ns", "end_anchor_width_ns",
    "hmac_key_identifier_sha256", "result", "anchor_hmac_sha256",
)

RECEIPT_KEYS = (
    "format_version", "canonicalization", "auth_domain", "campaign_id",
    "session_id", "nonce", "scenario", "subject", "campaign_manifest_sha256",
    "trace_anchor_receipt_sha256",
    "subject_identity_sha256", "subject_process_pid", "subject_process_start_sec",
    "subject_process_start_usec", "subject_code_identity_token",
    "run_metadata_sha256", "render_intent_sha256", "render_evidence_sha256",
    "driver_intent_sha256", "driver_receipt_sha256", "evidence_mode",
    "driver_receipt_verifier_path", "driver_receipt_verifier_device",
    "driver_receipt_verifier_inode", "driver_receipt_verifier_sha256",
    "render_tool_bundle_manifest_path", "render_tool_bundle_manifest_device",
    "render_tool_bundle_manifest_inode", "render_tool_bundle_manifest_sha256",
    "render_tool_bundle_source_commit",
    "render_profile_hmac_path", "render_profile_hmac_device",
    "render_profile_hmac_inode", "render_profile_hmac_sha256",
    "render_trace_receipt_helper_path", "render_trace_receipt_helper_device",
    "render_trace_receipt_helper_inode", "render_trace_receipt_helper_sha256",
    "process_inspector_path", "process_inspector_device", "process_inspector_inode",
    "process_inspector_sha256", "command_runner_path", "command_runner_device",
    "command_runner_inode", "command_runner_sha256",
    "workload_metadata_sha256", "workload_events_sha256",
    "workload_ready_receipt_sha256", "trace_metadata_sha256",
    "trace_archive_sha256", "trace_toc_sha256", "time_profiler_artifact_sha256",
    "allocations_artifact_sha256", "hangs_artifact_sha256", "action_video_sha256",
    "representative_stack_screenshot_sha256",
    "trace_verification_sha256", "capture_started_continuous_ns",
    "capture_ended_continuous_ns", "trace_started_epoch_ns", "trace_ended_epoch_ns",
    "start_anchor_continuous_ns", "start_anchor_epoch_ns", "start_anchor_width_ns",
    "end_anchor_continuous_ns", "end_anchor_epoch_ns", "end_anchor_width_ns",
    "xcrun_path", "xcrun_device", "xcrun_inode", "xcrun_sha256",
    "sips_path", "sips_device", "sips_inode", "sips_sha256",
    "python_path", "python_device", "python_inode", "python_sha256",
    "ffprobe_path", "ffprobe_device", "ffprobe_inode", "ffprobe_sha256",
    "trace_verifier_path", "trace_verifier_device", "trace_verifier_inode",
    "trace_verifier_sha256", "trace_archive_verifier_path",
    "trace_archive_verifier_device", "trace_archive_verifier_inode",
    "trace_archive_verifier_sha256", "action_video_verifier_path",
    "action_video_verifier_device", "action_video_verifier_inode",
    "action_video_verifier_sha256", "render_trace_receipt_verifier_path",
    "render_trace_receipt_verifier_device", "render_trace_receipt_verifier_inode",
    "render_trace_receipt_verifier_sha256", "hmac_key_identifier_sha256", "result",
    "receipt_hmac_sha256",
)

INTENT_KEYS = (
    "format_version", "canonicalization", "auth_domain", "scenario", "subject",
    "campaign_id", "session_id", "nonce", "plan_sha256", "plan_metadata_sha256",
    "pair_metadata_sha256", "run_intent_sha256", "command_sha256", "environment_sha256",
    "font_sha256", "initial_grid_sha256", "subject_identity_sha256", "subject_process_pid",
    "subject_process_start_identity", "expected_driver_events_path",
    "expected_driver_parent_device", "expected_driver_parent_inode", "action_video_path",
    "action_video_parent_device", "action_video_parent_inode", "final_metadata_path",
    "final_metadata_parent_device", "final_metadata_parent_inode", "warmup_ms",
    "measured_duration_ms", "required_action_count", "action_interval_ms",
    "hmac_key_identifier_sha256", "intent_hmac_sha256",
)

EVIDENCE_KEYS = (
    "format_version", "canonicalization", "auth_domain", "intent_sha256", "scenario",
    "subject", "campaign_id", "session_id", "nonce", "subject_identity_sha256",
    "subject_process_pid", "subject_process_start_identity", "driver_events_path",
    "driver_events_device", "driver_events_inode", "driver_events_sha256",
    "action_video_path", "action_video_device", "action_video_inode", "action_video_sha256",
    "render_workload_metadata_sha256", "required_action_count", "completed_action_count",
    "action_interval_ms", "started_continuous_ns", "ended_continuous_ns", "measured_span_ns",
    "result", "hmac_key_identifier_sha256", "evidence_hmac_sha256",
)

RUN_METADATA_KEYS = (
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


class InvalidReceipt(Exception):
    pass


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(1)


def safe_value(value: str, label: str) -> str:
    if not value or any(character in value for character in "\t\r\n"):
        raise InvalidReceipt(f"{label} is empty or contains a control character")
    return value


def sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def immutable_file(raw: str, label: str, *, executable: bool = False,
                   require_immutable: bool = True,
                   allow_sealed_system_links: bool = False) -> pathlib.Path:
    safe_value(raw, label)
    requested = pathlib.Path(raw)
    if not requested.is_absolute() or requested.is_symlink():
        raise InvalidReceipt(f"{label} must be an absolute, non-symbolic path")
    resolved = requested.resolve(strict=True)
    if str(resolved) != raw:
        raise InvalidReceipt(f"{label} must be a canonical physical path")
    before = resolved.stat()
    sealed_system_tool = (
        allow_sealed_system_links
        and str(resolved).startswith(("/usr/bin/", "/bin/", "/usr/sbin/", "/sbin/"))
        and before.st_uid == 0
        and before.st_nlink >= 1
        and not before.st_mode & 0o022
    )
    if (not stat.S_ISREG(before.st_mode) or before.st_size <= 0
            or (before.st_nlink != 1 and not sealed_system_tool)):
        raise InvalidReceipt(f"{label} must be a nonempty singleton regular file")
    if require_immutable and before.st_mode & 0o222 and not sealed_system_tool:
        raise InvalidReceipt(f"{label} must be immutable")
    if executable and not before.st_mode & 0o111:
        raise InvalidReceipt(f"{label} must be executable")
    digest = sha256(resolved)
    after = resolved.stat()
    identity = (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, digest)
    final_identity = (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, sha256(resolved))
    if identity != final_identity:
        raise InvalidReceipt(f"{label} changed while it was read")
    return resolved


def pending_path(raw: str, label: str) -> tuple[pathlib.Path, os.stat_result]:
    safe_value(raw, label)
    requested = pathlib.Path(raw)
    if not requested.is_absolute() or requested.name in ("", ".", ".."):
        raise InvalidReceipt(f"{label} must be an absolute file path")
    parent = requested.parent.resolve(strict=True)
    canonical = parent / requested.name
    if str(canonical) != raw or requested.exists() or requested.is_symlink():
        raise InvalidReceipt(f"{label} must be an absent canonical physical path")
    parent_stat = parent.stat()
    if not stat.S_ISDIR(parent_stat.st_mode) or parent.is_symlink():
        raise InvalidReceipt(f"{label} parent must be a physical directory")
    return canonical, parent_stat


def exact_kv(path: pathlib.Path, keys: Sequence[str]) -> tuple[dict[str, str], bytes]:
    raw = path.read_bytes()
    if not raw.endswith(b"\n") or b"\r" in raw or b"\0" in raw:
        raise InvalidReceipt(f"{path.name} is not canonical LF-terminated text")
    lines = raw[:-1].split(b"\n")
    if len(lines) != len(keys):
        raise InvalidReceipt(f"{path.name} has an invalid schema")
    values: dict[str, str] = {}
    for expected, line in zip(keys, lines):
        try:
            key_raw, value_raw = line.split(b"\t", 1)
            key = key_raw.decode("ascii")
            value = value_raw.decode("utf-8")
        except (ValueError, UnicodeDecodeError) as error:
            raise InvalidReceipt(f"{path.name} has invalid encoding") from error
        if key != expected or not value or "\t" in value:
            raise InvalidReceipt(f"{path.name} has an invalid ordered schema")
        values[key] = value
    return values, raw


def unique_kv(path: pathlib.Path, required: Sequence[str]) -> dict[str, str]:
    raw = path.read_bytes()
    if not raw.endswith(b"\n") or b"\r" in raw or b"\0" in raw:
        raise InvalidReceipt(f"{path.name} is not canonical LF-terminated text")
    values: dict[str, str] = {}
    for line in raw[:-1].split(b"\n"):
        try:
            key_raw, value_raw = line.split(b"\t", 1)
            key = key_raw.decode("ascii")
            value = value_raw.decode("utf-8")
        except (ValueError, UnicodeDecodeError) as error:
            raise InvalidReceipt(f"{path.name} has invalid encoding") from error
        if not key or not value or key in values or b"\t" in value_raw:
            raise InvalidReceipt(f"{path.name} has invalid key-value rows")
        values[key] = value
    if any(not values.get(key) for key in required):
        raise InvalidReceipt(f"{path.name} is missing required fields")
    return values


def secret_key(path_raw: str) -> tuple[bytes, str, os.stat_result]:
    path = pathlib.Path(path_raw)
    if not path.is_absolute() or path.is_symlink():
        raise InvalidReceipt("campaign secret must be an absolute non-symbolic path")
    before = path.lstat()
    if (not stat.S_ISREG(before.st_mode) or before.st_uid != os.getuid()
            or before.st_nlink != 1 or before.st_mode & 0o277 or not before.st_mode & 0o400
            or before.st_size != 65):
        raise InvalidReceipt("campaign secret must be owner-private and immutable")
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        if (opened.st_dev, opened.st_ino, opened.st_mode, opened.st_nlink,
                opened.st_uid, opened.st_size, opened.st_mtime_ns, opened.st_ctime_ns) != (
                before.st_dev, before.st_ino, before.st_mode, before.st_nlink,
                before.st_uid, before.st_size, before.st_mtime_ns, before.st_ctime_ns):
            raise InvalidReceipt("campaign secret identity changed while it was opened")
        raw = b""
        while True:
            block = os.read(descriptor, 4096)
            if not block:
                break
            raw += block
    finally:
        os.close(descriptor)
    if not re.fullmatch(rb"[0-9a-f]{64}\n", raw):
        raise InvalidReceipt("campaign secret format is invalid")
    after = path.lstat()
    if (before.st_dev, before.st_ino, before.st_mode, before.st_nlink,
            before.st_uid, before.st_mtime_ns, before.st_ctime_ns, before.st_size) != (
        after.st_dev, after.st_ino, after.st_mode, after.st_nlink,
        after.st_uid, after.st_mtime_ns, after.st_ctime_ns, after.st_size
    ):
        raise InvalidReceipt("campaign secret changed while it was read")
    key_hex = raw[:-1]
    return bytes.fromhex(key_hex.decode("ascii")), hashlib.sha256(key_hex).hexdigest(), before


def hmac_hex(key: bytes, domain: bytes, unsigned: bytes) -> str:
    return hmac.new(key, domain + unsigned, hashlib.sha256).hexdigest()


def encode(values: dict[str, str], keys: Sequence[str], hmac_key: str, signature: str) -> bytes:
    body_keys = keys[:-1]
    values[hmac_key] = signature
    return b"".join(f"{key}\t{safe_value(values[key], key)}\n".encode() for key in keys)


def unsigned(values: dict[str, str], keys: Sequence[str]) -> bytes:
    return b"".join(f"{key}\t{safe_value(values[key], key)}\n".encode() for key in keys[:-1])


def publish(output_raw: str, payload: bytes) -> None:
    output, parent_stat = pending_path(output_raw, "output")
    temporary = output.parent / f".{output.name}.{os.getpid()}.tmp"
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o400)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(payload)
            stream.flush()
            os.fsync(stream.fileno())
        if (output.parent.stat().st_dev, output.parent.stat().st_ino) != (
            parent_stat.st_dev, parent_stat.st_ino
        ):
            raise InvalidReceipt("output parent identity changed")
        os.link(temporary, output)
    finally:
        temporary.unlink(missing_ok=True)


def manifest_values(arguments: argparse.Namespace, key_id: str,
                    secret_stat: os.stat_result) -> dict[str, str]:
    if not LABEL.fullmatch(arguments.campaign_id) or not LABEL.fullmatch(arguments.session_id):
        raise InvalidReceipt("campaign and session identifiers are invalid")
    if not HASH.fullmatch(arguments.nonce):
        raise InvalidReceipt("nonce must be lowercase SHA-256 text")
    if arguments.subject not in ("spaceterm", "ghostty"):
        raise InvalidReceipt("subject is invalid")
    identity = immutable_file(arguments.subject_identity, "subject identity")
    expected_paths = {}
    for name in ("render_intent", "render_evidence", "driver_intent", "driver_receipt",
                 "trace_anchor_receipt", "trace_receipt"):
        expected_paths[name], _ = pending_path(getattr(arguments, name), name.replace("_", " "))
    _, anchor_parent = pending_path(arguments.trace_anchor_receipt, "trace anchor receipt")
    _, receipt_parent = pending_path(arguments.trace_receipt, "trace receipt")
    bundle_manifest, bundle = tool_bundle_values(arguments)
    bundle_stat = bundle_manifest.stat()
    values = {
        "format_version": "1", "canonicalization": CANONICALIZATION,
        "auth_domain": MANIFEST_DOMAIN[:-1].decode(), "campaign_id": arguments.campaign_id,
        "session_id": arguments.session_id, "nonce": arguments.nonce,
        "scenario": safe_value(arguments.scenario, "scenario"), "subject": arguments.subject,
        "subject_identity_sha256": sha256(identity),
        "render_intent_path": str(expected_paths["render_intent"]),
        "render_evidence_path": str(expected_paths["render_evidence"]),
        "driver_intent_path": str(expected_paths["driver_intent"]),
        "driver_receipt_path": str(expected_paths["driver_receipt"]),
        "trace_anchor_receipt_path": str(expected_paths["trace_anchor_receipt"]),
        "trace_anchor_receipt_parent_device": str(anchor_parent.st_dev),
        "trace_anchor_receipt_parent_inode": str(anchor_parent.st_ino),
        "trace_receipt_path": str(expected_paths["trace_receipt"]),
        "trace_receipt_parent_device": str(receipt_parent.st_dev),
        "trace_receipt_parent_inode": str(receipt_parent.st_ino),
        "campaign_secret_device": str(secret_stat.st_dev),
        "campaign_secret_inode": str(secret_stat.st_ino),
        "render_tool_bundle_manifest_path": str(bundle_manifest),
        "render_tool_bundle_manifest_device": str(bundle_stat.st_dev),
        "render_tool_bundle_manifest_inode": str(bundle_stat.st_ino),
        "render_tool_bundle_manifest_sha256": sha256(bundle_manifest),
        "render_tool_bundle_source_commit": bundle["source_commit"],
        "hmac_key_identifier_sha256": key_id,
    }
    for prefix, path in recorder_tools(arguments).items():
        file_binding(values, prefix, path)
    return values


def verify_authenticated(path: pathlib.Path, keys: Sequence[str], domain: bytes,
                         signature_key: str, key: bytes, key_id: str) -> dict[str, str]:
    values, _ = exact_kv(path, keys)
    if values["canonicalization"] != CANONICALIZATION or values["auth_domain"] != domain[:-1].decode():
        raise InvalidReceipt(f"{path.name} authentication domain is invalid")
    if values["hmac_key_identifier_sha256"] != key_id:
        raise InvalidReceipt(f"{path.name} key identifier does not match")
    expected = hmac_hex(key, domain, unsigned(values, keys))
    if not hmac.compare_digest(values[signature_key], expected):
        raise InvalidReceipt(f"{path.name} authentication failed")
    return values


def verify_case_tuple(
    manifest: dict[str, str],
    intent_path: pathlib.Path,
    key: bytes,
    key_id: str,
    evidence_path: pathlib.Path | None = None,
) -> tuple[dict[str, str], dict[str, str] | None]:
    intent = verify_authenticated(
        intent_path, INTENT_KEYS, INTENT_DOMAIN, "intent_hmac_sha256", key, key_id
    )
    if (
        str(intent_path) != manifest["render_intent_path"]
        or intent["format_version"] != "1"
        or intent["subject_identity_sha256"] != manifest["subject_identity_sha256"]
        or intent["final_metadata_path"] != manifest["render_evidence_path"]
    ):
        raise InvalidReceipt("render intent does not match campaign manifest")
    for field in ("campaign_id", "session_id", "nonce", "scenario", "subject"):
        if intent[field] != manifest[field]:
            raise InvalidReceipt(f"render intent {field} does not match campaign manifest")
    if evidence_path is None:
        return intent, None
    evidence = verify_authenticated(
        evidence_path,
        EVIDENCE_KEYS,
        EVIDENCE_DOMAIN,
        "evidence_hmac_sha256",
        key,
        key_id,
    )
    if (
        str(evidence_path) != manifest["render_evidence_path"]
        or evidence["format_version"] != "1"
        or evidence["intent_sha256"] != sha256(intent_path)
        or evidence["subject_identity_sha256"] != manifest["subject_identity_sha256"]
    ):
        raise InvalidReceipt("render evidence does not match campaign manifest and intent")
    for field in ("campaign_id", "session_id", "nonce", "scenario", "subject"):
        if evidence[field] != manifest[field] or evidence[field] != intent[field]:
            raise InvalidReceipt(f"render evidence {field} does not match campaign tuple")
    return intent, evidence


def verify_run_metadata(
    path: pathlib.Path, intent: dict[str, str], subject_identity_sha256: str
) -> dict[str, str]:
    values, _ = exact_kv(path, RUN_METADATA_KEYS)
    expected = {
        "format_version": "4", "subject": intent["subject"],
        "subject_identity_sha256": subject_identity_sha256,
        "scenario": intent["scenario"], "scenario_plan_sha256": intent["plan_sha256"],
        "command_sha256": intent["command_sha256"],
        "environment_sha256": intent["environment_sha256"],
        "font_sha256": intent["font_sha256"],
        "initial_grid_sha256": intent["initial_grid_sha256"],
        "measured_duration_ms": intent["measured_duration_ms"],
        "process_pid": intent["subject_process_pid"],
        "process_start_identity": intent["subject_process_start_identity"],
        "run_intent_sha256": intent["run_intent_sha256"],
        "evidence_mode": "production", "status": "complete",
    }
    if any(values[key] != value for key, value in expected.items()):
        raise InvalidReceipt("run metadata is not exact35 production evidence for the render intent")
    for key in (
        "workload_sha256", "command_sha256", "environment_sha256", "font_sha256",
        "initial_grid_sha256", "trace_provisional_receipt_sha256",
        "performance_tail_receipt_sha256", "performance_quit_receipt_sha256",
        "subject_exit_receipt_sha256", "lifecycle_ready_receipt_sha256",
        "lifecycle_registration_receipt_sha256", "lifecycle_helper_sha256",
        "terminator_source_sha256", "terminator_binary_sha256",
    ):
        if not HASH.fullmatch(values[key]):
            raise InvalidReceipt(f"run metadata {key} is invalid")
    if values["subject"] == "spaceterm":
        for key in (
            "native_observation_sha256", "native_runtime_metadata_sha256",
            "native_failure_actions_sha256",
        ):
            if not HASH.fullmatch(values[key]):
                raise InvalidReceipt(f"run metadata {key} is invalid")
        if values["native_failure_action_enabled"] != "false" or any(
            values[key] != "0" for key in (
                "native_failure_request_count", "native_failure_result_count",
                "native_failure_resource_staged_count", "native_failure_resource_staged_bytes",
                "native_failure_resource_rolled_back_count",
                "native_failure_resource_rolled_back_bytes",
            )
        ):
            raise InvalidReceipt("run metadata failure closure is invalid")
    elif any(
        values[key] != "not-applicable" for key in (
            "native_observation_sha256", "native_runtime_metadata_sha256",
            "native_failure_actions_sha256", "native_failure_action_enabled",
            "native_failure_request_count", "native_failure_result_count",
            "native_failure_resource_staged_count", "native_failure_resource_staged_bytes",
            "native_failure_resource_rolled_back_count",
            "native_failure_resource_rolled_back_bytes",
        )
    ):
        raise InvalidReceipt("Ghostty run metadata contains SpaceTerm native evidence")
    return values


def anchor_values(arguments: argparse.Namespace, manifest: dict[str, str], key_id: str,
                  key: bytes) -> dict[str, str]:
    require_manifest_tool_bundle(arguments, manifest)
    require_manifest_recorder_tools(manifest, recorder_tools(arguments))
    artifacts = {
        name: immutable_file(getattr(arguments, name), name.replace("_", " "))
        for name in ("subject_identity", "run_metadata", "render_intent", "render_evidence",
                     "trace_metadata")
    }
    metadata, _ = exact_kv(artifacts["trace_metadata"], (
        "format_version", "capture_status", "incomplete_reason", "subject_identity_sha256",
        "run_metadata_sha256", "workload_metadata_sha256", "workload_ready_receipt_sha256",
        "supplemental_evidence_sha256", "requested_duration_ms", "actual_duration_ms",
        "capture_started_continuous_ns", "capture_ended_continuous_ns", "target_identity_verified",
        "trace_target_pid_verified", "time_profiler_instrument", "allocations_instrument",
        "hangs_instrument", "time_profiler_target_verified", "allocations_target_verified",
        "hangs_target_verified", "time_profiler_rows", "allocations_rows", "hangs_rows",
        "maximum_main_thread_hang_ms", "status"))
    intent, _ = verify_case_tuple(
        manifest, artifacts["render_intent"], key, key_id, artifacts["render_evidence"]
    )
    verify_run_metadata(
        artifacts["run_metadata"], intent, sha256(artifacts["subject_identity"])
    )
    values = {
        "format_version": "1", "canonicalization": CANONICALIZATION,
        "auth_domain": ANCHOR_DOMAIN[:-1].decode(), "campaign_id": manifest["campaign_id"],
        "session_id": manifest["session_id"], "nonce": manifest["nonce"],
        "scenario": manifest["scenario"], "subject": manifest["subject"],
        "campaign_manifest_sha256": sha256(pathlib.Path(arguments.manifest)),
        "subject_identity_sha256": sha256(artifacts["subject_identity"]),
        "run_metadata_sha256": sha256(artifacts["run_metadata"]),
        "render_intent_sha256": sha256(artifacts["render_intent"]),
        "render_evidence_sha256": sha256(artifacts["render_evidence"]),
        "trace_metadata_sha256": sha256(artifacts["trace_metadata"]),
        "capture_started_continuous_ns": metadata["capture_started_continuous_ns"],
        "capture_ended_continuous_ns": metadata["capture_ended_continuous_ns"],
        "trace_started_epoch_ns": arguments.trace_started_epoch_ns,
        "trace_ended_epoch_ns": arguments.trace_ended_epoch_ns,
        "start_anchor_continuous_ns": arguments.start_anchor_continuous_ns,
        "start_anchor_epoch_ns": arguments.start_anchor_epoch_ns,
        "start_anchor_width_ns": arguments.start_anchor_width_ns,
        "end_anchor_continuous_ns": arguments.end_anchor_continuous_ns,
        "end_anchor_epoch_ns": arguments.end_anchor_epoch_ns,
        "end_anchor_width_ns": arguments.end_anchor_width_ns,
        "hmac_key_identifier_sha256": key_id, "result": "PASS",
    }
    validate_anchor_values(values)
    if (values["subject_identity_sha256"] != manifest["subject_identity_sha256"]
            or str(artifacts["render_intent"]) != manifest["render_intent_path"]
            or str(artifacts["render_evidence"]) != manifest["render_evidence_path"]):
        raise InvalidReceipt("anchor artifacts do not match campaign manifest")
    return values


def validate_anchor_values(values: dict[str, str]) -> None:
    for key in ANCHOR_KEYS[:-1]:
        if key.endswith("_sha256") and not HASH.fullmatch(values[key]):
            raise InvalidReceipt(f"{key} is invalid")
    numeric = {key: parse_uint(values[key], key) for key in ANCHOR_KEYS
               if key.endswith("_ns")}
    if values["result"] != "PASS":
        raise InvalidReceipt("anchor receipt is not a PASS")
    if numeric["capture_ended_continuous_ns"] <= numeric["capture_started_continuous_ns"]:
        raise InvalidReceipt("anchor continuous capture interval is invalid")
    if numeric["trace_ended_epoch_ns"] <= numeric["trace_started_epoch_ns"]:
        raise InvalidReceipt("anchor epoch trace interval is invalid")
    if numeric["start_anchor_width_ns"] > 10_000_000 or numeric["end_anchor_width_ns"] > 10_000_000:
        raise InvalidReceipt("anchor measurement is too wide")
    start_offset = numeric["start_anchor_continuous_ns"] - numeric["start_anchor_epoch_ns"]
    end_offset = numeric["end_anchor_continuous_ns"] - numeric["end_anchor_epoch_ns"]
    if abs(start_offset - end_offset) > 50_000_000:
        raise InvalidReceipt("anchor offsets disagree")
    mapping = (start_offset + end_offset) // 2
    if (abs(numeric["trace_started_epoch_ns"] + mapping - numeric["capture_started_continuous_ns"]) > 50_000_000
            or abs(numeric["trace_ended_epoch_ns"] + mapping - numeric["capture_ended_continuous_ns"]) > 50_000_000):
        raise InvalidReceipt("anchor epoch and continuous intervals disagree")


def parse_uint(value: str, label: str) -> int:
    if not value.isascii() or not value.isdecimal():
        raise InvalidReceipt(f"{label} must be unsigned decimal")
    return int(value)


def file_binding(values: dict[str, str], prefix: str, path: pathlib.Path) -> None:
    details = path.stat()
    values[f"{prefix}_path"] = str(path)
    values[f"{prefix}_device"] = str(details.st_dev)
    values[f"{prefix}_inode"] = str(details.st_ino)
    values[f"{prefix}_sha256"] = sha256(path)


RECORDER_TOOL_ARGUMENTS = (
    "render_profile_hmac", "render_trace_receipt_helper", "process_inspector",
    "trace_verifier", "command_runner",
)


def recorder_tools(arguments: argparse.Namespace) -> dict[str, pathlib.Path]:
    return {
        name: immutable_file(
            getattr(arguments, name), name.replace("_", " "), executable=True,
        )
        for name in RECORDER_TOOL_ARGUMENTS
    }


def require_manifest_recorder_tools(
    manifest: dict[str, str], tools: dict[str, pathlib.Path]
) -> None:
    current: dict[str, str] = {}
    for prefix, path in tools.items():
        file_binding(current, prefix, path)
        for suffix in ("path", "device", "inode", "sha256"):
            key = f"{prefix}_{suffix}"
            if current[key] != manifest[key]:
                raise InvalidReceipt(f"{prefix.replace('_', ' ')} does not match campaign manifest")


def require_manifest_tool_bundle(arguments: argparse.Namespace, manifest: dict[str, str]) -> None:
    path, bundle = tool_bundle_values(arguments)
    details = path.stat()
    expected = {
        "render_tool_bundle_manifest_path": str(path),
        "render_tool_bundle_manifest_device": str(details.st_dev),
        "render_tool_bundle_manifest_inode": str(details.st_ino),
        "render_tool_bundle_manifest_sha256": sha256(path),
        "render_tool_bundle_source_commit": bundle["source_commit"],
    }
    if any(manifest[key] != value for key, value in expected.items()):
        raise InvalidReceipt("render tool bundle does not match campaign manifest")
    expected_tools = {
        "render_profile_hmac": "render_profile_hmac",
        "render_trace_receipt_helper": "render_trace_receipt",
        "process_inspector": "inspect_release_performance_process",
        "trace_verifier": "verify_release_performance_trace",
        "command_runner": "run_release_performance_command",
    }
    for argument_name, bundle_name in expected_tools.items():
        if getattr(arguments, argument_name) != bundle[f"{bundle_name}_bundle_path"]:
            raise InvalidReceipt(
                f"{argument_name.replace('_', ' ')} is not the frozen bundle tool"
            )


def tool_bundle_values(arguments: argparse.Namespace) -> tuple[pathlib.Path, dict[str, str]]:
    path = immutable_file(arguments.render_tool_bundle_manifest, "render tool bundle manifest")
    names = (
        "record_release_performance_trace", "freeze_render_profile_intent",
        "finalize_render_profile_evidence", "render_profile_hmac", "render_trace_receipt",
        "analyze_release_render_profile_case", "archive_render_trace",
        "verify_render_action_video", "verify_render_trace_archive",
        "verify_release_performance_trace", "inspect_release_performance_process",
        "run_release_performance_command",
        "freeze_render_profile_tool_bundle",
    )
    relative_paths = (
        "scripts/record-release-performance-trace.sh",
        "scripts/acceptance/freeze-render-profile-intent.sh",
        "scripts/acceptance/finalize-render-profile-evidence.sh",
        "scripts/acceptance/render-profile-hmac.py",
        "scripts/acceptance/render-trace-receipt.py",
        "scripts/acceptance/analyze-release-render-profile-case.sh",
        "scripts/acceptance/archive-render-trace.py",
        "scripts/acceptance/verify-render-action-video.py",
        "scripts/acceptance/verify-render-trace-archive.py",
        "scripts/verify-release-performance-trace.py",
        "scripts/inspect-release-performance-process.py",
        "scripts/run-release-performance-command.py",
        "scripts/acceptance/freeze-render-profile-tool-bundle.sh",
    )
    keys = ["format_version", "schema", "source_commit", "tool_count"]
    for name in names:
        keys.extend((f"{name}_source_path", f"{name}_source_sha256",
                     f"{name}_bundle_path", f"{name}_bundle_sha256"))
    values, _ = exact_kv(path, tuple(keys))
    if (values["format_version"] != "1"
            or values["schema"] != "spaceterm.render-profile-tool-bundle/v1"
            or values["tool_count"] != str(len(names))
            or not COMMIT.fullmatch(values["source_commit"])
            or values["source_commit"] != arguments.expected_source_commit):
        raise InvalidReceipt("render tool bundle manifest identity is invalid")
    repository = pathlib.Path(arguments.trusted_source_repository)
    if (not repository.is_absolute() or repository.is_symlink()
            or repository.resolve(strict=True) != repository or not repository.is_dir()):
        raise InvalidReceipt("trusted source repository is invalid")
    for name, relative in zip(names, relative_paths):
        source = pathlib.Path(values[f"{name}_source_path"])
        bundle = immutable_file(values[f"{name}_bundle_path"], name.replace("_", " "), executable=True)
        blob = subprocess.run(
            ["/usr/bin/git", "--no-replace-objects", "-C", str(repository), "show",
             f"{arguments.expected_source_commit}:{relative}"],
            check=False, capture_output=True,
            env={"PATH": "/usr/bin:/bin", "HOME": "/var/empty",
                 "GIT_NO_REPLACE_OBJECTS": "1", "LC_ALL": "C"},
        )
        blob_hash = hashlib.sha256(blob.stdout).hexdigest()
        if (blob.returncode != 0 or source != repository / relative
                or not HASH.fullmatch(values[f"{name}_source_sha256"])
                or values[f"{name}_source_sha256"] != blob_hash
                or values[f"{name}_source_sha256"] != values[f"{name}_bundle_sha256"]
                or sha256(bundle) != values[f"{name}_bundle_sha256"]):
            raise InvalidReceipt(f"{name.replace('_', ' ')} bundle binding is invalid")
    supplied_tools = {
        "render_profile_hmac": "render_profile_hmac",
        "render_trace_receipt_helper": "render_trace_receipt",
        "process_inspector": "inspect_release_performance_process",
        "trace_verifier": "verify_release_performance_trace",
        "command_runner": "run_release_performance_command",
    }
    for argument_name, bundle_name in supplied_tools.items():
        if (hasattr(arguments, argument_name)
                and getattr(arguments, argument_name) != values[f"{bundle_name}_bundle_path"]):
            raise InvalidReceipt(
                f"{argument_name.replace('_', ' ')} is not the frozen bundle tool"
            )
    if pathlib.Path(__file__).resolve(strict=True) != pathlib.Path(
        values["render_trace_receipt_bundle_path"]
    ):
        raise InvalidReceipt("render trace receipt helper is not executing from the frozen bundle")
    return path, values


def receipt_values(arguments: argparse.Namespace, manifest: dict[str, str], key_id: str,
                   key: bytes) -> dict[str, str]:
    require_manifest_tool_bundle(arguments, manifest)
    artifacts = {}
    artifact_arguments = (
        "subject_identity", "run_metadata", "render_intent", "render_evidence",
        "trace_anchor_receipt",
        "driver_intent", "driver_receipt", "trace_metadata", "trace_archive", "trace_toc",
        "time_profiler_artifact", "allocations_artifact", "hangs_artifact",
        "trace_verifier", "trace_verification", "xcrun", "sips", "python", "ffprobe",
        "trace_archive_verifier", "action_video_verifier", "render_trace_receipt_verifier",
        "driver_receipt_verifier", "render_profile_hmac", "render_trace_receipt_helper",
        "process_inspector", "command_runner",
        "action_video", "representative_stack_screenshot",
    )
    executable_arguments = {"xcrun", "sips", "python", "ffprobe", "trace_verifier",
                            "trace_archive_verifier", "action_video_verifier",
                            "render_trace_receipt_verifier", "render_profile_hmac",
                            "render_trace_receipt_helper", "process_inspector", "command_runner"}
    executable_arguments.add("driver_receipt_verifier")
    for name in artifact_arguments:
        artifacts[name] = immutable_file(
            getattr(arguments, name), name.replace("_", " "),
            executable=name in executable_arguments,
            allow_sealed_system_links=name in executable_arguments,
        )
    intent, _ = verify_case_tuple(
        manifest, artifacts["render_intent"], key, key_id, artifacts["render_evidence"]
    )
    verify_run_metadata(
        artifacts["run_metadata"], intent, sha256(artifacts["subject_identity"])
    )
    optional_hashes = {}
    workload_names = ("workload_metadata", "workload_events", "workload_ready_receipt")
    if arguments.evidence_mode == "sustained-output-v3":
        for name in workload_names:
            optional_hashes[name] = sha256(immutable_file(getattr(arguments, name), name.replace("_", " ")))
    elif arguments.evidence_mode == "zero-workload":
        if any(getattr(arguments, name) for name in workload_names):
            raise InvalidReceipt("zero-workload mode forbids workload-v3 artifacts")
        optional_hashes = {name: ZERO_HASH for name in workload_names}
    else:
        raise InvalidReceipt("evidence mode is invalid")
    identity, _ = exact_kv(artifacts["subject_identity"], (
        "format_version", "subject", "app_bundle_path", "bundle_identifier", "bundle_version",
        "executable_path", "executable_sha256", "executable_device", "executable_inode",
        "executable_fsid", "signature_valid", "signing_identifier", "team_identifier", "cdhash",
        "process_pid", "process_start_identity", "identity_status"))
    start_match = START_IDENTITY.fullmatch(identity["process_start_identity"])
    if start_match is None:
        raise InvalidReceipt("subject process start identity is malformed")
    code_token = ":".join((identity["process_pid"], start_match[1], start_match[2],
                            identity["executable_device"], identity["executable_inode"],
                            identity["signing_identifier"], identity["team_identifier"],
                            identity["cdhash"]))
    anchor = verify_authenticated(artifacts["trace_anchor_receipt"], ANCHOR_KEYS,
                                  ANCHOR_DOMAIN, "anchor_hmac_sha256", key, key_id)
    validate_anchor_values(anchor)
    verification = unique_kv(artifacts["trace_verification"], (
        "trace_started_epoch_ns", "trace_ended_epoch_ns", "actual_record_duration_seconds",
        "time_profiler_rows", "allocations_rows", "hangs_rows",
        "maximum_main_thread_hang_ms", "reason"))
    metadata, _ = exact_kv(artifacts["trace_metadata"], (
        "format_version", "capture_status", "incomplete_reason", "subject_identity_sha256",
        "run_metadata_sha256", "workload_metadata_sha256", "workload_ready_receipt_sha256",
        "supplemental_evidence_sha256", "requested_duration_ms", "actual_duration_ms",
        "capture_started_continuous_ns", "capture_ended_continuous_ns", "target_identity_verified",
        "trace_target_pid_verified", "time_profiler_instrument", "allocations_instrument",
        "hangs_instrument", "time_profiler_target_verified", "allocations_target_verified",
        "hangs_target_verified", "time_profiler_rows", "allocations_rows", "hangs_rows",
        "maximum_main_thread_hang_ms", "status"))
    values = {
        "format_version": "1", "canonicalization": CANONICALIZATION,
        "auth_domain": RECEIPT_DOMAIN[:-1].decode(), "campaign_id": manifest["campaign_id"],
        "session_id": manifest["session_id"], "nonce": manifest["nonce"],
        "scenario": manifest["scenario"], "subject": manifest["subject"],
        "campaign_manifest_sha256": sha256(pathlib.Path(arguments.manifest)),
        "trace_anchor_receipt_sha256": sha256(artifacts["trace_anchor_receipt"]),
        "subject_identity_sha256": sha256(artifacts["subject_identity"]),
        "subject_process_pid": identity["process_pid"],
        "subject_process_start_sec": start_match[1], "subject_process_start_usec": start_match[2],
        "subject_code_identity_token": code_token,
        "run_metadata_sha256": sha256(artifacts["run_metadata"]),
        "render_intent_sha256": sha256(artifacts["render_intent"]),
        "render_evidence_sha256": sha256(artifacts["render_evidence"]),
        "driver_intent_sha256": sha256(artifacts["driver_intent"]),
        "driver_receipt_sha256": sha256(artifacts["driver_receipt"]),
        "evidence_mode": arguments.evidence_mode,
        "render_tool_bundle_manifest_path": manifest["render_tool_bundle_manifest_path"],
        "render_tool_bundle_manifest_device": manifest["render_tool_bundle_manifest_device"],
        "render_tool_bundle_manifest_inode": manifest["render_tool_bundle_manifest_inode"],
        "render_tool_bundle_manifest_sha256": manifest["render_tool_bundle_manifest_sha256"],
        "render_tool_bundle_source_commit": manifest["render_tool_bundle_source_commit"],
        "workload_metadata_sha256": optional_hashes["workload_metadata"],
        "workload_events_sha256": optional_hashes["workload_events"],
        "workload_ready_receipt_sha256": optional_hashes["workload_ready_receipt"],
        "trace_metadata_sha256": sha256(artifacts["trace_metadata"]),
        "trace_archive_sha256": sha256(artifacts["trace_archive"]),
        "trace_toc_sha256": sha256(artifacts["trace_toc"]),
        "time_profiler_artifact_sha256": sha256(artifacts["time_profiler_artifact"]),
        "allocations_artifact_sha256": sha256(artifacts["allocations_artifact"]),
        "hangs_artifact_sha256": sha256(artifacts["hangs_artifact"]),
        "action_video_sha256": sha256(artifacts["action_video"]),
        "representative_stack_screenshot_sha256": sha256(
            artifacts["representative_stack_screenshot"]),
        "trace_verification_sha256": sha256(artifacts["trace_verification"]),
        "capture_started_continuous_ns": anchor["capture_started_continuous_ns"],
        "capture_ended_continuous_ns": anchor["capture_ended_continuous_ns"],
        "trace_started_epoch_ns": anchor["trace_started_epoch_ns"],
        "trace_ended_epoch_ns": anchor["trace_ended_epoch_ns"],
        "start_anchor_continuous_ns": anchor["start_anchor_continuous_ns"],
        "start_anchor_epoch_ns": anchor["start_anchor_epoch_ns"],
        "start_anchor_width_ns": anchor["start_anchor_width_ns"],
        "end_anchor_continuous_ns": anchor["end_anchor_continuous_ns"],
        "end_anchor_epoch_ns": anchor["end_anchor_epoch_ns"],
        "end_anchor_width_ns": anchor["end_anchor_width_ns"],
        "hmac_key_identifier_sha256": key_id, "result": "PASS",
    }
    if values["subject_identity_sha256"] != manifest["subject_identity_sha256"]:
        raise InvalidReceipt("subject identity does not match campaign manifest")
    if (str(artifacts["trace_anchor_receipt"]) != manifest["trace_anchor_receipt_path"]
            or anchor["campaign_manifest_sha256"] != values["campaign_manifest_sha256"]
            or anchor["subject_identity_sha256"] != values["subject_identity_sha256"]
            or anchor["run_metadata_sha256"] != values["run_metadata_sha256"]
            or anchor["render_intent_sha256"] != values["render_intent_sha256"]
            or anchor["render_evidence_sha256"] != values["render_evidence_sha256"]
            or anchor["trace_metadata_sha256"] != values["trace_metadata_sha256"]
            or anchor["trace_started_epoch_ns"] != verification["trace_started_epoch_ns"]
            or anchor["trace_ended_epoch_ns"] != verification["trace_ended_epoch_ns"]):
        raise InvalidReceipt("trace anchor receipt does not bind current trace")
    if (str(artifacts["render_intent"]) != manifest["render_intent_path"]
            or str(artifacts["render_evidence"]) != manifest["render_evidence_path"]
            or str(artifacts["driver_intent"]) != manifest["driver_intent_path"]
            or str(artifacts["driver_receipt"]) != manifest["driver_receipt_path"]):
        raise InvalidReceipt("case artifact path does not match campaign manifest")
    require_manifest_recorder_tools(
        manifest, {name: artifacts[name] for name in RECORDER_TOOL_ARGUMENTS}
    )
    for prefix in ("xcrun", "sips", "python", "ffprobe", "trace_verifier",
                   "trace_archive_verifier", "action_video_verifier",
                   "render_trace_receipt_verifier", "driver_receipt_verifier",
                   "render_profile_hmac", "render_trace_receipt_helper",
                   "process_inspector", "command_runner"):
        file_binding(values, prefix, artifacts[prefix])
    validate_receipt_values(values, metadata, verification)
    return values


def validate_receipt_values(values: dict[str, str], metadata: dict[str, str] | None = None,
                            verification: dict[str, str] | None = None) -> None:
    for key in RECEIPT_KEYS[:-1]:
        if key.endswith("_sha256") and not HASH.fullmatch(values[key]):
            raise InvalidReceipt(f"{key} is invalid")
    numeric_keys = [key for key in RECEIPT_KEYS if key.endswith(("_ns", "_device", "_inode"))]
    numeric_keys += ["subject_process_pid", "subject_process_start_sec", "subject_process_start_usec"]
    numbers = {key: parse_uint(values[key], key) for key in numeric_keys}
    if numbers["subject_process_pid"] <= 0 or numbers["subject_process_start_usec"] > 999_999:
        raise InvalidReceipt("subject process identity is invalid")
    if values["result"] != "PASS":
        raise InvalidReceipt("trace receipt is not a PASS")
    if numbers["capture_ended_continuous_ns"] <= numbers["capture_started_continuous_ns"]:
        raise InvalidReceipt("continuous capture interval is invalid")
    if numbers["trace_ended_epoch_ns"] <= numbers["trace_started_epoch_ns"]:
        raise InvalidReceipt("epoch capture interval is invalid")
    if numbers["start_anchor_width_ns"] > 10_000_000 or numbers["end_anchor_width_ns"] > 10_000_000:
        raise InvalidReceipt("clock anchor is too wide")
    start_offset = numbers["start_anchor_continuous_ns"] - numbers["start_anchor_epoch_ns"]
    end_offset = numbers["end_anchor_continuous_ns"] - numbers["end_anchor_epoch_ns"]
    if abs(start_offset - end_offset) > 50_000_000:
        raise InvalidReceipt("clock anchors disagree")
    mapping = (start_offset + end_offset) // 2
    if (abs(numbers["trace_started_epoch_ns"] + mapping - numbers["capture_started_continuous_ns"]) > 50_000_000
            or abs(numbers["trace_ended_epoch_ns"] + mapping - numbers["capture_ended_continuous_ns"]) > 50_000_000):
        raise InvalidReceipt("trace epoch and continuous intervals disagree")
    if not (numbers["start_anchor_continuous_ns"] <= numbers["capture_started_continuous_ns"]
            and numbers["end_anchor_continuous_ns"] >= numbers["capture_ended_continuous_ns"]):
        raise InvalidReceipt("clock anchors do not bracket capture")
    if metadata is not None:
        if (metadata["capture_status"] != "CAPTURED" or metadata["status"] != "complete"
                or metadata["subject_identity_sha256"] != values["subject_identity_sha256"]
                or metadata["run_metadata_sha256"] != values["run_metadata_sha256"]
                or metadata["supplemental_evidence_sha256"] != values["render_evidence_sha256"]):
            raise InvalidReceipt("trace metadata is not complete or bound")
        expected_workload = values["workload_metadata_sha256"]
        expected_ready = values["workload_ready_receipt_sha256"]
        if (metadata["workload_metadata_sha256"] != expected_workload
                or metadata["workload_ready_receipt_sha256"] != expected_ready):
            raise InvalidReceipt("trace workload mode does not match receipt")
    if verification is not None and verification["reason"] != "none":
        raise InvalidReceipt("trace verification is not a PASS")


def common_artifact_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--manifest", required=True)
    parser.add_argument("--render-tool-bundle-manifest", required=True)
    parser.add_argument("--expected-source-commit", required=True)
    parser.add_argument("--trusted-source-repository", required=True)
    parser.add_argument("--trace-anchor-receipt", required=True)
    parser.add_argument("--campaign-secret-file", required=True)
    for name in ("subject-identity", "run-metadata", "render-intent", "render-evidence",
                 "driver-intent", "driver-receipt", "trace-metadata", "trace-archive",
                 "trace-toc", "time-profiler-artifact", "allocations-artifact", "hangs-artifact",
                 "trace-verifier", "trace-verification", "xcrun", "sips", "python", "ffprobe",
                 "trace-archive-verifier", "action-video-verifier",
                 "render-trace-receipt-verifier", "action-video",
                 "representative-stack-screenshot", "driver-receipt-verifier",
                 "render-profile-hmac", "render-trace-receipt-helper",
                 "process-inspector", "command-runner"):
        parser.add_argument(f"--{name}", required=True)
    parser.add_argument("--evidence-mode", choices=("zero-workload", "sustained-output-v3"), required=True)
    parser.add_argument("--workload-metadata")
    parser.add_argument("--workload-events")
    parser.add_argument("--workload-ready-receipt")


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser()
    commands = result.add_subparsers(dest="command", required=True)
    manifest = commands.add_parser("manifest")
    manifest.add_argument("--campaign-id", required=True)
    manifest.add_argument("--session-id", required=True)
    manifest.add_argument("--nonce", required=True)
    manifest.add_argument("--scenario", required=True)
    manifest.add_argument("--subject", required=True)
    manifest.add_argument("--subject-identity", required=True)
    manifest.add_argument("--render-tool-bundle-manifest", required=True)
    manifest.add_argument("--expected-source-commit", required=True)
    manifest.add_argument("--trusted-source-repository", required=True)
    for name in ("render-intent", "render-evidence", "driver-intent", "driver-receipt",
                 "trace-anchor-receipt", "trace-receipt"):
        manifest.add_argument(f"--{name}", required=True)
    for name in RECORDER_TOOL_ARGUMENTS:
        manifest.add_argument(f"--{name.replace('_', '-')}", required=True)
    manifest.add_argument("--campaign-secret-file", required=True)
    manifest.add_argument("--output", required=True)
    anchor = commands.add_parser("anchor")
    anchor.add_argument("--manifest", required=True)
    anchor.add_argument("--render-tool-bundle-manifest", required=True)
    anchor.add_argument("--expected-source-commit", required=True)
    anchor.add_argument("--trusted-source-repository", required=True)
    anchor.add_argument("--campaign-secret-file", required=True)
    for name in ("subject-identity", "run-metadata", "render-intent", "render-evidence",
                 "trace-metadata"):
        anchor.add_argument(f"--{name}", required=True)
    for name in RECORDER_TOOL_ARGUMENTS:
        anchor.add_argument(f"--{name.replace('_', '-')}", required=True)
    for name in ("trace-started-epoch-ns", "trace-ended-epoch-ns",
                 "start-anchor-continuous-ns", "start-anchor-epoch-ns", "start-anchor-width-ns",
                 "end-anchor-continuous-ns", "end-anchor-epoch-ns", "end-anchor-width-ns"):
        anchor.add_argument(f"--{name}", required=True)
    anchor.add_argument("--output", required=True)
    verify_case = commands.add_parser("verify-case")
    verify_case.add_argument("--manifest", required=True)
    verify_case.add_argument("--render-tool-bundle-manifest", required=True)
    verify_case.add_argument("--expected-source-commit", required=True)
    verify_case.add_argument("--trusted-source-repository", required=True)
    verify_case.add_argument("--campaign-secret-file", required=True)
    verify_case.add_argument("--subject-identity", required=True)
    verify_case.add_argument("--render-intent", required=True)
    verify_case.add_argument("--campaign-id", required=True)
    verify_case.add_argument("--session-id", required=True)
    verify_case.add_argument("--nonce", required=True)
    verify_case.add_argument("--scenario", required=True)
    verify_case.add_argument("--subject", required=True)
    for name in RECORDER_TOOL_ARGUMENTS:
        verify_case.add_argument(f"--{name.replace('_', '-')}", required=True)
    finalize = commands.add_parser("finalize")
    common_artifact_arguments(finalize)
    finalize.add_argument("--output", required=True)
    verify = commands.add_parser("verify")
    common_artifact_arguments(verify)
    verify.add_argument("--receipt", required=True)
    verify_manifest = commands.add_parser("verify-manifest")
    verify_manifest.add_argument("--manifest", required=True)
    verify_manifest.add_argument("--render-tool-bundle-manifest", required=True)
    verify_manifest.add_argument("--expected-source-commit", required=True)
    verify_manifest.add_argument("--trusted-source-repository", required=True)
    verify_manifest.add_argument("--campaign-secret-file", required=True)
    for name in RECORDER_TOOL_ARGUMENTS:
        verify_manifest.add_argument(f"--{name.replace('_', '-')}", required=True)
    verify_bundle = commands.add_parser("verify-tool-bundle")
    verify_bundle.add_argument("--render-tool-bundle-manifest", required=True)
    verify_bundle.add_argument("--expected-source-commit", required=True)
    verify_bundle.add_argument("--trusted-source-repository", required=True)
    verify_bundle.add_argument("--invoked-logical-name", required=True)
    verify_bundle.add_argument("--invoked-path", required=True)
    for name in RECORDER_TOOL_ARGUMENTS:
        verify_bundle.add_argument(f"--{name.replace('_', '-')}", required=True)
    return result


def run(arguments: argparse.Namespace) -> None:
    _, bundle = tool_bundle_values(arguments)
    if arguments.command == "verify-tool-bundle":
        logical = arguments.invoked_logical_name
        key = f"{logical}_bundle_path"
        if key not in bundle or pathlib.Path(arguments.invoked_path).resolve(strict=True) != pathlib.Path(
            bundle[key]
        ):
            raise InvalidReceipt("invoked tool is not the selected frozen bundle executable")
        print("format_version\t1")
        print("result\tPASS")
        print("reason\trender-tool-bundle-git-blobs-and-invocation-bound")
        return
    key, key_id, secret_stat = secret_key(arguments.campaign_secret_file)
    if arguments.command == "manifest":
        values = manifest_values(arguments, key_id, secret_stat)
        signature = hmac_hex(key, MANIFEST_DOMAIN, unsigned(values, MANIFEST_KEYS))
        publish(arguments.output, encode(values, MANIFEST_KEYS, "manifest_hmac_sha256", signature))
        return
    manifest_path = immutable_file(arguments.manifest, "campaign manifest")
    manifest = verify_authenticated(manifest_path, MANIFEST_KEYS, MANIFEST_DOMAIN,
                                    "manifest_hmac_sha256", key, key_id)
    if (manifest["campaign_secret_device"] != str(secret_stat.st_dev)
            or manifest["campaign_secret_inode"] != str(secret_stat.st_ino)):
        raise InvalidReceipt("campaign secret identity does not match manifest")
    if arguments.command == "verify-manifest":
        require_manifest_tool_bundle(arguments, manifest)
        require_manifest_recorder_tools(manifest, recorder_tools(arguments))
        print("format_version\t1")
        print("result\tPASS")
        print("reason\trender-campaign-manifest-authenticated-and-tools-bound")
        return
    if arguments.command == "verify-case":
        require_manifest_tool_bundle(arguments, manifest)
        require_manifest_recorder_tools(manifest, recorder_tools(arguments))
        identity = immutable_file(arguments.subject_identity, "subject identity")
        intent = immutable_file(arguments.render_intent, "render intent")
        verify_case_tuple(manifest, intent, key, key_id)
        expected = {
            "campaign_id": arguments.campaign_id,
            "session_id": arguments.session_id,
            "nonce": arguments.nonce,
            "scenario": arguments.scenario,
            "subject": arguments.subject,
            "subject_identity_sha256": sha256(identity),
        }
        if any(manifest[field] != value for field, value in expected.items()):
            raise InvalidReceipt("campaign manifest does not match requested case tuple")
        print("format_version\t1")
        print("result\tPASS")
        print("reason\trender-campaign-case-tuple-authenticated-and-bound")
        return
    if arguments.command == "anchor":
        output_path, output_parent = pending_path(arguments.output, "trace anchor receipt")
        if (str(output_path) != manifest["trace_anchor_receipt_path"]
                or str(output_parent.st_dev) != manifest["trace_anchor_receipt_parent_device"]
                or str(output_parent.st_ino) != manifest["trace_anchor_receipt_parent_inode"]):
            raise InvalidReceipt("trace anchor output does not match campaign manifest")
        values = anchor_values(arguments, manifest, key_id, key)
        signature = hmac_hex(key, ANCHOR_DOMAIN, unsigned(values, ANCHOR_KEYS))
        publish(arguments.output, encode(values, ANCHOR_KEYS, "anchor_hmac_sha256", signature))
        return
    if arguments.command == "finalize":
        output_path, output_parent = pending_path(arguments.output, "trace receipt")
        if (str(output_path) != manifest["trace_receipt_path"]
                or str(output_parent.st_dev) != manifest["trace_receipt_parent_device"]
                or str(output_parent.st_ino) != manifest["trace_receipt_parent_inode"]):
            raise InvalidReceipt("trace receipt output does not match campaign manifest")
        values = receipt_values(arguments, manifest, key_id, key)
        signature = hmac_hex(key, RECEIPT_DOMAIN, unsigned(values, RECEIPT_KEYS))
        publish(arguments.output, encode(values, RECEIPT_KEYS, "receipt_hmac_sha256", signature))
        return
    receipt_path = immutable_file(arguments.receipt, "trace receipt")
    values = verify_authenticated(receipt_path, RECEIPT_KEYS, RECEIPT_DOMAIN,
                                  "receipt_hmac_sha256", key, key_id)
    expected = receipt_values(arguments, manifest, key_id, key)
    expected_signature = hmac_hex(key, RECEIPT_DOMAIN, unsigned(expected, RECEIPT_KEYS))
    expected_payload = encode(expected, RECEIPT_KEYS, "receipt_hmac_sha256", expected_signature)
    if receipt_path.read_bytes() != expected_payload:
        raise InvalidReceipt("trace receipt does not match current artifacts")
    print("format_version\t1")
    print("result\tPASS")
    print("reason\trender-trace-receipt-authenticated-and-bound")


if __name__ == "__main__":
    try:
        run(parser().parse_args())
    except (InvalidReceipt, OSError) as error:
        fail(str(error))
