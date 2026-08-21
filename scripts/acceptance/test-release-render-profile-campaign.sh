#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
umask 077

SCRIPT_DIRECTORY="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIRECTORY
TEMP_ROOT="$(realpath "$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-render-profile.XXXXXX")")"
readonly TEMP_ROOT
readonly HASH_A="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
readonly RENDER_SCENARIOS=(
    perf-render-idle-cursor-blink
    perf-render-text-blink
    perf-render-sustained-output
    perf-render-selection
    perf-render-marked-text
    perf-render-live-resize
)
SEAL_CASE_INPUTS=true

cleanup() {
    chmod -R u+w "$TEMP_ROOT" 2>/dev/null || true
    rm -rf -- "$TEMP_ROOT"
}
trap cleanup EXIT INT TERM

fail() {
    echo "test failure: $*" >&2
    exit 1
}

sha256() {
    shasum -a 256 "$1" | awk '{ print $1 }'
}

sha256_text() {
    printf '%s' "$1" | shasum -a 256 | awk '{ print $1 }'
}

metric() {
    awk -F '\t' -v wanted="$2" '$1 == wanted { count += 1; value = $2 } \
        END { if (count == 1) print value }' "$1"
}

expect_result() {
    local expected_exit="$1"
    local expected_result="$2"
    local expected_reason="$3"
    local label="$4"
    shift 4
    local output="$TEMP_ROOT/result.tsv"
    local actual_exit=0
    "$@" > "$output" 2>/dev/null || actual_exit=$?
    if [[ "$actual_exit" != "$expected_exit" ]]; then
        sed 's/^/  /' "$output" >&2
        fail "$label exit: expected $expected_exit, observed $actual_exit"
    fi
    [[ "$(metric "$output" result)" == "$expected_result" ]] \
        || fail "$label result: expected $expected_result"
    [[ "$(metric "$output" reason)" == "$expected_reason" ]] \
        || fail "$label reason: expected $expected_reason, observed $(metric "$output" reason)"
}

expect_command_failure() {
    local label="$1"
    shift
    if "$@" >/dev/null 2>&1; then
        fail "$label unexpectedly succeeded"
    fi
}

write_subject_identity() {
    local subject="$1"
    local output="$2"
    local pid="$3"
    {
        printf 'format_version\t1\n'
        printf 'subject\t%s\n' "$subject"
        printf 'app_bundle_path\t/Applications/%s.app\n' "$subject"
        printf 'bundle_identifier\tcom.example.%s\n' "$subject"
        printf 'bundle_version\t1.0+1\n'
        printf 'executable_path\t/Applications/%s.app/Contents/MacOS/%s\n' \
            "$subject" "$subject"
        printf 'executable_sha256\t%s\n' "$HASH_A"
        printf 'executable_device\t1\n'
        printf 'executable_inode\t2\n'
        printf 'executable_fsid\t1\n'
        printf 'signature_valid\ttrue\n'
        printf 'signing_identifier\tcom.example.%s\n' "$subject"
        printf 'team_identifier\tnone\n'
        printf 'cdhash\tabcd1234\n'
        printf 'process_pid\t%s\n' "$pid"
        printf 'process_start_identity\t%s:123456\n' "$pid"
        printf 'identity_status\tfrozen\n'
    } > "$output"
}

scenario_duration() {
    metric "$TEMP_ROOT/$1-plan-metadata.tsv" measured_duration_ms
}

scenario_action_count() {
    metric "$TEMP_ROOT/$1-plan-metadata.tsv" required_action_count
}

write_run_intent_fixture() {
    local subject="$1" scenario="$2" identity="$3" pair="$4" nonce="$5" output="$6"
    local native=not-applicable
    [[ "$subject" != spaceterm ]] || native="$HASH_A"
    {
        printf 'format_version\t1\nsubject\t%s\n' "$subject"
        printf 'subject_identity_sha256\t%s\n' "$(sha256 "$identity")"
        printf 'scenario\t%s\nscenario_plan_sha256\t%s\n' "$scenario" \
            "$(metric "$pair" plan_sha256)"
        for key in workload_sha256 command_sha256 environment_sha256 font_sha256 \
            initial_grid_sha256; do
            printf '%s\t%s\n' "$key" "$(metric "$pair" "$key")"
        done
        printf 'measured_duration_ms\t%s\n' "$(metric "$pair" duration_ms)"
        printf 'process_pid\t%s\n' "$(metric "$identity" process_pid)"
        printf 'process_start_identity\t%s\n' "$(metric "$identity" process_start_identity)"
        printf 'campaign_id\trender-campaign\nsession_id\trender-session\n'
        printf 'nonce\t%s\nnative_provisional_observation_sha256\t%s\n' "$nonce" "$native"
        printf 'evidence_mode\tproduction\nstatus\tprepared\n'
    } > "$output"
    chmod 0444 "$output"
}

write_run_metadata_fixture() {
    local subject="$1" intent="$2" output="$3"
    local native_hash="$HASH_A" native_enabled=false native_count=0
    if [[ "$subject" == ghostty ]]; then
        native_hash=not-applicable
        native_enabled=not-applicable
        native_count=not-applicable
    fi
    {
        printf 'format_version\t4\n'
        for key in subject subject_identity_sha256 scenario scenario_plan_sha256 \
            workload_sha256 command_sha256 environment_sha256 font_sha256 \
            initial_grid_sha256 measured_duration_ms process_pid \
            process_start_identity; do
            printf '%s\t%s\n' "$key" "$(metric "$intent" "$key")"
        done
        printf 'run_intent_sha256\t%s\n' "$(sha256 "$intent")"
        printf 'native_observation_sha256\t%s\n' "$native_hash"
        printf 'native_runtime_metadata_sha256\t%s\n' "$native_hash"
        printf 'native_failure_actions_sha256\t%s\n' "$native_hash"
        printf 'native_failure_action_enabled\t%s\n' "$native_enabled"
        for key in native_failure_request_count native_failure_result_count \
            native_failure_resource_staged_count native_failure_resource_staged_bytes \
            native_failure_resource_rolled_back_count \
            native_failure_resource_rolled_back_bytes; do
            printf '%s\t%s\n' "$key" "$native_count"
        done
        for key in trace_provisional_receipt_sha256 performance_tail_receipt_sha256 \
            performance_quit_receipt_sha256 subject_exit_receipt_sha256 \
            lifecycle_ready_receipt_sha256 lifecycle_registration_receipt_sha256 \
            lifecycle_helper_sha256 terminator_source_sha256 \
            terminator_binary_sha256; do
            printf '%s\t%s\n' "$key" "$HASH_A"
        done
        printf 'evidence_mode\tproduction\nstatus\tcomplete\n'
    } > "$output"
    chmod 0444 "$output"
}

write_trace_metadata() {
    local identity="$1"
    local run_metadata="$2"
    local render_evidence="$3"
    local render_workload_metadata="$4"
    local duration="$5"
    local output="$6"
    local common_workload_metadata="${7:-}"
    local common_ready_receipt="${8:-}"
    local workload_started
    local workload_ended
    local common_workload_hash=0000000000000000000000000000000000000000000000000000000000000000
    local common_ready_hash=0000000000000000000000000000000000000000000000000000000000000000
    if [[ -n "$common_workload_metadata" && -n "$common_ready_receipt" ]]; then
        common_workload_hash="$(sha256 "$common_workload_metadata")"
        common_ready_hash="$(sha256 "$common_ready_receipt")"
    fi
    workload_started="$(metric "$render_workload_metadata" started_continuous_ns)"
    workload_ended="$(metric "$render_workload_metadata" ended_continuous_ns)"
    {
        printf 'format_version\t3\n'
        printf 'capture_status\tCAPTURED\n'
        printf 'incomplete_reason\tnone\n'
        printf 'subject_identity_sha256\t%s\n' "$(sha256 "$identity")"
        printf 'run_metadata_sha256\t%s\n' "$(sha256 "$run_metadata")"
        printf 'workload_metadata_sha256\t%s\n' "$common_workload_hash"
        printf 'workload_ready_receipt_sha256\t%s\n' "$common_ready_hash"
        printf 'supplemental_evidence_sha256\t%s\n' \
            "$(sha256 "$render_evidence")"
        printf 'requested_duration_ms\t%s\n' "$duration"
        printf 'actual_duration_ms\t%s\n' "$duration"
        printf 'capture_started_continuous_ns\t%s\n' "$workload_started"
        printf 'capture_ended_continuous_ns\t%s\n' "$workload_ended"
        printf 'target_identity_verified\ttrue\n'
        printf 'trace_target_pid_verified\ttrue\n'
        printf 'time_profiler_instrument\ttrue\n'
        printf 'allocations_instrument\ttrue\n'
        printf 'hangs_instrument\ttrue\n'
        printf 'time_profiler_target_verified\ttrue\n'
        printf 'allocations_target_verified\ttrue\n'
        printf 'hangs_target_verified\ttrue\n'
        printf 'time_profiler_rows\t120\n'
        printf 'allocations_rows\t20\n'
        printf 'hangs_rows\t0\n'
        printf 'maximum_main_thread_hang_ms\t0\n'
        printf 'status\tcomplete\n'
    } > "$output"
}

write_driver_events() {
    local plan="$1"
    local identity="$2"
    local output="$3"
    local pid
    pid="$(metric "$identity" process_pid)"
    awk -F '\t' -v OFS='\t' -v pid="$pid" '
        BEGIN {
            print "sequence", "continuous_ns", "event_id", "action", \
                "target_pid", "window_number", "requested_a", "requested_b", \
                "observed_a", "observed_b", "result"
        }
        NR > 1 {
            sequence = NR - 2
            observed_a = $3 == "resize-grid" ? $4 : 1
            observed_b = $3 == "resize-grid" ? $5 : 1
            print sequence, 1000000000000 + $2 * 1000000 + sequence, $1, $3, \
                pid, 44, $4, $5, observed_a, observed_b, "verified"
        }
    ' "$plan" > "$output"
    chmod 0444 "$output"
}

write_common_workload_v3_fixture() {
    local scenario="$1"
    local identity="$2"
    local run_metadata="$3"
    local render_workload_metadata="$4"
    local events="$5"
    local ready="$6"
    local output="$7"
    local pid start_identity started ended warmup duration plan_start producer_started
    pid="$(metric "$identity" process_pid)"
    start_identity="$(metric "$identity" process_start_identity)"
    started="$(metric "$render_workload_metadata" started_continuous_ns)"
    ended="$(metric "$render_workload_metadata" ended_continuous_ns)"
    warmup="$(metric "$TEMP_ROOT/$scenario-plan-metadata.tsv" warmup_ms)"
    duration="$(scenario_duration "$scenario")"
    plan_start=$((started - warmup * 1000000))
    producer_started=$((plan_start - 200000000))
    {
        printf 'sequence\tcontinuous_ns\tkind\tevent_id\tbyte_count\trows\tcolumns\tpixel_width\tpixel_height\tstatus\n'
        printf '0\t%s\tstarted\tnone\t0\t24\t80\t800\t600\tok\n' "$producer_started"
        printf '1\t%s\tgeometry\tnone\t0\t24\t80\t800\t600\tok\n' "$((producer_started + 1))"
        printf '2\t%s\tseed-complete\tnone\t100\t24\t80\t800\t600\tok\n' "$((producer_started + 2))"
        printf '3\t%s\tproducer-end\tnone\t1000\t24\t80\t800\t600\tsuccess\n' "$ended"
    } > "$events"
    local events_device events_inode prefix_bytes prefix_hash
    events_device="$(stat -f '%d' "$events")"
    events_inode="$(stat -f '%i' "$events")"
    prefix_bytes="$(head -n 4 "$events" | wc -c | tr -d ' ')"
    prefix_hash="$(head -c "$prefix_bytes" "$events" | shasum -a 256 | awk '{ print $1 }')"
    {
        printf 'format_version\t1\n'
        printf 'campaign_id\trender-campaign\n'
        printf 'session_id\trender-session\n'
        printf 'nonce\t%s\n' "$HASH_A"
        printf 'subject_identity_sha256\t%s\n' "$(sha256 "$identity")"
        printf 'producer_pid\t900\n'
        printf 'producer_started_continuous_ns\t%s\n' "$producer_started"
        printf 'producer_session_id\t900\n'
        printf 'producer_process_group\t900\n'
        printf 'tty_device\t1\n'
        printf 'tty_inode\t2\n'
        printf 'tty_rdev\t3\n'
        printf 'events_device\t%s\n' "$events_device"
        printf 'events_inode\t%s\n' "$events_inode"
        printf 'events_prefix_bytes\t%s\n' "$prefix_bytes"
        printf 'events_prefix_sha256\t%s\n' "$prefix_hash"
        printf 'measurement_ready_continuous_ns\t%s\n' "$((plan_start - 100000000))"
        printf 'measurement_ready_byte_count\t10\n'
        printf 'auth_algorithm\thmac-sha256\n'
    } > "$ready"
    python3 - "$ready" "$RENDER_SECRET" <<'PY'
import hashlib, hmac, pathlib, struct, sys
path = pathlib.Path(sys.argv[1])
unsigned = path.read_bytes()
authenticated = b"spaceterm.performance.workload-ready/v1\0" + struct.pack(">Q", len(unsigned)) + unsigned
with path.open("ab") as destination:
    destination.write(b"ready_hmac_sha256\t" + hmac.new(
        pathlib.Path(sys.argv[2]).read_bytes(), authenticated, hashlib.sha256
    ).hexdigest().encode() + b"\n")
PY
    {
        printf 'format_version\t3\n'
        printf 'scenario\t%s\n' "$scenario"
        printf 'campaign_id\trender-campaign\n'
        printf 'session_id\trender-session\n'
        printf 'nonce\t%s\n' "$HASH_A"
        printf 'subject_identity_sha256\t%s\n' "$(sha256 "$identity")"
        printf 'subject_process_pid\t%s\n' "$pid"
        printf 'subject_process_start_identity\t%s\n' "$start_identity"
        printf 'producer_sha256\t%s\n' "$(metric "$run_metadata" workload_sha256)"
        printf 'producer_pid\t900\n'
        printf 'producer_started_continuous_ns\t%s\n' "$producer_started"
        printf 'producer_session_id\t900\n'
        printf 'producer_process_group\t900\n'
        printf 'tty_device\t1\n'
        printf 'tty_inode\t2\n'
        printf 'tty_rdev\t3\n'
        printf 'ready_receipt_sha256\t%s\n' "$(sha256 "$ready")"
        printf 'events_sha256\t%s\n' "$(sha256 "$events")"
        printf 'auth_algorithm\thmac-sha256\n'
        printf 'seed_sha256\t%s\n' "$HASH_A"
        printf 'seed_bytes\t100\n'
        printf 'requested_duration_ms\t%s\n' "$duration"
        printf 'warmup_ms\t%s\n' "$warmup"
        printf 'requested_iterations\t0\n'
        printf 'requested_seed_rows\t0\n'
        printf 'emitted_bytes\t1000\n'
        printf 'input_events\t0\n'
        printf 'plan_start_continuous_ns\t%s\n' "$plan_start"
        printf 'started_continuous_ns\t%s\n' "$started"
        printf 'ended_continuous_ns\t%s\n' "$ended"
        printf 'status\tcomplete\n'
    } > "$output"
    python3 - "$output" "$events" "$RENDER_SECRET" <<'PY'
import hashlib, hmac, pathlib, struct, sys
metadata = pathlib.Path(sys.argv[1])
unsigned = metadata.read_bytes()
events = pathlib.Path(sys.argv[2]).read_bytes()
authenticated = (b"spaceterm.performance.workload-auth/v1\0"
    + struct.pack(">Q", len(unsigned)) + unsigned
    + struct.pack(">Q", len(events)) + events)
with metadata.open("ab") as destination:
    destination.write(b"events_hmac_sha256\t" + hmac.new(
        pathlib.Path(sys.argv[3]).read_bytes(), authenticated, hashlib.sha256
    ).hexdigest().encode() + b"\n")
PY
    chmod 0444 "$events" "$ready" "$output"
}

artifact_path() {
    printf '%s/%s--%s--%s' "$TEMP_ROOT" "$1" "$2" "$3"
}

write_trace_zip() {
    python3 - "$1" "$2" <<'PY'
import pathlib
import sys
import zipfile

output = pathlib.Path(sys.argv[1])
trace_id = sys.argv[2]
with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_STORED) as archive:
    archive.writestr("capture.trace/data/id", trace_id)
PY
}

write_manual_review() {
    local scenario="$1"
    local subject="$2"
    local trace_index="$3"
    local trace_metadata="$4"
    local output="$5"
    local plan="$TEMP_ROOT/$scenario-plan.tsv"
    local pair="$TEMP_ROOT/$scenario-pair.tsv"
    local run_metadata
    local render_intent
    local render_evidence
    local render_workload_metadata
    local campaign_manifest
    local trace_anchor_receipt
    local driver_intent
    local driver_receipt
    local trace_receipt
    local trace_artifact
    local trace_toc
    local trace_verification
    local time_artifact
    local allocations_artifact
    local hangs_artifact
    local screenshot
    local action_video
    local render_root=TerminalGridElement::paint
    [[ "$subject" == spaceterm ]] || render_root=ghostty-render-root
    trace_artifact="$(artifact_path "$scenario" "$subject" trace.zip)"
    trace_toc="$(artifact_path "$scenario" "$subject" trace-toc.xml)"
    trace_verification="$(artifact_path "$scenario" "$subject" trace-verification.tsv)"
    time_artifact="$(artifact_path "$scenario" "$subject" time-profiler.xml)"
    allocations_artifact="$(artifact_path "$scenario" "$subject" allocations.xml)"
    hangs_artifact="$(artifact_path "$scenario" "$subject" hangs.xml)"
    screenshot="$(artifact_path "$scenario" "$subject" stacks.png)"
    action_video="$(artifact_path "$scenario" "$subject" actions.mov)"
    run_metadata="$(artifact_path "$scenario" "$subject" run-metadata.tsv)"
    render_intent="$(artifact_path "$scenario" "$subject" render-intent.tsv)"
    render_evidence="$(artifact_path "$scenario" "$subject" render-evidence.tsv)"
    render_workload_metadata="$(artifact_path "$scenario" "$subject" workload-metadata.tsv)"
    campaign_manifest="$(artifact_path "$scenario" "$subject" campaign-manifest.tsv)"
    trace_anchor_receipt="$(artifact_path "$scenario" "$subject" trace-anchor-receipt.tsv)"
    driver_intent="$(artifact_path "$scenario" "$subject" driver-intent.tsv)"
    driver_receipt="$(artifact_path "$scenario" "$subject" driver-receipt.tsv)"
    trace_receipt="$(artifact_path "$scenario" "$subject" trace-receipt.tsv)"
    {
        printf 'format_version\t1\n'
        printf 'scenario\t%s\n' "$scenario"
        printf 'subject\t%s\n' "$subject"
        printf 'plan_sha256\t%s\n' "$(sha256 "$plan")"
        printf 'pair_metadata_sha256\t%s\n' "$(sha256 "$pair")"
        printf 'run_metadata_sha256\t%s\n' "$(sha256 "$run_metadata")"
        printf 'render_intent_sha256\t%s\n' "$(sha256 "$render_intent")"
        printf 'render_workload_metadata_sha256\t%s\n' \
            "$(sha256 "$render_workload_metadata")"
        printf 'render_evidence_sha256\t%s\n' "$(sha256 "$render_evidence")"
        printf 'campaign_manifest_sha256\t%s\n' "$(sha256 "$campaign_manifest")"
        printf 'trace_anchor_receipt_sha256\t%s\n' "$(sha256 "$trace_anchor_receipt")"
        printf 'driver_intent_sha256\t%s\n' "$(sha256 "$driver_intent")"
        printf 'driver_receipt_sha256\t%s\n' "$(sha256 "$driver_receipt")"
        printf 'trace_receipt_sha256\t%s\n' "$(sha256 "$trace_receipt")"
        printf 'trace_index_sha256\t%s\n' "$(sha256 "$trace_index")"
        printf 'trace_metadata_sha256\t%s\n' "$(sha256 "$trace_metadata")"
        printf 'trace_artifact_sha256\t%s\n' "$(sha256 "$trace_artifact")"
        printf 'trace_toc_sha256\t%s\n' "$(sha256 "$trace_toc")"
        printf 'trace_verification_sha256\t%s\n' "$(sha256 "$trace_verification")"
        printf 'trace_verifier_sha256\t%s\n' \
            "$(sha256 "$SPACETERM_RENDER_PROFILE_TRACE_VERIFIER")"
        printf 'time_profiler_artifact_sha256\t%s\n' "$(sha256 "$time_artifact")"
        printf 'allocations_artifact_sha256\t%s\n' "$(sha256 "$allocations_artifact")"
        printf 'hangs_artifact_sha256\t%s\n' "$(sha256 "$hangs_artifact")"
        printf 'representative_stack_screenshot_sha256\t%s\n' "$(sha256 "$screenshot")"
        printf 'action_video_sha256\t%s\n' "$(sha256 "$action_video")"
        printf 'instruments_version\tInstruments 26.0\n'
        printf 'sampling_settings\t1 ms running-time sample interval, invert call tree off\n'
        printf 'render_root_symbol\t%s\n' "$render_root"
        printf 'render_root_sample_count\t10\n'
        printf 'call_tree_filters\t%s, target process only\n' "$render_root"
        printf 'action_video_review\tPASS\n'
        printf 'time_profiler_call_tree_checked\tPASS\n'
        printf 'allocations_call_tree_checked\tPASS\n'
        printf 'hangs_timeline_checked\tPASS\n'
        printf 'render_root_text_shaping_stack_present\tfalse\n'
        printf 'render_root_path_construction_stack_present\tfalse\n'
        printf 'render_root_symbol_plan_construction_stack_present\tfalse\n'
        printf 'render_root_row_plan_construction_stack_present\tfalse\n'
        printf 'render_root_image_placement_geometry_stack_present\tfalse\n'
        printf 'render_root_normal_frame_allocation_stack_present\tfalse\n'
        printf 'unchanged_row_reshaping_present\tfalse\n'
        printf 'changed_row_proportionality_result\tPASS\n'
        printf 'overlay_change_proportionality_result\tPASS\n'
        printf 'completed_action_count\t%s\n' "$(scenario_action_count "$scenario")"
        printf 'exceptional_error_allocations_excluded\tNo injected error interval overlapped the reviewed normal-frame range\n'
        printf 'reviewer\tacceptance-operator\n'
        printf 'result\tPASS\n'
    } > "$output"
}

write_pair_closure() {
    local scenario="$1"
    python3 - "$TEMP_ROOT" "$scenario" "$RENDER_SECRET" <<'PY'
import hashlib, hmac, pathlib, stat, struct, sys, unicodedata, zipfile
root = pathlib.Path(sys.argv[1]); scenario = sys.argv[2]; secret = pathlib.Path(sys.argv[3]).read_bytes()
subjects = ("spaceterm", "ghostty")
def path(subject, suffix): return root / f"{scenario}--{subject}--{suffix}"
def digest(value): return hashlib.sha256(pathlib.Path(value).read_bytes()).hexdigest()
def values(value): return dict(line.split("\t", 1) for line in pathlib.Path(value).read_text().splitlines())
def trace_tree(value):
 entries = {}; trace_root = None
 with zipfile.ZipFile(value) as archive:
  for info in archive.infolist():
   parts = pathlib.PurePosixPath(info.filename.rstrip("/")).parts
   if not parts or any(part in ("", ".", "..") for part in parts): raise SystemExit(1)
   trace_root = parts[0] if trace_root is None else trace_root
   if parts[0] != trace_root or not trace_root.endswith(".trace"): raise SystemExit(1)
   mode = (info.external_attr >> 16) & 0xFFFF
   if stat.S_IFMT(mode) == stat.S_IFLNK: raise SystemExit(1)
   if info.is_dir(): continue
   relative = pathlib.PurePosixPath(*parts[1:]).as_posix()
   if not relative or unicodedata.normalize("NFC", relative) != relative or relative in entries:
    raise SystemExit(1)
   entries[relative] = archive.read(info)
 result = hashlib.sha256(b"spaceterm.performance.trace-tree/v1\0")
 for relative, data in sorted(entries.items()):
  encoded = relative.encode()
  result.update(struct.pack(">Q", len(encoded))); result.update(encoded)
  result.update(struct.pack(">Q", len(data))); result.update(data)
 return result.hexdigest()
pair_metadata_path = root / f"{scenario}-pair.tsv"
pair_metadata = values(pair_metadata_path)
case_hashes = {}
trees = {}
for subject in subjects:
 intent = values(path(subject, "render-intent.tsv"))
 run = values(path(subject, "run-metadata.tsv"))
 trees[subject] = trace_tree(path(subject, "trace.zip"))
 case_path = path(subject, "case-report.tsv")
 case_rows = (
  ("format_version", "2"), ("subject", subject), ("scenario", scenario),
  ("session_id", intent["session_id"]), ("nonce", intent["nonce"]),
  ("run_intent_sha256", run["run_intent_sha256"]),
  ("run_metadata_sha256", digest(path(subject, "run-metadata.tsv"))),
  ("trace_metadata_sha256", digest(path(subject, "trace-metadata.tsv"))),
  ("trace_archive_sha256", trees[subject]),
  ("manual_artifacts_sha256", digest(path(subject, "manual-review.tsv"))),
  ("manual_screenshot_sha256", digest(path(subject, "stacks.png"))),
  ("manual_video_sha256", digest(path(subject, "actions.mov"))),
  ("result", "CASE-COMPLETE"), ("reason", "all-required-evidence-complete"),
 )
 case_path.write_bytes(b"".join(f"{key}\t{value}\n".encode() for key, value in case_rows))
 case_path.chmod(0o444); case_hashes[subject] = digest(case_path)
rows = [
 ("format_version", "3"), ("campaign_id", "render-campaign"),
 ("pair_metadata_sha256", digest(pair_metadata_path)),
 ("scenario_plan_sha256", pair_metadata["plan_sha256"]),
 ("workload_sha256", pair_metadata["workload_sha256"]),
 ("command_sha256", pair_metadata["command_sha256"]),
 ("environment_sha256", pair_metadata["environment_sha256"]),
 ("font_sha256", pair_metadata["font_sha256"]),
 ("initial_grid_sha256", pair_metadata["initial_grid_sha256"]),
]
for subject in subjects:
 intent = values(path(subject, "render-intent.tsv"))
 run = values(path(subject, "run-metadata.tsv"))
 rows.extend((
  (f"{subject}_session_id", intent["session_id"]),
  (f"{subject}_nonce", intent["nonce"]),
  (f"{subject}_run_intent_sha256", run["run_intent_sha256"]),
  (f"{subject}_run_metadata_sha256", digest(path(subject, "run-metadata.tsv"))),
  (f"{subject}_driver_intent_sha256", digest(path(subject, "driver-intent.tsv"))),
  (f"{subject}_driver_events_sha256", digest(path(subject, "driver-events.tsv"))),
  (f"{subject}_driver_receipt_sha256", digest(path(subject, "driver-receipt.tsv"))),
  (f"{subject}_window_identity_sha256", "a" * 64),
  (f"{subject}_driver_binary_sha256", "a" * 64),
  (f"{subject}_driver_source_sha256", "a" * 64),
  (f"{subject}_driver_controller_sha256", "a" * 64),
  (f"{subject}_plan_start_gate_sha256", "a" * 64),
  (f"{subject}_tail_receipt_sha256", run["performance_tail_receipt_sha256"]),
  (f"{subject}_quit_receipt_sha256", run["performance_quit_receipt_sha256"]),
  (f"{subject}_exit_receipt_sha256", run["subject_exit_receipt_sha256"]),
  (f"{subject}_case_report_sha256", case_hashes[subject]),
  (f"{subject}_trace_metadata_sha256", digest(path(subject, "trace-metadata.tsv"))),
  (f"{subject}_trace_archive_sha256", trees[subject]),
  (f"{subject}_manual_artifacts_sha256", digest(path(subject, "manual-review.tsv"))),
  (f"{subject}_manual_screenshot_sha256", digest(path(subject, "stacks.png"))),
  (f"{subject}_manual_video_sha256", digest(path(subject, "actions.mov"))),
 ))
spaceterm_run = values(path("spaceterm", "run-metadata.tsv"))
ghostty_run = values(path("ghostty", "run-metadata.tsv"))
rows.extend((
 ("spaceterm_lifecycle_ready_receipt_sha256", spaceterm_run["lifecycle_ready_receipt_sha256"]),
 ("spaceterm_lifecycle_registration_receipt_sha256",
  spaceterm_run["lifecycle_registration_receipt_sha256"]),
 ("ghostty_lifecycle_ready_receipt_sha256", ghostty_run["lifecycle_ready_receipt_sha256"]),
 ("ghostty_lifecycle_registration_receipt_sha256",
  ghostty_run["lifecycle_registration_receipt_sha256"]),
 ("lifecycle_helper_sha256", spaceterm_run["lifecycle_helper_sha256"]),
 ("terminator_source_sha256", spaceterm_run["terminator_source_sha256"]),
 ("terminator_binary_sha256", spaceterm_run["terminator_binary_sha256"]),
 ("evidence_mode", "production"), ("status", "complete"), ("auth_algorithm", "hmac-sha256"),
))
unsigned = b"".join(f"{key}\t{value}\n".encode() for key, value in rows)
payload = b"spaceterm.performance.pair-result/v3\0" + struct.pack(">Q", len(unsigned)) + unsigned
signature = hmac.new(secret, payload, hashlib.sha256).hexdigest()
output = root / f"{scenario}-pair-result.tsv"
output.write_bytes(unsigned + f"pair_result_hmac_sha256\t{signature}\n".encode())
output.chmod(0o444)
PY
}

run_case() {
    local scenario="$1"
    local subject="$2"
    local trace_index="${3:-$TRACE_INDEX}"
    local trace_metadata="${4:-$(artifact_path "$scenario" "$subject" trace-metadata.tsv)}"
    local trace_artifact="${5:-$(artifact_path "$scenario" "$subject" trace.zip)}"
    local manual_review="${6:-$(artifact_path "$scenario" "$subject" manual-review.tsv)}"
    local hangs_artifact="${7:-$(artifact_path "$scenario" "$subject" hangs.xml)}"
    local trace_verification="${8:-$(artifact_path "$scenario" "$subject" trace-verification.tsv)}"
    local workload_metadata="${9:-$(artifact_path "$scenario" "$subject" workload-metadata.tsv)}"
    local action_video="${10:-$(artifact_path "$scenario" "$subject" actions.mov)}"
    local stack_screenshot="${11:-$(artifact_path "$scenario" "$subject" stacks.png)}"
    local common_workload_metadata="${12:-}"
    local common_workload_events="${13:-}"
    local common_ready_receipt="${14:-}"
    local subject_identity="$SPACETERM_IDENTITY"
    local comparison_identity="$GHOSTTY_IDENTITY"
    if [[ "$subject" == ghostty ]]; then
        subject_identity="$GHOSTTY_IDENTITY"
        comparison_identity="$SPACETERM_IDENTITY"
    fi
    local -a workload_arguments=()
    if [[ "$scenario" == perf-render-sustained-output ]]; then
        common_workload_metadata="${common_workload_metadata:-$(artifact_path "$scenario" "$subject" common-workload-metadata.tsv)}"
        common_workload_events="${common_workload_events:-$(artifact_path "$scenario" "$subject" common-workload-events.tsv)}"
        common_ready_receipt="${common_ready_receipt:-$(artifact_path "$scenario" "$subject" common-workload-ready.tsv)}"
    fi
    if [[ -n "$common_workload_metadata$common_workload_events$common_ready_receipt" ]]; then
        workload_arguments=(
            --workload-metadata "$common_workload_metadata"
            --workload-events "$common_workload_events"
            --workload-ready-receipt "$common_ready_receipt"
        )
    fi
    if [[ "$SEAL_CASE_INPUTS" == true ]]; then
        for evidence in \
            "$TEMP_ROOT/$scenario-plan.tsv" \
            "$TEMP_ROOT/$scenario-plan-metadata.tsv" \
            "$TEMP_ROOT/$scenario-pair.tsv" \
            "$TEMP_ROOT/$scenario-pair-result.tsv" \
            "$(artifact_path "$scenario" "$subject" case-report.tsv)" \
            "$subject_identity" "$comparison_identity" \
            "$(artifact_path "$scenario" "$subject" run-metadata.tsv)" \
            "$(artifact_path "$scenario" "$subject" render-intent.tsv)" \
            "$(artifact_path "$scenario" "$subject" render-evidence.tsv)" \
            "$workload_metadata" "$common_workload_metadata" \
            "$common_workload_events" "$common_ready_receipt" \
            "$(artifact_path "$scenario" "$subject" driver-events.tsv)" \
            "$trace_index" "$trace_metadata" "$trace_artifact" \
            "$(artifact_path "$scenario" "$subject" trace-toc.xml)" \
            "$trace_verification" \
            "$(artifact_path "$scenario" "$subject" campaign-manifest.tsv)" \
            "$(artifact_path "$scenario" "$subject" trace-anchor-receipt.tsv)" \
            "$(artifact_path "$scenario" "$subject" driver-intent.tsv)" \
            "$(artifact_path "$scenario" "$subject" driver-receipt.tsv)" \
            "$(artifact_path "$scenario" "$subject" trace-receipt.tsv)" \
            "$(artifact_path "$scenario" "$subject" time-profiler.xml)" \
            "$(artifact_path "$scenario" "$subject" allocations.xml)" \
            "$hangs_artifact" "$manual_review" "$stack_screenshot" "$action_video"; do
            [[ -z "$evidence" || ! -f "$evidence" ]] || chmod a-w -- "$evidence"
        done
    fi
    "$SCRIPT_DIRECTORY/analyze-release-render-profile-case.sh" \
        --subject "$subject" \
        --scenario "$scenario" \
        --plan "$TEMP_ROOT/$scenario-plan.tsv" \
        --plan-metadata "$TEMP_ROOT/$scenario-plan-metadata.tsv" \
        --pair-metadata "$TEMP_ROOT/$scenario-pair.tsv" \
        --pair-result "$TEMP_ROOT/$scenario-pair-result.tsv" \
        --case-report "$(artifact_path "$scenario" "$subject" case-report.tsv)" \
        --subject-identity "$subject_identity" \
        --comparison-subject-identity "$comparison_identity" \
        --run-metadata "$(artifact_path "$scenario" "$subject" run-metadata.tsv)" \
        --render-intent "$(artifact_path "$scenario" "$subject" render-intent.tsv)" \
        --render-evidence "$(artifact_path "$scenario" "$subject" render-evidence.tsv)" \
        --render-workload-metadata "$workload_metadata" \
        "${workload_arguments[@]}" \
        --campaign-secret-file "$RENDER_SECRET" \
        --driver-events "$(artifact_path "$scenario" "$subject" driver-events.tsv)" \
        --trace-index "$trace_index" \
        --trace-metadata "$trace_metadata" \
        --trace-artifact "$trace_artifact" \
        --trace-toc "$(artifact_path "$scenario" "$subject" trace-toc.xml)" \
        --trace-verification "$trace_verification" \
        --campaign-manifest \
            "$(artifact_path "$scenario" "$subject" campaign-manifest.tsv)" \
        --trace-anchor-receipt \
            "$(artifact_path "$scenario" "$subject" trace-anchor-receipt.tsv)" \
        --driver-intent "$(artifact_path "$scenario" "$subject" driver-intent.tsv)" \
        --driver-receipt "$(artifact_path "$scenario" "$subject" driver-receipt.tsv)" \
        --trace-receipt "$(artifact_path "$scenario" "$subject" trace-receipt.tsv)" \
        --time-profiler-artifact \
            "$(artifact_path "$scenario" "$subject" time-profiler.xml)" \
        --allocations-artifact \
            "$(artifact_path "$scenario" "$subject" allocations.xml)" \
        --hangs-artifact "$hangs_artifact" \
        --manual-review "$manual_review" \
        --stack-screenshot "$stack_screenshot" \
        --action-video "$action_video" \
        --ffprobe "$FAKE_FFPROBE"
}

for command in awk bash chmod cp grep ln mktemp mv openssl python3 rm sed shasum; do
    command -v "$command" >/dev/null 2>&1 || fail "missing command: $command"
done

FAKE_XCRUN="$TEMP_ROOT/fake-xcrun"
FAKE_SIPS="$TEMP_ROOT/fake-sips"
FAKE_FFPROBE="$TEMP_ROOT/fake-ffprobe"
FAKE_TRACE_VERIFIER="$TEMP_ROOT/fake-trace-verifier"
cat > "$FAKE_XCRUN" <<'EOF'
#!/bin/bash
set -euo pipefail
[[ "$1" == xctrace && "$2" == export ]]
shift 2
input="" output="" mode="" xpath=""
while (( $# > 0 )); do
    case "$1" in
        --input) input="${2:?}"; shift ;;
        --output) output="${2:?}"; shift ;;
        --toc) mode=toc ;;
        --xpath) mode=xpath; xpath="${2:?}"; shift ;;
        *) exit 64 ;;
    esac
    shift
done
trace_id="$(<"$input/data/id")"
if [[ "$mode" == toc ]]; then
    printf '<trace-toc trace="%s"/>\n' "$trace_id" > "$output"
else
    case "$xpath" in
        '/trace-toc/run[@number="1"]/data/table[@schema="time-profile"]')
            printf '<time-profile trace="%s"/>\n' "$trace_id" > "$output" ;;
        '/trace-toc/run[@number="1"]/tracks/track[@name="Allocations"]/details/detail[@name="Allocations List"]')
            printf '<allocations trace="%s"/>\n' "$trace_id" > "$output" ;;
        '/trace-toc/run[@number="1"]/data/table[@schema="potential-hangs"]')
            printf '<hangs trace="%s"/>\n' "$trace_id" > "$output" ;;
        *) exit 65 ;;
    esac
fi
EOF
cat > "$FAKE_SIPS" <<'EOF'
#!/bin/bash
set -euo pipefail
for argument in "$@"; do
    [[ -f "$argument" ]] || continue
    grep -Fq invalid-screenshot "$argument" && exit 1
done
printf '  format: png\n  pixelWidth: 100\n  pixelHeight: 100\n'
EOF
cat > "$FAKE_FFPROBE" <<'EOF'
#!/bin/bash
set -euo pipefail
video="${!#}"
duration="$(awk -F = '$1 == "duration" { print $2 }' "$video")"
kind="$(awk -F = '$1 == "kind" { print $2 }' "$video")"
if [[ " $* " == *" -of json "* ]]; then
    if [[ "$kind" == audio-only ]]; then
        printf '{"format":{"duration":"%s"},"streams":[{"index":0,"codec_type":"audio"}]}\n' "$duration"
    else
        frames="$((duration * 5 + 1))"
        printf '{"format":{"duration":"%s"},"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":100,"height":100,"duration":"%s","nb_read_frames":"%s","nb_read_packets":"%s","disposition":{"attached_pic":0}}]}\n' "$duration" "$duration" "$frames" "$frames"
    fi
    exit 0
fi
interval=""
while (( $# > 0 )); do
    if [[ "$1" == -read_intervals ]]; then interval="${2:?}"; break; fi
    shift
done
if [[ -z "$interval" ]]; then
    awk -v duration="$duration" 'BEGIN {
        for (frame = 0; frame <= duration * 5; frame += 1) {
            printf "media_type=video|best_effort_timestamp_time=%.3f|pkt_duration_time=0.033|width=100|height=100\n", frame / 5
        }
    }'
elif [[ "$interval" == "%+2" ]]; then
    printf 'media_type=video|best_effort_timestamp_time=0.000|pkt_duration_time=0.033|width=100|height=100\n'
else
    awk -v duration="$duration" 'BEGIN { printf "media_type=video|best_effort_timestamp_time=%.3f|pkt_duration_time=0.033|width=100|height=100\n", duration - 0.033 }'
fi
EOF
cat > "$FAKE_TRACE_VERIFIER" <<'EOF'
#!/usr/bin/python3
import argparse
from pathlib import Path

parser = argparse.ArgumentParser()
for name in ("toc", "time-profile", "allocations", "hangs", "pid",
             "process-name", "requested-seconds", "command-elapsed-seconds"):
    parser.add_argument(f"--{name}")
arguments = parser.parse_args()
hang_ms = "0"
for line in Path(arguments.hangs).read_text().splitlines():
    if line.startswith("hang_ms="):
        hang_ms = line.partition("=")[2]
print("reason\tnone")
print(f"actual_record_duration_seconds\t{arguments.requested_seconds}.000000")
print("time_profiler_rows\t120")
print("allocations_rows\t20")
print("hangs_rows\t0")
print(f"maximum_main_thread_hang_ms\t{hang_ms}")
EOF
chmod 0755 "$FAKE_XCRUN" "$FAKE_SIPS" "$FAKE_FFPROBE" \
    "$FAKE_TRACE_VERIFIER"
export SPACETERM_RENDER_PROFILE_XCRUN="$FAKE_XCRUN"
export SPACETERM_RENDER_PROFILE_SIPS="$FAKE_SIPS"
export SPACETERM_RENDER_PROFILE_FFPROBE="$FAKE_FFPROBE"
export SPACETERM_RENDER_PROFILE_TRACE_VERIFIER="$FAKE_TRACE_VERIFIER"

SPACETERM_IDENTITY="$TEMP_ROOT/spaceterm-identity.tsv"
GHOSTTY_IDENTITY="$TEMP_ROOT/ghostty-identity.tsv"
write_subject_identity spaceterm "$SPACETERM_IDENTITY" 123
write_subject_identity ghostty "$GHOSTTY_IDENTITY" 456
chmod 0444 "$SPACETERM_IDENTITY" "$GHOSTTY_IDENTITY"
readonly SPACETERM_IDENTITY GHOSTTY_IDENTITY
WORKLOAD_BINARY="$TEMP_ROOT/performance-workload"
printf 'render-profile fixture workload\n' > "$WORKLOAD_BINARY"
for manifest in command environment font initial-grid; do
    printf '%s-manifest-v1\n' "$manifest" > "$TEMP_ROOT/$manifest.tsv"
    chmod 0444 "$TEMP_ROOT/$manifest.tsv"
done
RENDER_SECRET="$TEMP_ROOT/render-secret"
printf '%s\n' "$HASH_A" > "$RENDER_SECRET"
chmod 0400 "$RENDER_SECRET"
readonly RENDER_SECRET

TOOL_SOURCE_REPOSITORY="$TEMP_ROOT/tool-source"
TOOL_BUNDLE="$TEMP_ROOT/tool-bundle"
mkdir -p "$TOOL_SOURCE_REPOSITORY/scripts/acceptance" "$TOOL_BUNDLE/scripts/acceptance"
TOOL_NAMES='record_release_performance_trace
freeze_render_profile_intent
finalize_render_profile_evidence
render_profile_hmac
render_trace_receipt
analyze_release_render_profile_case
archive_render_trace
verify_render_action_video
verify_render_trace_archive
verify_release_performance_trace
inspect_release_performance_process
run_release_performance_command
freeze_render_profile_tool_bundle'
TOOL_RELATIVES='scripts/record-release-performance-trace.sh
scripts/acceptance/freeze-render-profile-intent.sh
scripts/acceptance/finalize-render-profile-evidence.sh
scripts/acceptance/render-profile-hmac.py
scripts/acceptance/render-trace-receipt.py
scripts/acceptance/analyze-release-render-profile-case.sh
scripts/acceptance/archive-render-trace.py
scripts/acceptance/verify-render-action-video.py
scripts/acceptance/verify-render-trace-archive.py
scripts/verify-release-performance-trace.py
scripts/inspect-release-performance-process.py
scripts/run-release-performance-command.py
scripts/acceptance/freeze-render-profile-tool-bundle.sh'
for relative in $TOOL_RELATIVES; do
    source="$TOOL_SOURCE_REPOSITORY/$relative"
    bundle="$TOOL_BUNDLE/$relative"
    mkdir -p "$(dirname "$source")" "$(dirname "$bundle")"
    case "$relative" in
        scripts/acceptance/freeze-render-profile-intent.sh)
            cp "$SCRIPT_DIRECTORY/freeze-render-profile-intent.sh" "$source" ;;
        scripts/acceptance/finalize-render-profile-evidence.sh)
            cp "$SCRIPT_DIRECTORY/finalize-render-profile-evidence.sh" "$source" ;;
        scripts/acceptance/render-profile-hmac.py)
            cp "$SCRIPT_DIRECTORY/render-profile-hmac.py" "$source" ;;
        *) printf '#!/bin/sh\nexit 0\n' > "$source" ;;
    esac
    cp "$source" "$bundle"
    chmod 0555 "$source" "$bundle"
done
/usr/bin/git init -q "$TOOL_SOURCE_REPOSITORY"
/usr/bin/git -C "$TOOL_SOURCE_REPOSITORY" add .
GIT_AUTHOR_NAME=fixture GIT_AUTHOR_EMAIL=fixture@example.test \
GIT_COMMITTER_NAME=fixture GIT_COMMITTER_EMAIL=fixture@example.test \
    /usr/bin/git -C "$TOOL_SOURCE_REPOSITORY" commit -qm fixture
TOOL_SOURCE_COMMIT="$(/usr/bin/git -C "$TOOL_SOURCE_REPOSITORY" rev-parse HEAD)"
TOOL_BUNDLE_MANIFEST="$TOOL_BUNDLE/tool-bundle-manifest.tsv"
{
    printf 'format_version\t1\nschema\tspaceterm.render-profile-tool-bundle/v1\n'
    printf 'source_commit\t%s\ntool_count\t13\n' "$TOOL_SOURCE_COMMIT"
    tool_index=0
    for name in $TOOL_NAMES; do
        tool_index=$((tool_index + 1))
        relative="$(printf '%s\n' "$TOOL_RELATIVES" | sed -n "${tool_index}p")"
        hash="$(sha256 "$TOOL_SOURCE_REPOSITORY/$relative")"
        printf '%s_source_path\t%s\n' "$name" "$TOOL_SOURCE_REPOSITORY/$relative"
        printf '%s_source_sha256\t%s\n' "$name" "$hash"
        printf '%s_bundle_path\t%s\n' "$name" "$TOOL_BUNDLE/$relative"
        printf '%s_bundle_sha256\t%s\n' "$name" "$hash"
    done
} > "$TOOL_BUNDLE_MANIFEST"
chmod 0444 "$TOOL_BUNDLE_MANIFEST"
INTENT_TOOL="$TOOL_BUNDLE/scripts/acceptance/freeze-render-profile-intent.sh"
FINALIZER_TOOL="$TOOL_BUNDLE/scripts/acceptance/finalize-render-profile-evidence.sh"
declare -a TOOL_BUNDLE_ARGS=(
    --render-tool-bundle-manifest "$TOOL_BUNDLE_MANIFEST"
    --expected-source-commit "$TOOL_SOURCE_COMMIT"
    --trusted-source-repository "$TOOL_SOURCE_REPOSITORY"
)

for scenario in "${RENDER_SCENARIOS[@]}"; do
    plan="$TEMP_ROOT/$scenario-plan.tsv"
    metadata="$TEMP_ROOT/$scenario-plan-metadata.tsv"
    "$SCRIPT_DIRECTORY/performance-plan.sh" \
        --scenario "$scenario" --plan "$plan" --metadata "$metadata" >/dev/null
    [[ ! -w "$plan" && ! -w "$metadata" ]] || fail "$scenario plan is mutable"
    [[ "$(metric "$metadata" format_version)" == 2 \
        && "$(metric "$metadata" scenario)" == "$scenario" \
        && "$(metric "$metadata" plan_sha256)" == "$(sha256 "$plan")" ]] \
        || fail "$scenario plan metadata is invalid"
    "$SCRIPT_DIRECTORY/freeze-performance-pair.sh" \
        --pair-id "pair-$scenario" \
        --scenario "$scenario" \
        --plan "$plan" \
        --plan-metadata "$metadata" \
        --workload-binary "$WORKLOAD_BINARY" \
        --command-manifest "$TEMP_ROOT/command.tsv" \
        --environment-manifest "$TEMP_ROOT/environment.tsv" \
        --font-manifest "$TEMP_ROOT/font.tsv" \
        --initial-grid-manifest "$TEMP_ROOT/initial-grid.tsv" \
        --spaceterm-identity "$SPACETERM_IDENTITY" \
        --ghostty-identity "$GHOSTTY_IDENTITY" \
        --output "$TEMP_ROOT/$scenario-pair.tsv" >/dev/null
    for subject in spaceterm ghostty; do
        identity="$SPACETERM_IDENTITY"
        [[ "$subject" == spaceterm ]] || identity="$GHOSTTY_IDENTITY"
        run_metadata="$(artifact_path "$scenario" "$subject" run-metadata.tsv)"
        run_intent="$(artifact_path "$scenario" "$subject" run-intent.tsv)"
        workload_metadata="$(artifact_path "$scenario" "$subject" workload-metadata.tsv)"
        driver_events="$(artifact_path "$scenario" "$subject" driver-events.tsv)"
        action_video="$(artifact_path "$scenario" "$subject" actions.mov)"
        render_intent="$(artifact_path "$scenario" "$subject" render-intent.tsv)"
        render_evidence="$(artifact_path "$scenario" "$subject" render-evidence.tsv)"
        session_id="render-session"
        nonce="$(sha256_text "$scenario:$subject")"
        write_run_intent_fixture "$subject" "$scenario" "$identity" \
            "$TEMP_ROOT/$scenario-pair.tsv" "$nonce" "$run_intent"
        "$INTENT_TOOL" \
            --subject "$subject" --scenario "$scenario" \
            --campaign-id render-campaign --session-id "$session_id" --nonce "$nonce" \
            --plan "$plan" --plan-metadata "$metadata" \
            --pair-metadata "$TEMP_ROOT/$scenario-pair.tsv" \
            --run-intent "$run_intent" --command-manifest "$TEMP_ROOT/command.tsv" \
            --environment-manifest "$TEMP_ROOT/environment.tsv" \
            --font-manifest "$TEMP_ROOT/font.tsv" \
            --initial-grid-manifest "$TEMP_ROOT/initial-grid.tsv" \
            --subject-identity "$identity" --expected-driver-events "$driver_events" \
            --action-video "$action_video" --final-metadata "$render_evidence" \
            --hmac-secret "$RENDER_SECRET" --output "$render_intent" \
            "${TOOL_BUNDLE_ARGS[@]}" >/dev/null
        trace_id="$scenario--$subject"
        write_trace_zip "$(artifact_path "$scenario" "$subject" trace.zip)" "$trace_id"
        printf '<trace-toc trace="%s"/>\n' "$trace_id" \
            > "$(artifact_path "$scenario" "$subject" trace-toc.xml)"
        printf '<time-profile trace="%s"/>\n' "$trace_id" \
            > "$(artifact_path "$scenario" "$subject" time-profiler.xml)"
        printf '<allocations trace="%s"/>\n' "$trace_id" \
            > "$(artifact_path "$scenario" "$subject" allocations.xml)"
        printf '<hangs trace="%s"/>\n' "$trace_id" \
            > "$(artifact_path "$scenario" "$subject" hangs.xml)"
        "$FAKE_TRACE_VERIFIER" \
            --requested-seconds "$(( $(scenario_duration "$scenario") / 1000 ))" \
            --hangs "$(artifact_path "$scenario" "$subject" hangs.xml)" \
            > "$(artifact_path "$scenario" "$subject" trace-verification.tsv)"
        printf 'synthetic png checked only through test override %s %s\n' "$scenario" "$subject" \
            > "$(artifact_path "$scenario" "$subject" stacks.png)"
        printf 'duration=%s\nkind=valid\nscenario=%s\nsubject=%s\n' \
            "$(( $(scenario_duration "$scenario") / 1000 ))" "$scenario" "$subject" \
            > "$action_video"
        chmod 0444 "$action_video"
        write_driver_events "$plan" "$identity" "$driver_events"
        "$SCRIPT_DIRECTORY/freeze-render-profile-workload.sh" \
            --subject "$subject" \
            --scenario "$scenario" \
            --plan "$plan" \
            --plan-metadata "$metadata" \
            --pair-metadata "$TEMP_ROOT/$scenario-pair.tsv" \
            --subject-identity "$identity" \
            --driver-events "$driver_events" \
            --action-video "$action_video" \
            --output "$workload_metadata" >/dev/null
        "$FINALIZER_TOOL" \
            --intent "$render_intent" --plan "$plan" --plan-metadata "$metadata" \
            --pair-metadata "$TEMP_ROOT/$scenario-pair.tsv" \
            --run-intent "$run_intent" --command-manifest "$TEMP_ROOT/command.tsv" \
            --environment-manifest "$TEMP_ROOT/environment.tsv" \
            --font-manifest "$TEMP_ROOT/font.tsv" \
            --initial-grid-manifest "$TEMP_ROOT/initial-grid.tsv" \
            --subject-identity "$identity" --driver-events "$driver_events" \
            --action-video "$action_video" \
            --render-workload-metadata "$workload_metadata" \
            --hmac-secret "$RENDER_SECRET" --output "$render_evidence" \
            "${TOOL_BUNDLE_ARGS[@]}" >/dev/null
        write_run_metadata_fixture "$subject" "$run_intent" "$run_metadata"
        common_workload_metadata=""
        common_workload_events=""
        common_ready_receipt=""
        if [[ "$scenario" == perf-render-sustained-output ]]; then
            common_workload_metadata="$(artifact_path "$scenario" "$subject" common-workload-metadata.tsv)"
            common_workload_events="$(artifact_path "$scenario" "$subject" common-workload-events.tsv)"
            common_ready_receipt="$(artifact_path "$scenario" "$subject" common-workload-ready.tsv)"
            write_common_workload_v3_fixture "$scenario" "$identity" "$run_metadata" \
                "$workload_metadata" "$common_workload_events" \
                "$common_ready_receipt" "$common_workload_metadata"
        fi
        trace_metadata="$(artifact_path "$scenario" "$subject" trace-metadata.tsv)"
        write_trace_metadata "$identity" "$run_metadata" "$render_evidence" \
            "$workload_metadata" \
            "$(scenario_duration "$scenario")" "$trace_metadata" \
            "$common_workload_metadata" "$common_ready_receipt"
        for receipt_suffix in campaign-manifest trace-anchor-receipt driver-intent \
            driver-receipt trace-receipt; do
            receipt_fixture="$(artifact_path "$scenario" "$subject" "$receipt_suffix.tsv")"
            printf 'synthetic-%s\t%s\t%s\n' "$receipt_suffix" "$scenario" "$subject" \
                > "$receipt_fixture"
            chmod 0444 "$receipt_fixture"
        done
    done
done

TRACE_INDEX="$TEMP_ROOT/render-profile-trace-index.tsv"
{
    printf 'scenario\tsubject\tsubject_identity_sha256\tpair_metadata_sha256\tcampaign_id\tsession_id\tnonce\tcampaign_manifest_sha256\ttrace_anchor_receipt_sha256\tdriver_intent_sha256\tdriver_receipt_sha256\ttrace_receipt_sha256\ttrace_metadata_sha256\ttrace_artifact_sha256\ttrace_toc_sha256\ttrace_verification_sha256\ttrace_verifier_sha256\ttime_profiler_artifact_sha256\tallocations_artifact_sha256\thangs_artifact_sha256\trepresentative_stack_screenshot_sha256\taction_video_sha256\n'
    for scenario in "${RENDER_SCENARIOS[@]}"; do
        for subject in spaceterm ghostty; do
            identity="$SPACETERM_IDENTITY"
            [[ "$subject" == spaceterm ]] || identity="$GHOSTTY_IDENTITY"
            render_intent="$(artifact_path "$scenario" "$subject" render-intent.tsv)"
            printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
                "$scenario" "$subject" "$(sha256 "$identity")" \
                "$(sha256 "$TEMP_ROOT/$scenario-pair.tsv")" \
                "$(metric "$render_intent" campaign_id)" \
                "$(metric "$render_intent" session_id)" \
                "$(metric "$render_intent" nonce)" \
                "$(sha256 "$(artifact_path "$scenario" "$subject" campaign-manifest.tsv)")" \
                "$(sha256 "$(artifact_path "$scenario" "$subject" trace-anchor-receipt.tsv)")" \
                "$(sha256 "$(artifact_path "$scenario" "$subject" driver-intent.tsv)")" \
                "$(sha256 "$(artifact_path "$scenario" "$subject" driver-receipt.tsv)")" \
                "$(sha256 "$(artifact_path "$scenario" "$subject" trace-receipt.tsv)")" \
                "$(sha256 "$(artifact_path "$scenario" "$subject" trace-metadata.tsv)")" \
                "$(sha256 "$(artifact_path "$scenario" "$subject" trace.zip)")" \
                "$(sha256 "$(artifact_path "$scenario" "$subject" trace-toc.xml)")" \
                "$(sha256 "$(artifact_path "$scenario" "$subject" trace-verification.tsv)")" \
                "$(sha256 "$FAKE_TRACE_VERIFIER")" \
                "$(sha256 "$(artifact_path "$scenario" "$subject" time-profiler.xml)")" \
                "$(sha256 "$(artifact_path "$scenario" "$subject" allocations.xml)")" \
                "$(sha256 "$(artifact_path "$scenario" "$subject" hangs.xml)")" \
                "$(sha256 "$(artifact_path "$scenario" "$subject" stacks.png)")" \
                "$(sha256 "$(artifact_path "$scenario" "$subject" actions.mov)")"
        done
    done
} > "$TRACE_INDEX"
chmod 0444 "$TRACE_INDEX"
readonly TRACE_INDEX

for scenario in "${RENDER_SCENARIOS[@]}"; do
    for subject in spaceterm ghostty; do
        manual="$(artifact_path "$scenario" "$subject" manual-review.tsv)"
        write_manual_review "$scenario" "$subject" "$TRACE_INDEX" \
            "$(artifact_path "$scenario" "$subject" trace-metadata.tsv)" "$manual"
    done
    write_pair_closure "$scenario"
    for subject in spaceterm ghostty; do
        expect_result 2 NOT-RUN render-profile-test-overrides-active \
            "synthetic $scenario $subject cannot become final evidence" \
            run_case "$scenario" "$subject"
    done
done

snapshot_scenario=perf-render-idle-cursor-blink
snapshot_subject=ghostty
snapshot_manual="$(artifact_path "$snapshot_scenario" "$snapshot_subject" manual-review.tsv)"
PAIR_RESULT_PATH="$TEMP_ROOT/$snapshot_scenario-pair-result.tsv"
PAIR_RESULT_BACKUP="$TEMP_ROOT/$snapshot_scenario-pair-result.original.tsv"
mv -- "$PAIR_RESULT_PATH" "$PAIR_RESULT_BACKUP"
cp -- "$PAIR_RESULT_BACKUP" "$PAIR_RESULT_PATH"
chmod u+w "$PAIR_RESULT_PATH"
sed -i '' 's/evidence_mode\tproduction/evidence_mode\ttest-only/' "$PAIR_RESULT_PATH"
chmod 0444 "$PAIR_RESULT_PATH"
expect_result 2 NOT-RUN performance-pair-result-or-case-report-invalid \
    "tampered exact62 pair result" run_case "$snapshot_scenario" "$snapshot_subject"
rm -- "$PAIR_RESULT_PATH"
mv -- "$PAIR_RESULT_BACKUP" "$PAIR_RESULT_PATH"
CASE_REPORT_PATH="$(artifact_path "$snapshot_scenario" "$snapshot_subject" case-report.tsv)"
CASE_REPORT_BACKUP="$TEMP_ROOT/$snapshot_scenario-case-report.original.tsv"
mv -- "$CASE_REPORT_PATH" "$CASE_REPORT_BACKUP"
cp -- "$CASE_REPORT_BACKUP" "$CASE_REPORT_PATH"
chmod u+w "$CASE_REPORT_PATH"
sed -i '' 's/result\tCASE-COMPLETE/result\tNOT-RUN/' "$CASE_REPORT_PATH"
chmod 0444 "$CASE_REPORT_PATH"
expect_result 2 NOT-RUN performance-pair-result-or-case-report-invalid \
    "tampered exact14 case report" run_case "$snapshot_scenario" "$snapshot_subject"
rm -- "$CASE_REPORT_PATH"
mv -- "$CASE_REPORT_BACKUP" "$CASE_REPORT_PATH"
WRITABLE_MANUAL="$TEMP_ROOT/writable-manual-review.tsv"
cp -- "$snapshot_manual" "$WRITABLE_MANUAL"
chmod 0644 "$WRITABLE_MANUAL"
SEAL_CASE_INPUTS=false
expect_result 2 NOT-RUN render-evidence-is-not-canonical-singleton-immutable \
    "writable manual review" run_case "$snapshot_scenario" "$snapshot_subject" \
    "$TRACE_INDEX" \
    "$(artifact_path "$snapshot_scenario" "$snapshot_subject" trace-metadata.tsv)" \
    "$(artifact_path "$snapshot_scenario" "$snapshot_subject" trace.zip)" \
    "$WRITABLE_MANUAL"
SEAL_CASE_INPUTS=true

HARDLINKED_MANUAL="$TEMP_ROOT/hardlinked-manual-review.tsv"
ln -- "$snapshot_manual" "$HARDLINKED_MANUAL"
expect_result 2 NOT-RUN render-evidence-is-not-canonical-singleton-immutable \
    "hardlinked manual review" run_case "$snapshot_scenario" "$snapshot_subject" \
    "$TRACE_INDEX" \
    "$(artifact_path "$snapshot_scenario" "$snapshot_subject" trace-metadata.tsv)" \
    "$(artifact_path "$snapshot_scenario" "$snapshot_subject" trace.zip)" \
    "$HARDLINKED_MANUAL"
rm -- "$HARDLINKED_MANUAL"

SWAP_RESTORE_HOOK="$TEMP_ROOT/swap-restore-evidence-hook"
cat > "$SWAP_RESTORE_HOOK" <<'EOF'
#!/bin/bash
set -euo pipefail
target="${1:?}"
backup="$target.snapshot-original.$$"
replacement="$target.snapshot-replacement.$$"
cleanup_hook() {
    [[ ! -e "$replacement" ]] || /bin/rm -- "$replacement"
    if [[ -e "$backup" ]]; then
        [[ ! -e "$target" ]] || /bin/rm -- "$target"
        /bin/mv -- "$backup" "$target"
    fi
}
trap cleanup_hook EXIT INT TERM
/bin/cp -- "$target" "$replacement"
/bin/chmod 0444 "$replacement"
/bin/mv -- "$target" "$backup"
/bin/mv -- "$replacement" "$target"
/bin/rm -- "$target"
/bin/mv -- "$backup" "$target"
/bin/chmod 0644 "$target"
/bin/chmod 0444 "$target"
trap - EXIT INT TERM
EOF
chmod 0555 "$SWAP_RESTORE_HOOK"
export SPACETERM_RENDER_PROFILE_TEST_EVIDENCE_SNAPSHOT_HOOK="$SWAP_RESTORE_HOOK"
expect_result 2 NOT-RUN render-evidence-identity-changed-during-validation \
    "mid-validation evidence swap and restore" \
    run_case "$snapshot_scenario" "$snapshot_subject"
unset SPACETERM_RENDER_PROFILE_TEST_EVIDENCE_SNAPSHOT_HOOK

FAKE_PATH="$TEMP_ROOT/fake-path"
mkdir -p -- "$FAKE_PATH"
for fake_tool in awk python3 realpath shasum; do
    fake_path="$FAKE_PATH/$fake_tool"
    {
        printf '#!/bin/bash\n'
        printf 'printf "forged tool must never run\\n" >&2\n'
        printf 'exit 99\n'
    } > "$fake_path"
    chmod 0755 "$fake_path"
done
for fake_tool in awk python3 realpath shasum; do
    isolated_path="$TEMP_ROOT/fake-path-$fake_tool"
    mkdir -p -- "$isolated_path"
    cp "$FAKE_PATH/$fake_tool" "$isolated_path/$fake_tool"
    output="$TEMP_ROOT/fake-path-$fake_tool.out"
    status=0
    PATH="$isolated_path:/usr/bin:/bin:/usr/sbin:/sbin" \
        run_case perf-render-idle-cursor-blink ghostty \
        > "$output" 2>/dev/null || status=$?
    [[ "$status" == 2 ]] \
        || fail "PATH-substituted $fake_tool exit: expected 2, observed $status"
    [[ "$(metric "$output" result)" == NOT-RUN \
        && "$(metric "$output" reason)" == render-profile-test-overrides-active ]] \
        || fail "PATH-substituted $fake_tool did not fail closed as a test override"
done

saved_xcrun="$SPACETERM_RENDER_PROFILE_XCRUN"
saved_sips="$SPACETERM_RENDER_PROFILE_SIPS"
saved_ffprobe="$SPACETERM_RENDER_PROFILE_FFPROBE"
saved_trace_verifier="$SPACETERM_RENDER_PROFILE_TRACE_VERIFIER"
unset SPACETERM_RENDER_PROFILE_XCRUN SPACETERM_RENDER_PROFILE_SIPS \
    SPACETERM_RENDER_PROFILE_FFPROBE SPACETERM_RENDER_PROFILE_TRACE_VERIFIER
missing_tool_receipt_output="$TEMP_ROOT/missing-tool-receipt.out"
missing_tool_receipt_status=0
run_case perf-render-idle-cursor-blink ghostty \
    > "$missing_tool_receipt_output" 2>/dev/null || missing_tool_receipt_status=$?
[[ "$missing_tool_receipt_status" == 2 \
    && "$(metric "$missing_tool_receipt_output" result)" == NOT-RUN \
    && "$(metric "$missing_tool_receipt_output" reason)" \
        == render-trace-receipt-required ]] \
    || fail "production render analyzer accepted evidence without tool receipt binding"
export SPACETERM_RENDER_PROFILE_XCRUN="$saved_xcrun"
export SPACETERM_RENDER_PROFILE_SIPS="$saved_sips"
export SPACETERM_RENDER_PROFILE_FFPROBE="$saved_ffprobe"
export SPACETERM_RENDER_PROFILE_TRACE_VERIFIER="$saved_trace_verifier"

sustained=perf-render-sustained-output
sustained_subject=ghostty
sustained_render_workload="$(artifact_path "$sustained" "$sustained_subject" workload-metadata.tsv)"
sustained_video="$(artifact_path "$sustained" "$sustained_subject" actions.mov)"
sustained_stack="$(artifact_path "$sustained" "$sustained_subject" stacks.png)"
sustained_common_metadata="$(artifact_path "$sustained" "$sustained_subject" common-workload-metadata.tsv)"
sustained_common_events="$(artifact_path "$sustained" "$sustained_subject" common-workload-events.tsv)"
sustained_common_ready="$(artifact_path "$sustained" "$sustained_subject" common-workload-ready.tsv)"

expect_result 2 NOT-RUN sustained-output-workload-v3-evidence-missing \
    "sustained output rejects missing workload-v3 evidence" \
    "$SCRIPT_DIRECTORY/analyze-release-render-profile-case.sh" \
        --subject "$sustained_subject" --scenario "$sustained" \
        --plan "$TEMP_ROOT/$sustained-plan.tsv" \
        --plan-metadata "$TEMP_ROOT/$sustained-plan-metadata.tsv"

expect_result 2 NOT-RUN sustained-output-workload-v3-authentication-invalid \
    "sustained output rejects wrong-session workload-v3 evidence" \
    run_case "$sustained" "$sustained_subject" "$TRACE_INDEX" \
        "$(artifact_path "$sustained" "$sustained_subject" trace-metadata.tsv)" \
        "$(artifact_path "$sustained" "$sustained_subject" trace.zip)" \
        "$(artifact_path "$sustained" "$sustained_subject" manual-review.tsv)" \
        "$(artifact_path "$sustained" "$sustained_subject" hangs.xml)" \
        "$(artifact_path "$sustained" "$sustained_subject" trace-verification.tsv)" \
        "$sustained_render_workload" "$sustained_video" "$sustained_stack" \
        "$sustained_common_metadata" "$sustained_common_events" \
        "$(artifact_path "$sustained" spaceterm common-workload-ready.tsv)"

TAMPERED_COMMON_EVENTS="$TEMP_ROOT/tampered-sustained-common-events.tsv"
cp "$sustained_common_events" "$TAMPERED_COMMON_EVENTS"
chmod 0644 "$TAMPERED_COMMON_EVENTS"
sed -i '' 's/producer-end\tnone\t1000/producer-end\tnone\t1001/' \
    "$TAMPERED_COMMON_EVENTS"
chmod 0444 "$TAMPERED_COMMON_EVENTS"
expect_result 2 NOT-RUN sustained-output-workload-v3-authentication-invalid \
    "sustained output rejects tampered authenticated event stream" \
    run_case "$sustained" "$sustained_subject" "$TRACE_INDEX" \
        "$(artifact_path "$sustained" "$sustained_subject" trace-metadata.tsv)" \
        "$(artifact_path "$sustained" "$sustained_subject" trace.zip)" \
        "$(artifact_path "$sustained" "$sustained_subject" manual-review.tsv)" \
        "$(artifact_path "$sustained" "$sustained_subject" hangs.xml)" \
        "$(artifact_path "$sustained" "$sustained_subject" trace-verification.tsv)" \
        "$sustained_render_workload" "$sustained_video" "$sustained_stack" \
        "$sustained_common_metadata" "$TAMPERED_COMMON_EVENTS" \
        "$sustained_common_ready"

ZERO_OUTPUT_METADATA="$TEMP_ROOT/zero-output-sustained-common-metadata.tsv"
write_common_workload_v3_fixture "$sustained" "$GHOSTTY_IDENTITY" \
    "$(artifact_path "$sustained" "$sustained_subject" run-metadata.tsv)" \
    "$sustained_render_workload" "$TEMP_ROOT/zero-output-events.tsv" \
    "$TEMP_ROOT/zero-output-ready.tsv" "$ZERO_OUTPUT_METADATA"
chmod 0644 "$ZERO_OUTPUT_METADATA"
sed -i '' 's/^emitted_bytes\t1000$/emitted_bytes\t0/' "$ZERO_OUTPUT_METADATA"
python3 - "$ZERO_OUTPUT_METADATA" "$TEMP_ROOT/zero-output-events.tsv" \
    "$RENDER_SECRET" <<'PY'
import hashlib, hmac, pathlib, struct, sys
metadata = pathlib.Path(sys.argv[1])
lines = metadata.read_bytes().splitlines(keepends=True)
unsigned = b"".join(lines[:-1])
events = pathlib.Path(sys.argv[2]).read_bytes()
authenticated = (b"spaceterm.performance.workload-auth/v1\0"
    + struct.pack(">Q", len(unsigned)) + unsigned
    + struct.pack(">Q", len(events)) + events)
metadata.write_bytes(unsigned + b"events_hmac_sha256\t" + hmac.new(
    pathlib.Path(sys.argv[3]).read_bytes(), authenticated, hashlib.sha256
).hexdigest().encode() + b"\n")
PY
chmod 0444 "$ZERO_OUTPUT_METADATA"
expect_result 2 NOT-RUN sustained-output-workload-v3-authentication-invalid \
    "sustained output rejects authenticated zero-output metadata" \
    run_case "$sustained" "$sustained_subject" "$TRACE_INDEX" \
        "$(artifact_path "$sustained" "$sustained_subject" trace-metadata.tsv)" \
        "$(artifact_path "$sustained" "$sustained_subject" trace.zip)" \
        "$(artifact_path "$sustained" "$sustained_subject" manual-review.tsv)" \
        "$(artifact_path "$sustained" "$sustained_subject" hangs.xml)" \
        "$(artifact_path "$sustained" "$sustained_subject" trace-verification.tsv)" \
        "$sustained_render_workload" "$sustained_video" "$sustained_stack" \
        "$ZERO_OUTPUT_METADATA" "$TEMP_ROOT/zero-output-events.tsv" \
        "$TEMP_ROOT/zero-output-ready.tsv"

expect_result 2 NOT-RUN unexpected-workload-v3-evidence-for-render-scenario \
    "idle render rejects common output workload evidence" \
    run_case perf-render-idle-cursor-blink ghostty "$TRACE_INDEX" \
        "$(artifact_path perf-render-idle-cursor-blink ghostty trace-metadata.tsv)" \
        "$(artifact_path perf-render-idle-cursor-blink ghostty trace.zip)" \
        "$(artifact_path perf-render-idle-cursor-blink ghostty manual-review.tsv)" \
        "$(artifact_path perf-render-idle-cursor-blink ghostty hangs.xml)" \
        "$(artifact_path perf-render-idle-cursor-blink ghostty trace-verification.tsv)" \
        "$(artifact_path perf-render-idle-cursor-blink ghostty workload-metadata.tsv)" \
        "$(artifact_path perf-render-idle-cursor-blink ghostty actions.mov)" \
        "$(artifact_path perf-render-idle-cursor-blink ghostty stacks.png)" \
        "$sustained_common_metadata" "$sustained_common_events" "$sustained_common_ready"

scenario=perf-render-idle-cursor-blink
subject=ghostty
expect_result 2 NOT-RUN missing-or-empty-trace-capture \
    "missing trace artifact" run_case "$scenario" "$subject" "$TRACE_INDEX" \
        "$(artifact_path "$scenario" "$subject" trace-metadata.tsv)" \
        "$TEMP_ROOT/missing.zip"

SHORT_TRACE_METADATA="$TEMP_ROOT/short-trace-metadata.tsv"
sed 's/actual_duration_ms\t120000/actual_duration_ms\t119999/' \
    "$(artifact_path "$scenario" "$subject" trace-metadata.tsv)" \
    > "$SHORT_TRACE_METADATA"
SHORT_TRACE_INDEX="$TEMP_ROOT/short-trace-index.tsv"
awk -F '\t' -v OFS='\t' -v replacement="$(sha256 "$SHORT_TRACE_METADATA")" \
    -v scenario="$scenario" -v subject="$subject" \
    '$1 == scenario && $2 == subject { $13 = replacement } { print }' \
    "$TRACE_INDEX" > "$SHORT_TRACE_INDEX"
chmod 0444 "$SHORT_TRACE_INDEX"
SHORT_MANUAL="$TEMP_ROOT/short-manual.tsv"
write_manual_review "$scenario" "$subject" "$SHORT_TRACE_INDEX" \
    "$SHORT_TRACE_METADATA" "$SHORT_MANUAL"
expect_result 2 NOT-RUN render-trace-duration-incomplete \
    "short trace" run_case "$scenario" "$subject" "$SHORT_TRACE_INDEX" \
        "$SHORT_TRACE_METADATA" \
        "$(artifact_path "$scenario" "$subject" trace.zip)" "$SHORT_MANUAL"

SKEWED_TRACE_METADATA="$TEMP_ROOT/skewed-trace-metadata.tsv"
awk -F '\t' -v OFS='\t' \
    '$1 == "capture_started_continuous_ns" { $2 -= 3000000000 } { print }' \
    "$(artifact_path "$scenario" "$subject" trace-metadata.tsv)" \
    > "$SKEWED_TRACE_METADATA"
SKEWED_TRACE_INDEX="$TEMP_ROOT/skewed-trace-index.tsv"
awk -F '\t' -v OFS='\t' -v replacement="$(sha256 "$SKEWED_TRACE_METADATA")" \
    -v scenario="$scenario" -v subject="$subject" \
    '$1 == scenario && $2 == subject { $13 = replacement } { print }' \
    "$TRACE_INDEX" > "$SKEWED_TRACE_INDEX"
chmod 0444 "$SKEWED_TRACE_INDEX"
SKEWED_MANUAL="$TEMP_ROOT/skewed-manual.tsv"
write_manual_review "$scenario" "$subject" "$SKEWED_TRACE_INDEX" \
    "$SKEWED_TRACE_METADATA" "$SKEWED_MANUAL"
expect_result 2 NOT-RUN render-trace-does-not-bracket-workload \
    "trace/workload continuous-time skew" run_case "$scenario" "$subject" \
        "$SKEWED_TRACE_INDEX" "$SKEWED_TRACE_METADATA" \
        "$(artifact_path "$scenario" "$subject" trace.zip)" "$SKEWED_MANUAL"

OLD_V3_TRACE_METADATA="$TEMP_ROOT/old-v3-trace-metadata.tsv"
awk -F '\t' '$1 != "run_metadata_sha256" \
    && $1 != "workload_metadata_sha256" \
    && $1 != "capture_started_continuous_ns" \
    && $1 != "capture_ended_continuous_ns"' \
    "$(artifact_path "$scenario" "$subject" trace-metadata.tsv)" \
    > "$OLD_V3_TRACE_METADATA"
OLD_V3_TRACE_INDEX="$TEMP_ROOT/old-v3-trace-index.tsv"
awk -F '\t' -v OFS='\t' -v replacement="$(sha256 "$OLD_V3_TRACE_METADATA")" \
    -v scenario="$scenario" -v subject="$subject" \
    '$1 == scenario && $2 == subject { $13 = replacement } { print }' \
    "$TRACE_INDEX" > "$OLD_V3_TRACE_INDEX"
chmod 0444 "$OLD_V3_TRACE_INDEX"
OLD_V3_MANUAL="$TEMP_ROOT/old-v3-manual.tsv"
write_manual_review "$scenario" "$subject" "$OLD_V3_TRACE_INDEX" \
    "$OLD_V3_TRACE_METADATA" "$OLD_V3_MANUAL"
expect_result 2 NOT-RUN invalid-trace-metadata-schema \
    "old v3 trace metadata lacks run/workload and continuous-time binding" \
    run_case "$scenario" "$subject" "$OLD_V3_TRACE_INDEX" \
        "$OLD_V3_TRACE_METADATA" \
        "$(artifact_path "$scenario" "$subject" trace.zip)" "$OLD_V3_MANUAL"

expect_result 2 NOT-RUN render-plan-scenario-mismatch \
    "wrong scenario" \
    "$SCRIPT_DIRECTORY/analyze-release-render-profile-case.sh" \
        --subject "$subject" \
        --scenario perf-render-text-blink \
        --plan "$TEMP_ROOT/$scenario-plan.tsv" \
        --plan-metadata "$TEMP_ROOT/$scenario-plan-metadata.tsv" \
        --pair-metadata "$TEMP_ROOT/$scenario-pair.tsv" \
        --subject-identity "$GHOSTTY_IDENTITY" \
        --comparison-subject-identity "$SPACETERM_IDENTITY" \
        --run-metadata "$(artifact_path "$scenario" "$subject" run-metadata.tsv)" \
        --render-intent "$(artifact_path "$scenario" "$subject" render-intent.tsv)" \
        --render-evidence "$(artifact_path "$scenario" "$subject" render-evidence.tsv)" \
        --render-workload-metadata \
            "$(artifact_path "$scenario" "$subject" workload-metadata.tsv)" \
        --campaign-secret-file "$RENDER_SECRET" \
        --driver-events "$(artifact_path "$scenario" "$subject" driver-events.tsv)" \
        --trace-index "$TRACE_INDEX" \
        --trace-metadata "$(artifact_path "$scenario" "$subject" trace-metadata.tsv)" \
        --trace-artifact "$(artifact_path "$scenario" "$subject" trace.zip)" \
        --trace-toc "$(artifact_path "$scenario" "$subject" trace-toc.xml)" \
        --trace-verification \
            "$(artifact_path "$scenario" "$subject" trace-verification.tsv)" \
        --time-profiler-artifact "$(artifact_path "$scenario" "$subject" time-profiler.xml)" \
        --allocations-artifact "$(artifact_path "$scenario" "$subject" allocations.xml)" \
        --hangs-artifact "$(artifact_path "$scenario" "$subject" hangs.xml)" \
        --manual-review "$(artifact_path "$scenario" "$subject" manual-review.tsv)" \
        --stack-screenshot "$(artifact_path "$scenario" "$subject" stacks.png)" \
        --action-video "$(artifact_path "$scenario" "$subject" actions.mov)" \
        --ffprobe "$FAKE_FFPROBE"

WRONG_COMPARISON="$TEMP_ROOT/wrong-spaceterm-identity.tsv"
sed 's/process_pid\t123/process_pid\t999/' "$SPACETERM_IDENTITY" > "$WRONG_COMPARISON"
expect_result 2 NOT-RUN paired-spaceterm-identity-mismatch \
    "mismatched comparison pair" \
    "$SCRIPT_DIRECTORY/analyze-release-render-profile-case.sh" \
        --subject ghostty \
        --scenario "$scenario" \
        --plan "$TEMP_ROOT/$scenario-plan.tsv" \
        --plan-metadata "$TEMP_ROOT/$scenario-plan-metadata.tsv" \
        --pair-metadata "$TEMP_ROOT/$scenario-pair.tsv" \
        --subject-identity "$GHOSTTY_IDENTITY" \
        --comparison-subject-identity "$WRONG_COMPARISON" \
        --run-metadata "$(artifact_path "$scenario" ghostty run-metadata.tsv)" \
        --render-intent "$(artifact_path "$scenario" ghostty render-intent.tsv)" \
        --render-evidence "$(artifact_path "$scenario" ghostty render-evidence.tsv)" \
        --render-workload-metadata \
            "$(artifact_path "$scenario" ghostty workload-metadata.tsv)" \
        --campaign-secret-file "$RENDER_SECRET" \
        --driver-events "$(artifact_path "$scenario" ghostty driver-events.tsv)" \
        --trace-index "$TRACE_INDEX" \
        --trace-metadata "$(artifact_path "$scenario" ghostty trace-metadata.tsv)" \
        --trace-artifact "$(artifact_path "$scenario" ghostty trace.zip)" \
        --trace-toc "$(artifact_path "$scenario" ghostty trace-toc.xml)" \
        --trace-verification \
            "$(artifact_path "$scenario" ghostty trace-verification.tsv)" \
        --time-profiler-artifact "$(artifact_path "$scenario" ghostty time-profiler.xml)" \
        --allocations-artifact "$(artifact_path "$scenario" ghostty allocations.xml)" \
        --hangs-artifact "$(artifact_path "$scenario" ghostty hangs.xml)" \
        --manual-review "$(artifact_path "$scenario" ghostty manual-review.tsv)" \
        --stack-screenshot "$(artifact_path "$scenario" ghostty stacks.png)" \
        --action-video "$(artifact_path "$scenario" ghostty actions.mov)" \
        --ffprobe "$FAKE_FFPROBE"

REUSED_TRACE_INDEX="$TEMP_ROOT/reused-trace-index.tsv"
reused_hash="$(sha256 "$(artifact_path "$scenario" ghostty trace.zip)")"
awk -F '\t' -v OFS='\t' -v replacement="$reused_hash" \
    '$1 == "perf-render-text-blink" && $2 == "ghostty" { $14 = replacement } \
    { print }' "$TRACE_INDEX" > "$REUSED_TRACE_INDEX"
chmod 0444 "$REUSED_TRACE_INDEX"
expect_result 2 NOT-RUN campaign-trace-index-incomplete-mismatched-or-reused \
    "reused trace capture" run_case "$scenario" ghostty "$REUSED_TRACE_INDEX"

UNCHECKED_ALLOCATIONS="$TEMP_ROOT/unchecked-allocations.tsv"
sed 's/allocations_call_tree_checked\tPASS/allocations_call_tree_checked\tunchecked/' \
    "$(artifact_path "$scenario" ghostty manual-review.tsv)" > "$UNCHECKED_ALLOCATIONS"
expect_result 2 NOT-RUN manual-allocations_call_tree_checked-missing \
    "unchecked allocation call tree" run_case "$scenario" ghostty "$TRACE_INDEX" \
        "$(artifact_path "$scenario" ghostty trace-metadata.tsv)" \
        "$(artifact_path "$scenario" ghostty trace.zip)" "$UNCHECKED_ALLOCATIONS"

EMPTY_RENDER_ROOT="$TEMP_ROOT/empty-render-root.tsv"
sed 's/render_root_sample_count\t10/render_root_sample_count\t0/' \
    "$(artifact_path "$scenario" ghostty manual-review.tsv)" > "$EMPTY_RENDER_ROOT"
expect_result 2 NOT-RUN render-root-has-no-positive-sample-evidence \
    "empty render-root filter" run_case "$scenario" ghostty "$TRACE_INDEX" \
        "$(artifact_path "$scenario" ghostty trace-metadata.tsv)" \
        "$(artifact_path "$scenario" ghostty trace.zip)" "$EMPTY_RENDER_ROOT"

MANUAL_FAILURE="$TEMP_ROOT/manual-failure.tsv"
sed 's/^result\tPASS$/result\tFAIL/' \
    "$(artifact_path "$scenario" ghostty manual-review.tsv)" > "$MANUAL_FAILURE"
expect_result 1 FAIL manual-render-review-failed \
    "manual FAIL verdict" run_case "$scenario" ghostty "$TRACE_INDEX" \
        "$(artifact_path "$scenario" ghostty trace-metadata.tsv)" \
        "$(artifact_path "$scenario" ghostty trace.zip)" "$MANUAL_FAILURE"

FORBIDDEN_STACK="$TEMP_ROOT/forbidden-stack.tsv"
sed 's/render_root_text_shaping_stack_present\tfalse/render_root_text_shaping_stack_present\ttrue/' \
    "$(artifact_path "$scenario" ghostty manual-review.tsv)" > "$FORBIDDEN_STACK"
expect_result 1 FAIL render_root_text_shaping_stack_present \
    "forbidden render-root shaping stack" run_case "$scenario" ghostty \
        "$TRACE_INDEX" "$(artifact_path "$scenario" ghostty trace-metadata.tsv)" \
        "$(artifact_path "$scenario" ghostty trace.zip)" "$FORBIDDEN_STACK"

BAD_PROPORTIONALITY="$TEMP_ROOT/bad-proportionality.tsv"
sed 's/changed_row_proportionality_result\tPASS/changed_row_proportionality_result\tFAIL/' \
    "$(artifact_path "$scenario" ghostty manual-review.tsv)" > "$BAD_PROPORTIONALITY"
expect_result 1 FAIL changed_row_proportionality_result \
    "failed changed-row proportionality" run_case "$scenario" ghostty \
        "$TRACE_INDEX" "$(artifact_path "$scenario" ghostty trace-metadata.tsv)" \
        "$(artifact_path "$scenario" ghostty trace.zip)" "$BAD_PROPORTIONALITY"

REUSED_EXPORT_INDEX="$TEMP_ROOT/reused-export-index.tsv"
IFS=$'\t' read -r reused_time reused_allocations reused_hangs <<< "$(awk -F '\t' \
    -v scenario="$scenario" '$1 == scenario && $2 == "ghostty" { \
        print $18 "\t" $19 "\t" $20 }' "$TRACE_INDEX")"
awk -F '\t' -v OFS='\t' -v time="$reused_time" -v allocations="$reused_allocations" \
    -v hangs="$reused_hangs" \
    '$1 == "perf-render-text-blink" && $2 == "ghostty" { \
        $18 = time; $19 = allocations; $20 = hangs } { print }' \
    "$TRACE_INDEX" > "$REUSED_EXPORT_INDEX"
chmod 0444 "$REUSED_EXPORT_INDEX"
expect_result 2 NOT-RUN campaign-trace-index-incomplete-mismatched-or-reused \
    "reused trace export tuple" run_case "$scenario" ghostty "$REUSED_EXPORT_INDEX"

REUSED_VIDEO_INDEX="$TEMP_ROOT/reused-video-index.tsv"
reused_video="$(awk -F '\t' -v scenario="$scenario" \
    '$1 == scenario && $2 == "ghostty" { print $22 }' "$TRACE_INDEX")"
awk -F '\t' -v OFS='\t' -v video="$reused_video" \
    '$1 == "perf-render-text-blink" && $2 == "ghostty" { $22 = video } { print }' \
    "$TRACE_INDEX" > "$REUSED_VIDEO_INDEX"
chmod 0444 "$REUSED_VIDEO_INDEX"
expect_result 2 NOT-RUN campaign-trace-index-incomplete-mismatched-or-reused \
    "reused action video" run_case "$scenario" ghostty "$REUSED_VIDEO_INDEX"

REUSED_SCREENSHOT_INDEX="$TEMP_ROOT/reused-screenshot-index.tsv"
reused_screenshot="$(awk -F '\t' -v scenario="$scenario" \
    '$1 == scenario && $2 == "ghostty" { print $21 }' "$TRACE_INDEX")"
awk -F '\t' -v OFS='\t' -v screenshot="$reused_screenshot" \
    '$1 == "perf-render-text-blink" && $2 == "ghostty" { $21 = screenshot } { print }' \
    "$TRACE_INDEX" > "$REUSED_SCREENSHOT_INDEX"
chmod 0444 "$REUSED_SCREENSHOT_INDEX"
expect_result 2 NOT-RUN campaign-trace-index-incomplete-mismatched-or-reused \
    "reused representative stack screenshot" \
    run_case "$scenario" ghostty "$REUSED_SCREENSHOT_INDEX"

MIXED_CAMPAIGN_INDEX="$TEMP_ROOT/mixed-campaign-index.tsv"
awk -F '\t' -v OFS='\t' '
    $1 == "perf-render-text-blink" && $2 == "ghostty" { $5 = "other-campaign" }
    { print }
' "$TRACE_INDEX" > "$MIXED_CAMPAIGN_INDEX"
chmod 0444 "$MIXED_CAMPAIGN_INDEX"
expect_result 2 NOT-RUN campaign-trace-index-incomplete-mismatched-or-reused \
    "mixed campaign index" run_case "$scenario" ghostty "$MIXED_CAMPAIGN_INDEX"

MIXED_SESSION_INDEX="$TEMP_ROOT/mixed-session-index.tsv"
awk -F '\t' -v OFS='\t' '
    $1 == "perf-render-text-blink" && $2 == "ghostty" { $6 = "other-session" }
    { print }
' "$TRACE_INDEX" > "$MIXED_SESSION_INDEX"
chmod 0444 "$MIXED_SESSION_INDEX"
expect_result 2 NOT-RUN campaign-trace-index-incomplete-mismatched-or-reused \
    "mixed session index" run_case "$scenario" ghostty "$MIXED_SESSION_INDEX"

TAMPERED_DRIVER="$TEMP_ROOT/tampered-driver.tsv"
awk -F '\t' -v OFS='\t' 'NR == 4 { $11 = "failed" } { print }' \
    "$(artifact_path "$scenario" ghostty driver-events.tsv)" > "$TAMPERED_DRIVER"
chmod 0444 "$TAMPERED_DRIVER"
expect_command_failure "render freezer accepts failed native driver action" \
    "$SCRIPT_DIRECTORY/freeze-render-profile-workload.sh" \
        --subject ghostty --scenario "$scenario" \
        --plan "$TEMP_ROOT/$scenario-plan.tsv" \
        --plan-metadata "$TEMP_ROOT/$scenario-plan-metadata.tsv" \
        --pair-metadata "$TEMP_ROOT/$scenario-pair.tsv" \
        --subject-identity "$GHOSTTY_IDENTITY" \
        --driver-events "$TAMPERED_DRIVER" \
        --action-video "$(artifact_path "$scenario" ghostty actions.mov)" \
        --output "$TEMP_ROOT/tampered-workload.tsv"

BAD_CHECKPOINT_OBSERVATION="$TEMP_ROOT/bad-checkpoint-observation.tsv"
awk -F '\t' -v OFS='\t' '
    $4 == "checkpoint" && !changed { $9 = 0; changed = 1 }
    { print }
' "$(artifact_path "$scenario" ghostty driver-events.tsv)" \
    > "$BAD_CHECKPOINT_OBSERVATION"
chmod 0444 "$BAD_CHECKPOINT_OBSERVATION"
expect_command_failure "render freezer accepts verified offscreen checkpoint" \
    "$SCRIPT_DIRECTORY/freeze-render-profile-workload.sh" \
        --subject ghostty --scenario "$scenario" \
        --plan "$TEMP_ROOT/$scenario-plan.tsv" \
        --plan-metadata "$TEMP_ROOT/$scenario-plan-metadata.tsv" \
        --pair-metadata "$TEMP_ROOT/$scenario-pair.tsv" \
        --subject-identity "$GHOSTTY_IDENTITY" \
        --driver-events "$BAD_CHECKPOINT_OBSERVATION" \
        --action-video "$(artifact_path "$scenario" ghostty actions.mov)" \
        --output "$TEMP_ROOT/bad-checkpoint-workload.tsv"

INVALID_SCREENSHOT="$TEMP_ROOT/invalid-stacks.png"
printf 'invalid-screenshot\n' > "$INVALID_SCREENSHOT"
INVALID_SCREENSHOT_INDEX="$TEMP_ROOT/invalid-screenshot-index.tsv"
awk -F '\t' -v OFS='\t' -v scenario="$scenario" \
    -v screenshot="$(sha256 "$INVALID_SCREENSHOT")" \
    '$1 == scenario && $2 == "ghostty" { $21 = screenshot } { print }' \
    "$TRACE_INDEX" > "$INVALID_SCREENSHOT_INDEX"
chmod 0444 "$INVALID_SCREENSHOT_INDEX"
INVALID_SCREENSHOT_MANUAL="$TEMP_ROOT/invalid-screenshot-manual.tsv"
awk -F '\t' -v OFS='\t' \
    -v index_hash="$(sha256 "$INVALID_SCREENSHOT_INDEX")" \
    -v screenshot="$(sha256 "$INVALID_SCREENSHOT")" '
    $1 == "trace_index_sha256" { $2 = index_hash }
    $1 == "representative_stack_screenshot_sha256" { $2 = screenshot }
    { print }
' "$(artifact_path "$scenario" ghostty manual-review.tsv)" > "$INVALID_SCREENSHOT_MANUAL"
expect_result 2 NOT-RUN representative-stack-screenshot-is-not-valid-png \
    "invalid stack screenshot container" run_case "$scenario" ghostty \
        "$INVALID_SCREENSHOT_INDEX" \
        "$(artifact_path "$scenario" ghostty trace-metadata.tsv)" \
        "$(artifact_path "$scenario" ghostty trace.zip)" \
        "$INVALID_SCREENSHOT_MANUAL" "$(artifact_path "$scenario" ghostty hangs.xml)" \
        "$(artifact_path "$scenario" ghostty trace-verification.tsv)" \
        "$(artifact_path "$scenario" ghostty workload-metadata.tsv)" \
        "$(artifact_path "$scenario" ghostty actions.mov)" "$INVALID_SCREENSHOT"

MISMATCHED_VERIFICATION="$TEMP_ROOT/mismatched-trace-verification.tsv"
sed 's/^reason\tnone$/reason\twrong-receipt/' \
    "$(artifact_path "$scenario" ghostty trace-verification.tsv)" \
    > "$MISMATCHED_VERIFICATION"
MISMATCHED_VERIFICATION_INDEX="$TEMP_ROOT/mismatched-verification-index.tsv"
awk -F '\t' -v OFS='\t' -v scenario="$scenario" \
    -v verification="$(sha256 "$MISMATCHED_VERIFICATION")" \
    '$1 == scenario && $2 == "ghostty" { $16 = verification } { print }' \
    "$TRACE_INDEX" > "$MISMATCHED_VERIFICATION_INDEX"
chmod 0444 "$MISMATCHED_VERIFICATION_INDEX"
MISMATCHED_VERIFICATION_MANUAL="$TEMP_ROOT/mismatched-verification-manual.tsv"
awk -F '\t' -v OFS='\t' \
    -v index_hash="$(sha256 "$MISMATCHED_VERIFICATION_INDEX")" \
    -v verification="$(sha256 "$MISMATCHED_VERIFICATION")" '
    $1 == "trace_index_sha256" { $2 = index_hash }
    $1 == "trace_verification_sha256" { $2 = verification }
    { print }
' "$(artifact_path "$scenario" ghostty manual-review.tsv)" \
    > "$MISMATCHED_VERIFICATION_MANUAL"
expect_result 2 NOT-RUN trace-verification-receipt-does-not-match-regenerated-exports \
    "trace verification receipt mismatch" run_case "$scenario" ghostty \
        "$MISMATCHED_VERIFICATION_INDEX" \
        "$(artifact_path "$scenario" ghostty trace-metadata.tsv)" \
        "$(artifact_path "$scenario" ghostty trace.zip)" \
        "$MISMATCHED_VERIFICATION_MANUAL" \
        "$(artifact_path "$scenario" ghostty hangs.xml)" "$MISMATCHED_VERIFICATION"

echo "release render-profile campaign fixtures passed"
