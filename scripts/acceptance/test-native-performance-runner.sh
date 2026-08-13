#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
umask 077

SCRIPT_DIRECTORY="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIRECTORY
readonly BUILDER="$SCRIPT_DIRECTORY/build-native-performance-tools.sh"
readonly RUNNER="$SCRIPT_DIRECTORY/run-native-performance-scenario.sh"
readonly PROCESS_GROUP_RUNNER="$SCRIPT_DIRECTORY/run-performance-process-group.py"
TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-native-perf-runner.XXXXXX")"
GROUP_LEADER=""

cleanup() {
    if [[ -n "$GROUP_LEADER" ]]; then
        kill -TERM -- "-$GROUP_LEADER" 2>/dev/null || true
        kill -TERM "$GROUP_LEADER" 2>/dev/null || true
        wait "$GROUP_LEADER" 2>/dev/null || true
    fi
    rm -rf -- "$TEMP_ROOT"
}
trap cleanup EXIT INT TERM HUP

fail() { echo "test failure: $*" >&2; exit 1; }
sha256() { shasum -a 256 "$1" | awk '{ print $1 }'; }

expect_failure() {
    local label="$1"
    shift
    if "$@" >"$TEMP_ROOT/failure.stdout" 2>"$TEMP_ROOT/failure.stderr"; then
        fail "$label unexpectedly succeeded"
    fi
}

for command in awk bash chmod cp kill mkfifo mktemp ps rm sed shasum; do
    command -v "$command" >/dev/null 2>&1 || fail "missing command: $command"
done

GROUP_WITNESS="$TEMP_ROOT/group-child.pid"
GROUP_FIXTURE="$TEMP_ROOT/group-fixture.sh"
write_group_fixture() {
    # shellcheck disable=SC2016 # The fixture must expand these in its own process.
    printf '%s\n' '#!/bin/bash' 'sleep 60 &' 'printf '\''%s\n'\'' "$!" > "$1"' 'wait' > "$GROUP_FIXTURE"
    chmod 0500 "$GROUP_FIXTURE"
}
write_group_fixture
python3 "$PROCESS_GROUP_RUNNER" "$GROUP_FIXTURE" "$GROUP_WITNESS" &
group_leader=$!
GROUP_LEADER="$group_leader"
for _ in {1..500}; do
    [[ -s "$GROUP_WITNESS" ]] && break
    sleep 0.01
done
[[ -s "$GROUP_WITNESS" ]] || fail "process-group child witness was not published"
group_child="$(cat "$GROUP_WITNESS")"
[[ "$(ps -o pgid= -p "$group_leader" | tr -d '[:space:]')" == "$group_leader" \
    && "$(ps -o pgid= -p "$group_child" | tr -d '[:space:]')" == "$group_leader" ]] \
    || fail "child tree did not share the dedicated process group"
python3 - "$group_leader" <<'PY'
import os
import signal
import sys
os.killpg(int(sys.argv[1]), signal.SIGTERM)
PY
wait "$group_leader" 2>/dev/null || true
GROUP_LEADER=""
for _ in {1..100}; do
    group_child_state="$(ps -o state= -p "$group_child" 2>/dev/null | tr -d '[:space:]' || true)"
    [[ -z "$group_child_state" || "$group_child_state" == Z* ]] && break
    sleep 0.01
done
[[ -z "$group_child_state" || "$group_child_state" == Z* ]] \
    || fail "process-group grandchild survived termination"

# The production recorder captures D+3 seconds. Launching exactly two seconds
# before the authenticated measurement boundary leaves one second of startup
# tolerance while preserving the required bounded pre/post-roll envelope.
fake_measurement_start=10000000000
fake_duration_ms=10000
fake_trace_start=$((fake_measurement_start - 2000000000))
fake_workload_end=$((fake_measurement_start + fake_duration_ms * 1000000))
fake_trace_start_delay=750000000
fake_capture_start=$((fake_trace_start + fake_trace_start_delay))
fake_trace_end=$((fake_capture_start + (fake_duration_ms + 3000) * 1000000))
(( fake_measurement_start - fake_trace_start == 2000000000 \
    && fake_measurement_start - fake_capture_start == 1250000000 \
    && fake_trace_end - fake_workload_end == 1750000000 )) \
    || fail "normal trace timing does not provide the exact bounded envelope"

RUN_DIR="$TEMP_ROOT/i43-test-run"
mkdir -p "$RUN_DIR"
"$BUILDER" --run-directory "$RUN_DIR" \
    --output-directory "$RUN_DIR/tools" --architecture "$(uname -m)" >/dev/null
TOOLS="$RUN_DIR/tools"
METADATA="$TOOLS/native-performance-tools.tsv"
[[ -f "$METADATA" && ! -w "$METADATA" ]] || fail "tools metadata is not immutable"
for tool in performance-workload performance-driver performance-rss-sampler \
    performance-window-resolver; do
    [[ -x "$TOOLS/$tool" && ! -L "$TOOLS/$tool" ]] || fail "missing native tool: $tool"
    "$TOOLS/$tool" --help >/dev/null
done

expect_failure "builder reused output" "$BUILDER" --run-directory "$RUN_DIR" \
    --output-directory "$RUN_DIR/tools" --architecture "$(uname -m)"
mkdir -p "$TEMP_ROOT/outside"
expect_failure "builder escaped run" "$BUILDER" --run-directory "$RUN_DIR" \
    --output-directory "$TEMP_ROOT/outside/tools" --architecture "$(uname -m)"

ALTERED_TOOLS="$RUN_DIR/altered-tools"
cp -R "$TOOLS" "$ALTERED_TOOLS"
chmod 0700 "$ALTERED_TOOLS/performance-workload"
printf 'changed\n' >> "$ALTERED_TOOLS/performance-workload"
chmod 0500 "$ALTERED_TOOLS/performance-workload"

write_file() {
    local path="$1"
    shift
    printf '%s' "$*" > "$path"
    chmod 0400 "$path"
}

SUBJECT="$RUN_DIR/subject.tsv"
WINDOW="$RUN_DIR/window.tsv"
PLAN="$RUN_DIR/plan.tsv"
PLAN_METADATA="$RUN_DIR/plan-metadata.tsv"
RUN_INTENT="$RUN_DIR/run-intent.tsv"
NATIVE_PROVISIONAL="$RUN_DIR/native-observation-live.tsv"
readonly HASH_A=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
readonly HASH_B=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
write_file "$SUBJECT" "$(cat <<EOF
format_version	1
subject	spaceterm
app_bundle_path	/Applications/Fake.app
bundle_identifier	org.example.fake
bundle_version	1+1
executable_path	/Applications/Fake.app/Contents/MacOS/Fake
executable_sha256	$HASH_A
executable_device	1
executable_inode	2
executable_fsid	1
signature_valid	true
signing_identifier	org.example.fake
team_identifier	none
cdhash	12345678
process_pid	999999
process_start_identity	1:2
identity_status	frozen
EOF
)"
write_file "$PLAN" "$(cat <<'EOF'
event_id	offset_ms	action	arg0	arg1
seed-checkpoint	0	checkpoint	10000	0
stop	10000	stop	0	0
EOF
)"
PLAN_HASH="$(sha256 "$PLAN")"
write_file "$PLAN_METADATA" "$(cat <<EOF
format_version	1
scenario	resize
plan_sha256	$PLAN_HASH
input_schedule_sha256	$HASH_B
warmup_ms	0
measured_duration_ms	10000
input_interval_ms	10000
required_seed_rows	10000
required_resize_cycles	0
geometry_authority	producer-tiocgwinsz
native_resize_arguments	window-pixel-deltas-not-grid-claims
EOF
)"
SUBJECT_HASH="$(sha256 "$SUBJECT")"
write_file "$WINDOW" "$(cat <<EOF
format_version	1
subject_identity_sha256	$SUBJECT_HASH
subject	spaceterm
process_pid	999999
process_start_identity	1:2
bundle_identifier	org.example.fake
executable_sha256	$HASH_A
window_number	42
window_owner_pid_verified	true
window_layer	0
window_onscreen	true
window_minimized	false
window_x	0.000
window_y	0.000
window_width	800.000
window_height	600.000
resolved_continuous_ns	1
selector_kind	unique
status	frozen
EOF
)"
WORKLOAD_HASH="$(sha256 "$TOOLS/performance-workload")"
write_file "$NATIVE_PROVISIONAL" "$(cat <<'EOF'
schema	spaceterm.acceptance.native-launch-proof/v5
observation.source	production-app
launch.nonce	dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd
run.id	i43-test
package.app.sha256	aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
runtime.schema	spaceterm.acceptance.runtime-stream/v1
runtime.sample_interval_ms	1000
runtime.transition_capacity	64
failure.action.schema	spaceterm.acceptance.failure-action/v1
failure.action.enabled	false
process.pid	999999
process.pidversion	1
process.executable.path	/Applications/Fake.app/Contents/MacOS/Fake
process.executable.device	1
process.executable.inode	2
process.executable.fsid	1
process.signature.cdhash	12345678
process.signature.identifier	org.example.fake
process.signature.team_identifier	none
terminal_font_selected	Test Font
initial_grid.rows	24
initial_grid.columns	80
initial_grid.logical_width	800
initial_grid.logical_height	600
initial_grid.backing_pixel_width	800
initial_grid.backing_pixel_height	600
observation.complete	true
EOF
)"
write_file "$RUN_INTENT" "$(cat <<EOF
format_version	1
subject	spaceterm
subject_identity_sha256	$SUBJECT_HASH
scenario	resize
scenario_plan_sha256	$PLAN_HASH
workload_sha256	$WORKLOAD_HASH
command_sha256	$HASH_B
environment_sha256	$HASH_B
font_sha256	$HASH_B
initial_grid_sha256	$HASH_B
measured_duration_ms	10000
process_pid	999999
process_start_identity	1:2
campaign_id	i43-test
session_id	spaceterm-resize-01
nonce	cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
native_provisional_observation_sha256	$(sha256 "$NATIVE_PROVISIONAL")
status	prepared
EOF
)"

TRACE="$SCRIPT_DIRECTORY/../record-release-performance-trace.sh"
SECRET="$RUN_DIR/campaign-secret.bin"
printf '0123456789abcdef0123456789abcdef' > "$SECRET"
chmod 0400 "$SECRET"
readonly CAMPAIGN_ID=i43-test
readonly SESSION_ID=spaceterm-resize-01
readonly NONCE=cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc

run_runner() {
    local tools="$1" suffix="$2" window="${3:-$WINDOW}"
    local quit_control="$RUN_DIR/quit-$suffix.fifo"
    [[ -e "$quit_control" ]] || mkfifo -m 600 "$quit_control"
    "$RUNNER" --run-directory "$RUN_DIR" --tools-directory "$tools" \
        --subject-identity "$SUBJECT" --window-identity "$window" \
        --scenario resize --scenario-plan "$PLAN" --plan-metadata "$PLAN_METADATA" \
        --run-intent "$RUN_INTENT" --run-metadata "$RUN_DIR/run-$suffix.tsv" \
        --workload-events "$RUN_DIR/events-$suffix.tsv" \
        --workload-metadata "$RUN_DIR/workload-$suffix.tsv" \
        --workload-ready-receipt "$RUN_DIR/ready-$suffix.tsv" \
        --plan-start-gate "$RUN_DIR/plan-start-$suffix.tsv" \
        --driver-output "$RUN_DIR/driver-$suffix.tsv" \
        --driver-intent "$RUN_DIR/driver-intent-$suffix.tsv" \
        --driver-receipt "$RUN_DIR/driver-receipt-$suffix.tsv" \
        --rss-output "$RUN_DIR/rss-$suffix.tsv" \
        --trace-output-directory "$RUN_DIR/trace-$suffix" \
        --trace-provisional-receipt "$RUN_DIR/trace-provisional-$suffix.tsv" \
        --performance-tail-receipt "$RUN_DIR/tail-$suffix.tsv" \
        --performance-quit-control "$quit_control" \
        --performance-quit-receipt "$RUN_DIR/quit-receipt-$suffix.tsv" \
        --subject-exit-receipt "$RUN_DIR/exit-$suffix.tsv" \
        --native-provisional-observation "$NATIVE_PROVISIONAL" \
        --native-observation "$RUN_DIR/native-$suffix.tsv" \
        --native-runtime-metadata "$RUN_DIR/runtime-metadata-$suffix.tsv" \
        --native-runtime-samples "$RUN_DIR/runtime-samples-$suffix.tsv" \
        --native-runtime-events "$RUN_DIR/runtime-events-$suffix.tsv" \
        --native-failure-actions "$RUN_DIR/failure-actions-$suffix.tsv" \
        --result-output "$RUN_DIR/result-$suffix.tsv" --trace-recorder "$TRACE" \
        --campaign-secret-file "$SECRET" --campaign-id "$CAMPAIGN_ID" \
        --session-id "$SESSION_ID" --nonce "$NONCE" \
        --seed-timeout-seconds 1
}

SPACETERM_PERFORMANCE_TEST_MODE=1 SPACETERM_PERFORMANCE_TAIL_MS=0 \
    expect_failure "wrong native binary hash" \
    run_runner "$ALTERED_TOOLS" wrong-binary

BAD_SOURCE_TOOLS="$RUN_DIR/bad-source-tools"
cp -R "$TOOLS" "$BAD_SOURCE_TOOLS"
chmod 0600 "$BAD_SOURCE_TOOLS/native-performance-tools.tsv"
sed -i '' 's/^performance_workload_source_sha256.*/performance_workload_source_sha256\tffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff/' \
    "$BAD_SOURCE_TOOLS/native-performance-tools.tsv"
chmod 0400 "$BAD_SOURCE_TOOLS/native-performance-tools.tsv"
SPACETERM_PERFORMANCE_TEST_MODE=1 SPACETERM_PERFORMANCE_TAIL_MS=0 \
    expect_failure "source hash changed after build" \
    run_runner "$BAD_SOURCE_TOOLS" source-hash

BAD_COMPILER_TOOLS="$RUN_DIR/bad-compiler-tools"
cp -R "$TOOLS" "$BAD_COMPILER_TOOLS"
chmod 0600 "$BAD_COMPILER_TOOLS/native-performance-tools.tsv"
sed -i '' 's/^compiler_sha256.*/compiler_sha256\tffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff/' \
    "$BAD_COMPILER_TOOLS/native-performance-tools.tsv"
chmod 0400 "$BAD_COMPILER_TOOLS/native-performance-tools.tsv"
SPACETERM_PERFORMANCE_TEST_MODE=1 SPACETERM_PERFORMANCE_TAIL_MS=0 \
    expect_failure "wrong compiler hash" \
    run_runner "$BAD_COMPILER_TOOLS" compiler-hash

BAD_WINDOW="$RUN_DIR/bad-window.tsv"
sed 's/process_pid	999999/process_pid	888888/' "$WINDOW" > "$BAD_WINDOW"
chmod 0400 "$BAD_WINDOW"
SPACETERM_PERFORMANCE_TEST_MODE=1 SPACETERM_PERFORMANCE_TAIL_MS=0 \
    expect_failure "wrong window owner PID" run_runner "$TOOLS" window "$BAD_WINDOW"

BAD_START="$RUN_DIR/bad-start.tsv"
sed 's/process_start_identity	1:2/process_start_identity	1:3/' "$WINDOW" > "$BAD_START"
chmod 0400 "$BAD_START"
SPACETERM_PERFORMANCE_TEST_MODE=1 SPACETERM_PERFORMANCE_TAIL_MS=0 \
    expect_failure "stale process generation" run_runner "$TOOLS" start "$BAD_START"

SPACETERM_PERFORMANCE_TEST_MODE=1 SPACETERM_PERFORMANCE_TAIL_MS=0 \
    expect_failure "seed timeout produces no orphans" run_runner "$TOOLS" timeout
[[ -z "$(jobs -pr)" ]] || fail "orphaned background job after failure"
[[ "$(awk -F '\t' '$1 == "status" {print $2}' "$RUN_DIR/result-timeout.tsv")" == incomplete ]] \
    || fail "timeout result is not incomplete"
[[ "$(awk -F '\t' '$1 == "result_reason" {print $2}' "$RUN_DIR/result-timeout.tsv")" == workload-ready-receipt-timeout ]] \
    || fail "timeout result did not reach authenticated readiness wait"

echo "native performance runner tests passed"
