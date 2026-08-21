#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIR
readonly TOOL="$SCRIPT_DIR/issue-43-campaign-evidence.py"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-i43-finalizer-test.XXXXXX")"
readonly TEST_ROOT
trap 'rm -rf -- "$TEST_ROOT"' EXIT INT TERM

export HOME="$TEST_ROOT/home"
export TMPDIR="$TEST_ROOT/tmp"
mkdir -p -- "$HOME/SpaceTerm-Acceptance" "$TMPDIR"

fail() {
    echo "test failure: $*" >&2
    exit 1
}

expect_failure() {
    local label="$1"
    shift
    if "$@" >"$TEST_ROOT/expected-failure.stdout" 2>"$TEST_ROOT/expected-failure.stderr"; then
        fail "$label unexpectedly succeeded"
    fi
}

rehash_bundle() {
    local root="$1"
    python3 - "$root" <<'PY'
import hashlib
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
campaign_path = root / "campaign.yaml"
manifest_path = root / "artifacts.tsv"
campaign = json.loads(campaign_path.read_text(encoding="utf-8"))
campaign["payload_manifest"]["sha256"] = hashlib.sha256(manifest_path.read_bytes()).hexdigest()
campaign["payload_manifest"]["payload_rows"] = len(manifest_path.read_text(encoding="utf-8").splitlines()) - 1
campaign_path.write_text(json.dumps(campaign, ensure_ascii=True, indent=2, sort_keys=True) + "\n", encoding="utf-8")
control = (
    f"{hashlib.sha256(campaign_path.read_bytes()).hexdigest()}  campaign.yaml\n"
    f"{hashlib.sha256(manifest_path.read_bytes()).hexdigest()}  artifacts.tsv\n"
)
(root / "control.sha256").write_text(control, encoding="ascii")
PY
}

copy_case() {
    local label="$1"
    local parent="$TEST_ROOT/cases/$label"
    mkdir -p -- "$parent"
    cp -R -- "$BASELINE_ROOT" "$parent/$RUN_ID"
    chmod -R u+w "$parent/$RUN_ID"
    printf '%s' "$parent/$RUN_ID"
}

# The collector is used only for append-only CLI rejection tests. The complete
# replay fixture below is deliberately a FAIL campaign, never synthetic PASS.
ACTIVE_ROOT="$HOME/SpaceTerm-Acceptance/.acceptance-identity.TEST01"
mkdir -p -- \
    "$ACTIVE_ROOT/identity/dmg-stage" \
    "$ACTIVE_ROOT/identity/dmg-mount/SpaceTerm.app/Contents/MacOS" \
    "$ACTIVE_ROOT/logs" \
    "$ACTIVE_ROOT/workspace"
chmod 0700 "$ACTIVE_ROOT"
printf 'issue 43 adversarial test DMG fixture\n' > "$ACTIVE_ROOT/identity/dmg-stage/staged-package.dmg"
printf '#!/bin/sh\nexit 0\n' > "$ACTIVE_ROOT/identity/acceptance-launch-verifier"
chmod 0700 "$ACTIVE_ROOT/identity/acceptance-launch-verifier"
printf 'authenticated mounted app is ready; quit it after acceptance completes\n' \
    > "$ACTIVE_ROOT/logs/native-launch.stderr"

DMG_SHA="$(shasum -a 256 "$ACTIVE_ROOT/identity/dmg-stage/staged-package.dmg" | awk '{ print $1 }')"
readonly DMG_SHA
RUN_ID="i43-20260813T000000Z-aaaaaaaaaaaa-${DMG_SHA:0:12}"
readonly RUN_ID
"$TOOL" init --run-id "$RUN_ID" >/dev/null

BASELINE_PARENT="$TEST_ROOT/baseline"
BASELINE_ROOT="$BASELINE_PARENT/$RUN_ID"
readonly BASELINE_ROOT
mkdir -p -- "$BASELINE_ROOT"

python3 - "$TOOL" "$BASELINE_ROOT" "$RUN_ID" "$DMG_SHA" "$TEST_ROOT" <<'PY'
import base64
import csv
import hashlib
import importlib.util
import json
import pathlib
import sys

tool_path = pathlib.Path(sys.argv[1])
root = pathlib.Path(sys.argv[2])
run_id = sys.argv[3]
dmg_sha = sys.argv[4]
test_root = pathlib.Path(sys.argv[5])
spec = importlib.util.spec_from_file_location("issue43_campaign_evidence", tool_path)
assert spec and spec.loader
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

started = "2026-08-13T00:00:00Z"
record_started = "2026-08-13T00:10:00Z"
record_finished = "2026-08-13T00:20:00Z"
artifact_created = "2026-08-13T00:21:00Z"
reviewed = "2026-08-13T00:30:00Z"
finished = "2026-08-13T01:00:00Z"

for directory in sorted(module.PAYLOAD_DIRS):
    (root / directory).mkdir()

def canonical(value):
    return (json.dumps(value, ensure_ascii=True, indent=2, sort_keys=True) + "\n").encode("utf-8")

def digest(value):
    return hashlib.sha256(value).hexdigest()

def record_id(case_id, subject):
    return f"{run_id}-{case_id}-{subject}-a01"

def conditional_subcases(case_id, subject):
    result = []
    if case_id == "capability-keyboard" and subject == "spaceterm":
        result.append({
            "name": "numpad input where available",
            "status": "SKIPPED-UNAVAILABLE",
            "availability_or_precondition_evidence": "test host records no numpad",
        })
    if case_id == "focus-non-key-os-window" and subject == "spaceterm":
        result.append({
            "name": "non-key while SpaceTerm remains active where possible",
            "status": "SKIPPED-UNAVAILABLE",
            "availability_or_precondition_evidence": "test host records no suitable non-key window",
        })
    if case_id == "capability-resize-scrollback" and subject == "spaceterm":
        result.append({
            "name": "backing-scale/display movement when a second display is available",
            "status": "SKIPPED-UNAVAILABLE",
            "availability_or_precondition_evidence": "test host records one display",
        })
    if case_id == "perf-resize":
        result.append({
            "name": "backing-scale/display movement when a second display is available",
            "status": "SKIPPED-UNAVAILABLE",
            "availability_or_precondition_evidence": "test host records one display",
        })
    if case_id in {"native-claude-code", "native-pi-coding-agent"} and subject == "spaceterm":
        result.append({
            "name": "detected/OSC 8 link if presented",
            "status": "NOT-APPLICABLE",
            "availability_or_precondition_evidence": "no link was presented in this NOT-RUN fixture",
        })
    if case_id == "package-doctor" and subject == "spaceterm":
        result.append({
            "name": "just doctor when tool availability is known",
            "status": "NOT-APPLICABLE",
            "availability_or_precondition_evidence": "fixture tool availability is pre-recorded",
        })
    return result

records = []
rows = []
record_reviews = []
artifact_reviews = []
comparison_inputs = {
    "workload_sha256": "1" * 64,
    "duration_seconds": 600,
    "warmup_seconds": 60,
    "font_sha256": "2" * 64,
    "grid_sha256": "3" * 64,
    "configuration_sha256": "4" * 64,
    "shell_process_sha256": "5" * 64,
    "input_sha256": "6" * 64,
    "host_identity_sha256": "7" * 64,
    "scenario_settings_sha256": "8" * 64,
}
comparison_digest = digest(canonical(comparison_inputs))
program_names = ["Bash", "Zsh", "Vim", "Neovim", "tmux", "less", "fzf", "btop", "Yazi", "Claude Code", "pi-coding-agent"]
program_ids = ["bash", "zsh", "vim", "neovim", "tmux", "less", "fzf", "btop", "yazi", "claude-code", "pi-coding-agent"]
ghostty_reference = {
    "original_prd_revision": "46767b521358200bfe3f268f365ccd2f218db558",
    "embedded_conformance_revision": "a887df42c56f6de86c0fe6da9c4eeca37931e083",
    "runnable_build_source": "not executed structural fixture",
    "public_version": "not executed",
    "commit_sha": "unavailable for structural fixture",
    "marketing_version": "not executed",
    "build_version": "not executed",
    "executable": "$HOME/Applications/Ghostty.app/Contents/MacOS/ghostty",
    "executable_architectures": ["arm64"],
    "code_signing_result": "PASS",
    "app_bundle_sha256": "f" * 64,
    "config_path": "$TMPDIR/issue-43/ghostty-config",
    "config_sha256": "9" * 64,
    "selected_font": "test monospace",
    "initial_grid": {"rows": 1, "columns": 1, "logical_width": 1, "logical_height": 1, "backing_pixel_width": 1, "backing_pixel_height": 1},
    "behavior_settings_sha256": "8" * 64,
    "relationship_to_recorded_revisions": "reference not executed",
    "ambiguity_notes": "structural FAIL campaign only",
}

png_payload = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="
)
display_payload = (
    "display\tname\tphysical_pixels\tlogical_resolution_refresh\tbacking_scale\tmain\tonline\n"
    "0\ttest display\t1 x 1\t1 x 1 @ 60\t1\tyes\tyes\n"
).encode("utf-8")
ghostty_identity_fields = {"schema": "spaceterm.acceptance.ghostty-identity/v1"}
for key, value in ghostty_reference.items():
    ghostty_identity_fields[f"ghostty.{key}"] = value if isinstance(value, str) else json.dumps(
        value, ensure_ascii=True, sort_keys=True, separators=(",", ":")
    )
ghostty_identity_payload = b"".join(
    f"{key}\t{ghostty_identity_fields[key]}\n".encode("utf-8")
    for key in sorted(ghostty_identity_fields)
)
host_preconditions_payload = (
    "schema\tspaceterm.acceptance.host-preconditions/v1\n"
    "numpad_available\tfalse\n"
    "non_key_window_possible\tfalse\n"
    f"input_sources_sha256\t{digest(canonical(['test ordinary', 'test non-US']))}\n"
    "review\tPASS\n"
).encode("utf-8")
identity_lines = [
    f"run.id\t{run_id}\n"
    "run.origin\tmounted-dmg\n"
    f"repository.commit\t{'a' * 40}\n"
    f"repository.cargo_lock_sha256\t{'c' * 64}\n"
    "repository.clean\ttrue\n"
    "package.app.marketing_version\t0.0.0\n"
    "package.app.build_version\t1\n"
    "package.app.executable.architectures\tarm64\n"
    f"package.app.sha256\t{'d' * 64}\n"
    "package.app.signature.verified\ttrue\n"
    f"package.dmg.sha256\t{dmg_sha}\n"
    "package.dmg.verified\ttrue\n"
    "host.macos.product_version\t26.6.1\n"
    "host.macos.build_version\t25G90\n"
    "host.machine.model\tMac-test\n"
    "host.cpu\ttest Apple silicon\n"
    "host.memory_bytes\t1\n"
    "host.terminal_font_selected\ttest monospace\n"
    "font.jetbrainsmono-nerd-font.available\tfalse\n"
    "host.initial_grid.rows\t1\n"
    "host.initial_grid.columns\t1\n"
    "host.initial_grid.logical_width\t1\n"
    "host.initial_grid.logical_height\t1\n"
    "host.initial_grid.backing_pixel_width\t1\n"
    "host.initial_grid.backing_pixel_height\t1\n"
    "host.display.count\t1\n"
    f"host.display.summary_sha256\t{digest(display_payload)}\n"
]
for name, executable_id in zip(program_names, program_ids):
    identity_lines.extend([
        f"executable.{executable_id}.status\tavailable\n",
        f"executable.{executable_id}.path\t$EXECUTABLE\n",
        f"executable.{executable_id}.sha256\t{digest(name.encode('utf-8'))}\n",
        f"executable.{executable_id}.version\tnot executed in structural fixture\n",
    ])
identity_payload = "".join(identity_lines).encode("utf-8")
identity_digest = digest(identity_payload)
comparison_inputs["host_identity_sha256"] = identity_digest
comparison_inputs["font_sha256"] = digest(b"test monospace")
comparison_inputs["grid_sha256"] = digest(canonical({
    "rows": 1, "columns": 1, "logical_width": 1, "logical_height": 1,
    "backing_pixel_width": 1, "backing_pixel_height": 1,
}))
comparison_digest = digest(canonical(comparison_inputs))
identity_replay = {
    "command": "scripts/acceptance-identity.sh verify --run-dir $RUN_DIR --final",
    "completed_utc": "2026-08-13T00:40:00Z",
    "public_identity_sha256": identity_digest,
    "run_id": run_id,
    "schema": "spaceterm.acceptance.final-identity-replay/v1",
    "status": "PASS",
    "stderr_sha256": "d" * 64,
    "stdout_sha256": "c" * 64,
    "verifier_sha256": "a" * 64,
}
identity_replay_payload = b"".join(
    f"{key}\t{identity_replay[key]}\n".encode("utf-8") for key in sorted(identity_replay)
)
for matrix, cases in module.MATRIX_CASES.items():
    subjects = ("spaceterm", "ghostty") if matrix == "performance" else ("spaceterm",)
    for case_id in cases:
        for subject in subjects:
            rid = record_id(case_id, subject)
            text_payload = (f"Structural NOT-RUN evidence marker for {case_id}/{subject}.\n").encode("utf-8")
            artifact_specs = [("evidence", "txt", "text/plain", text_payload)]
            if matrix == "focus":
                artifact_specs = [("pty-bytes", "txt", "text/plain", text_payload)]
            elif matrix == "performance":
                artifact_specs = [
                    ("time-profiler", "txt", "text/plain", text_payload),
                    ("allocations", "txt", "text/plain", text_payload),
                    ("screen", "png", "image/png", png_payload),
                ]
                if case_id not in module.RENDER_CASES:
                    artifact_specs.append(("rss", "tsv", "text/tab-separated-values", text_payload))
            elif case_id == "package-identity":
                artifact_specs.extend([
                    ("public-identity", "tsv", "text/tab-separated-values", identity_payload),
                    ("final-identity-replay", "tsv", "text/tab-separated-values", identity_replay_payload),
                    ("display-summary", "tsv", "text/tab-separated-values", display_payload),
                    ("ghostty-identity", "tsv", "text/tab-separated-values", ghostty_identity_payload),
                    ("host-preconditions", "tsv", "text/tab-separated-values", host_preconditions_payload),
                ])
            artifact_ids = [f"{rid}-{kind}" for kind, _extension, _media, _payload in artifact_specs]
            primary_artifact_id = artifact_ids[0]
            artifact_id_by_kind = {
                kind: f"{rid}-{kind}" for kind, _extension, _media, _payload in artifact_specs
            }
            opposite = None
            if matrix == "performance":
                opposite_subject = "ghostty" if subject == "spaceterm" else "spaceterm"
                opposite = record_id(case_id, opposite_subject)
            clauses = list(module.required_case_clauses(case_id, subject))
            record = {
                "record_id": rid,
                "case_id": case_id,
                "subject": subject,
                "matrix": matrix,
                "attempt": 1,
                "comparison_record_id": opposite,
                "supersedes_record_id": None,
                "status": "NOT-RUN",
                "started_utc": record_started,
                "finished_utc": record_finished,
                "frozen_identity_verified": False,
                "command": "not executed; structural negative-test fixture",
                "environment_and_config": "$TMPDIR/issue-43/negative-fixture",
                "interactions": [{
                    "order": 1,
                    "action": "inventory the exact clauses without claiming execution",
                    "timing": "not executed",
                    "clause_ids": clauses,
                }],
                "expected": "the authoritative row would be exercised by a real acceptance operator",
                "authority": "issue 43 and published platform protocols",
                "observed": "NOT-RUN structural fixture; no product behavior is claimed",
                "artifacts": artifact_ids,
                "comparison_observation": "NOT-RUN comparison fixture",
                "smallest_reproduction": "none",
                "skip_reason": "none",
                "conditional_subcases": conditional_subcases(case_id, subject),
                "requirement_checks": [{
                    "clause_id": clause,
                    "requirement": f"authoritative clause {clause}",
                    "status": "NOT-RUN",
                    "evidence_artifact_ids": [primary_artifact_id],
                } for clause in clauses],
                "notes": "This fixture validates bundle structure and rejection behavior only.",
            }
            if case_id in {"native-claude-code", "native-pi-coding-agent"}:
                record["link_presented"] = False
            if matrix == "focus":
                record.update({
                    "focused_pane_identity_before": "pane-fixture",
                    "focused_pane_identity_blocked": "pane-fixture",
                    "terminal_input_focus_before": False,
                    "terminal_input_focus_blocked": False,
                    "terminal_input_focus_restored": False,
                    "cursor_negotiated_before": "block blinking",
                    "cursor_blocked": "not observed",
                    "cursor_restored": "not observed",
                    "hollow_visible_on_next_presented_frame": False,
                    "dec_1004": {
                        "enabled": False,
                        "enable_current_state_bytes_hex": "00",
                        "loss_bytes_hex": "00",
                        "gain_bytes_hex": "00",
                        "duplicate_reports": 0,
                        "held_key_release_bytes_hex": "00",
                        "pty_artifact_id": primary_artifact_id,
                    },
                })
            elif matrix == "failure":
                record.update({
                    "injection_or_trigger": "not executed",
                    "presentation_generation_before": 0,
                    "presentation_generation_visible_during_failure": 0,
                    "visible_state": "not observed",
                    "terminal_input_usable_during_failure": False,
                    "recovery_action": "not executed",
                    "post_recovery_result": "not observed",
                    "owned_processes_remaining": 0,
                    "diagnostics_bytes": 0,
                    "diagnostics_content_audit": "NOT-RUN",
                })
            elif matrix == "performance":
                record.update({
                    "comparison_inputs": comparison_inputs,
                    "comparison_inputs_sha256": comparison_digest,
                    "subject_identity_sha256": identity_digest if subject == "spaceterm" else digest(ghostty_identity_payload),
                })
                if case_id in module.RENDER_CASES:
                    record.update({
                        "trace_duration_seconds": comparison_inputs["duration_seconds"],
                        "sampling_settings": "not executed",
                        "process_identity": "not executed",
                        "inspected_call_tree_filters": "not executed",
                        "time_profiler_artifact_id": artifact_id_by_kind["time-profiler"],
                        "allocations_artifact_id": artifact_id_by_kind["allocations"],
                        "screen_artifact_ids": [artifact_id_by_kind["screen"]],
                    })
                    if subject == "spaceterm":
                        record.update({
                            "paint_text_shaping_stack_present": False,
                            "paint_path_or_plan_construction_present": False,
                            "paint_normal_frame_allocation_stack_present": False,
                            "cursor_or_blink_reshaped_unchanged_rows": False,
                            "changed_row_proportionality_result": "NOT-RUN",
                            "exceptional_error_allocations_excluded": False,
                        })
                    else:
                        record["reference_render_observation"] = "NOT-RUN reference fixture"
                else:
                    record.update({
                        "optimization_profile": "release",
                        "workload_command": "not executed",
                        "workload_input_sha256": comparison_inputs["workload_sha256"],
                        "duration_seconds": comparison_inputs["duration_seconds"],
                        "warmup_seconds": comparison_inputs["warmup_seconds"],
                        "bytes_processed": 1,
                        "initial_grid": "40x120/1440x800",
                        "rss_samples_artifact_id": artifact_id_by_kind["rss"],
                        "rss_sample_interval_seconds": 10,
                        "first_post_warmup_five_minutes": {"minimum_bytes": 0, "maximum_bytes": 0, "range_bytes": 0},
                        "final_five_minutes": {"minimum_bytes": 0, "maximum_bytes": 0, "range_bytes": 0},
                        "allowed_range_delta_bytes": 67108864,
                        "memory_plateau_result": "FAIL",
                        "maximum_main_thread_stall_ms": 0,
                        "input_responsiveness_observation": "not observed",
                        "ui_backlog_observation": "not observed",
                        "final_presentation_observation": "not observed",
                        "shell_process_exit_observation": "not observed",
                        "time_profiler_artifact_id": artifact_id_by_kind["time-profiler"],
                        "allocations_artifact_id": artifact_id_by_kind["allocations"],
                        "screen_artifact_ids": [artifact_id_by_kind["screen"]],
                    })
                    if case_id == "perf-resize":
                        record.update({
                            "resize_count": 1,
                            "reflow_timings": "not observed",
                            "pty_geometry_samples": "not observed",
                            "final_grid": "not observed",
                            "selection_anchoring": "not observed",
                            "viewport_anchoring": "not observed",
                            "backing_scale_transition": "not observed",
                            "second_display_available": False,
                        })
            # Validate each generated record before it becomes the replay baseline.
            module.validate_record(record, run_id)
            records.append(record)
            record_reviews.append({
                "record_id": rid,
                "record_sha256": digest(canonical(record)),
                "artifact_inventory_sha256": "0" * 64,
                "reviewer_role": module.RECORD_REVIEWER_ROLE,
                "reviewer": "github:negative-fixture",
                "review_url": "https://api.github.com/repos/sadiksaifi/SpaceTerm/issues/comments/1",
                "reviewed_utc": reviewed,
                "decision": "PASS",
                "attestation": module.RECORD_REVIEW_ATTESTATION,
            })
            for kind, extension, media_type, payload in artifact_specs:
                artifact_id = artifact_id_by_kind[kind]
                artifact_directory = "identity" if kind in {
                    "public-identity", "final-identity-replay", "display-summary",
                    "ghostty-identity",
                    "host-preconditions",
                } else matrix
                relative_path = f"{artifact_directory}/{case_id}--{subject}--01--{kind}.{extension}"
                (root / relative_path).write_bytes(payload)
                rows.append({
                    "artifact_id": artifact_id,
                    "record_id": rid,
                    "subject": subject,
                    "case_id": case_id,
                    "relative_path": relative_path,
                    "sha256": digest(payload),
                    "bytes": str(len(payload)),
                    "media_type": media_type,
                    "created_utc": artifact_created,
                    "run_id": run_id,
                    "producer": "issue-43-negative-fixture",
                    "producer_version": "1",
                    "privacy_review": "PASS",
                    "redaction_notes": "content-free structural marker",
                    "public_url": f"https://github.com/sadiksaifi/SpaceTerm/{relative_path}",
                    "content_class": "content-free",
                })
                artifact_reviews.append({
                    "artifact_id": artifact_id,
                    "artifact_sha256": digest(payload),
                    "reviewer_role": module.ARTIFACT_REVIEWER_ROLE,
                    "reviewer": "github:negative-privacy-fixture",
                    "review_url": "https://api.github.com/repos/sadiksaifi/SpaceTerm/issues/comments/2",
                    "reviewed_utc": reviewed,
                    "decision": "PASS",
                    "attestation": module.ARTIFACT_REVIEW_ATTESTATION,
                })

metadata = {
    "schema_version": 2,
    "issue": 43,
    "run_id": run_id,
    "started_utc": started,
    "frozen_artifact": {
        "repository": "https://github.com/sadiksaifi/SpaceTerm",
        "commit_sha": "a" * 40,
        "cargo_lock_sha256": "c" * 64,
        "working_tree_clean": True,
        "package_command": "just package",
        "marketing_version": "0.0.0",
        "build_version": "1",
        "executable_architectures": ["arm64"],
        "code_signing_command": "codesign --verify $APP_BUNDLE",
        "code_signing_result": "PASS",
        "app_bundle_sha256": "d" * 64,
        "dmg_sha256": dmg_sha,
        "package_verification_artifact": f"{record_id('package-build', 'spaceterm')}-evidence",
        "launch_source": "mounted verified DMG",
    },
    "host": {
        "macos_version": "26.6.1",
        "macos_build": "25G90",
        "machine_model": "Mac-test",
        "model_identifier": "Mac-test",
        "cpu": "test Apple silicon",
        "memory_bytes": 1,
        "displays": [{"display_id": "display-1", "logical_resolution": "1x1", "backing_resolution": "1x1", "refresh_hz": 60, "backing_scale": 1}],
        "terminal_font_selected": "test monospace",
        "jetbrains_mono_nerd_font_available": False,
        "initial_grid": {"rows": 1, "columns": 1, "logical_width": 1, "logical_height": 1, "backing_pixel_width": 1, "backing_pixel_height": 1},
        "input_sources": ["test ordinary", "test non-US"],
        "second_display_available": False,
        "numpad_available": False,
        "non_key_window_possible": False,
    },
    "clean_environment": {
        "workspace_root": "$TMPDIR/issue-43/negative-fixture",
        "temporary_configurations": [{"program": "all", "path": "$TMPDIR/issue-43/config", "sha256": "e" * 64}],
        "permanent_user_configuration_used": False,
        "privacy_review": "PASS",
    },
    "programs": [{
        "name": name,
        "executable": "$EXECUTABLE",
        "executable_sha256": digest(name.encode("utf-8")),
        "version_command": f"{name} --version",
        "version_output": "not executed in structural fixture",
    } for name in program_names],
    "ghostty_reference": ghostty_reference,
    "drivers": [{
        "purpose": purpose,
        "path": path,
        "commit_sha": "a" * 40,
        "sha256": digest(
            (tool_path.parents[2] / path).read_bytes()
            if (tool_path.parents[2] / path).is_file() else path.encode("utf-8")
        ),
        "version_or_help": "repository driver",
        "invocation": "negative structural fixture",
    } for purpose, path in [
        ("payload-manifest", "scripts/acceptance/issue-43-campaign-evidence.py"),
        ("identity", "scripts/acceptance-identity.sh"),
        ("native-launch-verifier", "scripts/acceptance-launch-verifier.m"),
        ("failure-action-driver", "scripts/acceptance/failure-action-driver.sh"),
        ("accessibility-probe", "scripts/acceptance/native-ax-probe.sh"),
        ("accessibility-probe-source", "scripts/acceptance/native-ax-probe.m"),
        ("workload", "scripts/acceptance/freeze-performance-pair.sh"),
        ("run-intent", "scripts/acceptance/freeze-performance-run-intent.sh"),
        ("native", "scripts/acceptance/freeze-performance-run.sh"),
        ("focus", "scripts/acceptance/freeze-performance-subject.sh"),
        ("pair-result", "scripts/acceptance/performance-pair-result.py"),
        ("driver-receipt", "scripts/acceptance/performance-driver-receipt.py"),
        ("lifecycle", "scripts/acceptance/performance-subject-lifecycle.py"),
        ("tail-receipt", "scripts/acceptance/performance-tail-receipt.py"),
        ("lifecycle-verifier", "scripts/acceptance/verify-performance-lifecycle-receipts.py"),
        ("exit-verifier", "scripts/acceptance/verify-performance-subject-exit.py"),
        ("workload-ready-verifier", "scripts/acceptance/verify-performance-workload-ready.py"),
        ("workload-auth-verifier", "scripts/acceptance/verify-performance-workload-auth.py"),
        ("native-runner", "scripts/acceptance/run-native-performance-scenario.sh"),
        ("native-tools", "scripts/acceptance/build-native-performance-tools.sh"),
        ("rss", "scripts/acceptance/assemble-release-performance-rss-v3.sh"),
        ("failure", "scripts/acceptance/analyze-release-performance-sustained.awk"),
        ("package", "scripts/acceptance/analyze-release-performance-resize.awk"),
        ("control-digest", "scripts/acceptance/analyze-release-performance-case.sh"),
        ("performance-plan", "scripts/acceptance/performance-plan.sh"),
        ("performance-workload", "scripts/acceptance/performance-workload.sh"),
        ("performance-workload-source", "scripts/acceptance/performance-workload.c"),
        ("rss-sampler", "scripts/acceptance/performance-rss-sampler.m"),
        ("window-resolver", "scripts/acceptance/performance-window-resolver.m"),
        ("appkit-terminator", "scripts/acceptance/performance-appkit-terminate.m"),
        ("performance-driver", "scripts/acceptance/performance-driver.m"),
        ("render-intent", "scripts/acceptance/freeze-render-profile-intent.sh"),
        ("render-finalizer", "scripts/acceptance/finalize-render-profile-evidence.sh"),
        ("render-receipt", "scripts/acceptance/render-trace-receipt.py"),
        ("render-archive", "scripts/acceptance/archive-render-trace.py"),
        ("render-archive-verifier", "scripts/acceptance/verify-render-trace-archive.py"),
        ("render-video-verifier", "scripts/acceptance/verify-render-action-video.py"),
        ("render-analyzer", "scripts/acceptance/analyze-release-render-profile-case.sh"),
        ("trace-recorder", "scripts/record-release-performance-trace.sh"),
        ("rss-sampler-wrapper", "scripts/sample-release-performance-rss.sh"),
        ("workload-wrapper", "scripts/release-performance-workload.sh"),
        ("process-inspector", "scripts/inspect-release-performance-process.py"),
        ("command-runner", "scripts/run-release-performance-command.py"),
        ("trace-verifier", "scripts/verify-release-performance-trace.py"),
    ]],
    "validation": {
        "command": "just validate",
        "status": "NOT-RUN",
        "artifact_id": f"{record_id('package-final-validate', 'spaceterm')}-evidence",
    },
    "issue_42_conformance": {
        "issue_url": "https://github.com/sadiksaifi/SpaceTerm/issues/42",
        "candidate_commit_sha": "a" * 40,
        "command": "just validate",
        "status": "NOT-RUN",
        "artifact_id": f"{record_id('package-final-validate', 'spaceterm')}-evidence",
    },
    "identity_evidence": {
        "public_identity_artifact_id": f"{record_id('package-identity', 'spaceterm')}-public-identity",
        "final_identity_replay_artifact_id": f"{record_id('package-identity', 'spaceterm')}-final-identity-replay",
        "display_summary_artifact_id": f"{record_id('package-identity', 'spaceterm')}-display-summary",
        "ghostty_identity_artifact_id": f"{record_id('package-identity', 'spaceterm')}-ghostty-identity",
        "host_preconditions_artifact_id": f"{record_id('package-identity', 'spaceterm')}-host-preconditions",
        "native_closure_replay_artifact_id": f"{record_id('package-identity', 'spaceterm')}-native-closure-replay",
        "native_runtime_metadata_artifact_id": f"{record_id('package-identity', 'spaceterm')}-native-runtime-metadata",
        "native_runtime_samples_artifact_id": f"{record_id('package-identity', 'spaceterm')}-native-runtime-samples",
        "native_runtime_events_artifact_id": f"{record_id('package-identity', 'spaceterm')}-native-runtime-events",
        "native_failure_actions_artifact_id": f"{record_id('package-identity', 'spaceterm')}-native-failure-actions",
    },
    "known_deviations": [],
}
module.validate_metadata(metadata, run_id)
metadata_sha = digest(canonical(metadata))
for review in record_reviews:
    record = next(item for item in records if item["record_id"] == review["record_id"])
    owned = [row for row in rows if row["record_id"] == record["record_id"]]
    inventory = [module.artifact_review_projection(row)
                 for row in sorted(owned, key=lambda item: item["artifact_id"])]
    review["artifact_inventory_sha256"] = digest(canonical(inventory))
    review["campaign_metadata_sha256"] = metadata_sha

with (root / "artifacts.tsv").open("w", encoding="utf-8", newline="") as handle:
    writer = csv.DictWriter(handle, fieldnames=module.ARTIFACT_HEADER, delimiter="\t", lineterminator="\n")
    writer.writeheader()
    writer.writerows(rows)
manifest_bytes = (root / "artifacts.tsv").read_bytes()
campaign = dict(metadata)
campaign.update({
    "campaign_status": "FAIL",
    "finished_utc": finished,
    "identity_replay": {key: value for key, value in identity_replay.items() if key not in {"run_id", "schema"}},
    "case_results": sorted(records, key=lambda item: item["record_id"]),
    "manual_review": {
        "record_reviews": sorted(record_reviews, key=lambda item: item["record_id"]),
        "artifact_reviews": sorted(artifact_reviews, key=lambda item: item["artifact_id"]),
    },
    "payload_manifest": {
        "path": "artifacts.tsv",
        "sha256": digest(manifest_bytes),
        "payload_rows": len(rows),
        "excluded_control_files": ["campaign.yaml", "artifacts.tsv", "control.sha256"],
        "privacy_review": "PASS",
    },
    "control_digest": {
        "path": "control.sha256",
        "algorithm": "sha256",
        "entries_in_order": ["campaign.yaml", "artifacts.tsv"],
        "digest_anchored_in": "final GitHub issue comment",
    },
})
(root / "campaign.yaml").write_bytes(canonical(campaign))
(root / "control.sha256").write_text(
    f"{digest((root / 'campaign.yaml').read_bytes())}  campaign.yaml\n{digest(manifest_bytes)}  artifacts.tsv\n",
    encoding="ascii",
)
for comment_id, kind, reviews in (
    (1, "records", campaign["manual_review"]["record_reviews"]),
    (2, "artifacts", campaign["manual_review"]["artifact_reviews"]),
):
    reviewer = reviews[0]["reviewer"].removeprefix("github:")
    (test_root / f"review-api-{comment_id}.json").write_text(
        json.dumps({
            "html_url": f"https://github.com/sadiksaifi/SpaceTerm/issues/43#issuecomment-{comment_id}",
            "body": module.review_batch_body(reviews, kind),
            "created_at": reviewed,
            "updated_at": reviewed,
            "author_association": "COLLABORATOR",
            "user": {"login": reviewer},
        }),
        encoding="utf-8",
    )
comment = module.render_issue_comment(
    campaign,
    rows,
    {
        "campaign": "https://github.com/sadiksaifi/SpaceTerm/campaign.yaml",
        "artifacts": "https://github.com/sadiksaifi/SpaceTerm/artifacts.tsv",
        "control": "https://github.com/sadiksaifi/SpaceTerm/control.sha256",
    },
    digest((root / "control.sha256").read_bytes()),
)
assert len(comment) <= 65536 and len(comment.encode("utf-8")) <= 65536

# Targeted input documents for append-only CLI validation.
native_bash = next(item for item in records if item["case_id"] == "native-bash")
(test_root / "valid-record.json").write_bytes(canonical(native_bash))
generic = json.loads(json.dumps(native_bash))
generic["interactions"][0]["clause_ids"] = ["complete-row"]
generic["requirement_checks"] = [{
    "clause_id": "complete-row",
    "requirement": "generic whole-row assertion",
    "status": "PASS",
    "evidence_artifact_ids": generic["artifacts"],
}]
generic["status"] = "PASS"
generic["frozen_identity_verified"] = True
(test_root / "generic-pass-record.json").write_bytes(canonical(generic))
path_leak = next(item for item in records if item["case_id"] == "native-zsh")
path_leak = json.loads(json.dumps(path_leak))
path_leak["notes"] = "/Users/private-account/acceptance-secret"
(test_root / "path-leak-record.json").write_bytes(canonical(path_leak))
PY

python3 - "$TOOL" "$TEST_ROOT" "$RUN_ID" <<'PY'
import importlib.util, pathlib, sys, zipfile
tool, root, run_id = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]), sys.argv[3]
spec = importlib.util.spec_from_file_location("issue43_contracts", tool)
assert spec and spec.loader
module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
assert (len(module.RUN_INTENT_V1_KEYS), len(module.RUN_METADATA_V4_KEYS),
        len(module.CASE_REPORT_V2_KEYS), len(module.PAIR_RESULT_V3_KEYS)) == (19, 35, 14, 62)
assert len(module.REQUIRED_SPACETERM) + len(module.PERFORMANCE_CASES) == 75
ax = root / "native-ax.tsv"
values = {
 "schema":"spaceterm.acceptance.native-ax-observation/v1", "run.id":run_id,
 "probe.binary.sha256":"1"*64, "subject.package.app.sha256":"2"*64,
 "subject.launch.nonce.sha256":"3"*64, "subject.launch.observation.sha256":"4"*64,
 "subject.failure-action.enabled":"false", "subject.process.pid":"123",
 "subject.process.start-sec":"1", "subject.executable.device":"1",
 "subject.executable.inode":"2", "subject.signature.cdhash":"abcd",
 "subject.signature.identifier.sha256":"5"*64,
 "subject.revalidated.before-query":"true",
 "subject.revalidated.before-mutation":"not-applicable",
 "subject.revalidated.after-observation":"true", "privacy.mode":"metadata-only",
 "privacy.axvalue-content-emitted":"false", "privacy.fixture-sha256":"none",
 "pane.role":"AXTextArea", "pane.label.sha256":"6"*64,
 "pane.label.matches":"true", "pane.count":"1", "pane.navigation-order":"0",
 "selection.requested":"false", "selection.generation-guard":"not-applicable",
 "notifications.baseline-continuous-ns":"1",
 "notifications.selection.dispatch-continuous-ns":"0",
 "notifications.selection.subscription-continuous-ns":"1",
 "notifications.observation-deadline-continuous-ns":"2",
 "notifications.clock":"mach-continuous",
 "notifications.target-identity":"pane-parent-and-same-pid-focus",
 "observation.complete":"true",
}
for prefix in ("before", "after"):
 for suffix, value in {
  "frame.x":"0.000", "frame.y":"0.000", "frame.width":"1.000",
  "frame.height":"1.000", "focused":"true", "utf16-count":"1",
  "visible-range":"0:1", "selected-range":"0:0", "cursor-empty":"true",
  "value-queried":"false", "selected-text-queried":"false",
 }.items(): values[f"{prefix}.{suffix}"] = value
for kind in ("value", "selection", "focus", "focus-target", "focus-other", "layout"):
 for suffix in ("count", "first-continuous-ns", "last-continuous-ns"):
  values[f"notifications.{kind}.{suffix}"] = "0"
ax.write_bytes(b"".join(f"{key}\t{values[key]}\n".encode() for key in sorted(values)))
record = {"record_id": f"{run_id}-capability-accessibility-spaceterm-a01"}
module.validate_native_ax_observation(ax, record)
archive = root / "nested.trace.zip"
with zipfile.ZipFile(archive, "w") as output:
 output.writestr("nested.trace/a", b"a"); output.writestr("nested.trace/deep/b", b"b")
first = module.trace_tree_sha256_from_zip(archive)
with zipfile.ZipFile(archive, "a") as output: output.writestr("nested.trace/deep/c", b"c")
second = module.trace_tree_sha256_from_zip(archive)
assert first != second
PY

cat > "$TEST_ROOT/sitecustomize.py" <<'PY'
import io
import os
import pathlib
import urllib.request

_original_urlopen = urllib.request.urlopen

class _Response(io.BytesIO):
    status = 200
    def __init__(self, payload, url):
        super().__init__(payload)
        self._url = url
    def geturl(self):
        return self._url
    def __enter__(self):
        return self
    def __exit__(self, *_args):
        self.close()

def _urlopen(request, *args, **kwargs):
    url = request.full_url if hasattr(request, "full_url") else str(request)
    prefix = "https://api.github.com/repos/sadiksaifi/SpaceTerm/issues/comments/"
    if url.startswith(prefix):
        comment_id = url.removeprefix(prefix)
        root = pathlib.Path(os.environ["I43_REVIEW_MOCK_ROOT"])
        return _Response((root / f"review-api-{comment_id}.json").read_bytes(), url)
    return _original_urlopen(request, *args, **kwargs)

class _Opener:
    def open(self, request, *args, **kwargs):
        return _urlopen(request, *args, **kwargs)

urllib.request.urlopen = _urlopen
urllib.request.build_opener = lambda *_handlers: _Opener()
PY
export PYTHONPATH="$TEST_ROOT${PYTHONPATH:+:$PYTHONPATH}"
export I43_REVIEW_MOCK_ROOT="$TEST_ROOT"

CONTROL_SHA="$(shasum -a 256 "$BASELINE_ROOT/control.sha256" | awk '{ print $1 }')"
readonly CONTROL_SHA
"$TOOL" verify --run-dir "$BASELINE_ROOT" --expected-control-sha256 "$CONTROL_SHA" \
    | grep -F "FAIL" >/dev/null || fail "complete structural FAIL campaign did not replay"

cp -- "$TEST_ROOT/review-api-1.json" "$TEST_ROOT/review-api-1.saved.json"
for mutation in wrong-author wrong-association edited-comment wrong-body; do
    cp -- "$TEST_ROOT/review-api-1.saved.json" "$TEST_ROOT/review-api-1.json"
    python3 - "$TEST_ROOT/review-api-1.json" "$mutation" <<'PY'
import json, pathlib, sys
path = pathlib.Path(sys.argv[1]); mutation = sys.argv[2]
document = json.loads(path.read_text(encoding="utf-8"))
if mutation == "wrong-author":
    document["user"]["login"] = "forged-reviewer"
elif mutation == "wrong-association":
    document["author_association"] = "NONE"
elif mutation == "edited-comment":
    document["updated_at"] = "2026-08-13T00:31:00Z"
elif mutation == "wrong-body":
    document["body"] += "\nforged"
path.write_text(json.dumps(document), encoding="utf-8")
PY
    expect_failure "authenticated review batch $mutation" \
        "$TOOL" verify --run-dir "$BASELINE_ROOT" --expected-control-sha256 "$CONTROL_SHA"
done
mv -- "$TEST_ROOT/review-api-1.saved.json" "$TEST_ROOT/review-api-1.json"

python3 - "$TOOL" <<'PY'
import importlib.util, urllib.parse, sys
spec = importlib.util.spec_from_file_location("issue43_campaign_evidence", sys.argv[1])
assert spec and spec.loader
module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
url = "https://api.github.com/repos/sadiksaifi/SpaceTerm/issues/comments/3"
base = {
    "html_url": "https://github.com/sadiksaifi/SpaceTerm/issues/43#issuecomment-3",
    "body": "anchor",
    "created_at": "2026-08-13T00:30:00Z",
    "updated_at": "2026-08-13T00:30:00Z",
    "author_association": "MEMBER",
    "user": {"login": "maintainer"},
}
module.validate_public_url = urllib.parse.urlsplit
module.fetch_public_json = lambda _url: dict(base)
assert module.fetch_issue_comment_body(url) == "anchor"
for key, value in (("author_association", "NONE"), ("updated_at", "2026-08-13T00:31:00Z")):
    forged = dict(base); forged[key] = value
    module.fetch_public_json = lambda _url, document=forged: document
    try:
        module.fetch_issue_comment_body(url)
    except module.EvidenceError:
        continue
    raise AssertionError(f"forged issue anchor accepted: {key}")
PY

python3 - "$TOOL" <<'PY'
import importlib.util, pathlib, sys
spec = importlib.util.spec_from_file_location("issue43_campaign_evidence", sys.argv[1])
assert spec and spec.loader
module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
assert module.render_value("x|y") == "x&#124;y"
assert "|" not in module.render_value("[spoof](https://example.test)|x")
try:
    module.validate_publication_capacity(
        [{"bytes": str(module.MAX_CAMPAIGN_PAYLOAD_BYTES + 1)}], pathlib.Path("/"),
    )
except module.EvidenceError:
    pass
else:
    raise AssertionError("oversized publication passed the pre-copy budget gate")
for secret in (
    "/tmp/private/result", "/private/tmp/private/result", "/Volumes/Private/user.txt",
    "ghp_abcdefghijklmnopqrstuvwxyz123456", "AKIAABCDEFGHIJKLMNOP",
    "AWS_SECRET_ACCESS_KEY=not-public", "-----BEGIN PRIVATE KEY-----",
    "-----BEGIN DSA PRIVATE KEY-----",
):
    try:
        module.privacy_scan(secret, "privacy adversary")
    except module.EvidenceError:
        continue
    raise AssertionError(f"privacy scanner accepted {secret!r}")
try:
    module.privacy_scan({"aws_secret_access_key": "not-public"}, "privacy adversary")
except module.EvidenceError:
    pass
else:
    raise AssertionError("privacy scanner accepted a structured AWS secret")

import urllib.parse, urllib.request
module.validate_public_url = urllib.parse.urlsplit
request = urllib.request.Request(
    "https://api.github.com/repos/sadiksaifi/SpaceTerm/issues/43",
    headers={"Authorization": "Bearer SECRET"},
)
try:
    module.ValidatingRedirectHandler().redirect_request(
        request, None, 302, "Found", {},
        "https://raw.githubusercontent.com/sadiksaifi/SpaceTerm/main/README.md",
    )
except module.EvidenceError:
    pass
else:
    raise AssertionError("authenticated cross-origin redirect retained authorization")
PY

python3 - "$TOOL" "$TEST_ROOT" <<'PY'
import binascii, importlib.util, pathlib, struct, sys, zipfile, zlib
spec = importlib.util.spec_from_file_location("issue43_campaign_evidence", sys.argv[1])
assert spec and spec.loader
module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
root = pathlib.Path(sys.argv[2])

def png_chunk(kind, payload):
    return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", binascii.crc32(kind + payload) & 0xffffffff)

png = root / "hidden-metadata.png"
ihdr = struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0)
png.write_bytes(b"\x89PNG\r\n\x1a\n" + png_chunk(b"IHDR", ihdr)
                + png_chunk(b"iCCP", b"private-profile\0\0" + zlib.compress(b"secret"))
                + png_chunk(b"IDAT", zlib.compress(b"\0\0\0\0")) + png_chunk(b"IEND", b""))
surplus_png = root / "surplus-raster.png"
surplus_png.write_bytes(
    b"\x89PNG\r\n\x1a\n" + png_chunk(b"IHDR", ihdr)
    + png_chunk(b"IDAT", zlib.compress(b"\0\0\0\0/Users/private/secret"))
    + png_chunk(b"IEND", b"")
)

archive_comment = root / "archive-comment.zip"
with zipfile.ZipFile(archive_comment, "w") as archive:
    archive.writestr("trace-metadata.tsv", "format_version\t2\n")
    archive.comment = b"hidden secret"
member_extra = root / "member-extra.zip"
with zipfile.ZipFile(member_extra, "w") as archive:
    info = zipfile.ZipInfo("trace-metadata.tsv")
    info.extra = b"\x01\x00\x00\x00"
    archive.writestr(info, "format_version\t2\n")

def atom(kind, payload):
    return struct.pack(">I", len(payload) + 8) + kind + payload
mov = root / "hidden-metadata.mov"
mov.write_bytes(atom(b"ftyp", b"qt  \0\0\0\0qt  ")
                + atom(b"moov", atom(b"mvhd", b"\0" * 20) + atom(b"trak", b"")
                       + atom(b"udta", b"hidden secret"))
                + atom(b"mdat", b"frame"))
utf16_mov = root / "utf16-metadata.mov"
handler = b"\0" * 8 + b"vide" + b"\0" * 12 + "/Users/private/secret".encode("utf-16-le")
utf16_mov.write_bytes(
    atom(b"ftyp", b"qt  \0\0\0\0qt  ")
    + atom(b"moov", atom(b"mvhd", b"\0" * 20)
           + atom(b"trak", atom(b"mdia", atom(b"hdlr", handler))))
    + atom(b"mdat", b"frame")
)

for path, validator, needle in (
    (png, module.validate_png, "unknown ancillary"),
    (surplus_png, module.validate_png, "exactly match"),
    (archive_comment, lambda value: module.validate_trace_archive(value, {}), "ZIP comments"),
    (member_extra, lambda value: module.validate_trace_archive(value, {}), "extra fields"),
    (mov, module.validate_quicktime, "metadata atom"),
    (utf16_mov, module.validate_quicktime, "prohibited private"),
):
    try:
        validator(path)
    except module.EvidenceError as error:
        assert needle in str(error), (path, error)
        continue
    raise AssertionError(f"hidden media metadata accepted: {path}")
PY

python3 - "$TOOL" "$TEST_ROOT" <<'PY'
import hashlib, importlib.util, pathlib, sys
spec = importlib.util.spec_from_file_location("issue43_campaign_evidence", sys.argv[1])
assert spec and spec.loader
module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
root = pathlib.Path(sys.argv[2]) / "native-closure"
root.mkdir()
run_id = "i43-20260813T000000Z-aaaaaaaaaaaa-bbbbbbbbbbbb"
app_hash = "d" * 64
samples = root / "runtime-samples.tsv"
sample_values = [
    "0", "1", "0", "0", "0", "0", "0", "0", "0", "0", "0", "0", "0", "0",
    "0", "0", "0", "0", "0", "0", "0", "0", "0", "0", "0", "0", "0", "0",
    "0", "0", "0", "0", "0", "0", "exited", "0",
]
samples.write_text(
    "\t".join(module.RUNTIME_SAMPLES_V1_HEADER) + "\n" + "\t".join(sample_values) + "\n",
    encoding="utf-8",
)
events = root / "runtime-events.tsv"
events.write_text("\t".join(module.RUNTIME_EVENTS_V1_HEADER) + "\n", encoding="utf-8")
failure = root / "failure-actions.tsv"
failure.write_text("\t".join(module.FAILURE_ACTION_V2_HEADER) + "\n", encoding="utf-8")
h = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
runtime_values = {
    "schema": "spaceterm.acceptance.runtime-observation-metadata/v3",
    "observation.source": "production-app", "run.id": run_id,
    "package.app.sha256": app_hash, "process.pid": "123",
    "runtime.samples.path": "runtime-samples.tsv", "runtime.samples.sha256": h(samples),
    "runtime.events.path": "runtime-events.tsv", "runtime.events.sha256": h(events),
    "failure.action.schema": "spaceterm.acceptance.failure-action/v1",
    "failure.action.enabled": "false",
    "failure.result.schema": "spaceterm.acceptance.failure-action-result/v2",
    "failure.actions.path": "failure-actions.tsv", "failure.actions.sha256": h(failure),
    "failure.request_count": "0", "failure.result_count": "0",
    "observer.started_continuous_ns": "1", "observer.ended_continuous_ns": "1",
    "observer.sample_interval_ms": "1000", "observer.transition_capacity": "64",
    "observer.sample_count": "1", "observer.event_count": "0",
    "observer.status": "complete", "observation.complete": "true",
}
runtime = root / "runtime-metadata.tsv"
runtime.write_text("".join(f"{key}\t{runtime_values[key]}\n" for key in module.RUNTIME_METADATA_V3_KEYS), encoding="utf-8")
provisional_values = {
    "schema": "spaceterm.acceptance.native-launch-proof/v5",
    "observation.source": "production-app", "launch.nonce": "a" * 64,
    "run.id": run_id, "package.app.sha256": app_hash,
    "runtime.schema": "spaceterm.acceptance.runtime-stream/v1",
    "runtime.sample_interval_ms": "1000", "runtime.transition_capacity": "64",
    "failure.action.schema": "spaceterm.acceptance.failure-action/v1",
    "failure.action.enabled": "false", "process.pid": "123", "process.pidversion": "1",
    "process.executable.path": "/Applications/SpaceTerm.app/Contents/MacOS/SpaceTerm",
    "process.executable.device": "1", "process.executable.inode": "2",
    "process.executable.fsid": "1:2", "process.signature.cdhash": "ABCDEF",
    "process.signature.identifier": "dev.sadiksaifi.spaceterm",
    "process.signature.team_identifier": "", "terminal_font_selected": "Menlo",
    "initial_grid.rows": "24", "initial_grid.columns": "80",
    "initial_grid.logical_width": "800", "initial_grid.logical_height": "480",
    "initial_grid.backing_pixel_width": "1600", "initial_grid.backing_pixel_height": "960",
    "observation.complete": "true",
}
provisional = "".join(f"{key}\t{provisional_values[key]}\n" for key in module.NATIVE_PROVISIONAL_V5_KEYS).encode()
final_values = dict(provisional_values)
final_values.update({
    "provisional.observation.sha256": hashlib.sha256(provisional).hexdigest(),
    "runtime.metadata.schema": runtime_values["schema"], "runtime.metadata.path": "runtime-metadata.tsv",
    "runtime.metadata.sha256": h(runtime), "failure.result.schema": runtime_values["failure.result.schema"],
    "failure.actions.path": "failure-actions.tsv", "failure.actions.sha256": h(failure),
    "failure.request_count": "0", "failure.result_count": "0",
})
native = root / "native-observation.tsv"
native.write_text("".join(f"{key}\t{final_values[key]}\n" for key in module.NATIVE_FINAL_V5_KEYS), encoding="utf-8")
identity = {
    "schema": "spaceterm.acceptance.run-identity-public/v2", "run.id": run_id,
    "run.origin": "mounted-dmg", "package.app.sha256": app_hash,
    "package.app.signature.cdhash": "ABCDEF",
    "package.app.signature.identifier": "dev.sadiksaifi.spaceterm",
    "package.app.signature.team_identifier": "not-set",
    "native.observation.path": "identity/native-observation.tsv",
    "native.provisional.observation.sha256": final_values["provisional.observation.sha256"],
    "native.observation.sha256": h(native), "native.runtime.metadata.sha256": h(runtime),
    "native.runtime.metadata.schema": runtime_values["schema"],
    "native.runtime.metadata.path": "identity/runtime-metadata.tsv",
    "native.failure.action.enabled": "false",
    "native.failure.actions.path": "identity/failure-actions.tsv",
    "native.failure.actions.sha256": h(failure), "native.observation.source": "production-app",
    "native.failure.result.schema": "spaceterm.acceptance.failure-action-result/v2",
    "native.failure.request_count": "0", "native.failure.result_count": "0",
    "font.selected.family": "Menlo", "font.selected.source": "production-app-observation",
    "host.terminal_font_selected": "Menlo",
}
for key in ("rows", "columns", "logical_width", "logical_height", "backing_pixel_width", "backing_pixel_height"):
    identity[f"host.initial_grid.{key}"] = provisional_values[f"initial_grid.{key}"]
metadata = {"run_id": run_id, "frozen_artifact": {"app_bundle_sha256": app_hash}}
module.validate_native_failure_closure(native, runtime, samples, events, failure, identity, metadata)
module.validate_published_runtime_failure_closure(
    runtime, samples, events, failure, identity, run_id, app_hash
)
receipt = module.native_closure_receipt(native, runtime, samples, events, failure, identity, run_id)
assert b"producer_commit\tb74249dc03722dc6083ed32ae5934abdadc07403\n" in receipt

def rejected(call, label):
    try:
        call()
    except module.EvidenceError:
        return
    raise AssertionError(f"adversarial native/runtime mutation was accepted: {label}")

bad_samples = root / "bad-samples.tsv"
bad_values = list(sample_values); bad_values[15] = "2"
bad_samples.write_text(
    "\t".join(module.RUNTIME_SAMPLES_V1_HEADER) + "\n" + "\t".join(bad_values) + "\n",
    encoding="utf-8",
)
rejected(
    lambda: module.validate_runtime_stream_exports(bad_samples, events, runtime_values),
    "non-boolean runtime sample",
)
bad_cadence = root / "bad-cadence.tsv"
late_sample = list(sample_values); late_sample[0] = "1"; late_sample[1] = "10000000000"
bad_cadence.write_text(
    "\t".join(module.RUNTIME_SAMPLES_V1_HEADER) + "\n"
    + "\t".join(sample_values) + "\n" + "\t".join(late_sample) + "\n",
    encoding="utf-8",
)
bad_cadence_runtime = dict(runtime_values)
bad_cadence_runtime.update({
    "observer.sample_count": "2", "observer.ended_continuous_ns": "10000000000",
})
rejected(
    lambda: module.validate_runtime_stream_exports(
        bad_cadence, events, bad_cadence_runtime
    ),
    "two-sample cadence gap",
)

one_event = root / "one-event.tsv"
one_event.write_text(
    "\t".join(module.RUNTIME_EVENTS_V1_HEADER)
    + "\n0\t1\tvisibility-lost\t0\t0\t0\n",
    encoding="utf-8",
)
runtime_with_event = dict(runtime_values); runtime_with_event["observer.event_count"] = "1"
module.validate_runtime_stream_exports(samples, one_event, runtime_with_event)
late_start_values = list(sample_values); late_start_values[1] = "10"
late_start_samples = root / "late-start-samples.tsv"
late_start_samples.write_text(
    "\t".join(module.RUNTIME_SAMPLES_V1_HEADER) + "\n"
    + "\t".join(late_start_values) + "\n", encoding="utf-8",
)
late_start_runtime = dict(runtime_with_event)
late_start_runtime.update({
    "observer.started_continuous_ns": "10", "observer.ended_continuous_ns": "10",
})
rejected(
    lambda: module.validate_runtime_stream_exports(
        late_start_samples, one_event, late_start_runtime
    ),
    "event before observer interval",
)
bad_event = root / "bad-event.tsv"
bad_event.write_text(
    "\t".join(module.RUNTIME_EVENTS_V1_HEADER)
    + "\n0\t1\tvisibility-lost\t0\t0\t1\n",
    encoding="utf-8",
)
rejected(
    lambda: module.validate_runtime_stream_exports(samples, bad_event, runtime_with_event),
    "event auxiliary covert value",
)

request = "e" * 64
zero_resources = ["0", "0", "0", "0"]
failure_rows = [
    [request, "0", "presentation-invalid-scale", "armed", "accepted", "1", "running",
     "none", "none", "none", "1", "4", "4", "4", "none", "1", "1", *zero_resources],
    [request, "0", "presentation-invalid-scale", "injected", "failed-state", "1", "failed",
     "presentation", "recoverable", "update-backing-scale", "2", "4", "4", "4",
     "presentation", "1", "1", *zero_resources],
    [request, "0", "presentation-invalid-scale", "retry-requested", "accepted", "1", "failed",
     "presentation", "recoverable", "update-backing-scale", "3", "4", "4", "4",
     "presentation", "1", "1", *zero_resources],
    [request, "0", "presentation-invalid-scale", "completed", "recovered", "1", "running",
     "none", "none", "none", "4", "4", "4", "4", "none", "1", "1", *zero_resources],
]
assert module.validate_failure_action_rows(failure_rows, "true") == (1, 4)
import copy
switched_pane = copy.deepcopy(failure_rows); switched_pane[2][5] = "2"
rejected(lambda: module.validate_failure_action_rows(switched_pane, "true"), "switched Pane")
generation_drift = copy.deepcopy(failure_rows); generation_drift[2][11] = "5"
rejected(lambda: module.validate_failure_action_rows(generation_drift, "true"), "retry drift")
rejected(lambda: module.validate_failure_action_rows(failure_rows[:-1], "true"), "missing completion")
rejected(lambda: module.validate_failure_action_rows(failure_rows, "false"), "disabled control rows")

bad_identity = dict(identity); bad_identity["native.runtime.metadata.sha256"] = "0" * 64
rejected(
    lambda: module.validate_published_runtime_failure_closure(
        runtime, samples, events, failure, bad_identity, run_id, app_hash
    ),
    "public identity hash forgery",
)
tampered = native.read_text(encoding="utf-8").replace(
    final_values["provisional.observation.sha256"], "0" * 64
)
native.write_text(tampered, encoding="utf-8")
try:
    module.validate_native_failure_closure(native, runtime, samples, events, failure, identity, metadata)
except module.EvidenceError:
    pass
else:
    raise AssertionError("tampered provisional native observation hash was accepted")
PY

expect_failure "generic clause forged PASS" \
    "$TOOL" record --run-id "$RUN_ID" --input "$TEST_ROOT/generic-pass-record.json"
expect_failure "private path leak" \
    "$TOOL" record --run-id "$RUN_ID" --input "$TEST_ROOT/path-leak-record.json"
"$TOOL" record --run-id "$RUN_ID" --input "$TEST_ROOT/valid-record.json"
expect_failure "duplicate immutable record ID" \
    "$TOOL" record --run-id "$RUN_ID" --input "$TEST_ROOT/valid-record.json"

case_root="$(copy_case omitted-required-scope)"
python3 - "$case_root/campaign.yaml" <<'PY'
import json, pathlib, sys
p = pathlib.Path(sys.argv[1]); d = json.loads(p.read_text(encoding="utf-8"))
r = next(item for item in d["case_results"] if item["case_id"] == "native-bash")
d["case_results"].remove(r)
d["manual_review"]["record_reviews"] = [item for item in d["manual_review"]["record_reviews"] if item["record_id"] != r["record_id"]]
p.write_text(json.dumps(d, ensure_ascii=True, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY
rehash_bundle "$case_root"
expect_failure "omitted required scope" "$TOOL" verify --run-dir "$case_root"

case_root="$(copy_case duplicate-record-id)"
python3 - "$case_root/campaign.yaml" <<'PY'
import json, pathlib, sys
p = pathlib.Path(sys.argv[1]); d = json.loads(p.read_text(encoding="utf-8"))
d["case_results"].append(d["case_results"][0])
p.write_text(json.dumps(d, ensure_ascii=True, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY
rehash_bundle "$case_root"
expect_failure "duplicate public record ID" "$TOOL" verify --run-dir "$case_root"

case_root="$(copy_case payload-tampering)"
payload_path="$(awk -F '\t' 'NR == 2 { print $5 }' "$case_root/artifacts.tsv")"
printf 'tamper\n' >> "$case_root/$payload_path"
expect_failure "payload tampering" "$TOOL" verify --run-dir "$case_root"

case_root="$(copy_case forged-campaign-pass)"
python3 - "$case_root/campaign.yaml" <<'PY'
import json, pathlib, sys
p = pathlib.Path(sys.argv[1]); d = json.loads(p.read_text(encoding="utf-8"))
d["campaign_status"] = "PASS"
p.write_text(json.dumps(d, ensure_ascii=True, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY
rehash_bundle "$case_root"
expect_failure "forged campaign PASS" "$TOOL" verify --run-dir "$case_root"

case_root="$(copy_case public-path-leak)"
python3 - "$case_root/campaign.yaml" <<'PY'
import json, pathlib, sys
p = pathlib.Path(sys.argv[1]); d = json.loads(p.read_text(encoding="utf-8"))
d["host"]["cpu"] = "/Users/private-account/private-model"
p.write_text(json.dumps(d, ensure_ascii=True, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY
rehash_bundle "$case_root"
expect_failure "public path leak" "$TOOL" verify --run-dir "$case_root"

case_root="$(copy_case digest-cycle)"
printf '%064d  control.sha256\n' 0 >> "$case_root/control.sha256"
expect_failure "control digest cycle" "$TOOL" verify --run-dir "$case_root"

case_root="$(copy_case stale-comparison-pair)"
python3 - "$case_root/campaign.yaml" <<'PY'
import hashlib, json, pathlib, sys
p = pathlib.Path(sys.argv[1]); d = json.loads(p.read_text(encoding="utf-8"))
r = next(item for item in d["case_results"] if item["case_id"] == "perf-sustained-ascii" and item["subject"] == "spaceterm")
r["comparison_record_id"] = r["record_id"]
payload = (json.dumps(r, ensure_ascii=True, indent=2, sort_keys=True) + "\n").encode("utf-8")
review = next(item for item in d["manual_review"]["record_reviews"] if item["record_id"] == r["record_id"])
review["record_sha256"] = hashlib.sha256(payload).hexdigest()
p.write_text(json.dumps(d, ensure_ascii=True, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY
rehash_bundle "$case_root"
expect_failure "stale performance comparison pair" "$TOOL" verify --run-dir "$case_root"

case_root="$(copy_case missing-manual-review)"
python3 - "$case_root/campaign.yaml" <<'PY'
import json, pathlib, sys
p = pathlib.Path(sys.argv[1]); d = json.loads(p.read_text(encoding="utf-8"))
d["manual_review"]["artifact_reviews"].pop()
p.write_text(json.dumps(d, ensure_ascii=True, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY
rehash_bundle "$case_root"
expect_failure "missing manual artifact review" "$TOOL" verify --run-dir "$case_root"

case_root="$(copy_case impossible-review-chronology)"
python3 - "$case_root/campaign.yaml" <<'PY'
import json, pathlib, sys
p = pathlib.Path(sys.argv[1]); d = json.loads(p.read_text(encoding="utf-8"))
# The review predates both the owning record and its payload creation.
d["manual_review"]["record_reviews"][0]["reviewed_utc"] = "2026-08-13T00:01:00Z"
d["manual_review"]["artifact_reviews"][0]["reviewed_utc"] = "2026-08-13T00:01:00Z"
p.write_text(json.dumps(d, ensure_ascii=True, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY
rehash_bundle "$case_root"
expect_failure "impossible review chronology" "$TOOL" verify --run-dir "$case_root"

expect_failure "anonymous public payload retrieval" \
    "$TOOL" verify --run-dir "$BASELINE_ROOT" --fetch-public

case_root="$(copy_case private-state-publication)"
mkdir -- "$case_root/.issue-43-campaign-private"
expect_failure "private collector state publication" "$TOOL" verify --run-dir "$case_root"

case_root="$(copy_case missing-conditional-subcase)"
python3 - "$case_root/campaign.yaml" <<'PY'
import hashlib, json, pathlib, sys
p = pathlib.Path(sys.argv[1]); d = json.loads(p.read_text(encoding="utf-8"))
r = next(item for item in d["case_results"] if item["case_id"] == "capability-keyboard" and item["subject"] == "spaceterm")
r["conditional_subcases"] = []
payload = (json.dumps(r, ensure_ascii=True, indent=2, sort_keys=True) + "\n").encode("utf-8")
review = next(item for item in d["manual_review"]["record_reviews"] if item["record_id"] == r["record_id"])
review["record_sha256"] = hashlib.sha256(payload).hexdigest()
p.write_text(json.dumps(d, ensure_ascii=True, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY
rehash_bundle "$case_root"
expect_failure "missing host-bound conditional" "$TOOL" verify --run-dir "$case_root"

case_root="$(copy_case missing-known-deviation)"
python3 - "$case_root/campaign.yaml" <<'PY'
import hashlib, json, pathlib, sys
p = pathlib.Path(sys.argv[1]); d = json.loads(p.read_text(encoding="utf-8"))
r = next(item for item in d["case_results"] if item["case_id"] == "package-build" and item["subject"] == "spaceterm")
r["status"] = "FAIL"
r["smallest_reproduction"] = "run just package with the frozen fixture"
payload = (json.dumps(r, ensure_ascii=True, indent=2, sort_keys=True) + "\n").encode("utf-8")
review = next(item for item in d["manual_review"]["record_reviews"] if item["record_id"] == r["record_id"])
review["record_sha256"] = hashlib.sha256(payload).hexdigest()
p.write_text(json.dumps(d, ensure_ascii=True, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY
rehash_bundle "$case_root"
expect_failure "missing known deviation for SpaceTerm FAIL" "$TOOL" verify --run-dir "$case_root"

case_root="$(copy_case forged-public-identity)"
python3 - "$case_root" <<'PY'
import csv, hashlib, io, json, pathlib, sys
root = pathlib.Path(sys.argv[1])
campaign_path = root / "campaign.yaml"
campaign = json.loads(campaign_path.read_text(encoding="utf-8"))
identity_id = campaign["identity_evidence"]["public_identity_artifact_id"]
replay_id = campaign["identity_evidence"]["final_identity_replay_artifact_id"]
with (root / "artifacts.tsv").open("r", encoding="utf-8", newline="") as handle:
    reader = csv.DictReader(handle, delimiter="\t")
    fields = reader.fieldnames
    rows = list(reader)
identity_row = next(row for row in rows if row["artifact_id"] == identity_id)
replay_row = next(row for row in rows if row["artifact_id"] == replay_id)
identity_path = root / identity_row["relative_path"]
identity_text = identity_path.read_text(encoding="utf-8").replace(
    "repository.clean\ttrue", "repository.clean\tfalse"
)
identity_path.write_text(identity_text, encoding="utf-8")
identity_sha = hashlib.sha256(identity_path.read_bytes()).hexdigest()
identity_row["sha256"] = identity_sha
identity_row["bytes"] = str(identity_path.stat().st_size)
campaign["identity_replay"]["public_identity_sha256"] = identity_sha
replay_path = root / replay_row["relative_path"]
replay_lines = []
for line in replay_path.read_text(encoding="utf-8").splitlines():
    key, value = line.split("\t", 1)
    replay_lines.append(f"{key}\t{identity_sha if key == 'public_identity_sha256' else value}")
replay_path.write_text("\n".join(replay_lines) + "\n", encoding="utf-8")
replay_sha = hashlib.sha256(replay_path.read_bytes()).hexdigest()
replay_row["sha256"] = replay_sha
replay_row["bytes"] = str(replay_path.stat().st_size)
for review in campaign["manual_review"]["artifact_reviews"]:
    if review["artifact_id"] == identity_id:
        review["artifact_sha256"] = identity_sha
    if review["artifact_id"] == replay_id:
        review["artifact_sha256"] = replay_sha
output = io.StringIO(newline="")
writer = csv.DictWriter(output, fieldnames=fields, delimiter="\t", lineterminator="\n")
writer.writeheader()
writer.writerows(rows)
(root / "artifacts.tsv").write_text(output.getvalue(), encoding="utf-8")
campaign_path.write_text(
    json.dumps(campaign, ensure_ascii=True, indent=2, sort_keys=True) + "\n",
    encoding="utf-8",
)
PY
rehash_bundle "$case_root"
expect_failure "forged collector identity projection" "$TOOL" verify --run-dir "$case_root"

case_root="$(copy_case arbitrary-issue-comment)"
printf 'arbitrary comment for %s\n' "$RUN_ID" > "$case_root/issue-comment.md"
expect_failure "non-deterministic issue comment" \
    "$TOOL" verify --run-dir "$case_root" --require-comment \
    --expected-control-sha256 "$CONTROL_SHA"

printf 'plain text disguised as PNG\n' > "$TEST_ROOT/fake.png"
printf 'plain text disguised as MOV\n' > "$TEST_ROOT/fake.mov"
printf 'plain text disguised as Instruments\n' > "$TEST_ROOT/fake.trace.zip"
if python3 - "$TOOL" "$TEST_ROOT" >"$TEST_ROOT/fake-png.stdout" 2>"$TEST_ROOT/fake-png.stderr" <<'PY'
import importlib.util, pathlib, sys
spec = importlib.util.spec_from_file_location("issue43", pathlib.Path(sys.argv[1]))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
module.validate_png(pathlib.Path(sys.argv[2]) / "fake.png")
PY
then
    fail "plain text PNG unexpectedly passed media validation"
fi
if python3 - "$TOOL" "$TEST_ROOT" >"$TEST_ROOT/fake-mov.stdout" 2>"$TEST_ROOT/fake-mov.stderr" <<'PY'
import importlib.util, pathlib, sys
spec = importlib.util.spec_from_file_location("issue43", pathlib.Path(sys.argv[1]))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
module.validate_quicktime(pathlib.Path(sys.argv[2]) / "fake.mov")
PY
then
    fail "plain text MOV unexpectedly passed media validation"
fi
if python3 - "$TOOL" "$TEST_ROOT" >"$TEST_ROOT/fake-trace.stdout" 2>"$TEST_ROOT/fake-trace.stderr" <<'PY'
import importlib.util, pathlib, sys
spec = importlib.util.spec_from_file_location("issue43", pathlib.Path(sys.argv[1]))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
module.validate_trace_archive(pathlib.Path(sys.argv[2]) / "fake.trace.zip", {})
PY
then
    fail "plain text trace archive unexpectedly passed validation"
fi

echo "issue 43 campaign evidence adversarial tests passed"
