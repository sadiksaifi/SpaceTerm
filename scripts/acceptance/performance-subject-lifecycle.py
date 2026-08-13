#!/usr/bin/env python3

"""Own normal AppKit termination for one frozen performance subject.

Production live-code/process validation is delegated to the canonical frozen process
inspector. A tiny Objective-C bridge performs the sole NSRunningApplication action.
"""

from __future__ import annotations

import argparse
import hashlib
import hmac
import os
import re
import select
import stat
import struct
import subprocess
import sys
import time
import ctypes
from dataclasses import dataclass
from pathlib import Path


READY_MAGIC = b"spaceterm.acceptance.performance-lifecycle-ready/v1\0"
REGISTER_MAGIC = b"spaceterm.acceptance.performance-lifecycle-registration/v1\0"
EXIT_MAGIC = b"spaceterm.acceptance.performance-subject-exit/v1\0"
TAIL_KEYS = (
    "format_version", "campaign_id", "session_id", "nonce", "quit_token",
    "run_intent_sha256", "subject_identity_sha256", "subject_process_pid",
    "subject_process_start_identity", "driver_receipt_sha256", "driver_events_sha256",
    "workload_metadata_sha256", "workload_events_sha256", "rss_samples_sha256",
    "trace_provisional_receipt_sha256", "tail_completed_continuous_ns",
    "appkit_terminator_source_device", "appkit_terminator_source_inode",
    "appkit_terminator_source_sha256", "appkit_terminator_binary_device",
    "appkit_terminator_binary_inode", "appkit_terminator_binary_sha256",
    "evidence_mode", "terminal_status", "auth_algorithm", "tail_hmac_sha256",
)
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
SUBJECT_KEYS = (
    "format_version", "subject", "app_bundle_path", "bundle_identifier", "bundle_version",
    "executable_path", "executable_sha256", "executable_device", "executable_inode",
    "executable_fsid", "signature_valid", "signing_identifier", "team_identifier", "cdhash",
    "process_pid", "process_start_identity", "identity_status",
)
INTENT_KEYS = (
    "format_version", "subject", "subject_identity_sha256", "scenario",
    "scenario_plan_sha256", "workload_sha256", "command_sha256", "environment_sha256",
    "font_sha256", "initial_grid_sha256", "measured_duration_ms", "process_pid",
    "process_start_identity", "campaign_id", "session_id", "nonce",
    "native_provisional_observation_sha256", "evidence_mode", "status",
)
QUIT_KEYS = (
    "format_version", "campaign_id", "session_id", "nonce", "run_intent_sha256",
    "subject_process_pid", "subject_process_start_identity", "quit_token",
    "request_continuous_ns", "exit_continuous_ns", "termination_method",
    "runtime_closure_status", "appkit_terminator_source_device",
    "appkit_terminator_source_inode", "appkit_terminator_source_sha256",
    "appkit_terminator_binary_device", "appkit_terminator_binary_inode",
    "appkit_terminator_binary_sha256", "evidence_mode", "status",
)
EXIT_KEYS = (
    "schema", "subject", "campaign_id", "session_id", "nonce", "run_intent_sha256",
    "subject_identity_sha256", "process_pid", "process_start_identity",
    "tail_receipt_sha256", "quit_receipt_sha256", "exit_requested_continuous_ns",
    "process_exited_continuous_ns", "exit_status", "native_observation_sha256",
    "appkit_terminator_source_device", "appkit_terminator_source_inode",
    "appkit_terminator_source_sha256", "appkit_terminator_binary_device",
    "appkit_terminator_binary_inode", "appkit_terminator_binary_sha256",
    "evidence_mode", "auth_algorithm", "receipt_hmac_sha256", "status",
)
HEX = re.compile(r"[0-9a-f]{64}\Z")
SAFE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,79}\Z")


class Invalid(Exception):
    pass


@dataclass(frozen=True)
class ToolIdentity:
    path: Path
    device: int
    inode: int
    sha256: str


def snapshot_tool(path_text: str, *, executable: bool) -> ToolIdentity:
    path = Path(path_text)
    if not path.is_absolute():
        raise Invalid("terminator-path-not-absolute")
    before = path.lstat()
    if not stat.S_ISREG(before.st_mode) or stat.S_ISLNK(before.st_mode) \
            or before.st_uid != os.geteuid() or before.st_nlink != 1 \
            or before.st_mode & 0o022 or (executable and before.st_mode & 0o111 == 0) \
            or before.st_size <= 0:
        raise Invalid("unsafe-terminator-tool")
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(fd)
        hasher = hashlib.sha256()
        while True:
            chunk = os.read(fd, 1024 * 1024)
            if not chunk:
                break
            hasher.update(chunk)
        after = os.fstat(fd)
    finally:
        os.close(fd)
    current = path.lstat()
    fields = (
        "st_dev", "st_ino", "st_mode", "st_uid", "st_nlink", "st_size",
        "st_mtime_ns", "st_ctime_ns",
    )
    if any(getattr(before, key) != getattr(other, key)
           for other in (opened, after, current) for key in fields):
        raise Invalid("terminator-tool-changed")
    return ToolIdentity(path, before.st_dev, before.st_ino, hasher.hexdigest())


def expected_tool(args: argparse.Namespace) -> tuple[ToolIdentity, ToolIdentity]:
    source = snapshot_tool(args.appkit_terminator_source, executable=False)
    binary = snapshot_tool(args.appkit_terminator, executable=True)
    expected = (
        (source, args.expected_appkit_terminator_source_device,
         args.expected_appkit_terminator_source_inode,
         args.expected_appkit_terminator_source_sha256),
        (binary, args.expected_appkit_terminator_binary_device,
         args.expected_appkit_terminator_binary_inode,
         args.expected_appkit_terminator_binary_sha256),
    )
    for identity, device, inode, sha256 in expected:
        if device < 0 or inode <= 0 or HEX.fullmatch(sha256) is None \
                or identity.device != device or identity.inode != inode \
                or identity.sha256 != sha256:
            raise Invalid("terminator-tool-provenance")
    if os.environ.get("SPACETERM_PERFORMANCE_TEST_MODE") != "1":
        canonical_source = Path(__file__).resolve().with_name("performance-appkit-terminate.m")
        if source.path.resolve(strict=True) != canonical_source:
            raise Invalid("non-production-terminator-source")
    return source, binary


def recheck_tool(expected: ToolIdentity, *, executable: bool) -> None:
    if snapshot_tool(str(expected.path), executable=executable) != expected:
        raise Invalid("terminator-tool-replaced")


def tool_values(source: ToolIdentity, binary: ToolIdentity) -> dict[str, str]:
    return {
        "appkit_terminator_source_device": str(source.device),
        "appkit_terminator_source_inode": str(source.inode),
        "appkit_terminator_source_sha256": source.sha256,
        "appkit_terminator_binary_device": str(binary.device),
        "appkit_terminator_binary_inode": str(binary.inode),
        "appkit_terminator_binary_sha256": binary.sha256,
    }


def clock_ns() -> int:
    library = ctypes.CDLL("/usr/lib/libSystem.B.dylib")
    library.mach_continuous_time.restype = ctypes.c_uint64
    library.mach_timebase_info.argtypes = [ctypes.c_void_p]
    class Timebase(ctypes.Structure):
        _fields_ = [("numer", ctypes.c_uint32), ("denom", ctypes.c_uint32)]
    info = Timebase()
    if library.mach_timebase_info(ctypes.byref(info)) != 0 or info.denom == 0:
        raise Invalid("continuous-clock")
    return library.mach_continuous_time() * info.numer // info.denom


def read(path_text: str, maximum: int = 64 * 1024, *, private: bool = False) -> bytes:
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
    fields = ("st_dev", "st_ino", "st_mode", "st_uid", "st_nlink", "st_size", "st_mtime_ns")
    if any(getattr(before, key) != getattr(other, key)
           for other in (opened, after, current) for key in fields) or len(data) != before.st_size:
        raise Invalid("input-changed")
    return data


def parse(data: bytes, keys: tuple[str, ...], hmac_key: str | None = None) \
        -> tuple[dict[str, str], bytes]:
    if not data.endswith(b"\n") or b"\0" in data or b"\r" in data:
        raise Invalid("encoding")
    lines = data[:-1].split(b"\n")
    if len(lines) != len(keys):
        raise Invalid("record-count")
    values: dict[str, str] = {}
    unsigned = bytearray()
    for expected, line in zip(keys, lines):
        fields = line.split(b"\t")
        if len(fields) != 2 or fields[0].decode("ascii") != expected or not fields[1]:
            raise Invalid("record-order")
        values[expected] = fields[1].decode("utf-8")
        if expected != hmac_key:
            unsigned.extend(line + b"\n")
    return values, bytes(unsigned)


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sign(secret: bytes, magic: bytes, unsigned: bytes) -> str:
    return hmac.new(secret, magic + struct.pack(">Q", len(unsigned)) + unsigned,
                    hashlib.sha256).hexdigest()


def encode_signed(keys: tuple[str, ...], values: dict[str, str], hmac_key: str,
                  magic: bytes, secret: bytes) -> bytes:
    unsigned = b"".join(f"{key}\t{values[key]}\n".encode() for key in keys if key != hmac_key)
    values[hmac_key] = sign(secret, magic, unsigned)
    return b"".join(f"{key}\t{values[key]}\n".encode() for key in keys)


def publish(path_text: str, data: bytes) -> None:
    path = Path(path_text)
    if not path.is_absolute() or path.exists() or path.is_symlink():
        raise Invalid("output-exists")
    parent = path.parent.resolve(strict=True)
    if parent.stat().st_uid != os.geteuid() or parent.stat().st_mode & 0o077:
        raise Invalid("unsafe-output-parent")
    temporary = parent / f".{path.name}.{os.getpid()}.tmp"
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o400)
    try:
        if os.write(fd, data) != len(data):
            raise OSError("short-write")
        os.fchmod(fd, 0o400); os.fsync(fd)
    finally:
        os.close(fd)
    try:
        os.link(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def verify_live(args: argparse.Namespace, subject: dict[str, str]) -> None:
    if os.environ.get("SPACETERM_PERFORMANCE_TEST_MODE") == "1":
        if os.environ.get("SPACETERM_TEST_LIFECYCLE_IDENTITY") != "valid":
            raise Invalid("test-live-identity")
        return
    command = [args.process_inspector, "--pid", subject["process_pid"],
        "--expected-executable", subject["executable_path"],
        "--expected-sha256", subject["executable_sha256"],
        "--expected-device", subject["executable_device"],
        "--expected-inode", subject["executable_inode"],
        "--expected-start-identity", subject["process_start_identity"],
        "--expected-signing-identifier", subject["signing_identifier"],
        "--expected-team-identifier", subject["team_identifier"],
        "--expected-cdhash", subject["cdhash"]]
    completed = subprocess.run(command, check=False, capture_output=True, text=True)
    if completed.returncode or "live_code_identity_verified\ttrue" not in completed.stdout:
        raise Invalid("live-code-identity")


def request_normal_termination(args: argparse.Namespace, subject: dict[str, str]) -> None:
    if os.environ.get("SPACETERM_PERFORMANCE_TEST_MODE") == "1":
        if os.environ.get("SPACETERM_TEST_LIFECYCLE_TERMINATION") != "normal":
            raise Invalid("normal-termination-refused")
        return
    completed = subprocess.run([args.appkit_terminator, "--pid", subject["process_pid"],
        "--process-start-identity", subject["process_start_identity"],
        "--bundle-identifier", subject["bundle_identifier"],
        "--executable", subject["executable_path"],
        "--timeout-seconds", str(min(args.timeout_seconds, 120))], check=False)
    if completed.returncode:
        raise Invalid("normal-termination-refused")


def process_absent(args: argparse.Namespace, subject: dict[str, str], deadline: float) -> None:
    if os.environ.get("SPACETERM_PERFORMANCE_TEST_MODE") == "1":
        if os.environ.get("SPACETERM_TEST_LIFECYCLE_TERMINATION") != "normal":
            raise Invalid("process-not-absent")
        return
    while time.monotonic() < deadline:
        completed = subprocess.run([args.process_inspector, "--pid", subject["process_pid"],
            "--print-start-identity"], check=False, capture_output=True)
        if completed.returncode:
            return
        time.sleep(0.02)
    raise Invalid("normal-termination-timeout")


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--subject-identity", required=True)
    parser.add_argument("--campaign-secret-file", required=True)
    parser.add_argument("--campaign-id", required=True); parser.add_argument("--session-id", required=True)
    parser.add_argument("--nonce", required=True); parser.add_argument("--live-ready-receipt", required=True)
    parser.add_argument("--registration-control", required=True)
    parser.add_argument("--quit-receipt", required=True); parser.add_argument("--subject-exit-receipt", required=True)
    parser.add_argument("--native-observation", required=True); parser.add_argument("--timeout-seconds", type=int, default=120)
    parser.add_argument("--driver-receipt", required=True); parser.add_argument("--driver-events", required=True)
    parser.add_argument("--workload-metadata", required=True); parser.add_argument("--workload-events", required=True)
    parser.add_argument("--workload-ready-receipt", required=True); parser.add_argument("--rss-samples", required=True)
    parser.add_argument("--trace-provisional-receipt", required=True); parser.add_argument("--plan-start-gate", required=True)
    here = Path(__file__).resolve().parent
    parser.add_argument("--process-inspector", default=str(here.parent / "inspect-release-performance-process.py"))
    parser.add_argument("--appkit-terminator-source", required=True)
    parser.add_argument("--appkit-terminator", default=str(here / "performance-appkit-terminate"))
    parser.add_argument("--expected-appkit-terminator-source-device", required=True, type=int)
    parser.add_argument("--expected-appkit-terminator-source-inode", required=True, type=int)
    parser.add_argument("--expected-appkit-terminator-source-sha256", required=True)
    parser.add_argument("--expected-appkit-terminator-binary-device", required=True, type=int)
    parser.add_argument("--expected-appkit-terminator-binary-inode", required=True, type=int)
    parser.add_argument("--expected-appkit-terminator-binary-sha256", required=True)
    return parser.parse_args()


def main() -> int:
    args = arguments()
    evidence_mode = "test-only" if os.environ.get("SPACETERM_PERFORMANCE_TEST_MODE") == "1" else "production"
    control_fd = -1
    try:
        if not 1 <= args.timeout_seconds <= 1800 or SAFE.fullmatch(args.campaign_id) is None \
                or SAFE.fullmatch(args.session_id) is None or HEX.fullmatch(args.nonce) is None:
            raise Invalid("arguments")
        secret = read(args.campaign_secret_file, 4096, private=True)
        if len(secret) < 32:
            raise Invalid("secret")
        terminator_source, terminator_binary = expected_tool(args)
        subject_data = read(args.subject_identity)
        subject, _ = parse(subject_data, SUBJECT_KEYS)
        if subject["format_version"] != "1" or subject["identity_status"] != "frozen" \
                or subject["subject"] not in ("spaceterm", "ghostty"):
            raise Invalid("subject")
        verify_live(args, subject)
        control = Path(args.registration_control)
        if not control.is_absolute() or control.exists() or control.is_symlink():
            raise Invalid("control-exists")
        os.mkfifo(control, 0o600)
        control_stat = control.lstat()
        ready_values = {"schema": "spaceterm.acceptance.performance-lifecycle-ready/v1",
            "subject": subject["subject"], "campaign_id": args.campaign_id,
            "session_id": args.session_id, "nonce": args.nonce,
            "subject_identity_sha256": digest(subject_data), "process_pid": subject["process_pid"],
            "process_start_identity": subject["process_start_identity"],
            "executable_sha256": subject["executable_sha256"], "ready_continuous_ns": str(clock_ns()),
            "registration_control_device": str(control_stat.st_dev),
            "registration_control_inode": str(control_stat.st_ino),
            **tool_values(terminator_source, terminator_binary), "evidence_mode": evidence_mode,
            "auth_algorithm": "hmac-sha256",
            "status": "ready"}
        publish(args.live_ready_receipt, encode_signed(READY_KEYS, ready_values,
            "receipt_hmac_sha256", READY_MAGIC, secret))
        control_fd = os.open(control, os.O_RDWR | os.O_NONBLOCK | os.O_NOFOLLOW)
        deadline = time.monotonic() + args.timeout_seconds
        command = b""
        while b"\n" not in command and time.monotonic() < deadline:
            select.select([control_fd], [], [], min(0.1, deadline - time.monotonic()))
            try: command += os.read(control_fd, 4096)
            except BlockingIOError: pass
        parts = command.rstrip(b"\n").split(b"\t")
        if len(parts) != 3 or parts[0] != b"register" or HEX.fullmatch(parts[1].decode()) is None:
            raise Invalid("registration-command")
        registration_path = parts[2].decode(); registration_data = read(registration_path, private=True)
        registration, unsigned = parse(registration_data, REGISTER_KEYS, "registration_hmac_sha256")
        if registration["format_version"] != "1" or registration["status"] != "registered" \
                or registration["evidence_mode"] != evidence_mode \
                or registration["auth_algorithm"] != "hmac-sha256" \
                or registration["registration_token"] != parts[1].decode() \
                or registration["campaign_id"] != args.campaign_id \
                or registration["session_id"] != args.session_id or registration["nonce"] != args.nonce \
                or registration["subject_identity_sha256"] != digest(subject_data) \
                or registration["process_pid"] != subject["process_pid"] \
                or registration["process_start_identity"] != subject["process_start_identity"] \
                or any(registration[key] != value for key, value in
                       tool_values(terminator_source, terminator_binary).items()) \
                or not hmac.compare_digest(registration["registration_hmac_sha256"],
                    sign(secret, REGISTER_MAGIC, unsigned)):
            raise Invalid("registration")
        intent_data = read(registration["run_intent_path"]); intent, _ = parse(intent_data, INTENT_KEYS)
        if digest(intent_data) != registration["run_intent_sha256"] or intent["subject"] != subject["subject"] \
                or intent["subject_identity_sha256"] != digest(subject_data) \
                or intent["campaign_id"] != args.campaign_id or intent["session_id"] != args.session_id \
                or intent["nonce"] != args.nonce or intent["evidence_mode"] != evidence_mode \
                or intent["process_pid"] != subject["process_pid"] \
                or intent["process_start_identity"] != subject["process_start_identity"] \
                or Path(registration["workload_metadata_path"]).resolve() != Path(args.workload_metadata).resolve() \
                or Path(registration["workload_events_path"]).resolve() != Path(args.workload_events).resolve() \
                or Path(registration["workload_ready_receipt_path"]).resolve() != Path(args.workload_ready_receipt).resolve() \
                or Path(registration["quit_receipt_path"]).resolve() != Path(args.quit_receipt).resolve() \
                or Path(registration["subject_exit_receipt_path"]).resolve() != Path(args.subject_exit_receipt).resolve():
            raise Invalid("intent")
        expected_native = "not-applicable" if subject["subject"] == "ghostty" \
            else str(Path(args.native_observation).resolve())
        registered_native = registration["native_observation_path"] if subject["subject"] == "ghostty" \
            else str(Path(registration["native_observation_path"]).resolve())
        if registered_native != expected_native:
            raise Invalid("native-observation-registration")
        tail_path = Path(registration["tail_receipt_path"])
        while not tail_path.exists() and time.monotonic() < deadline: time.sleep(0.02)
        tail_data = read(str(tail_path), private=True)
        tail, _ = parse(tail_data, TAIL_KEYS)
        if any(tail[key] != value for key, value in
               tool_values(terminator_source, terminator_binary).items()):
            raise Invalid("tail-terminator-provenance")
        workload_data = read(args.workload_metadata)
        workload, _ = parse(workload_data, WORKLOAD_KEYS)
        tail_completed = tail["tail_completed_continuous_ns"]
        subprocess.run([str(Path(__file__).with_name("performance-tail-receipt.py")), "verify",
            "--campaign-secret-file", args.campaign_secret_file, "--campaign-id", args.campaign_id,
            "--session-id", args.session_id, "--nonce", args.nonce,
            "--quit-token", registration["registration_token"], "--run-intent", registration["run_intent_path"],
            "--subject-identity", args.subject_identity, "--driver-receipt", args.driver_receipt,
            "--driver-events", args.driver_events, "--workload-metadata", args.workload_metadata,
            "--workload-events", args.workload_events,
            "--workload-ready-receipt", args.workload_ready_receipt,
            "--rss-samples", args.rss_samples,
            "--trace-provisional-receipt", args.trace_provisional_receipt,
            "--appkit-terminator-source", args.appkit_terminator_source,
            "--appkit-terminator-binary", args.appkit_terminator,
            "--tail-completed-continuous-ns", tail_completed, "--receipt", str(tail_path)], check=True)
        subprocess.run([str(Path(__file__).with_name("verify-performance-workload-ready.py")),
            "--ready-receipt", args.workload_ready_receipt, "--events", args.workload_events,
            "--subject-identity", args.subject_identity,
            "--campaign-secret-file", args.campaign_secret_file,
            "--campaign-id", args.campaign_id, "--session-id", args.session_id,
            "--nonce", args.nonce, "--plan-start-gate", args.plan_start_gate,
            "--expected-plan-start-continuous-ns", workload["plan_start_continuous_ns"]], check=True)
        verify_live(args, subject)
        recheck_tool(terminator_source, executable=False)
        recheck_tool(terminator_binary, executable=True)
        requested_ns = clock_ns(); request_normal_termination(args, subject)
        process_absent(args, subject, deadline); exited_ns = clock_ns()
        recheck_tool(terminator_source, executable=False)
        recheck_tool(terminator_binary, executable=True)
        native_hash = "not-applicable"
        if subject["subject"] == "spaceterm":
            native_path = Path(registration["native_observation_path"])
            while not native_path.exists() and time.monotonic() < deadline: time.sleep(0.02)
            native_hash = digest(read(str(native_path), private=True))
        quit_values = {"format_version": "1", "campaign_id": args.campaign_id,
            "session_id": args.session_id, "nonce": args.nonce, "run_intent_sha256": digest(intent_data),
            "subject_process_pid": subject["process_pid"], "subject_process_start_identity": subject["process_start_identity"],
            "quit_token": registration["registration_token"], "request_continuous_ns": str(requested_ns),
            "exit_continuous_ns": str(exited_ns), "termination_method": "appkit-terminate",
            "runtime_closure_status": "confirmed",
            **tool_values(terminator_source, terminator_binary), "evidence_mode": evidence_mode,
            "status": "completed"}
        quit_data = b"".join(f"{key}\t{quit_values[key]}\n".encode() for key in QUIT_KEYS)
        publish(args.quit_receipt, quit_data)
        exit_values = {"schema": "spaceterm.acceptance.performance-subject-exit/v1",
            "subject": subject["subject"], "campaign_id": args.campaign_id, "session_id": args.session_id,
            "nonce": args.nonce, "run_intent_sha256": digest(intent_data),
            "subject_identity_sha256": digest(subject_data), "process_pid": subject["process_pid"],
            "process_start_identity": subject["process_start_identity"], "tail_receipt_sha256": digest(tail_data),
            "quit_receipt_sha256": digest(quit_data), "exit_requested_continuous_ns": str(requested_ns),
            "process_exited_continuous_ns": str(exited_ns), "exit_status": "normal",
            "native_observation_sha256": native_hash,
            **tool_values(terminator_source, terminator_binary), "evidence_mode": evidence_mode,
            "auth_algorithm": "hmac-sha256", "status": "complete"}
        publish(args.subject_exit_receipt, encode_signed(EXIT_KEYS, exit_values,
            "receipt_hmac_sha256", EXIT_MAGIC, secret))
    except (Invalid, OSError, UnicodeError, ValueError, subprocess.SubprocessError) as error:
        print(f"performance subject lifecycle failed: {error}", file=sys.stderr); return 1
    finally:
        if control_fd >= 0: os.close(control_fd)
        try: Path(args.registration_control).unlink()
        except OSError: pass
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
