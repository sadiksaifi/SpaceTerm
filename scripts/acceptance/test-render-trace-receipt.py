#!/usr/bin/python3
"""Focused adversarial fixtures for render-trace-receipt.py."""

from __future__ import annotations

import hashlib
import hmac
import os
import pathlib
import shutil
import stat
import subprocess
import tempfile
import importlib.util
from unittest import mock


HERE = pathlib.Path(__file__).resolve().parent
TOOL = HERE / "render-trace-receipt.py"
COMMAND_TOOL = TOOL
ZERO = "0" * 64
IDENTITY_KEYS = (
    "format_version", "subject", "app_bundle_path", "bundle_identifier", "bundle_version",
    "executable_path", "executable_sha256", "executable_device", "executable_inode",
    "executable_fsid", "signature_valid", "signing_identifier", "team_identifier", "cdhash",
    "process_pid", "process_start_identity", "identity_status",
)
TRACE_KEYS = (
    "format_version", "capture_status", "incomplete_reason", "subject_identity_sha256",
    "run_metadata_sha256", "workload_metadata_sha256", "workload_ready_receipt_sha256",
    "supplemental_evidence_sha256", "requested_duration_ms", "actual_duration_ms",
    "capture_started_continuous_ns", "capture_ended_continuous_ns", "target_identity_verified",
    "trace_target_pid_verified", "time_profiler_instrument", "allocations_instrument",
    "hangs_instrument", "time_profiler_target_verified", "allocations_target_verified",
    "hangs_target_verified", "time_profiler_rows", "allocations_rows", "hangs_rows",
    "maximum_main_thread_hang_ms", "status",
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


def write(path: pathlib.Path, text: str, mode: int = 0o444) -> pathlib.Path:
    path.write_text(text, encoding="utf-8")
    path.chmod(mode)
    return path.resolve()


def digest(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def kv(path: pathlib.Path, keys: tuple[str, ...], values: dict[str, str]) -> pathlib.Path:
    return write(path, "".join(f"{key}\t{values[key]}\n" for key in keys))


def authenticated_kv(
    path: pathlib.Path,
    keys: tuple[str, ...],
    values: dict[str, str],
    domain: bytes,
    secret_hex: str,
) -> pathlib.Path:
    unsigned = "".join(f"{key}\t{values[key]}\n" for key in keys[:-1]).encode()
    values[keys[-1]] = hmac.new(
        bytes.fromhex(secret_hex), domain + unsigned, hashlib.sha256
    ).hexdigest()
    return kv(path, keys, values)


def command(*arguments: str, expect: int = 0) -> subprocess.CompletedProcess[str]:
    result = subprocess.run([str(COMMAND_TOOL), *arguments], text=True, capture_output=True, check=False)
    if result.returncode != expect:
        raise AssertionError(
            f"unexpected exit {result.returncode}, expected {expect}: {' '.join(arguments)}\n"
            f"stdout={result.stdout}\nstderr={result.stderr}"
        )
    return result


def tool_file(root: pathlib.Path, name: str) -> pathlib.Path:
    return write(root / name, "#!/bin/sh\nexit 0\n", 0o555)


def tool_bundle_manifest(
    root: pathlib.Path, tools: dict[str, pathlib.Path],
) -> tuple[pathlib.Path, str, pathlib.Path]:
    names = (
        "record_release_performance_trace", "freeze_render_profile_intent",
        "finalize_render_profile_evidence", "render_profile_hmac", "render_trace_receipt",
        "analyze_release_render_profile_case", "archive_render_trace",
        "verify_render_action_video", "verify_render_trace_archive",
        "verify_release_performance_trace", "inspect_release_performance_process",
        "run_release_performance_command",
        "freeze_render_profile_tool_bundle",
    )
    relatives = (
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
    mapped = {
        "render_profile_hmac": tools["render-profile-hmac"],
        "render_trace_receipt": tools["render-trace-receipt-helper"],
        "verify_release_performance_trace": tools["trace-verifier"],
        "inspect_release_performance_process": tools["process-inspector"],
        "run_release_performance_command": tools["command-runner"],
    }
    fallback = tools["trace-archive-verifier"]
    repository = root / "source-repository"
    repository.mkdir()
    for name, relative in zip(names, relatives):
        source = repository / relative
        source.parent.mkdir(parents=True, exist_ok=True)
        source.write_bytes(mapped.get(name, fallback).read_bytes())
    subprocess.run(["/usr/bin/git", "init", "-q", str(repository)], check=True)
    subprocess.run(["/usr/bin/git", "-C", str(repository), "add", "."], check=True)
    environment = dict(os.environ, GIT_AUTHOR_NAME="fixture", GIT_AUTHOR_EMAIL="fixture@example.test",
                       GIT_COMMITTER_NAME="fixture", GIT_COMMITTER_EMAIL="fixture@example.test")
    subprocess.run(["/usr/bin/git", "-C", str(repository), "commit", "-qm", "fixture"],
                   check=True, env=environment)
    source_commit = subprocess.run(
        ["/usr/bin/git", "-C", str(repository), "rev-parse", "HEAD"], check=True,
        capture_output=True, text=True,
    ).stdout.strip()
    lines = [
        "format_version\t1\n", "schema\tspaceterm.render-profile-tool-bundle/v1\n",
        f"source_commit\t{source_commit}\n", f"tool_count\t{len(names)}\n",
    ]
    for name, relative in zip(names, relatives):
        tool = mapped.get(name, fallback)
        tool_hash = digest(tool)
        lines.extend((
            f"{name}_source_path\t{repository / relative}\n",
            f"{name}_source_sha256\t{tool_hash}\n",
            f"{name}_bundle_path\t{tool}\n", f"{name}_bundle_sha256\t{tool_hash}\n",
        ))
    return write(root / "tool-bundle-manifest.tsv", "".join(lines)), source_commit, repository


def main() -> None:
    global COMMAND_TOOL
    spec = importlib.util.spec_from_file_location("render_trace_receipt", TOOL)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    system_python = pathlib.Path("/usr/bin/python3")
    python_stat = system_python.stat()
    assert python_stat.st_uid == 0 and not python_stat.st_mode & 0o022
    assert module.immutable_file(
        str(system_python), "system python", executable=True,
        allow_sealed_system_links=True,
    ) == system_python
    synthetic_system_stat = mock.Mock(
        st_uid=0, st_nlink=78, st_mode=stat.S_IFREG | 0o555, st_size=1,
    )
    assert (synthetic_system_stat.st_nlink != 1
            and synthetic_system_stat.st_uid == 0
            and not synthetic_system_stat.st_mode & 0o022)

    with tempfile.TemporaryDirectory(prefix="spaceterm-trace-receipt-") as raw_root:
        root = pathlib.Path(raw_root).resolve()
        root.chmod(0o700)
        secret_hex = "11" * 32
        secret = write(root / "secret", secret_hex + "\n", 0o400)
        key_id = hashlib.sha256(secret_hex.encode()).hexdigest()
        identity_values = {
            "format_version": "1", "subject": "spaceterm", "app_bundle_path": "/Applications/SpaceTerm.app",
            "bundle_identifier": "dev.spaceterm", "bundle_version": "1", "executable_path": "/bin/test",
            "executable_sha256": "22" * 32, "executable_device": "1", "executable_inode": "2",
            "executable_fsid": "1", "signature_valid": "true", "signing_identifier": "dev.spaceterm",
            "team_identifier": "none", "cdhash": "abcd", "process_pid": "4242",
            "process_start_identity": "100:200", "identity_status": "frozen",
        }
        identity = kv(root / "subject.tsv", IDENTITY_KEYS, identity_values)
        run_intent_hash = "4b" * 32
        run_values = {
            "format_version": "4", "subject": "spaceterm",
            "subject_identity_sha256": digest(identity),
            "scenario": "perf-render-idle-cursor-blink", "scenario_plan_sha256": "44" * 32,
            "workload_sha256": "4c" * 32, "command_sha256": "47" * 32,
            "environment_sha256": "48" * 32, "font_sha256": "49" * 32,
            "initial_grid_sha256": "4a" * 32, "measured_duration_ms": "120000",
            "process_pid": "4242", "process_start_identity": "100:200",
            "run_intent_sha256": run_intent_hash, "native_observation_sha256": "51" * 32,
            "native_runtime_metadata_sha256": "52" * 32,
            "native_failure_actions_sha256": "53" * 32,
            "native_failure_action_enabled": "false", "native_failure_request_count": "0",
            "native_failure_result_count": "0", "native_failure_resource_staged_count": "0",
            "native_failure_resource_staged_bytes": "0",
            "native_failure_resource_rolled_back_count": "0",
            "native_failure_resource_rolled_back_bytes": "0",
            "trace_provisional_receipt_sha256": "54" * 32,
            "performance_tail_receipt_sha256": "55" * 32,
            "performance_quit_receipt_sha256": "56" * 32,
            "subject_exit_receipt_sha256": "57" * 32,
            "lifecycle_ready_receipt_sha256": "58" * 32,
            "lifecycle_registration_receipt_sha256": "59" * 32,
            "lifecycle_helper_sha256": "5a" * 32,
            "terminator_source_sha256": "5b" * 32,
            "terminator_binary_sha256": "5c" * 32,
            "evidence_mode": "production", "status": "complete",
        }
        run = kv(root / "run.tsv", RUN_KEYS, run_values)
        render_intent = root / "render-intent.tsv"
        render_evidence = root / "render-evidence.tsv"
        driver_intent = root / "driver-intent.tsv"
        driver_receipt = root / "driver-receipt.tsv"
        anchor_receipt = root / "trace-anchor-receipt.tsv"
        receipt = root / "trace-receipt.tsv"
        manifest = root / "manifest.tsv"
        nonce = "33" * 32
        tools = {name: tool_file(root, name) for name in (
            "xcrun", "sips", "python", "ffprobe", "trace-verifier",
            "trace-archive-verifier", "action-video-verifier",
            "render-trace-receipt-verifier", "driver-receipt-verifier",
            "render-profile-hmac", "render-trace-receipt-helper",
            "process-inspector", "command-runner")}
        receipt_tool = root / "render-trace-receipt.py"
        receipt_tool.write_bytes(TOOL.read_bytes())
        receipt_tool.chmod(0o555)
        tools["render-trace-receipt-helper"] = receipt_tool
        COMMAND_TOOL = receipt_tool
        bundle_manifest, source_commit, source_repository = tool_bundle_manifest(root, tools)
        bundle_args = ["--render-tool-bundle-manifest", str(bundle_manifest),
                       "--expected-source-commit", source_commit,
                       "--trusted-source-repository", str(source_repository)]
        checkout_rejected = subprocess.run(
            [str(TOOL), "verify-tool-bundle", *bundle_args,
             "--invoked-logical-name", "render_trace_receipt", "--invoked-path", str(TOOL),
             "--render-profile-hmac", str(tools["render-profile-hmac"]),
             "--render-trace-receipt-helper", str(receipt_tool),
             "--process-inspector", str(tools["process-inspector"]),
             "--trace-verifier", str(tools["trace-verifier"]),
             "--command-runner", str(tools["command-runner"])],
            capture_output=True, text=True, check=False,
        )
        assert checkout_rejected.returncode == 1
        bundle_verified = command(
            "verify-tool-bundle", *bundle_args,
            "--invoked-logical-name", "render_trace_receipt", "--invoked-path", str(receipt_tool),
            "--render-profile-hmac", str(tools["render-profile-hmac"]),
            "--render-trace-receipt-helper", str(receipt_tool),
            "--process-inspector", str(tools["process-inspector"]),
            "--trace-verifier", str(tools["trace-verifier"]),
            "--command-runner", str(tools["command-runner"]),
        )
        assert "result\tPASS" in bundle_verified.stdout
        manifest_args = [
            "manifest", "--campaign-id", "campaign-a", "--session-id", "session-a",
            "--nonce", nonce, "--scenario", "perf-render-idle-cursor-blink", "--subject", "spaceterm",
            "--subject-identity", str(identity), "--render-intent", str(render_intent),
            "--render-evidence", str(render_evidence), "--driver-intent", str(driver_intent),
            "--driver-receipt", str(driver_receipt), "--trace-receipt", str(receipt),
            "--trace-anchor-receipt", str(anchor_receipt),
            "--render-profile-hmac", str(tools["render-profile-hmac"]),
            "--render-trace-receipt-helper", str(tools["render-trace-receipt-helper"]),
            "--process-inspector", str(tools["process-inspector"]),
            "--trace-verifier", str(tools["trace-verifier"]),
            "--command-runner", str(tools["command-runner"]),
            *bundle_args,
            "--campaign-secret-file", str(secret), "--output", str(manifest),
        ]
        command(*manifest_args)
        root_identity = root.stat()
        intent_values = {
            "format_version": "1", "canonicalization": "utf8-lf-tab-kv-fixed-order-domain-nul-v1",
            "auth_domain": "SPACETERM_RENDER_PROFILE_INTENT_V1",
            "scenario": "perf-render-idle-cursor-blink", "subject": "spaceterm",
            "campaign_id": "campaign-a", "session_id": "session-a", "nonce": nonce,
            "plan_sha256": "44" * 32, "plan_metadata_sha256": "45" * 32,
            "pair_metadata_sha256": "46" * 32, "run_intent_sha256": run_intent_hash,
            "command_sha256": "47" * 32, "environment_sha256": "48" * 32,
            "font_sha256": "49" * 32, "initial_grid_sha256": "4a" * 32,
            "subject_identity_sha256": digest(identity), "subject_process_pid": "4242",
            "subject_process_start_identity": "100:200",
            "expected_driver_events_path": str(root / "driver-events.tsv"),
            "expected_driver_parent_device": str(root_identity.st_dev),
            "expected_driver_parent_inode": str(root_identity.st_ino),
            "action_video_path": str(root / "actions.mov"),
            "action_video_parent_device": str(root_identity.st_dev),
            "action_video_parent_inode": str(root_identity.st_ino),
            "final_metadata_path": str(render_evidence),
            "final_metadata_parent_device": str(root_identity.st_dev),
            "final_metadata_parent_inode": str(root_identity.st_ino),
            "warmup_ms": "15000", "measured_duration_ms": "120000",
            "required_action_count": "60", "action_interval_ms": "2000",
            "hmac_key_identifier_sha256": key_id,
        }
        verify_case_args = [
            "verify-case", "--manifest", str(manifest),
            "--campaign-secret-file", str(secret), "--subject-identity", str(identity),
            "--render-intent", str(render_intent), "--campaign-id", "campaign-a",
            "--session-id", "session-a", "--nonce", nonce,
            "--scenario", "perf-render-idle-cursor-blink", "--subject", "spaceterm",
            "--render-profile-hmac", str(tools["render-profile-hmac"]),
            "--render-trace-receipt-helper", str(tools["render-trace-receipt-helper"]),
            "--process-inspector", str(tools["process-inspector"]),
            "--trace-verifier", str(tools["trace-verifier"]),
            "--command-runner", str(tools["command-runner"]),
            *bundle_args,
        ]
        for field, wrong_value in (
            ("campaign_id", "campaign-b"), ("session_id", "session-b"), ("nonce", "55" * 32)
        ):
            wrong_intent = dict(intent_values)
            wrong_intent[field] = wrong_value
            authenticated_kv(
                render_intent, INTENT_KEYS, wrong_intent,
                b"SPACETERM_RENDER_PROFILE_INTENT_V1\0", secret_hex,
            )
            command(*verify_case_args, expect=1)
            render_intent.unlink()
        authenticated_kv(
            render_intent, INTENT_KEYS, intent_values,
            b"SPACETERM_RENDER_PROFILE_INTENT_V1\0", secret_hex,
        )
        command(*verify_case_args)
        evidence_values = {
            "format_version": "1", "canonicalization": "utf8-lf-tab-kv-fixed-order-domain-nul-v1",
            "auth_domain": "SPACETERM_RENDER_PROFILE_EVIDENCE_V1",
            "intent_sha256": digest(render_intent), "scenario": intent_values["scenario"],
            "subject": "spaceterm", "campaign_id": "campaign-a", "session_id": "session-a",
            "nonce": nonce, "subject_identity_sha256": digest(identity),
            "subject_process_pid": "4242", "subject_process_start_identity": "100:200",
            "driver_events_path": str(root / "driver-events.tsv"), "driver_events_device": "1",
            "driver_events_inode": "2", "driver_events_sha256": "5a" * 32,
            "action_video_path": str(root / "actions.mov"), "action_video_device": "1",
            "action_video_inode": "3", "action_video_sha256": "5b" * 32,
            "render_workload_metadata_sha256": "5c" * 32, "required_action_count": "60",
            "completed_action_count": "60", "action_interval_ms": "2000",
            "started_continuous_ns": "5000000000", "ended_continuous_ns": "6000000000",
            "measured_span_ns": "1000000000", "result": "verified",
            "hmac_key_identifier_sha256": key_id,
        }
        authenticated_kv(
            render_evidence, EVIDENCE_KEYS, evidence_values,
            b"SPACETERM_RENDER_PROFILE_EVIDENCE_V1\0", secret_hex,
        )
        for path, body in ((driver_intent, "driver intent\n"),
                           (driver_receipt, "driver receipt\n")):
            write(path, body)
        archive = write(root / "trace.zip", "archive\n")
        toc = write(root / "toc.xml", "<toc/>\n")
        time_profile = write(root / "time.xml", "<time/>\n")
        allocations = write(root / "alloc.xml", "<alloc/>\n")
        hangs = write(root / "hangs.xml", "<hangs/>\n")
        action_video = write(root / "actions.mov", "video\n")
        screenshot = write(root / "stacks.png", "screenshot\n")
        verification = write(root / "verification.tsv", "".join((
            "reason\tnone\n", "trace_started_at\tdate\n", "trace_ended_at\tdate\n",
            "trace_started_epoch_ns\t2000000000\n", "trace_ended_epoch_ns\t3000000000\n",
            "actual_record_duration_seconds\t1.000000\n", "time_profiler_rows\t2\n",
            "allocations_rows\t1\n", "hangs_rows\t0\n", "maximum_main_thread_hang_ms\t0.000000\n",
        )))
        trace_values = {
            "format_version": "3", "capture_status": "CAPTURED", "incomplete_reason": "none",
            "subject_identity_sha256": digest(identity), "run_metadata_sha256": digest(run),
            "workload_metadata_sha256": ZERO, "workload_ready_receipt_sha256": ZERO,
            "supplemental_evidence_sha256": digest(render_evidence), "requested_duration_ms": "1000",
            "actual_duration_ms": "1000", "capture_started_continuous_ns": "5000000000",
            "capture_ended_continuous_ns": "6000000000", "target_identity_verified": "true",
            "trace_target_pid_verified": "true", "time_profiler_instrument": "true",
            "allocations_instrument": "true", "hangs_instrument": "true",
            "time_profiler_target_verified": "true", "allocations_target_verified": "true",
            "hangs_target_verified": "true", "time_profiler_rows": "2", "allocations_rows": "1",
            "hangs_rows": "0", "maximum_main_thread_hang_ms": "0.000000", "status": "complete",
        }
        trace_metadata = kv(root / "trace.tsv", TRACE_KEYS, trace_values)
        anchor_args = [
            "anchor", "--manifest", str(manifest), "--campaign-secret-file", str(secret),
            "--subject-identity", str(identity), "--run-metadata", str(run),
            "--render-intent", str(render_intent), "--render-evidence", str(render_evidence),
            "--trace-metadata", str(trace_metadata), "--trace-started-epoch-ns", "2000000000",
            "--trace-ended-epoch-ns", "3000000000", "--start-anchor-continuous-ns", "4900000000",
            "--start-anchor-epoch-ns", "1900000000", "--start-anchor-width-ns", "1000",
            "--end-anchor-continuous-ns", "6100000000", "--end-anchor-epoch-ns", "3100000000",
            "--end-anchor-width-ns", "1000",
            "--render-profile-hmac", str(tools["render-profile-hmac"]),
            "--render-trace-receipt-helper", str(tools["render-trace-receipt-helper"]),
            "--process-inspector", str(tools["process-inspector"]),
            "--trace-verifier", str(tools["trace-verifier"]),
            "--command-runner", str(tools["command-runner"]),
            *bundle_args,
            "--output", str(anchor_receipt),
        ]
        command(*anchor_args)
        common = [
            "--manifest", str(manifest), "--campaign-secret-file", str(secret),
            *bundle_args,
            "--trace-anchor-receipt", str(anchor_receipt),
            "--subject-identity", str(identity), "--run-metadata", str(run),
            "--render-intent", str(render_intent), "--render-evidence", str(render_evidence),
            "--driver-intent", str(driver_intent), "--driver-receipt", str(driver_receipt),
            "--trace-metadata", str(trace_metadata), "--trace-archive", str(archive),
            "--trace-toc", str(toc), "--time-profiler-artifact", str(time_profile),
            "--allocations-artifact", str(allocations), "--hangs-artifact", str(hangs),
            "--action-video", str(action_video),
            "--representative-stack-screenshot", str(screenshot),
            "--trace-verifier", str(tools["trace-verifier"]), "--trace-verification", str(verification),
            "--xcrun", str(tools["xcrun"]), "--sips", str(tools["sips"]),
            "--python", str(tools["python"]), "--ffprobe", str(tools["ffprobe"]),
            "--trace-archive-verifier", str(tools["trace-archive-verifier"]),
            "--action-video-verifier", str(tools["action-video-verifier"]),
            "--render-trace-receipt-verifier", str(tools["render-trace-receipt-verifier"]),
            "--driver-receipt-verifier", str(tools["driver-receipt-verifier"]),
            "--render-profile-hmac", str(tools["render-profile-hmac"]),
            "--render-trace-receipt-helper", str(tools["render-trace-receipt-helper"]),
            "--process-inspector", str(tools["process-inspector"]),
            "--command-runner", str(tools["command-runner"]),
            "--evidence-mode", "zero-workload",
        ]
        command("finalize", *common, "--output", str(receipt))
        verify = command("verify", *common, "--receipt", str(receipt))
        assert "reason\trender-trace-receipt-authenticated-and-bound" in verify.stdout
        assert len(manifest.read_text().splitlines()) == 48
        assert len(receipt.read_text().splitlines()) == 103

        forged_bundle = root / "forged-tool-bundle.tsv"
        forged_payload = bundle_manifest.read_bytes().replace(
            b"finalize_render_profile_evidence_source_sha256\t" + digest(
                tools["trace-archive-verifier"]
            ).encode(),
            b"finalize_render_profile_evidence_source_sha256\t" + b"f" * 64,
        ).replace(
            b"finalize_render_profile_evidence_bundle_sha256\t" + digest(
                tools["trace-archive-verifier"]
            ).encode(),
            b"finalize_render_profile_evidence_bundle_sha256\t" + b"f" * 64,
        )
        forged_bundle.write_bytes(forged_payload)
        forged_bundle.chmod(0o444)
        forged_manifest_args = list(manifest_args)
        forged_manifest_args[forged_manifest_args.index("--render-tool-bundle-manifest") + 1] = str(
            forged_bundle
        )
        command(*forged_manifest_args, expect=1)

        valid_evidence = render_evidence.read_bytes()
        render_evidence.unlink()
        wrong_evidence = dict(evidence_values)
        wrong_evidence["session_id"] = "session-b"
        authenticated_kv(
            render_evidence, EVIDENCE_KEYS, wrong_evidence,
            b"SPACETERM_RENDER_PROFILE_EVIDENCE_V1\0", secret_hex,
        )
        command("verify", *common, "--receipt", str(receipt), expect=1)
        render_evidence.unlink()
        render_evidence.write_bytes(valid_evidence)
        render_evidence.chmod(0o444)

        tampered = root / "tampered.tsv"
        receipt.chmod(0o600)
        tampered.write_bytes(receipt.read_bytes().replace(b"result\tPASS", b"result\tFAIL"))
        tampered.chmod(0o444)
        receipt.chmod(0o444)
        command("verify", *common, "--receipt", str(tampered), expect=1)

        old_archive = root / "old.zip"
        shutil.copy2(archive, old_archive)
        old_archive.chmod(0o644)
        old_archive.write_text("prior archive\n")
        old_archive.chmod(0o444)
        replay = list(common)
        replay[replay.index("--trace-archive") + 1] = str(old_archive)
        command("verify", *replay, "--receipt", str(receipt), expect=1)

        swapped_export = root / "time-old.xml"
        write(swapped_export, "<old-time/>\n")
        export_replay = list(common)
        export_replay[export_replay.index("--time-profiler-artifact") + 1] = str(swapped_export)
        command("verify", *export_replay, "--receipt", str(receipt), expect=1)

        mutated_video = root / "actions-old.mov"
        write(mutated_video, "old video\n")
        media_replay = list(common)
        media_replay[media_replay.index("--action-video") + 1] = str(mutated_video)
        command("verify", *media_replay, "--receipt", str(receipt), expect=1)

        forged_anchor = root / "forged-anchor.tsv"
        forged_anchor.write_bytes(anchor_receipt.read_bytes().replace(
            b"start_anchor_epoch_ns\t1900000000", b"start_anchor_epoch_ns\t1800000000"))
        forged_anchor.chmod(0o444)
        wrong_anchor = list(common)
        wrong_anchor[wrong_anchor.index("--trace-anchor-receipt") + 1] = str(forged_anchor)
        command("verify", *wrong_anchor, "--receipt", str(receipt), expect=1)

        replacement_tool = tool_file(root, "ffprobe-new")
        replaced = list(common)
        replaced[replaced.index("--ffprobe") + 1] = str(replacement_tool)
        command("verify", *replaced, "--receipt", str(receipt), expect=1)

        for helper_name in ("render-profile-hmac", "render-trace-receipt-helper"):
            helper = tools[helper_name]
            original = root / f"{helper_name}-original"
            helper.rename(original)
            replacement = helper
            replacement.write_text(
                "#!/bin/sh\nprintf 'forged helper\\n'\n", encoding="utf-8",
            )
            replacement.chmod(0o555)
            if helper_name == "render-trace-receipt-helper":
                COMMAND_TOOL = original
            command("verify", *common, "--receipt", str(receipt), expect=1)
            helper.unlink()
            original.rename(helper)
            COMMAND_TOOL = receipt_tool
            command("verify", *common, "--receipt", str(receipt))

        writable_tool = tools["ffprobe"]
        writable_tool.chmod(0o755)
        command("verify", *common, "--receipt", str(receipt), expect=1)
        writable_tool.chmod(0o555)

        other_manifest = root / "other-manifest.tsv"
        other_receipt = root / "other-receipt.tsv"
        other_args = list(manifest_args)
        other_args[other_args.index("--session-id") + 1] = "session-b"
        other_args[other_args.index("--nonce") + 1] = "44" * 32
        other_args[other_args.index("--trace-receipt") + 1] = str(other_receipt)
        other_args[other_args.index("--trace-anchor-receipt") + 1] = str(root / "other-anchor.tsv")
        other_args[other_args.index("--output") + 1] = str(other_manifest)
        # Existing case paths intentionally cause manifest fail-closed: every case path is pre-capture absent.
        command(*other_args, expect=1)

        secret.chmod(0o600)
        command("verify", *common, "--receipt", str(receipt), expect=1)
        secret.chmod(0o400)
        replacement_secret = root / "replacement-secret"
        write(replacement_secret, "11" * 32 + "\n", 0o400)
        replaced_secret = list(common)
        replaced_secret[replaced_secret.index("--campaign-secret-file") + 1] = str(replacement_secret)
        command("verify", *replaced_secret, "--receipt", str(receipt), expect=1)

    print("render trace receipt fixtures passed")


if __name__ == "__main__":
    main()
