#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
umask 077

SCRIPT_DIRECTORY="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
TOOL="$SCRIPT_DIRECTORY/performance-pair-result.py"
PAIR_ANALYZER="$SCRIPT_DIRECTORY/analyze-release-performance-pair.sh"
DRIVER_TOOL="$SCRIPT_DIRECTORY/performance-driver-receipt.py"
DRIVER_SOURCE="$SCRIPT_DIRECTORY/performance-driver.m"
DRIVER_CONTROLLER="$SCRIPT_DIRECTORY/run-native-performance-scenario.sh"
LIFECYCLE_HELPER="$SCRIPT_DIRECTORY/performance-subject-lifecycle.py"
TERMINATOR_SOURCE="$SCRIPT_DIRECTORY/performance-appkit-terminate.m"
TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-pair-result.XXXXXX")"
trap 'rm -rf -- "$TEMP_ROOT"' EXIT INT TERM

fail() { echo "test failure: $*" >&2; exit 1; }
sha256() { shasum -a 256 "$1" | awk '{ print $1 }'; }
expect_failure() {
    local label="$1"
    shift
    if "$@" >"$TEMP_ROOT/failure.stdout" 2>"$TEMP_ROOT/failure.stderr"; then
        fail "$label unexpectedly succeeded"
    fi
}

SECRET="$TEMP_ROOT/secret"
PLAN="$TEMP_ROOT/plan.tsv"
DRIVER_BINARY="$TEMP_ROOT/performance-driver"
TERMINATOR_BINARY="$TEMP_ROOT/performance-appkit-terminate"
PAIR="$TEMP_ROOT/pair.tsv"
RESULT="$TEMP_ROOT/pair-result.tsv"
printf '%064d' 0 > "$SECRET"
cat > "$PLAN" <<'EOF'
event_id	offset_ms	action	arg0	arg1
start	0	checkpoint	0	0
type	1000	input	a	1
finish	2000	stop	0	0
EOF
printf '#!/bin/sh\nexit 0\n' > "$DRIVER_BINARY"
printf '#!/bin/sh\nexit 0\n' > "$TERMINATOR_BINARY"
for name in workload command environment font initial-grid; do
    printf '%s\n' "$name" > "$TEMP_ROOT/$name"
done
chmod 0600 "$SECRET"
chmod 0444 "$PLAN" "$TEMP_ROOT"/{workload,command,environment,font,initial-grid}
chmod 0555 "$DRIVER_BINARY" "$TERMINATOR_BINARY"

write_subject() {
    local subject="$1" pid="$2" output="$3"
    cat > "$output" <<EOF
format_version	1
subject	$subject
app_bundle_path	/Applications/$subject.app
bundle_identifier	dev.spaceterm.$subject
bundle_version	1.0+1
executable_path	/Applications/$subject.app/Contents/MacOS/$subject
executable_sha256	bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
executable_device	1
executable_inode	$pid
executable_fsid	1
signature_valid	true
signing_identifier	dev.spaceterm.$subject
team_identifier	none
cdhash	abc123
process_pid	$pid
process_start_identity	100:$pid
identity_status	frozen
EOF
    chmod 0444 "$output"
}

write_window() {
    local subject="$1" pid="$2" identity="$3" number="$4" output="$5"
    cat > "$output" <<EOF
format_version	1
subject_identity_sha256	$(sha256 "$identity")
subject	$subject
process_pid	$pid
process_start_identity	100:$pid
bundle_identifier	dev.spaceterm.$subject
executable_sha256	bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
window_number	$number
window_owner_pid_verified	true
window_layer	0
window_onscreen	true
window_minimized	false
window_x	0.000
window_y	0.000
window_width	800.000
window_height	600.000
resolved_continuous_ns	900000000
selector_kind	unique
status	frozen
EOF
    chmod 0444 "$output"
}

SPACETERM_SUBJECT="$TEMP_ROOT/spaceterm-subject.tsv"
GHOSTTY_SUBJECT="$TEMP_ROOT/ghostty-subject.tsv"
SPACETERM_WINDOW="$TEMP_ROOT/spaceterm-window.tsv"
GHOSTTY_WINDOW="$TEMP_ROOT/ghostty-window.tsv"
write_subject spaceterm 4242 "$SPACETERM_SUBJECT"
write_subject ghostty 4343 "$GHOSTTY_SUBJECT"
write_window spaceterm 4242 "$SPACETERM_SUBJECT" 77 "$SPACETERM_WINDOW"
write_window ghostty 4343 "$GHOSTTY_SUBJECT" 88 "$GHOSTTY_WINDOW"

cat > "$PAIR" <<EOF
format_version	1
pair_id	pair-a
scenario	ascii
plan_sha256	$(sha256 "$PLAN")
workload_sha256	$(sha256 "$TEMP_ROOT/workload")
command_sha256	$(sha256 "$TEMP_ROOT/command")
environment_sha256	$(sha256 "$TEMP_ROOT/environment")
font_sha256	$(sha256 "$TEMP_ROOT/font")
initial_grid_sha256	$(sha256 "$TEMP_ROOT/initial-grid")
duration_ms	2000
spaceterm_subject_identity_sha256	$(sha256 "$SPACETERM_SUBJECT")
ghostty_subject_identity_sha256	$(sha256 "$GHOSTTY_SUBJECT")
EOF
chmod 0444 "$PAIR"

SPACETERM_NATIVE_PROVISIONAL="$TEMP_ROOT/spaceterm-native-provisional.tsv"
SPACETERM_NATIVE_OBSERVATION="$TEMP_ROOT/spaceterm-native-observation.tsv"
SPACETERM_NATIVE_METADATA="$TEMP_ROOT/spaceterm-runtime-metadata.tsv"
SPACETERM_NATIVE_SAMPLES="$TEMP_ROOT/spaceterm-runtime-samples.tsv"
SPACETERM_NATIVE_EVENTS="$TEMP_ROOT/spaceterm-runtime-events.tsv"
SPACETERM_NATIVE_FAILURES="$TEMP_ROOT/spaceterm-failure-actions.tsv"
cat > "$SPACETERM_NATIVE_PROVISIONAL" <<'EOF'
schema	spaceterm.acceptance.native-launch-proof/v5
observation.source	production-app
launch.nonce	cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
run.id	run-a
package.app.sha256	dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd
runtime.schema	spaceterm.acceptance.runtime-stream/v1
runtime.sample_interval_ms	1000
runtime.transition_capacity	64
failure.action.schema	spaceterm.acceptance.failure-action/v1
failure.action.enabled	false
process.pid	4242
process.pidversion	7
process.executable.path	/Applications/spaceterm.app/Contents/MacOS/spaceterm
process.executable.device	1
process.executable.inode	4242
process.executable.fsid	1:1
process.signature.cdhash	abc123
process.signature.identifier	dev.spaceterm.spaceterm
process.signature.team_identifier	__EMPTY__
terminal_font_selected	JetBrains Mono
initial_grid.rows	24
initial_grid.columns	80
initial_grid.logical_width	800
initial_grid.logical_height	480
initial_grid.backing_pixel_width	1600
initial_grid.backing_pixel_height	960
observation.complete	true
EOF
sed -i '' 's/process.signature.team_identifier\t__EMPTY__/process.signature.team_identifier\t/' \
    "$SPACETERM_NATIVE_PROVISIONAL"
cat > "$SPACETERM_NATIVE_SAMPLES" <<'EOF'
sequence	continuous_ns	worker_generation	screens_published	screens_enqueued	screens_superseded	event_queue_length	event_queue_high_water	ui_dispatches	ui_screen_events	ui_drain_high_water	ui_latest_generation	render_latest_generation	next_frame_generation	next_frame_count	presentable	minimized	occluded	workspace_visible	pane_visible	live_resize	viewport_total_rows	viewport_visible_rows	viewport_offset_rows	selection_present	resize_requests	resize_notifications	resize_applied	resize_coalesced	pty_rows	pty_columns	pty_pixel_width	pty_pixel_height	terminal_inputs_accepted	lifecycle	observer_drops
0	1000	1	1	1	0	0	0	1	1	1	1	1	1	1	1	0	0	1	1	0	24	24	0	0	0	0	0	0	24	80	800	480	1	2	0
EOF
printf 'sequence\tcontinuous_ns\tkind\tgeneration\taux0\taux1\n' \
    > "$SPACETERM_NATIVE_EVENTS"
printf 'request_id\tsequence\tcase_id\taction\tresult\tpane_id\tpane_state\tfailure_class\tfailure_recoverability\tfailure_operation\tstate_revision\tlatest_generation\tlast_valid_generation\tvisible_generation\tpending_recovery\tterminal_input_usable\tsession_attached\tresource_staged_count\tresource_staged_bytes\tresource_rolled_back_count\tresource_rolled_back_bytes\n' \
    > "$SPACETERM_NATIVE_FAILURES"
cat > "$SPACETERM_NATIVE_METADATA" <<EOF
schema	spaceterm.acceptance.runtime-observation-metadata/v3
observation.source	production-app
run.id	run-a
package.app.sha256	dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd
process.pid	4242
runtime.samples.path	runtime-samples.tsv
runtime.samples.sha256	$(sha256 "$SPACETERM_NATIVE_SAMPLES")
runtime.events.path	runtime-events.tsv
runtime.events.sha256	$(sha256 "$SPACETERM_NATIVE_EVENTS")
failure.action.schema	spaceterm.acceptance.failure-action/v1
failure.action.enabled	false
failure.result.schema	spaceterm.acceptance.failure-action-result/v2
failure.actions.path	failure-actions.tsv
failure.actions.sha256	$(sha256 "$SPACETERM_NATIVE_FAILURES")
failure.request_count	0
failure.result_count	0
observer.started_continuous_ns	1000
observer.ended_continuous_ns	1000
observer.sample_interval_ms	1000
observer.transition_capacity	64
observer.sample_count	1
observer.event_count	0
observer.status	complete
observation.complete	true
EOF
sed '$d' "$SPACETERM_NATIVE_PROVISIONAL" > "$SPACETERM_NATIVE_OBSERVATION"
cat >> "$SPACETERM_NATIVE_OBSERVATION" <<EOF
provisional.observation.sha256	$(sha256 "$SPACETERM_NATIVE_PROVISIONAL")
runtime.metadata.schema	spaceterm.acceptance.runtime-observation-metadata/v3
runtime.metadata.path	runtime-metadata.tsv
runtime.metadata.sha256	$(sha256 "$SPACETERM_NATIVE_METADATA")
failure.result.schema	spaceterm.acceptance.failure-action-result/v2
failure.actions.path	failure-actions.tsv
failure.actions.sha256	$(sha256 "$SPACETERM_NATIVE_FAILURES")
failure.request_count	0
failure.result_count	0
observation.complete	true
EOF
chmod 0400 "$SPACETERM_NATIVE_PROVISIONAL" "$SPACETERM_NATIVE_OBSERVATION" \
    "$SPACETERM_NATIVE_METADATA" "$SPACETERM_NATIVE_SAMPLES" \
    "$SPACETERM_NATIVE_EVENTS" "$SPACETERM_NATIVE_FAILURES"

write_intent() {
    local subject="$1" identity="$2" session="$3" nonce="$4" output="$5"
    local provisional=not-applicable
    [[ "$subject" != spaceterm ]] || provisional="$(sha256 "$SPACETERM_NATIVE_PROVISIONAL")"
    cat > "$output" <<EOF
format_version	1
subject	$subject
subject_identity_sha256	$(sha256 "$identity")
scenario	ascii
scenario_plan_sha256	$(sha256 "$PLAN")
workload_sha256	$(sha256 "$TEMP_ROOT/workload")
command_sha256	$(sha256 "$TEMP_ROOT/command")
environment_sha256	$(sha256 "$TEMP_ROOT/environment")
font_sha256	$(sha256 "$TEMP_ROOT/font")
initial_grid_sha256	$(sha256 "$TEMP_ROOT/initial-grid")
measured_duration_ms	2000
process_pid	$(awk -F '\t' '$1 == "process_pid" {print $2}' "$identity")
process_start_identity	$(awk -F '\t' '$1 == "process_start_identity" {print $2}' "$identity")
campaign_id	campaign-a
session_id	$session
nonce	$nonce
native_provisional_observation_sha256	$provisional
evidence_mode	production
status	prepared
EOF
    chmod 0444 "$output"
}

SPACETERM_NONCE=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
GHOSTTY_NONCE=dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd
SPACETERM_INTENT="$TEMP_ROOT/spaceterm-run-intent.tsv"
GHOSTTY_INTENT="$TEMP_ROOT/ghostty-run-intent.tsv"
write_intent spaceterm "$SPACETERM_SUBJECT" spaceterm-session "$SPACETERM_NONCE" \
    "$SPACETERM_INTENT"
write_intent ghostty "$GHOSTTY_SUBJECT" ghostty-session "$GHOSTTY_NONCE" "$GHOSTTY_INTENT"

write_driver_bundle() {
    local subject="$1" pid="$2" window_number="$3" identity="$4" window="$5"
    local intent="$6" session="$7" nonce="$8" plan_start="$9"
    local events="$TEMP_ROOT/$subject-driver-events.tsv"
    local driver_intent="$TEMP_ROOT/$subject-driver-intent.tsv"
    local receipt="$TEMP_ROOT/$subject-driver-receipt.tsv"
    local gate="$TEMP_ROOT/$subject-plan-start-gate.tsv"
    python3 - "$gate" "$SECRET" "$session" "$nonce" "$plan_start" <<'PY'
import hashlib, hmac, os, pathlib, struct, sys
output, secret_path, session, nonce, start = sys.argv[1:]
rows = [
    ("format_version", "1"), ("campaign_id", "campaign-a"),
    ("session_id", session), ("nonce", nonce), ("ready_receipt_sha256", "e" * 64),
    ("plan_start_continuous_ns", start),
]
unsigned = b"".join(f"{key}\t{value}\n".encode() for key, value in rows)
signature = hmac.new(pathlib.Path(secret_path).read_bytes(),
    b"spaceterm.performance.plan-start-gate/v1\0" + struct.pack(">Q", len(unsigned))
    + unsigned, hashlib.sha256).hexdigest()
data = unsigned + f"start_gate_hmac_sha256\t{signature}\n".encode()
fd = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o400)
try: os.write(fd, data)
finally: os.close(fd)
PY
    local bindings=(
        --campaign-secret-file "$SECRET" --campaign-id campaign-a --session-id "$session"
        --nonce "$nonce" --driver-output "$events" --driver-binary "$DRIVER_BINARY"
        --driver-source "$DRIVER_SOURCE" --controller "$DRIVER_CONTROLLER"
        --scenario-plan "$PLAN" --plan-start-continuous-ns "$plan_start"
        --subject-identity "$identity" --window-identity "$window"
    )
    "$DRIVER_TOOL" intent "${bindings[@]}" --output "$driver_intent"
    cat > "$events" <<EOF
sequence	continuous_ns	event_id	action	target_pid	window_number	requested_a	requested_b	observed_a	observed_b	result
0	$plan_start	start	checkpoint	$pid	$window_number	0	0	1	1	verified
1	$((plan_start + 1000000000))	type	input	$pid	$window_number	1	2	0	1	verified
2	$((plan_start + 2000000000))	finish	stop	$pid	$window_number	0	0	1	1	verified
EOF
    chmod 0400 "$events"
    "$DRIVER_TOOL" finalize "${bindings[@]}" --intent "$driver_intent" \
        --receipt-output "$receipt"
}

write_driver_bundle spaceterm 4242 77 "$SPACETERM_SUBJECT" "$SPACETERM_WINDOW" \
    "$SPACETERM_INTENT" spaceterm-session "$SPACETERM_NONCE" 1000000000
write_driver_bundle ghostty 4343 88 "$GHOSTTY_SUBJECT" "$GHOSTTY_WINDOW" \
    "$GHOSTTY_INTENT" ghostty-session "$GHOSTTY_NONCE" 2000000000

write_run_closure() {
    local subject="$1" identity="$2" intent="$3" session="$4" nonce="$5"
    local tail="$TEMP_ROOT/$subject-tail.tsv" quit="$TEMP_ROOT/$subject-quit.tsv"
    local exit_receipt="$TEMP_ROOT/$subject-exit.tsv" run="$TEMP_ROOT/$subject-run.tsv"
    local trace="$TEMP_ROOT/$subject-trace-provisional.tsv"
    local workload_metadata="$TEMP_ROOT/$subject-workload-metadata.tsv"
    local workload_events="$TEMP_ROOT/$subject-workload-events.tsv"
    local workload_ready="$TEMP_ROOT/$subject-workload-ready.tsv"
    local lifecycle_ready="$TEMP_ROOT/$subject-lifecycle-ready.tsv"
    local lifecycle_registration="$TEMP_ROOT/$subject-lifecycle-registration.tsv"
    local final_trace="$TEMP_ROOT/$subject-trace-metadata.tsv"
    local trace_archive="$TEMP_ROOT/$subject-ascii.trace"
    local manual="$TEMP_ROOT/$subject-manual.tsv"
    local screenshot="$TEMP_ROOT/$subject-screenshot.png"
    local video="$TEMP_ROOT/$subject-video.mov"
    local case_report="$TEMP_ROOT/$subject-case-report.tsv"
    local pid token native_hash native_runtime native_failures native_value
    pid="$(awk -F '\t' '$1 == "process_pid" {print $2}' "$identity")"
    token="$(printf '%064d' "$([[ "$subject" == spaceterm ]] && echo 1 || echo 2)")"
    native_hash=not-applicable; native_runtime=not-applicable
    native_failures=not-applicable; native_value=not-applicable
    if [[ "$subject" == spaceterm ]]; then
        native_hash="$(sha256 "$SPACETERM_NATIVE_OBSERVATION")"
        native_runtime="$(sha256 "$SPACETERM_NATIVE_METADATA")"
        native_failures="$(sha256 "$SPACETERM_NATIVE_FAILURES")"; native_value=0
    fi
    printf 'fixture\tworkload-metadata\n' > "$workload_metadata"
    printf 'fixture\tworkload-events\n' > "$workload_events"
    printf 'fixture\tworkload-ready\n' > "$workload_ready"
    mkdir -m 0700 "$trace_archive"
    printf 'trace payload %s\n' "$subject" > "$trace_archive/payload"
    chmod 0400 "$trace_archive/payload"
    printf 'screenshot %s\n' "$subject" > "$screenshot"
    printf 'video %s\n' "$subject" > "$video"
    chmod 0400 "$screenshot" "$video"
    chmod 0400 "$workload_metadata" "$workload_events" "$workload_ready"
    local native_path=not-applicable
    [[ "$subject" != spaceterm ]] || native_path="$SPACETERM_NATIVE_OBSERVATION"
    python3 - "$SECRET" "$subject" "$identity" "$intent" \
        "$TEMP_ROOT/$subject-driver-receipt.tsv" "$TEMP_ROOT/$subject-driver-events.tsv" \
        "$TEMP_ROOT/$subject-plan-start-gate.tsv" "$trace" \
        "$tail" "$quit" "$exit_receipt" "$run" "$token" "$native_hash" \
        "$native_runtime" "$native_failures" "$native_value" \
        "$workload_metadata" "$workload_events" "$workload_ready" \
        "$lifecycle_ready" "$lifecycle_registration" "$native_path" \
        "$LIFECYCLE_HELPER" "$TERMINATOR_SOURCE" "$TERMINATOR_BINARY" \
        "$final_trace" "$trace_archive" "$manual" "$screenshot" "$video" "$case_report" <<'PY'
import hashlib, hmac, os, pathlib, struct, sys
(secret_path, subject, identity_path, intent_path, driver_receipt_path,
 driver_events_path, gate_path, trace_path, tail_path, quit_path, exit_path, run_path, token,
 native_hash, native_runtime, native_failures, native_value, workload_metadata,
 workload_events, workload_ready, lifecycle_ready, lifecycle_registration, native_path,
 helper_path, terminator_source, terminator_binary, final_trace_path, trace_archive,
 manual_path, screenshot_path, video_path, case_report_path) = sys.argv[1:]
read = lambda path: pathlib.Path(path).read_bytes()
sha = lambda path: hashlib.sha256(read(path)).hexdigest()
secret = read(secret_path)
def trace_tree(root_text):
    root=pathlib.Path(root_text); value=hashlib.sha256(b"spaceterm.performance.trace-tree/v1\0")
    entries=[]
    for path in root.rglob("*"):
        if path.is_file(): entries.append((path.relative_to(root).as_posix().encode(),path))
    for encoded,path in sorted(entries):
        data=path.read_bytes(); value.update(struct.pack(">Q",len(encoded))); value.update(encoded)
        value.update(struct.pack(">Q",len(data))); value.update(data)
    return value.hexdigest()
trace_archive_hash=trace_tree(trace_archive)
intent_values = dict(line.split("\t", 1) for line in pathlib.Path(intent_path).read_text().splitlines())
identity_values = dict(line.split("\t", 1) for line in pathlib.Path(identity_path).read_text().splitlines())
source_stat, binary_stat = os.stat(terminator_source), os.stat(terminator_binary)
helper_stat = os.stat(helper_path)
inspector_path = str(pathlib.Path(helper_path).parent.parent / "inspect-release-performance-process.py")
inspector_stat = os.stat(inspector_path)
tools = [
    ("lifecycle_helper_device", str(helper_stat.st_dev)),
    ("lifecycle_helper_inode", str(helper_stat.st_ino)),
    ("lifecycle_helper_sha256", sha(helper_path)),
    ("process_inspector_device", str(inspector_stat.st_dev)),
    ("process_inspector_inode", str(inspector_stat.st_ino)),
    ("process_inspector_sha256", sha(inspector_path)),
    ("appkit_terminator_process_pid", "99" if subject == "spaceterm" else "100"),
    ("appkit_terminator_process_start_identity", "10:20"),
    ("appkit_terminator_source_device", str(source_stat.st_dev)),
    ("appkit_terminator_source_inode", str(source_stat.st_ino)),
    ("appkit_terminator_source_sha256", sha(terminator_source)),
    ("appkit_terminator_binary_device", str(binary_stat.st_dev)),
    ("appkit_terminator_binary_inode", str(binary_stat.st_ino)),
    ("appkit_terminator_binary_sha256", sha(terminator_binary)),
]
trace_rows = [
    ("format_version", "1"), ("subject_identity_sha256", sha(identity_path)),
    ("run_intent_sha256", sha(intent_path)), ("workload_metadata_sha256", sha(workload_metadata)),
    ("workload_ready_receipt_sha256", sha(workload_ready)),
    ("supplemental_evidence_sha256", sha(gate_path)), ("capture_status", "CAPTURED"),
    ("requested_duration_ms", "2000"), ("actual_duration_ms", "2000"),
    ("capture_started_continuous_ns", "1000000000"),
    ("capture_ended_continuous_ns", "3000000000"),
    ("trace_bundle_tree_sha256", trace_archive_hash), ("toc_sha256", "9" * 64),
    ("time_profile_export_sha256", "a" * 64),
    ("allocations_export_sha256", "b" * 64), ("hangs_export_sha256", "c" * 64),
    ("trace_verification_sha256", "d" * 64), ("verifier_sha256", "f" * 64),
    ("evidence_mode", "production"), ("status", "complete"),
    ("auth_algorithm", "hmac-sha256"),
]
trace_unsigned = b"".join(f"{key}\t{value}\n".encode() for key, value in trace_rows)
trace_hmac = hmac.new(secret, b"spaceterm.performance.trace-provisional/v1\0"
    + struct.pack(">Q", len(trace_unsigned)) + trace_unsigned, hashlib.sha256).hexdigest()
pathlib.Path(trace_path).write_bytes(trace_unsigned
    + f"provisional_hmac_sha256\t{trace_hmac}\n".encode())
tail_rows = [
    ("format_version", "1"), ("campaign_id", "campaign-a"),
    ("session_id", intent_values["session_id"]), ("nonce", intent_values["nonce"]),
    ("quit_token", token), ("run_intent_sha256", sha(intent_path)),
    ("subject_identity_sha256", sha(identity_path)),
    ("subject_process_pid", intent_values["process_pid"]),
    ("subject_process_start_identity", intent_values["process_start_identity"]),
    ("driver_receipt_sha256", sha(driver_receipt_path)),
    ("driver_events_sha256", sha(driver_events_path)),
    ("workload_metadata_sha256", sha(workload_metadata)),
    ("workload_events_sha256", sha(workload_events)),
    ("rss_samples_sha256", "6" * 64),
    ("trace_provisional_receipt_sha256", sha(trace_path)),
    ("tail_completed_continuous_ns", "5000000000"), *tools,
    ("evidence_mode", "production"),
    ("terminal_status", "tail-complete"), ("auth_algorithm", "hmac-sha256"),
]
tail_unsigned = b"".join(f"{key}\t{value}\n".encode() for key, value in tail_rows)
tail_hmac = hmac.new(secret, b"spaceterm.performance.tail-complete/v1\0"
    + struct.pack(">Q", len(tail_unsigned)) + tail_unsigned, hashlib.sha256).hexdigest()
pathlib.Path(tail_path).write_bytes(tail_unsigned + f"tail_hmac_sha256\t{tail_hmac}\n".encode())
quit_rows = [
    ("format_version", "1"), ("campaign_id", "campaign-a"),
    ("session_id", intent_values["session_id"]), ("nonce", intent_values["nonce"]),
    ("run_intent_sha256", sha(intent_path)),
    ("subject_process_pid", intent_values["process_pid"]),
    ("subject_process_start_identity", intent_values["process_start_identity"]),
    ("quit_token", token), ("request_continuous_ns", "5000000100"),
    ("exit_continuous_ns", "5000000200"), ("termination_method", "appkit-terminate"),
    ("runtime_closure_status", "confirmed"), *tools,
    ("evidence_mode", "production"), ("status", "completed"),
]
pathlib.Path(quit_path).write_bytes(b"".join(f"{key}\t{value}\n".encode() for key,value in quit_rows))
exit_rows = [
    ("schema", "spaceterm.acceptance.performance-subject-exit/v1"),
    ("subject", subject), ("campaign_id", "campaign-a"),
    ("session_id", intent_values["session_id"]), ("nonce", intent_values["nonce"]),
    ("run_intent_sha256", sha(intent_path)), ("subject_identity_sha256", sha(identity_path)),
    ("process_pid", intent_values["process_pid"]),
    ("process_start_identity", intent_values["process_start_identity"]),
    ("tail_receipt_sha256", sha(tail_path)), ("quit_receipt_sha256", sha(quit_path)),
    ("exit_requested_continuous_ns", "5000000100"),
    ("process_exited_continuous_ns", "5000000200"), ("exit_status", "normal"),
    ("native_observation_sha256", native_hash), *tools,
    ("evidence_mode", "production"), ("auth_algorithm", "hmac-sha256"),
]
exit_status = ("status", "complete")
exit_unsigned = b"".join(f"{key}\t{value}\n".encode() for key,value in exit_rows+[exit_status])
exit_hmac = hmac.new(secret, b"spaceterm.acceptance.performance-subject-exit/v1\0"
    + struct.pack(">Q", len(exit_unsigned)) + exit_unsigned, hashlib.sha256).hexdigest()
pathlib.Path(exit_path).write_bytes(b"".join(f"{key}\t{value}\n".encode() for key,value in exit_rows)
    + f"receipt_hmac_sha256\t{exit_hmac}\nstatus\tcomplete\n".encode())
ready_rows = [
    ("schema", "spaceterm.acceptance.performance-lifecycle-ready/v1"),
    ("subject", subject), ("campaign_id", "campaign-a"),
    ("session_id", intent_values["session_id"]), ("nonce", intent_values["nonce"]),
    ("subject_identity_sha256", sha(identity_path)),
    ("process_pid", intent_values["process_pid"]),
    ("process_start_identity", intent_values["process_start_identity"]),
    ("executable_sha256", identity_values["executable_sha256"]),
    ("ready_continuous_ns", "900000000"),
    ("registration_control_device", "1"), ("registration_control_inode", "2"),
    *tools, ("evidence_mode", "production"), ("auth_algorithm", "hmac-sha256"),
]
ready_status = ("status", "ready")
ready_unsigned = b"".join(f"{key}\t{value}\n".encode() for key,value in ready_rows+[ready_status])
ready_hmac = hmac.new(secret, b"spaceterm.acceptance.performance-lifecycle-ready/v1\0"
    + struct.pack(">Q", len(ready_unsigned)) + ready_unsigned, hashlib.sha256).hexdigest()
pathlib.Path(lifecycle_ready).write_bytes(
    b"".join(f"{key}\t{value}\n".encode() for key,value in ready_rows)
    + f"receipt_hmac_sha256\t{ready_hmac}\nstatus\tready\n".encode())
registration_rows = [
    ("format_version", "1"), ("campaign_id", "campaign-a"),
    ("session_id", intent_values["session_id"]), ("nonce", intent_values["nonce"]),
    ("registration_token", token), ("subject_identity_sha256", sha(identity_path)),
    ("process_pid", intent_values["process_pid"]),
    ("process_start_identity", intent_values["process_start_identity"]),
    ("run_intent_path", str(pathlib.Path(intent_path).resolve())),
    ("run_intent_sha256", sha(intent_path)),
    ("tail_receipt_path", str(pathlib.Path(tail_path).resolve())),
    ("workload_metadata_path", str(pathlib.Path(workload_metadata).resolve())),
    ("workload_events_path", str(pathlib.Path(workload_events).resolve())),
    ("workload_ready_receipt_path", str(pathlib.Path(workload_ready).resolve())),
    ("quit_receipt_path", str(pathlib.Path(quit_path).resolve())),
    ("subject_exit_receipt_path", str(pathlib.Path(exit_path).resolve())),
    ("native_observation_path", native_path if native_path == "not-applicable" else str(pathlib.Path(native_path).resolve())),
    *tools, ("evidence_mode", "production"), ("auth_algorithm", "hmac-sha256"),
]
registration_status = ("status", "registered")
registration_unsigned = b"".join(f"{key}\t{value}\n".encode()
    for key,value in registration_rows+[registration_status])
registration_hmac = hmac.new(secret,
    b"spaceterm.acceptance.performance-lifecycle-registration/v1\0"
    + struct.pack(">Q", len(registration_unsigned)) + registration_unsigned,
    hashlib.sha256).hexdigest()
pathlib.Path(lifecycle_registration).write_bytes(
    b"".join(f"{key}\t{value}\n".encode() for key,value in registration_rows)
    + f"registration_hmac_sha256\t{registration_hmac}\nstatus\tregistered\n".encode())
common = [(key, intent_values[key]) for key in (
    "subject", "subject_identity_sha256", "scenario", "scenario_plan_sha256",
    "workload_sha256", "command_sha256", "environment_sha256", "font_sha256",
    "initial_grid_sha256", "measured_duration_ms", "process_pid", "process_start_identity")]
na = "not-applicable"
run_rows = [("format_version", "4"), *common, ("run_intent_sha256", sha(intent_path)),
    ("native_observation_sha256", native_hash), ("native_runtime_metadata_sha256", native_runtime),
    ("native_failure_actions_sha256", native_failures),
    ("native_failure_action_enabled", "false" if subject == "spaceterm" else na),
    ("native_failure_request_count", native_value), ("native_failure_result_count", native_value),
    ("native_failure_resource_staged_count", native_value),
    ("native_failure_resource_staged_bytes", native_value),
    ("native_failure_resource_rolled_back_count", native_value),
    ("native_failure_resource_rolled_back_bytes", native_value),
    ("trace_provisional_receipt_sha256", sha(trace_path)),
    ("performance_tail_receipt_sha256", sha(tail_path)),
    ("performance_quit_receipt_sha256", sha(quit_path)),
    ("subject_exit_receipt_sha256", sha(exit_path)),
    ("lifecycle_ready_receipt_sha256", sha(lifecycle_ready)),
    ("lifecycle_registration_receipt_sha256", sha(lifecycle_registration)),
    ("lifecycle_helper_sha256", sha(helper_path)),
    ("terminator_source_sha256", sha(terminator_source)),
    ("terminator_binary_sha256", sha(terminator_binary)),
    ("evidence_mode", "production"),
    ("status", "complete")]
pathlib.Path(run_path).write_bytes(b"".join(f"{key}\t{value}\n".encode() for key,value in run_rows))
final_trace_rows=[("format_version","3"),("capture_status","CAPTURED"),("incomplete_reason","none"),
 ("subject_identity_sha256",sha(identity_path)),("run_metadata_sha256",sha(run_path)),
 ("workload_metadata_sha256",sha(workload_metadata)),("workload_ready_receipt_sha256",sha(workload_ready)),
 ("supplemental_evidence_sha256",sha(gate_path)),("requested_duration_ms","2000"),
 ("actual_duration_ms","2000"),("capture_started_continuous_ns","1000000000"),
 ("capture_ended_continuous_ns","3000000000"),("target_identity_verified","true"),
 ("trace_target_pid_verified","true"),("time_profiler_instrument","true"),
 ("allocations_instrument","true"),("hangs_instrument","true"),
 ("time_profiler_target_verified","true"),("allocations_target_verified","true"),
 ("hangs_target_verified","true"),("time_profiler_rows","1"),("allocations_rows","1"),
 ("hangs_rows","1"),("maximum_main_thread_hang_ms","0"),("status","complete")]
pathlib.Path(final_trace_path).write_bytes(b"".join(f"{k}\t{v}\n".encode() for k,v in final_trace_rows))
manual_rows=[("format_version","1"),("screenshot_sha256",sha(screenshot_path)),
 ("video_sha256",sha(video_path)),("final_content_review","PASS"),("anchor_review","PASS"),
 ("restoration_review","PASS"),("geometry_review","PASS"),("reviewer","fixture"),("result","PASS")]
pathlib.Path(manual_path).write_bytes(b"".join(f"{k}\t{v}\n".encode() for k,v in manual_rows))
case_rows=[("format_version","2"),("subject",subject),("scenario","ascii"),
 ("session_id",intent_values["session_id"]),("nonce",intent_values["nonce"]),
 ("run_intent_sha256",sha(intent_path)),("run_metadata_sha256",sha(run_path)),
 ("trace_metadata_sha256",sha(final_trace_path)),("trace_archive_sha256",trace_archive_hash),
 ("manual_artifacts_sha256",sha(manual_path)),("manual_screenshot_sha256",sha(screenshot_path)),
 ("manual_video_sha256",sha(video_path)),("result","CASE-COMPLETE"),
 ("reason","all-required-evidence-complete")]
pathlib.Path(case_report_path).write_bytes(b"".join(f"{k}\t{v}\n".encode() for k,v in case_rows))
for path in (trace_path, tail_path, quit_path, exit_path, lifecycle_ready, lifecycle_registration): os.chmod(path, 0o400)
for path in (final_trace_path,manual_path,case_report_path): os.chmod(path,0o400)
os.chmod(run_path, 0o444)
PY
}

write_run_closure spaceterm "$SPACETERM_SUBJECT" "$SPACETERM_INTENT" \
    spaceterm-session "$SPACETERM_NONCE"
write_run_closure ghostty "$GHOSTTY_SUBJECT" "$GHOSTTY_INTENT" \
    ghostty-session "$GHOSTTY_NONCE"

pair_arguments=(
    --campaign-secret-file "$SECRET" --campaign-id campaign-a --pair-metadata "$PAIR"
    --scenario-plan "$PLAN"
)
for subject in spaceterm ghostty; do
    pair_arguments+=(
        "--$subject-subject-identity" "$TEMP_ROOT/$subject-subject.tsv"
        "--$subject-run-intent" "$TEMP_ROOT/$subject-run-intent.tsv"
        "--$subject-run-metadata" "$TEMP_ROOT/$subject-run.tsv"
        "--$subject-window-identity" "$TEMP_ROOT/$subject-window.tsv"
        "--$subject-driver-intent" "$TEMP_ROOT/$subject-driver-intent.tsv"
        "--$subject-driver-events" "$TEMP_ROOT/$subject-driver-events.tsv"
        "--$subject-driver-receipt" "$TEMP_ROOT/$subject-driver-receipt.tsv"
        "--$subject-driver-binary" "$DRIVER_BINARY"
        "--$subject-driver-source" "$DRIVER_SOURCE"
        "--$subject-driver-controller" "$DRIVER_CONTROLLER"
        "--$subject-plan-start-gate" "$TEMP_ROOT/$subject-plan-start-gate.tsv"
        "--$subject-trace-provisional-receipt" "$TEMP_ROOT/$subject-trace-provisional.tsv"
        "--$subject-workload-metadata" "$TEMP_ROOT/$subject-workload-metadata.tsv"
        "--$subject-workload-events" "$TEMP_ROOT/$subject-workload-events.tsv"
        "--$subject-workload-ready-receipt" "$TEMP_ROOT/$subject-workload-ready.tsv"
        "--$subject-lifecycle-ready-receipt" "$TEMP_ROOT/$subject-lifecycle-ready.tsv"
        "--$subject-lifecycle-registration" "$TEMP_ROOT/$subject-lifecycle-registration.tsv"
        "--$subject-tail-receipt" "$TEMP_ROOT/$subject-tail.tsv"
        "--$subject-quit-receipt" "$TEMP_ROOT/$subject-quit.tsv"
        "--$subject-exit-receipt" "$TEMP_ROOT/$subject-exit.tsv"
        "--$subject-case-report" "$TEMP_ROOT/$subject-case-report.tsv"
        "--$subject-trace-metadata" "$TEMP_ROOT/$subject-trace-metadata.tsv"
        "--$subject-trace-archive" "$TEMP_ROOT/$subject-ascii.trace"
        "--$subject-manual-artifacts" "$TEMP_ROOT/$subject-manual.tsv"
        "--$subject-manual-screenshot" "$TEMP_ROOT/$subject-screenshot.png"
        "--$subject-manual-video" "$TEMP_ROOT/$subject-video.mov"
    )
done
pair_arguments+=(
    --spaceterm-native-provisional-observation "$SPACETERM_NATIVE_PROVISIONAL"
    --spaceterm-native-observation "$SPACETERM_NATIVE_OBSERVATION"
    --spaceterm-native-runtime-metadata "$SPACETERM_NATIVE_METADATA"
    --spaceterm-native-runtime-samples "$SPACETERM_NATIVE_SAMPLES"
    --spaceterm-native-runtime-events "$SPACETERM_NATIVE_EVENTS"
    --spaceterm-native-failure-actions "$SPACETERM_NATIVE_FAILURES"
    --common-lifecycle-helper "$LIFECYCLE_HELPER"
    --appkit-terminator-source "$TERMINATOR_SOURCE"
    --appkit-terminator-binary "$TERMINATOR_BINARY"
)

"$TOOL" create "${pair_arguments[@]}" --output "$RESULT"
"$TOOL" verify "${pair_arguments[@]}" --receipt "$RESULT"
[[ "$(wc -l < "$RESULT" | tr -d ' ')" == 62 ]] || fail "pair result is not exact62"
[[ "$(stat -f '%Lp' "$RESULT")" == 400 ]] || fail "pair result is not private"
python3 - "$SECRET" "$RESULT" <<'PY'
import hashlib
import hmac
import struct
import sys

expected_keys = (
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
    "terminator_binary_sha256", "evidence_mode", "status",
    "auth_algorithm", "pair_result_hmac_sha256",
)
secret = open(sys.argv[1], "rb").read()
rows = open(sys.argv[2], "rb").readlines()
keys = tuple(row.split(b"\t", 1)[0].decode("ascii") for row in rows)
if keys != expected_keys:
    raise SystemExit("pair result key order is not the frozen exact62 schema")
unsigned = b"".join(rows[:-1])
actual = rows[-1].rstrip(b"\n").split(b"\t", 1)[1].decode("ascii")
expected = hmac.new(
    secret,
    b"spaceterm.performance.pair-result/v3\0"
    + struct.pack(">Q", len(unsigned)) + unsigned,
    hashlib.sha256,
).hexdigest()
if not hmac.compare_digest(actual, expected):
    raise SystemExit("pair result HMAC does not match the frozen v2 domain")
PY

expect_failure "one-sided pair" "$TOOL" verify \
    "${pair_arguments[@]/$TEMP_ROOT\/ghostty-run.tsv/$TEMP_ROOT\/missing-run.tsv}" \
    --receipt "$RESULT"
expect_failure "missing lifecycle ready" "$TOOL" verify \
    "${pair_arguments[@]/$TEMP_ROOT\/ghostty-lifecycle-ready.tsv/$TEMP_ROOT\/missing-ready.tsv}" \
    --receipt "$RESULT"
expect_failure "cross-run lifecycle registration" "$TOOL" verify \
    "${pair_arguments[@]/$TEMP_ROOT\/ghostty-lifecycle-registration.tsv/$TEMP_ROOT\/spaceterm-lifecycle-registration.tsv}" \
    --receipt "$RESULT"
expect_failure "cross-run tail" "$TOOL" verify \
    "${pair_arguments[@]/$TEMP_ROOT\/ghostty-tail.tsv/$TEMP_ROOT\/spaceterm-tail.tsv}" \
    --receipt "$RESULT"
expect_failure "campaign replay" "$TOOL" verify \
    "${pair_arguments[@]/campaign-a/campaign-b}" --receipt "$RESULT"
expect_failure "cross-subject driver intent" "$TOOL" verify \
    "${pair_arguments[@]/$TEMP_ROOT\/ghostty-driver-intent.tsv/$TEMP_ROOT\/spaceterm-driver-intent.tsv}" \
    --receipt "$RESULT"

MUTATED_DRIVER_RECEIPT="$TEMP_ROOT/mutated-driver-receipt.tsv"
sed 's/terminal_result\tverified/terminal_result\tforged/' \
    "$TEMP_ROOT/ghostty-driver-receipt.tsv" > "$MUTATED_DRIVER_RECEIPT"
chmod 0400 "$MUTATED_DRIVER_RECEIPT"
expect_failure "driver receipt mutation" "$TOOL" verify \
    "${pair_arguments[@]/$TEMP_ROOT\/ghostty-driver-receipt.tsv/$MUTATED_DRIVER_RECEIPT}" \
    --receipt "$RESULT"

CLONED_EVENTS="$TEMP_ROOT/cloned-events.tsv"
cp "$TEMP_ROOT/ghostty-driver-events.tsv" "$CLONED_EVENTS"
chmod 0400 "$CLONED_EVENTS"
expect_failure "driver path substitution" "$TOOL" verify \
    "${pair_arguments[@]/$TEMP_ROOT\/ghostty-driver-events.tsv/$CLONED_EVENTS}" \
    --receipt "$RESULT"

MUTATED_RESULT="$TEMP_ROOT/mutated-result.tsv"
sed 's/status\tcomplete/status\tincomplete/' "$RESULT" > "$MUTATED_RESULT"
chmod 0400 "$MUTATED_RESULT"
expect_failure "pair result mutation" "$TOOL" verify "${pair_arguments[@]}" \
    --receipt "$MUTATED_RESULT"

REPLAY_INTENT="$TEMP_ROOT/replay-intent.tsv"
sed 's/session_id\tghostty-session/session_id\tspaceterm-session/' \
    "$GHOSTTY_INTENT" > "$REPLAY_INTENT"
chmod 0444 "$REPLAY_INTENT"
expect_failure "paired session replay" "$TOOL" verify \
    "${pair_arguments[@]/$GHOSTTY_INTENT/$REPLAY_INTENT}" --receipt "$RESULT"

TEST_ONLY_RUN="$TEMP_ROOT/ghostty-run-test-only.tsv"
sed 's/evidence_mode\tproduction/evidence_mode\ttest-only/' \
    "$TEMP_ROOT/ghostty-run.tsv" > "$TEST_ONLY_RUN"
chmod 0444 "$TEST_ONLY_RUN"
expect_failure "test-only final evidence" "$TOOL" verify \
    "${pair_arguments[@]/$TEMP_ROOT\/ghostty-run.tsv/$TEST_ONLY_RUN}" \
    --receipt "$RESULT"

BAD_NATIVE_COUNT="$TEMP_ROOT/spaceterm-run-bad-native-count.tsv"
sed 's/native_failure_request_count\t0/native_failure_request_count\t9/' \
    "$TEMP_ROOT/spaceterm-run.tsv" > "$BAD_NATIVE_COUNT"
chmod 0444 "$BAD_NATIVE_COUNT"
expect_failure "forged native failure request count" "$TOOL" verify \
    "${pair_arguments[@]/$TEMP_ROOT\/spaceterm-run.tsv/$BAD_NATIVE_COUNT}" \
    --receipt "$RESULT"

expect_failure "cross-subject trace swap" "$TOOL" verify \
    "${pair_arguments[@]/$TEMP_ROOT\/ghostty-trace-provisional.tsv/$TEMP_ROOT\/spaceterm-trace-provisional.tsv}" \
    --receipt "$RESULT"
expect_failure "native observation swap" "$TOOL" verify \
    "${pair_arguments[@]/$SPACETERM_NATIVE_OBSERVATION/$SPACETERM_NATIVE_PROVISIONAL}" \
    --receipt "$RESULT"

# A wrapper invocation without either complete case bundle can never expose PASS.
PAIR_WRAPPER_OUTPUT="$TEMP_ROOT/pair-wrapper-output.tsv"
pair_wrapper_status=0
"$PAIR_ANALYZER" --campaign-id campaign-a --campaign-secret-file "$SECRET" \
    --scenario ascii --plan "$PLAN" --plan-metadata "$TEMP_ROOT/missing-plan-metadata.tsv" \
    --pair-metadata "$PAIR" --pair-result "$RESULT" \
    > "$PAIR_WRAPPER_OUTPUT" 2>/dev/null || pair_wrapper_status=$?
[[ "$pair_wrapper_status" == 2 \
    && "$(awk -F '\t' '$1 == "result" {print $2}' "$PAIR_WRAPPER_OUTPUT")" == NOT-RUN ]] \
    || fail "incomplete pair wrapper exposed a final result"
grep -Fxq $'result\tPASS' "$PAIR_WRAPPER_OUTPUT" \
    && fail "incomplete pair wrapper exposed PASS"

echo "performance pair result tests passed"
