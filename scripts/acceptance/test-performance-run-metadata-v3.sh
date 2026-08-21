#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
umask 077

SCRIPT_DIRECTORY="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-run-v3.XXXXXX")"
trap 'rm -rf -- "$TEMP_ROOT"' EXIT INT TERM
NONCE=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
TERMINATOR_SOURCE="$SCRIPT_DIRECTORY/performance-appkit-terminate.m"
TERMINATOR_BINARY="$TEMP_ROOT/performance-appkit-terminate"
printf '%s\n' '#!/bin/sh' 'exit 0' > "$TERMINATOR_BINARY"
chmod 0500 "$TERMINATOR_BINARY"
TERMINATOR_SOURCE_DEVICE="$(stat -f '%d' "$TERMINATOR_SOURCE")"
TERMINATOR_SOURCE_INODE="$(stat -f '%i' "$TERMINATOR_SOURCE")"
TERMINATOR_SOURCE_SHA256="$(shasum -a 256 "$TERMINATOR_SOURCE" | awk '{ print $1 }')"
TERMINATOR_BINARY_DEVICE="$(stat -f '%d' "$TERMINATOR_BINARY")"
TERMINATOR_BINARY_INODE="$(stat -f '%i' "$TERMINATOR_BINARY")"
TERMINATOR_BINARY_SHA256="$(shasum -a 256 "$TERMINATOR_BINARY" | awk '{ print $1 }')"
LIFECYCLE_HELPER="$SCRIPT_DIRECTORY/performance-subject-lifecycle.py"
PROCESS_INSPECTOR="$SCRIPT_DIRECTORY/../inspect-release-performance-process.py"
LIFECYCLE_HELPER_DEVICE="$(stat -f '%d' "$LIFECYCLE_HELPER")"
LIFECYCLE_HELPER_INODE="$(stat -f '%i' "$LIFECYCLE_HELPER")"
LIFECYCLE_HELPER_SHA256="$(shasum -a 256 "$LIFECYCLE_HELPER" | awk '{ print $1 }')"
SPACETERM_LIFECYCLE_HELPER="$TEMP_ROOT/spaceterm-lifecycle-helper.py"
GHOSTTY_LIFECYCLE_HELPER="$TEMP_ROOT/ghostty-lifecycle-helper.py"
cp -- "$LIFECYCLE_HELPER" "$SPACETERM_LIFECYCLE_HELPER"
cp -- "$LIFECYCLE_HELPER" "$GHOSTTY_LIFECYCLE_HELPER"
chmod 0500 "$SPACETERM_LIFECYCLE_HELPER" "$GHOSTTY_LIFECYCLE_HELPER"
PROCESS_INSPECTOR_DEVICE="$(stat -f '%d' "$PROCESS_INSPECTOR")"
PROCESS_INSPECTOR_INODE="$(stat -f '%i' "$PROCESS_INSPECTOR")"
PROCESS_INSPECTOR_SHA256="$(shasum -a 256 "$PROCESS_INSPECTOR" | awk '{ print $1 }')"
TERMINATOR_PROCESS_PID=99
TERMINATOR_PROCESS_START_IDENTITY=10:20

fail() { echo "test failure: $*" >&2; exit 1; }
sha256() { shasum -a 256 "$1" | awk '{ print $1 }'; }
expect_failure() {
    local label="$1"
    shift
    if "$@" >"$TEMP_ROOT/failure.stdout" 2>"$TEMP_ROOT/failure.stderr"; then
        fail "$label unexpectedly succeeded"
    fi
}

write_subject() {
    local subject="$1" output="$2"
    cat > "$output" <<EOF
format_version	1
subject	$subject
app_bundle_path	/Applications/$subject.app
bundle_identifier	dev.spaceterm.$subject
bundle_version	1.0+1
executable_path	/Applications/$subject.app/Contents/MacOS/$subject
executable_sha256	bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
executable_device	17
executable_inode	19
executable_fsid	17
signature_valid	true
signing_identifier	dev.spaceterm.$subject
team_identifier	none
cdhash	abcdef0123456789
process_pid	1234
process_start_identity	100:200
identity_status	frozen
EOF
    chmod 0444 "$output"
}

PLAN="$TEMP_ROOT/plan.tsv"
WORKLOAD="$TEMP_ROOT/workload"
printf 'event_id\toffset_ms\taction\targ0\targ1\nstop\t1000\tstop\t0\t0\n' > "$PLAN"
printf 'workload\n' > "$WORKLOAD"
for name in command environment font initial-grid; do printf '%s\n' "$name" > "$TEMP_ROOT/$name.tsv"; done
chmod 0444 "$PLAN" "$WORKLOAD" "$TEMP_ROOT"/{command,environment,font,initial-grid}.tsv
SPACETERM_SUBJECT="$TEMP_ROOT/spaceterm-subject.tsv"
GHOSTTY_SUBJECT="$TEMP_ROOT/ghostty-subject.tsv"
write_subject spaceterm "$SPACETERM_SUBJECT"
write_subject ghostty "$GHOSTTY_SUBJECT"
PAIR="$TEMP_ROOT/pair.tsv"
cat > "$PAIR" <<EOF
format_version	1
pair_id	pair-a
scenario	ascii
plan_sha256	$(sha256 "$PLAN")
workload_sha256	$(sha256 "$WORKLOAD")
command_sha256	$(sha256 "$TEMP_ROOT/command.tsv")
environment_sha256	$(sha256 "$TEMP_ROOT/environment.tsv")
font_sha256	$(sha256 "$TEMP_ROOT/font.tsv")
initial_grid_sha256	$(sha256 "$TEMP_ROOT/initial-grid.tsv")
duration_ms	1000
spaceterm_subject_identity_sha256	$(sha256 "$SPACETERM_SUBJECT")
ghostty_subject_identity_sha256	$(sha256 "$GHOSTTY_SUBJECT")
EOF
chmod 0444 "$PAIR"

PROVISIONAL="$TEMP_ROOT/provisional.tsv"
cat > "$PROVISIONAL" <<'EOF'
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
process.pid	1234
process.pidversion	7
process.executable.path	/Applications/spaceterm.app/Contents/MacOS/spaceterm
process.executable.device	17
process.executable.inode	19
process.executable.fsid	23:29
process.signature.cdhash	abcdef0123456789
process.signature.identifier	dev.spaceterm.spaceterm
process.signature.team_identifier	
terminal_font_selected	JetBrains Mono
initial_grid.rows	24
initial_grid.columns	80
initial_grid.logical_width	800
initial_grid.logical_height	480
initial_grid.backing_pixel_width	1600
initial_grid.backing_pixel_height	960
observation.complete	true
EOF
chmod 0400 "$PROVISIONAL"

intent_args=(
    --pair-metadata "$PAIR" --plan "$PLAN" --workload-binary "$WORKLOAD"
    --command-manifest "$TEMP_ROOT/command.tsv" --environment-manifest "$TEMP_ROOT/environment.tsv"
    --font-manifest "$TEMP_ROOT/font.tsv" --initial-grid-manifest "$TEMP_ROOT/initial-grid.tsv"
    --campaign-id campaign-a --session-id session-a --nonce "$NONCE"
)
SPACETERM_INTENT="$TEMP_ROOT/spaceterm-intent.tsv"
"$SCRIPT_DIRECTORY/freeze-performance-run-intent.sh" --subject spaceterm \
    --subject-identity "$SPACETERM_SUBJECT" "${intent_args[@]}" \
    --native-provisional-observation "$PROVISIONAL" --output "$SPACETERM_INTENT" >/dev/null
[[ "$(wc -l < "$SPACETERM_INTENT" | tr -d ' ')" == 19 ]] || fail "intent is not exact19"

GHOSTTY_INTENT="$TEMP_ROOT/ghostty-intent.tsv"
"$SCRIPT_DIRECTORY/freeze-performance-run-intent.sh" --subject ghostty \
    --subject-identity "$GHOSTTY_SUBJECT" "${intent_args[@]}" \
    --output "$GHOSTTY_INTENT" >/dev/null
[[ "$(awk -F '\t' '$1 == "native_provisional_observation_sha256" {print $2}' "$GHOSTTY_INTENT")" \
    == not-applicable ]] || fail "Ghostty intent fabricated native evidence"
expect_failure "Ghostty provisional artifact" "$SCRIPT_DIRECTORY/freeze-performance-run-intent.sh" \
    --subject ghostty --subject-identity "$GHOSTTY_SUBJECT" "${intent_args[@]}" \
    --native-provisional-observation "$PROVISIONAL" --output "$TEMP_ROOT/bad-ghostty-intent.tsv"
TEST_ONLY_INTENT="$TEMP_ROOT/test-only-intent.tsv"
SPACETERM_PERFORMANCE_TEST_MODE=1 "$SCRIPT_DIRECTORY/freeze-performance-run-intent.sh" \
    --subject ghostty --subject-identity "$GHOSTTY_SUBJECT" "${intent_args[@]}" \
    --output "$TEST_ONLY_INTENT" >/dev/null
[[ "$(awk -F '\t' '$1 == "evidence_mode" {print $2}' "$TEST_ONLY_INTENT")" == test-only ]] \
    || fail "test-mode freezer did not mark the run intent test-only"

SAMPLES="$TEMP_ROOT/runtime-samples.tsv"
EVENTS="$TEMP_ROOT/runtime-events.tsv"
FAILURES="$TEMP_ROOT/failure-actions.tsv"
METADATA="$TEMP_ROOT/runtime-metadata.tsv"
cat > "$SAMPLES" <<'EOF'
sequence	continuous_ns	worker_generation	screens_published	screens_enqueued	screens_superseded	event_queue_length	event_queue_high_water	ui_dispatches	ui_screen_events	ui_drain_high_water	ui_latest_generation	render_latest_generation	next_frame_generation	next_frame_count	presentable	minimized	occluded	workspace_visible	pane_visible	live_resize	viewport_total_rows	viewport_visible_rows	viewport_offset_rows	selection_present	resize_requests	resize_notifications	resize_applied	resize_coalesced	pty_rows	pty_columns	pty_pixel_width	pty_pixel_height	terminal_inputs_accepted	lifecycle	observer_drops
0	1000	1	1	1	0	0	0	1	1	1	1	1	1	1	1	0	0	1	1	0	24	24	0	0	0	0	0	0	24	80	800	480	1	2	0
EOF
printf 'sequence\tcontinuous_ns\tkind\tgeneration\taux0\taux1\n' > "$EVENTS"
printf 'request_id\tsequence\tcase_id\taction\tresult\tpane_id\tpane_state\tfailure_class\tfailure_recoverability\tfailure_operation\tstate_revision\tlatest_generation\tlast_valid_generation\tvisible_generation\tpending_recovery\tterminal_input_usable\tsession_attached\tresource_staged_count\tresource_staged_bytes\tresource_rolled_back_count\tresource_rolled_back_bytes\n' > "$FAILURES"
cat > "$METADATA" <<EOF
schema	spaceterm.acceptance.runtime-observation-metadata/v3
observation.source	production-app
run.id	run-a
package.app.sha256	dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd
process.pid	1234
runtime.samples.path	runtime-samples.tsv
runtime.samples.sha256	$(sha256 "$SAMPLES")
runtime.events.path	runtime-events.tsv
runtime.events.sha256	$(sha256 "$EVENTS")
failure.action.schema	spaceterm.acceptance.failure-action/v1
failure.action.enabled	false
failure.result.schema	spaceterm.acceptance.failure-action-result/v2
failure.actions.path	failure-actions.tsv
failure.actions.sha256	$(sha256 "$FAILURES")
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
chmod 0400 "$SAMPLES" "$EVENTS" "$FAILURES" "$METADATA"
FINAL="$TEMP_ROOT/native-final.tsv"
sed '$d' "$PROVISIONAL" > "$FINAL"
cat >> "$FINAL" <<EOF
provisional.observation.sha256	$(sha256 "$PROVISIONAL")
runtime.metadata.schema	spaceterm.acceptance.runtime-observation-metadata/v3
runtime.metadata.path	runtime-metadata.tsv
runtime.metadata.sha256	$(sha256 "$METADATA")
failure.result.schema	spaceterm.acceptance.failure-action-result/v2
failure.actions.path	failure-actions.tsv
failure.actions.sha256	$(sha256 "$FAILURES")
failure.request_count	0
failure.result_count	0
observation.complete	true
EOF
chmod 0400 "$FINAL"

SECRET="$TEMP_ROOT/campaign-secret"
printf '%064d' 0 > "$SECRET"
chmod 0600 "$SECRET"

write_causal_closure() {
    local prefix="$1" intent="$2" identity="$3" native_observation="$4"
    local terminator_source="$5" terminator_binary="$6"
    local lifecycle_helper="$TEMP_ROOT/$prefix-lifecycle-helper.py"
    local token
    token="$(printf '%064d' "$([[ "$prefix" == spaceterm ]] && echo 1 || echo 2)")"
    printf '%s-rss-samples\n' "$prefix" > "$TEMP_ROOT/$prefix-rss-samples.tsv"
    chmod 0400 "$TEMP_ROOT/$prefix-rss-samples.tsv"
    python3 - "$TEMP_ROOT/$prefix-workload-events.tsv" \
        "$TEMP_ROOT/$prefix-workload-metadata.tsv" "$TEMP_ROOT/$prefix-ready.tsv" \
        "$SECRET" "$identity" "$prefix" <<'PY'
import hashlib, hmac, os, pathlib, struct, sys
events_path, metadata_path, ready_path, secret_path, identity_path, prefix_name = map(pathlib.Path, sys.argv[1:])
secret = secret_path.read_bytes(); sha = lambda data: hashlib.sha256(data).hexdigest()
events_prefix = ("sequence\tcontinuous_ns\tkind\tevent_id\tbyte_count\trows\tcolumns\tpixel_width\tpixel_height\tstatus\n"
    "0\t1000\tseed-complete\tnone\t1\t24\t80\t800\t600\tok\n"
    "1\t1100\tmeasurement-ready\tnone\t1\t24\t80\t800\t600\tok\n").encode()
events = events_prefix + b"2\t3000\tproducer-end\tnone\t1\t24\t80\t800\t600\tsuccess\n"
events_path.write_bytes(events); event_stat = events_path.stat(); subject_hash = sha(identity_path.read_bytes())
ready_rows = [("format_version","1"),("campaign_id","campaign-a"),("session_id","session-a"),
    ("nonce","a"*64),("subject_identity_sha256",subject_hash),("producer_pid","50"),
    ("producer_started_continuous_ns","500"),("producer_session_id","50"),
    ("producer_process_group","50"),("tty_device","1"),("tty_inode","2"),("tty_rdev","3"),
    ("events_device",str(event_stat.st_dev)),("events_inode",str(event_stat.st_ino)),
    ("events_prefix_bytes",str(len(events_prefix))),("events_prefix_sha256",sha(events_prefix)),
    ("measurement_ready_continuous_ns","1100"),("measurement_ready_byte_count","1"),
    ("auth_algorithm","hmac-sha256")]
unsigned_ready=b"".join(f"{k}\t{v}\n".encode() for k,v in ready_rows)
ready_hmac=hmac.new(secret,b"spaceterm.performance.workload-ready/v1\0"+struct.pack(">Q",len(unsigned_ready))+unsigned_ready,hashlib.sha256).hexdigest()
ready=unsigned_ready+f"ready_hmac_sha256\t{ready_hmac}\n".encode(); ready_path.write_bytes(ready)
rows=[("format_version","3"),("scenario","ascii"),("campaign_id","campaign-a"),("session_id","session-a"),
    ("nonce","a"*64),("subject_identity_sha256",subject_hash),("subject_process_pid","1234"),
    ("subject_process_start_identity","100:200"),("producer_sha256","c"*64),("producer_pid","50"),
    ("producer_started_continuous_ns","500"),("producer_session_id","50"),("producer_process_group","50"),
    ("tty_device","1"),("tty_inode","2"),("tty_rdev","3"),("ready_receipt_sha256",sha(ready)),
    ("events_sha256",sha(events)),("auth_algorithm","hmac-sha256"),("seed_sha256","c"*64),
    ("seed_bytes","1"),("requested_duration_ms","1"),("warmup_ms","0"),("requested_iterations","1"),
    ("requested_seed_rows","1"),("emitted_bytes","1"),("input_events","0"),
    ("plan_start_continuous_ns","1000"),("started_continuous_ns","1000"),("ended_continuous_ns","3000"),
    ("status","complete")]
unsigned=b"".join(f"{k}\t{v}\n".encode() for k,v in rows)
payload=b"spaceterm.performance.workload-auth/v1\0"+struct.pack(">Q",len(unsigned))+unsigned+struct.pack(">Q",len(events))+events
metadata_path.write_bytes(unsigned+f"events_hmac_sha256\t{hmac.new(secret,payload,hashlib.sha256).hexdigest()}\n".encode())
PY
    chmod 0400 "$TEMP_ROOT/$prefix-workload-events.tsv" \
        "$TEMP_ROOT/$prefix-workload-metadata.tsv" "$TEMP_ROOT/$prefix-ready.tsv"
    local window="$TEMP_ROOT/$prefix-window.tsv"
    local driver_binary="$TEMP_ROOT/$prefix-performance-driver"
    local driver_intent="$TEMP_ROOT/$prefix-driver-intent.tsv"
    local driver_events="$TEMP_ROOT/$prefix-driver-events.tsv"
    local driver_receipt="$TEMP_ROOT/$prefix-driver-receipt.tsv"
    local gate="$TEMP_ROOT/$prefix-plan-start-gate.tsv"
    local pid window_number=42 plan_start=1000000000
    pid="$(awk -F '\t' '$1 == "process_pid" {print $2}' "$identity")"
    printf 'driver-binary\n' > "$driver_binary"
    chmod 0500 "$driver_binary"
    cat > "$window" <<EOF
format_version	1
subject_identity_sha256	$(sha256 "$identity")
subject	$prefix
process_pid	$pid
process_start_identity	100:200
bundle_identifier	dev.spaceterm.$prefix
executable_sha256	bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
window_number	$window_number
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
    chmod 0400 "$window"
    python3 - "$gate" "$SECRET" "$prefix" "$plan_start" \
        "$(sha256 "$TEMP_ROOT/$prefix-ready.tsv")" <<'PY'
import hashlib,hmac,os,pathlib,struct,sys
output,secret,prefix,start,ready=sys.argv[1:]
unsigned=("format_version\t1\ncampaign_id\tcampaign-a\nsession_id\tsession-a\n"
    f"nonce\t{'a'*64}\nready_receipt_sha256\t{ready}\nplan_start_continuous_ns\t{start}\n").encode()
signature=hmac.new(pathlib.Path(secret).read_bytes(),
 b"spaceterm.performance.plan-start-gate/v1\0"+struct.pack(">Q",len(unsigned))+unsigned,
 hashlib.sha256).hexdigest()
fd=os.open(output,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o400)
try: os.write(fd,unsigned+f"start_gate_hmac_sha256\t{signature}\n".encode())
finally: os.close(fd)
PY
    local -a driver_bindings=(
        --campaign-secret-file "$SECRET" --campaign-id campaign-a --session-id session-a
        --nonce "$NONCE" --driver-output "$driver_events" --driver-binary "$driver_binary"
        --driver-source "$SCRIPT_DIRECTORY/performance-driver.m"
        --controller "$SCRIPT_DIRECTORY/run-native-performance-scenario.sh"
        --scenario-plan "$PLAN" --plan-start-continuous-ns "$plan_start"
        --subject-identity "$identity" --window-identity "$window"
    )
    "$SCRIPT_DIRECTORY/performance-driver-receipt.py" intent "${driver_bindings[@]}" \
        --output "$driver_intent"
    cat > "$driver_events" <<EOF
sequence	continuous_ns	event_id	action	target_pid	window_number	requested_a	requested_b	observed_a	observed_b	result
0	2000000000	stop	stop	$pid	$window_number	0	0	1	1	verified
EOF
    chmod 0400 "$driver_events"
    "$SCRIPT_DIRECTORY/performance-driver-receipt.py" finalize "${driver_bindings[@]}" \
        --intent "$driver_intent" --receipt-output "$driver_receipt"
    python3 - "$TEMP_ROOT/$prefix-trace-provisional.tsv" "$SECRET" "$identity" "$intent" \
        "$TEMP_ROOT/$prefix-workload-metadata.tsv" "$TEMP_ROOT/$prefix-ready.tsv" \
        "$gate" <<'PY'
import hashlib, hmac, pathlib, struct, sys
output, secret, subject, intent, workload, ready, gate = map(pathlib.Path, sys.argv[1:])
sha = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
rows = [
    ("format_version", "1"), ("subject_identity_sha256", sha(subject)),
    ("run_intent_sha256", sha(intent)), ("workload_metadata_sha256", sha(workload)),
    ("workload_ready_receipt_sha256", sha(ready)),
    ("supplemental_evidence_sha256", sha(gate)), ("capture_status", "CAPTURED"),
    ("requested_duration_ms", "1000"), ("actual_duration_ms", "1000"),
    ("capture_started_continuous_ns", "1000"),
    ("capture_ended_continuous_ns", "2000"), ("trace_bundle_tree_sha256", "c" * 64),
    ("toc_sha256", "c" * 64), ("time_profile_export_sha256", "c" * 64),
    ("allocations_export_sha256", "c" * 64), ("hangs_export_sha256", "c" * 64),
    ("trace_verification_sha256", "c" * 64), ("verifier_sha256", "c" * 64),
    ("evidence_mode", "production"),
    ("status", "complete"), ("auth_algorithm", "hmac-sha256"),
]
unsigned = b"".join(f"{key}\t{value}\n".encode() for key, value in rows)
payload = b"spaceterm.performance.trace-provisional/v1\0" + struct.pack(">Q", len(unsigned)) + unsigned
signature = hmac.new(secret.read_bytes(), payload, hashlib.sha256).hexdigest()
output.write_bytes(unsigned + f"provisional_hmac_sha256\t{signature}\n".encode())
PY
    chmod 0400 "$TEMP_ROOT/$prefix-trace-provisional.tsv"
    python3 - "$TEMP_ROOT/$prefix-lifecycle-ready.tsv" \
        "$TEMP_ROOT/$prefix-lifecycle-registration.tsv" "$SECRET" "$prefix" "$identity" \
        "$intent" "$TEMP_ROOT/$prefix-tail.tsv" "$TEMP_ROOT/$prefix-workload-metadata.tsv" \
        "$TEMP_ROOT/$prefix-workload-events.tsv" "$TEMP_ROOT/$prefix-ready.tsv" \
        "$TEMP_ROOT/$prefix-quit.tsv" "$TEMP_ROOT/$prefix-exit.tsv" "$native_observation" \
        "$lifecycle_helper" "$PROCESS_INSPECTOR" "$terminator_source" \
        "$terminator_binary" "$token" <<'PY'
import hashlib,hmac,pathlib,struct,sys
(ready_out,reg_out,secret_path,subject_name,identity_path,intent_path,tail_path,
 workload_path,events_path,workload_ready_path,quit_path,exit_path,native_path,
 helper_path,inspector_path,source_path,binary_path,token)=map(pathlib.Path,sys.argv[1:])
secret=secret_path.read_bytes(); sha=lambda path:hashlib.sha256(path.read_bytes()).hexdigest()
identity=dict(line.split("\t",1) for line in identity_path.read_text().splitlines())
tool=[("lifecycle_helper_device",str(helper_path.stat().st_dev)),
 ("lifecycle_helper_inode",str(helper_path.stat().st_ino)),
 ("lifecycle_helper_sha256",sha(helper_path)),
 ("process_inspector_device",str(inspector_path.stat().st_dev)),
 ("process_inspector_inode",str(inspector_path.stat().st_ino)),
 ("process_inspector_sha256",sha(inspector_path)),
 ("appkit_terminator_process_pid","99"),
 ("appkit_terminator_process_start_identity","10:20"),
 ("appkit_terminator_source_device",str(source_path.stat().st_dev)),
 ("appkit_terminator_source_inode",str(source_path.stat().st_ino)),
 ("appkit_terminator_source_sha256",sha(source_path)),
 ("appkit_terminator_binary_device",str(binary_path.stat().st_dev)),
 ("appkit_terminator_binary_inode",str(binary_path.stat().st_ino)),
 ("appkit_terminator_binary_sha256",sha(binary_path))]
ready=[("schema","spaceterm.acceptance.performance-lifecycle-ready/v1"),
 ("subject",subject_name.name),("campaign_id","campaign-a"),("session_id","session-a"),
 ("nonce","a"*64),("subject_identity_sha256",sha(identity_path)),
 ("process_pid",identity["process_pid"]),("process_start_identity",identity["process_start_identity"]),
 ("executable_sha256",identity["executable_sha256"]),("ready_continuous_ns","900"),
 ("registration_control_device","1"),("registration_control_inode","2")]+tool+[
 ("evidence_mode","production"),("auth_algorithm","hmac-sha256"),("status","ready")]
def signed(rows,field,magic):
 unsigned=b"".join(f"{k}\t{v}\n".encode() for k,v in rows)
 signature=hmac.new(secret,magic+struct.pack(">Q",len(unsigned))+unsigned,hashlib.sha256).hexdigest()
 return b"".join(f"{k}\t{v}\n".encode() for k,v in rows[:-1])+f"{field}\t{signature}\n".encode()+f"{rows[-1][0]}\t{rows[-1][1]}\n".encode()
ready_out.write_bytes(signed(ready,"receipt_hmac_sha256",b"spaceterm.acceptance.performance-lifecycle-ready/v1\0"))
native="not-applicable" if native_path.name=="not-applicable" else str(native_path.resolve())
reg=[("format_version","1"),("campaign_id","campaign-a"),("session_id","session-a"),
 ("nonce","a"*64),("registration_token",token.name),("subject_identity_sha256",sha(identity_path)),
 ("process_pid",identity["process_pid"]),("process_start_identity",identity["process_start_identity"]),
 ("run_intent_path",str(intent_path.resolve())),("run_intent_sha256",sha(intent_path)),
 ("tail_receipt_path",str(tail_path.resolve())),("workload_metadata_path",str(workload_path.resolve())),
 ("workload_events_path",str(events_path.resolve())),
 ("workload_ready_receipt_path",str(workload_ready_path.resolve())),
 ("quit_receipt_path",str(quit_path.resolve())),("subject_exit_receipt_path",str(exit_path.resolve())),
 ("native_observation_path",native)]+tool+[("evidence_mode","production"),
 ("auth_algorithm","hmac-sha256"),("status","registered")]
reg_out.write_bytes(signed(reg,"registration_hmac_sha256",b"spaceterm.acceptance.performance-lifecycle-registration/v1\0"))
PY
    chmod 0400 "$TEMP_ROOT/$prefix-lifecycle-ready.tsv" \
        "$TEMP_ROOT/$prefix-lifecycle-registration.tsv"
    "$SCRIPT_DIRECTORY/performance-tail-receipt.py" create \
        --campaign-secret-file "$SECRET" --campaign-id campaign-a --session-id session-a \
        --nonce "$NONCE" --quit-token "$token" --run-intent "$intent" \
        --subject-identity "$identity" \
        --driver-receipt "$TEMP_ROOT/$prefix-driver-receipt.tsv" \
        --driver-events "$TEMP_ROOT/$prefix-driver-events.tsv" \
        --workload-metadata "$TEMP_ROOT/$prefix-workload-metadata.tsv" \
        --workload-events "$TEMP_ROOT/$prefix-workload-events.tsv" \
        --workload-ready-receipt "$TEMP_ROOT/$prefix-ready.tsv" \
        --rss-samples "$TEMP_ROOT/$prefix-rss-samples.tsv" \
        --trace-provisional-receipt "$TEMP_ROOT/$prefix-trace-provisional.tsv" \
        --lifecycle-ready-receipt "$TEMP_ROOT/$prefix-lifecycle-ready.tsv" \
        --tail-completed-continuous-ns 5000003000 \
        --appkit-terminator-source "$TERMINATOR_SOURCE" \
        --appkit-terminator-binary "$TERMINATOR_BINARY" \
        --output "$TEMP_ROOT/$prefix-tail.tsv"
    cat > "$TEMP_ROOT/$prefix-quit.tsv" <<EOF
format_version	1
campaign_id	campaign-a
session_id	session-a
nonce	$NONCE
run_intent_sha256	$(sha256 "$intent")
subject_process_pid	1234
subject_process_start_identity	100:200
quit_token	$token
request_continuous_ns	5000003100
exit_continuous_ns	5000003200
termination_method	appkit-terminate
runtime_closure_status	confirmed
lifecycle_helper_device	$(stat -f '%d' "$lifecycle_helper")
lifecycle_helper_inode	$(stat -f '%i' "$lifecycle_helper")
lifecycle_helper_sha256	$LIFECYCLE_HELPER_SHA256
process_inspector_device	$PROCESS_INSPECTOR_DEVICE
process_inspector_inode	$PROCESS_INSPECTOR_INODE
process_inspector_sha256	$PROCESS_INSPECTOR_SHA256
appkit_terminator_process_pid	$TERMINATOR_PROCESS_PID
appkit_terminator_process_start_identity	$TERMINATOR_PROCESS_START_IDENTITY
appkit_terminator_source_device	$TERMINATOR_SOURCE_DEVICE
appkit_terminator_source_inode	$TERMINATOR_SOURCE_INODE
appkit_terminator_source_sha256	$TERMINATOR_SOURCE_SHA256
appkit_terminator_binary_device	$TERMINATOR_BINARY_DEVICE
appkit_terminator_binary_inode	$TERMINATOR_BINARY_INODE
appkit_terminator_binary_sha256	$TERMINATOR_BINARY_SHA256
evidence_mode	production
status	completed
EOF
    chmod 0400 "$TEMP_ROOT/$prefix-quit.tsv"
    python3 - "$TEMP_ROOT/$prefix-exit.tsv" "$SECRET" "$prefix" "$identity" "$intent" \
        "$TEMP_ROOT/$prefix-tail.tsv" "$TEMP_ROOT/$prefix-quit.tsv" "$native_observation" \
        "$lifecycle_helper" "$PROCESS_INSPECTOR" "$terminator_source" \
        "$terminator_binary" <<'PY'
import hashlib, hmac, pathlib, struct, sys
output, secret, subject_name, identity, intent, tail, quit_receipt = map(pathlib.Path, sys.argv[1:8])
native_arg = sys.argv[8]
sha = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
values = dict(line.split("\t", 1) for line in intent.read_text().splitlines())
native_hash = sha(pathlib.Path(native_arg)) if subject_name.name == "spaceterm" else "not-applicable"
rows = [
    ("schema", "spaceterm.acceptance.performance-subject-exit/v1"),
    ("subject", subject_name.name), ("campaign_id", values["campaign_id"]),
    ("session_id", values["session_id"]), ("nonce", values["nonce"]),
    ("run_intent_sha256", sha(intent)), ("subject_identity_sha256", sha(identity)),
    ("process_pid", values["process_pid"]),
    ("process_start_identity", values["process_start_identity"]),
    ("tail_receipt_sha256", sha(tail)), ("quit_receipt_sha256", sha(quit_receipt)),
    ("exit_requested_continuous_ns", "5000003100"),
    ("process_exited_continuous_ns", "5000003200"),
    ("exit_status", "normal"), ("native_observation_sha256", native_hash),
    ("lifecycle_helper_device", str(pathlib.Path(sys.argv[9]).stat().st_dev)),
    ("lifecycle_helper_inode", str(pathlib.Path(sys.argv[9]).stat().st_ino)),
    ("lifecycle_helper_sha256", sha(pathlib.Path(sys.argv[9]))),
    ("process_inspector_device", str(pathlib.Path(sys.argv[10]).stat().st_dev)),
    ("process_inspector_inode", str(pathlib.Path(sys.argv[10]).stat().st_ino)),
    ("process_inspector_sha256", sha(pathlib.Path(sys.argv[10]))),
    ("appkit_terminator_process_pid", "99"),
    ("appkit_terminator_process_start_identity", "10:20"),
    ("appkit_terminator_source_device", str(pathlib.Path(sys.argv[11]).stat().st_dev)),
    ("appkit_terminator_source_inode", str(pathlib.Path(sys.argv[11]).stat().st_ino)),
    ("appkit_terminator_source_sha256", sha(pathlib.Path(sys.argv[11]))),
    ("appkit_terminator_binary_device", str(pathlib.Path(sys.argv[12]).stat().st_dev)),
    ("appkit_terminator_binary_inode", str(pathlib.Path(sys.argv[12]).stat().st_ino)),
    ("appkit_terminator_binary_sha256", sha(pathlib.Path(sys.argv[12]))),
    ("evidence_mode", "production"),
    ("auth_algorithm", "hmac-sha256"),
]
status = ("status", "complete")
unsigned = b"".join(f"{key}\t{value}\n".encode() for key, value in rows + [status])
payload = b"spaceterm.acceptance.performance-subject-exit/v1\0" + struct.pack(">Q", len(unsigned)) + unsigned
signature = hmac.new(secret.read_bytes(), payload, hashlib.sha256).hexdigest()
output.write_bytes(b"".join(f"{key}\t{value}\n".encode() for key, value in rows)
    + f"receipt_hmac_sha256\t{signature}\nstatus\tcomplete\n".encode())
PY
    chmod 0400 "$TEMP_ROOT/$prefix-exit.tsv"
}

write_causal_closure spaceterm "$SPACETERM_INTENT" "$SPACETERM_SUBJECT" "$FINAL" \
    "$TERMINATOR_SOURCE" "$TERMINATOR_BINARY"
write_causal_closure ghostty "$GHOSTTY_INTENT" "$GHOSTTY_SUBJECT" not-applicable \
    "$TERMINATOR_SOURCE" "$TERMINATOR_BINARY"
SPACETERM_CAUSAL=(
    --campaign-secret-file "$SECRET"
    --trace-provisional-receipt "$TEMP_ROOT/spaceterm-trace-provisional.tsv"
    --performance-tail-receipt "$TEMP_ROOT/spaceterm-tail.tsv"
    --performance-quit-receipt "$TEMP_ROOT/spaceterm-quit.tsv"
    --subject-exit-receipt "$TEMP_ROOT/spaceterm-exit.tsv"
    --driver-receipt "$TEMP_ROOT/spaceterm-driver-receipt.tsv"
    --driver-events "$TEMP_ROOT/spaceterm-driver-events.tsv"
    --driver-intent "$TEMP_ROOT/spaceterm-driver-intent.tsv"
    --window-identity "$TEMP_ROOT/spaceterm-window.tsv"
    --driver-binary "$TEMP_ROOT/spaceterm-performance-driver"
    --driver-source "$SCRIPT_DIRECTORY/performance-driver.m"
    --driver-controller "$SCRIPT_DIRECTORY/run-native-performance-scenario.sh"
    --scenario-plan "$PLAN"
    --plan-start-gate "$TEMP_ROOT/spaceterm-plan-start-gate.tsv"
    --workload-metadata "$TEMP_ROOT/spaceterm-workload-metadata.tsv"
    --workload-events "$TEMP_ROOT/spaceterm-workload-events.tsv"
    --workload-ready-receipt "$TEMP_ROOT/spaceterm-ready.tsv"
    --rss-samples "$TEMP_ROOT/spaceterm-rss-samples.tsv"
    --performance-lifecycle-ready-receipt "$TEMP_ROOT/spaceterm-lifecycle-ready.tsv"
    --performance-lifecycle-registration "$TEMP_ROOT/spaceterm-lifecycle-registration.tsv"
    --subject-lifecycle-helper "$SPACETERM_LIFECYCLE_HELPER"
    --common-lifecycle-helper "$LIFECYCLE_HELPER"
    --expected-common-lifecycle-helper-device "$LIFECYCLE_HELPER_DEVICE"
    --expected-common-lifecycle-helper-inode "$LIFECYCLE_HELPER_INODE"
    --expected-common-lifecycle-helper-sha256 "$LIFECYCLE_HELPER_SHA256"
    --appkit-terminator-source "$TERMINATOR_SOURCE"
    --appkit-terminator-binary "$TERMINATOR_BINARY"
)
GHOSTTY_CAUSAL=(
    --campaign-secret-file "$SECRET"
    --trace-provisional-receipt "$TEMP_ROOT/ghostty-trace-provisional.tsv"
    --performance-tail-receipt "$TEMP_ROOT/ghostty-tail.tsv"
    --performance-quit-receipt "$TEMP_ROOT/ghostty-quit.tsv"
    --subject-exit-receipt "$TEMP_ROOT/ghostty-exit.tsv"
    --driver-receipt "$TEMP_ROOT/ghostty-driver-receipt.tsv"
    --driver-events "$TEMP_ROOT/ghostty-driver-events.tsv"
    --driver-intent "$TEMP_ROOT/ghostty-driver-intent.tsv"
    --window-identity "$TEMP_ROOT/ghostty-window.tsv"
    --driver-binary "$TEMP_ROOT/ghostty-performance-driver"
    --driver-source "$SCRIPT_DIRECTORY/performance-driver.m"
    --driver-controller "$SCRIPT_DIRECTORY/run-native-performance-scenario.sh"
    --scenario-plan "$PLAN"
    --plan-start-gate "$TEMP_ROOT/ghostty-plan-start-gate.tsv"
    --workload-metadata "$TEMP_ROOT/ghostty-workload-metadata.tsv"
    --workload-events "$TEMP_ROOT/ghostty-workload-events.tsv"
    --workload-ready-receipt "$TEMP_ROOT/ghostty-ready.tsv"
    --rss-samples "$TEMP_ROOT/ghostty-rss-samples.tsv"
    --performance-lifecycle-ready-receipt "$TEMP_ROOT/ghostty-lifecycle-ready.tsv"
    --performance-lifecycle-registration "$TEMP_ROOT/ghostty-lifecycle-registration.tsv"
    --subject-lifecycle-helper "$GHOSTTY_LIFECYCLE_HELPER"
    --common-lifecycle-helper "$LIFECYCLE_HELPER"
    --expected-common-lifecycle-helper-device "$LIFECYCLE_HELPER_DEVICE"
    --expected-common-lifecycle-helper-inode "$LIFECYCLE_HELPER_INODE"
    --expected-common-lifecycle-helper-sha256 "$LIFECYCLE_HELPER_SHA256"
    --appkit-terminator-source "$TERMINATOR_SOURCE"
    --appkit-terminator-binary "$TERMINATOR_BINARY"
)
expect_failure "test-only intent production finalization" \
    "$SCRIPT_DIRECTORY/freeze-performance-run.sh" --run-intent "$TEST_ONLY_INTENT" \
    --subject-identity "$GHOSTTY_SUBJECT" "${GHOSTTY_CAUSAL[@]}" \
    --output "$TEMP_ROOT/test-only-final.tsv"

closure_args=(
    --native-provisional-observation "$PROVISIONAL" --native-observation "$FINAL"
    --native-runtime-metadata "$METADATA" --native-runtime-samples "$SAMPLES"
    --native-runtime-events "$EVENTS" --native-failure-actions "$FAILURES"
)
SPACETERM_RUN="$TEMP_ROOT/spaceterm-run.tsv"
"$SCRIPT_DIRECTORY/freeze-performance-run.sh" --run-intent "$SPACETERM_INTENT" \
    --subject-identity "$SPACETERM_SUBJECT" "${SPACETERM_CAUSAL[@]}" \
    "${closure_args[@]}" --output "$SPACETERM_RUN" >/dev/null
[[ "$(wc -l < "$SPACETERM_RUN" | tr -d ' ')" == 35 \
    && "$(awk -F '\t' '$1 == "run_intent_sha256" {print $2}' "$SPACETERM_RUN")" \
        == "$(sha256 "$SPACETERM_INTENT")" ]] || fail "SpaceTerm final metadata is not exact35"
for binding in trace_provisional_receipt performance_tail_receipt \
    performance_quit_receipt subject_exit_receipt; do
    value="$(awk -F '\t' -v key="${binding}_sha256" '$1 == key {print $2}' "$SPACETERM_RUN")"
    [[ "$value" =~ ^[0-9a-f]{64}$ ]] || fail "final metadata omitted $binding binding"
done

GHOSTTY_RUN="$TEMP_ROOT/ghostty-run.tsv"
"$SCRIPT_DIRECTORY/freeze-performance-run.sh" --run-intent "$GHOSTTY_INTENT" \
    --subject-identity "$GHOSTTY_SUBJECT" "${GHOSTTY_CAUSAL[@]}" --output "$GHOSTTY_RUN" >/dev/null
[[ "$(awk -F '\t' '$2 == "not-applicable" {count++} END {print count}' "$GHOSTTY_RUN")" == 10 ]] \
    || fail "Ghostty final metadata does not contain ten N/A fields"
WRONG_LIFECYCLE_COPY="$TEMP_ROOT/wrong-lifecycle-copy.py"
cp -- "$LIFECYCLE_HELPER" "$WRONG_LIFECYCLE_COPY"
chmod 0500 "$WRONG_LIFECYCLE_COPY"
expect_failure "unregistered lifecycle helper copy" \
    "$SCRIPT_DIRECTORY/freeze-performance-run.sh" --run-intent "$SPACETERM_INTENT" \
    --subject-identity "$SPACETERM_SUBJECT" \
    "${SPACETERM_CAUSAL[@]/$SPACETERM_LIFECYCLE_HELPER/$WRONG_LIFECYCLE_COPY}" \
    "${closure_args[@]}" --output "$TEMP_ROOT/wrong-copy-run.tsv"
mkdir -m 0700 "$TEMP_ROOT/outside-run"
MOVED_LIFECYCLE_HELPER="$TEMP_ROOT/outside-run/spaceterm-lifecycle-helper.py"
mv -- "$SPACETERM_LIFECYCLE_HELPER" "$MOVED_LIFECYCLE_HELPER"
expect_failure "registered lifecycle helper outside run directory" \
    "$SCRIPT_DIRECTORY/freeze-performance-run.sh" --run-intent "$SPACETERM_INTENT" \
    --subject-identity "$SPACETERM_SUBJECT" \
    "${SPACETERM_CAUSAL[@]/$SPACETERM_LIFECYCLE_HELPER/$MOVED_LIFECYCLE_HELPER}" \
    "${closure_args[@]}" --output "$TEMP_ROOT/wrong-path-run.tsv"
mv -- "$MOVED_LIFECYCLE_HELPER" "$SPACETERM_LIFECYCLE_HELPER"
WRONG_LIFECYCLE_HASH=0000000000000000000000000000000000000000000000000000000000000000
expect_failure "wrong frozen lifecycle helper hash" \
    "$SCRIPT_DIRECTORY/freeze-performance-run.sh" --run-intent "$SPACETERM_INTENT" \
    --subject-identity "$SPACETERM_SUBJECT" \
    "${SPACETERM_CAUSAL[@]/$LIFECYCLE_HELPER_SHA256/$WRONG_LIFECYCLE_HASH}" \
    "${closure_args[@]}" --output "$TEMP_ROOT/wrong-hash-run.tsv"
BAD_LIFECYCLE_READY="$TEMP_ROOT/bad-lifecycle-ready.tsv"
sed 's/status\tready/status\tforged/' "$TEMP_ROOT/spaceterm-lifecycle-ready.tsv" \
    > "$BAD_LIFECYCLE_READY"
chmod 0400 "$BAD_LIFECYCLE_READY"
expect_failure "forged lifecycle ready receipt" "$SCRIPT_DIRECTORY/freeze-performance-run.sh" \
    --run-intent "$SPACETERM_INTENT" --subject-identity "$SPACETERM_SUBJECT" \
    "${SPACETERM_CAUSAL[@]/$TEMP_ROOT\/spaceterm-lifecycle-ready.tsv/$BAD_LIFECYCLE_READY}" \
    "${closure_args[@]}" --output "$TEMP_ROOT/forged-lifecycle-run.tsv"
expect_failure "cross-subject intent replay" "$SCRIPT_DIRECTORY/freeze-performance-run.sh" \
    --run-intent "$SPACETERM_INTENT" --subject-identity "$GHOSTTY_SUBJECT" \
    "${SPACETERM_CAUSAL[@]}" "${closure_args[@]}" --output "$TEMP_ROOT/cross-subject.tsv"
expect_failure "missing SpaceTerm closure" "$SCRIPT_DIRECTORY/freeze-performance-run.sh" \
    --run-intent "$SPACETERM_INTENT" --subject-identity "$SPACETERM_SUBJECT" \
    "${SPACETERM_CAUSAL[@]}" --output "$TEMP_ROOT/missing-closure.tsv"
expect_failure "Ghostty forged runtime" "$SCRIPT_DIRECTORY/freeze-performance-run.sh" \
    --run-intent "$GHOSTTY_INTENT" --subject-identity "$GHOSTTY_SUBJECT" \
    "${GHOSTTY_CAUSAL[@]}" --native-runtime-metadata "$METADATA" \
    --output "$TEMP_ROOT/ghostty-forged-runtime.tsv"

BAD_QUIT="$TEMP_ROOT/forced-quit.tsv"
sed 's/termination_method\tappkit-terminate/termination_method\tforced-signal/' \
    "$TEMP_ROOT/spaceterm-quit.tsv" > "$BAD_QUIT"
chmod 0400 "$BAD_QUIT"
expect_failure "forced termination receipt" "$SCRIPT_DIRECTORY/freeze-performance-run.sh" \
    --run-intent "$SPACETERM_INTENT" --subject-identity "$SPACETERM_SUBJECT" \
    "${SPACETERM_CAUSAL[@]/$TEMP_ROOT\/spaceterm-quit.tsv/$BAD_QUIT}" \
    "${closure_args[@]}" --output "$TEMP_ROOT/forced-run.tsv"
BAD_EXIT="$TEMP_ROOT/pid-reused-exit.tsv"
sed 's/process_start_identity\t100:200/process_start_identity\t101:0/' \
    "$TEMP_ROOT/spaceterm-exit.tsv" > "$BAD_EXIT"
chmod 0400 "$BAD_EXIT"
expect_failure "PID reuse exit receipt" "$SCRIPT_DIRECTORY/freeze-performance-run.sh" \
    --run-intent "$SPACETERM_INTENT" --subject-identity "$SPACETERM_SUBJECT" \
    "${SPACETERM_CAUSAL[@]/$TEMP_ROOT\/spaceterm-exit.tsv/$BAD_EXIT}" \
    "${closure_args[@]}" --output "$TEMP_ROOT/reused-run.tsv"
expect_failure "cross-intent trace provisional" "$SCRIPT_DIRECTORY/freeze-performance-run.sh" \
    --run-intent "$SPACETERM_INTENT" --subject-identity "$SPACETERM_SUBJECT" \
    "${SPACETERM_CAUSAL[@]/$TEMP_ROOT\/spaceterm-trace-provisional.tsv/$TEMP_ROOT\/ghostty-trace-provisional.tsv}" \
    "${closure_args[@]}" --output "$TEMP_ROOT/cross-trace.tsv"
expect_failure "missing exit receipt" "$SCRIPT_DIRECTORY/freeze-performance-run.sh" \
    --run-intent "$SPACETERM_INTENT" --subject-identity "$SPACETERM_SUBJECT" \
    "${SPACETERM_CAUSAL[@]/$TEMP_ROOT\/spaceterm-exit.tsv/$TEMP_ROOT\/missing-exit.tsv}" \
    "${closure_args[@]}" --output "$TEMP_ROOT/missing-exit-run.tsv"

BAD_PROVISIONAL="$TEMP_ROOT/bad-provisional.tsv"
sed 's/failure.action.enabled\tfalse/failure.action.enabled\ttrue/' "$PROVISIONAL" > "$BAD_PROVISIONAL"
chmod 0400 "$BAD_PROVISIONAL"
expect_failure "enabled SpaceTerm failure controller" "$SCRIPT_DIRECTORY/freeze-performance-run-intent.sh" \
    --subject spaceterm --subject-identity "$SPACETERM_SUBJECT" "${intent_args[@]}" \
    --native-provisional-observation "$BAD_PROVISIONAL" --output "$TEMP_ROOT/enabled-intent.tsv"

echo "performance run metadata v3 tests passed"
