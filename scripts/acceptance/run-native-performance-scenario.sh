#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
umask 077

RUN_DIRECTORY=""
TOOLS_DIRECTORY=""
SUBJECT_IDENTITY=""
WINDOW_IDENTITY=""
SCENARIO=""
SCENARIO_PLAN=""
PLAN_METADATA=""
RUN_METADATA=""
WORKLOAD_EVENTS=""
WORKLOAD_METADATA=""
WORKLOAD_READY_RECEIPT=""
WORKLOAD_AUTH_VERIFIER=""
PLAN_START_GATE=""
DRIVER_OUTPUT=""
RSS_OUTPUT=""
TRACE_OUTPUT_DIRECTORY=""
RESULT_OUTPUT=""
TRACE_RECORDER=""
CAMPAIGN_SECRET_FILE=""
CAMPAIGN_ID=""
SESSION_ID=""
NONCE=""
SEED_TIMEOUT_SECONDS=300

DRIVER_PID=""
RSS_PID=""
TRACE_PID=""
DRIVER_PGID=""
RSS_PGID=""
TRACE_PGID=""
DRIVER_STATUS=-1
RSS_STATUS=-1
TRACE_STATUS=-1
SEED_CONTINUOUS_NS=0
MEASUREMENT_READY_CONTINUOUS_NS=0
PLAN_START_CONTINUOUS_NS=0
CHILDREN_STARTED_CONTINUOUS_NS=0
TRACE_BOUNDARY_CONTINUOUS_NS=0
TRACE_INVOKED_CONTINUOUS_NS=0
PRODUCER_ENDED_CONTINUOUS_NS=0
TAIL_VERIFIED_CONTINUOUS_NS=0
SAVED_TERMIOS=""
TEMP_WINDOW_PROOF=""
FINALIZED=false
EXIT_TRAP_ACTIVE=false
SPAWNED_PID=""
CONTROLLER_SHA256=unavailable
PROCESS_GROUP_RUNNER_SHA256=unavailable
TRACE_RECORDER_SHA256=unavailable
TRACE_INSPECTOR_SHA256=unavailable
TRACE_VERIFIER_SHA256=unavailable
TRACE_COMMAND_RUNNER_SHA256=unavailable
WORKLOAD_READY_VERIFIER_SHA256=unavailable
WORKLOAD_AUTH_VERIFIER_SHA256=unavailable

usage() {
    cat <<EOF
Usage: $(basename -- "$0") --run-directory ABSOLUTE_DIRECTORY \\
  --tools-directory DIRECTORY --subject-identity FILE --window-identity FILE \\
  --scenario NAME --scenario-plan FILE --plan-metadata FILE --run-metadata FILE \\
  --workload-events ABSENT_OR_ACTIVE_FILE --workload-metadata ABSENT_FILE \\
  --workload-ready-receipt ABSENT_FILE --plan-start-gate ABSENT_FILE \\
  --driver-output ABSENT_FILE --rss-output ABSENT_FILE \\
  --trace-output-directory ABSENT_DIRECTORY --result-output ABSENT_FILE \\
  --trace-recorder FILE --campaign-secret-file PRIVATE_FILE \\
  --campaign-id LABEL --session-id LABEL --nonce SHA256 \\
  [--seed-timeout-seconds N]

Wait for the native producer's seed-complete event, authenticate it against the
frozen run and run-built workload, start the native plan driver and RSS sampler,
invoke trace capture at the warm-up boundary, then require the terminal app to
remain authenticated for a five-second post-producer tail. Every child is
trapped and reaped; any available controlling-terminal state is restored.

The trace recorder must implement the v3 subject-identity/run-metadata CLI.
Older recorder interfaces are rejected until their stack lane is rebased.
EOF
}

die() { echo "error: $*" >&2; exit 1; }
sha256() { shasum -a 256 "$1" | awk '{ print $1 }'; }
kv() {
    awk -F '\t' -v wanted="$2" '
        NF != 2 { bad = 1 }
        $1 == wanted { count += 1; value = $2 }
        END { if (!bad && count == 1) print value }
    ' "$1"
}
is_uint() { [[ "$1" =~ ^[0-9]+$ ]]; }
is_positive_uint() { [[ "$1" =~ ^[1-9][0-9]*$ ]]; }
is_hash() { [[ "$1" =~ ^[0-9a-f]{64}$ ]]; }
is_label() { [[ "$1" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]]; }

continuous_ns() {
    python3 - <<'PY'
import ctypes

class Timebase(ctypes.Structure):
    _fields_ = [("numer", ctypes.c_uint32), ("denom", ctypes.c_uint32)]

system = ctypes.CDLL("/usr/lib/libSystem.B.dylib")
system.mach_continuous_time.restype = ctypes.c_uint64
timebase = Timebase()
if system.mach_timebase_info(ctypes.byref(timebase)) != 0 or timebase.denom == 0:
    raise SystemExit(1)
print(system.mach_continuous_time() * timebase.numer // timebase.denom)
PY
}

sleep_until() {
    local deadline="$1" now remaining sleep_seconds
    while :; do
        now="$(continuous_ns)"
        (( now >= deadline )) && return 0
        remaining=$((deadline - now))
        if (( remaining > 100000000 )); then
            sleep_seconds=0.05
        else
            sleep_seconds=0.005
        fi
        sleep "$sleep_seconds"
    done
}

path_is_within_run() {
    local path="$1" parent
    [[ "$path" == /* && "$path" != *$'\n'* && "$path" != *$'\t'* ]] || return 1
    parent="$(dirname -- "$path")"
    [[ -d "$parent" && ! -L "$parent" ]] || return 1
    parent="$(realpath "$parent")"
    [[ "$parent" == "$RUN_DIRECTORY" || "$parent" == "$RUN_DIRECTORY/"* ]]
}

require_immutable_file() {
    local path="$1" label="$2" mode
    [[ -f "$path" && ! -L "$path" ]] || die "$label must be a non-symlink regular file"
    mode="$(stat -f '%Lp' "$path")"
    (( (8#$mode & 0222) == 0 )) || die "$label must be immutable"
}

require_absent_output() {
    local path="$1" label="$2"
    path_is_within_run "$path" || die "$label must be an absolute run-owned path"
    [[ ! -e "$path" && ! -L "$path" ]] || die "$label already exists"
}

process_group_exists() {
    local pgid="$1"
    [[ -n "$pgid" ]] && kill -0 -- "-$pgid" 2>/dev/null
}

process_has_exited() {
    local pid="$1" state
    state="$(ps -o state= -p "$pid" 2>/dev/null | tr -d '[:space:]' || true)"
    [[ -z "$state" || "$state" == Z* ]]
}

terminate_process_group() {
    local pgid="$1" leader_pid="$2" deadline now
    [[ -n "$pgid" ]] || return 0
    kill -TERM -- "-$pgid" 2>/dev/null || true
    deadline=$(( $(continuous_ns 2>/dev/null || printf '0') + 2000000000 ))
    while process_group_exists "$pgid"; do
        now="$(continuous_ns 2>/dev/null || printf '%s' "$deadline")"
        (( now < deadline )) || break
        sleep 0.02
    done
    if process_group_exists "$pgid"; then
        kill -KILL -- "-$pgid" 2>/dev/null || true
    fi
    [[ -z "$leader_pid" ]] || wait "$leader_pid" 2>/dev/null || true
}

cleanup_children() {
    terminate_process_group "$TRACE_PGID" "$TRACE_PID"
    terminate_process_group "$RSS_PGID" "$RSS_PID"
    terminate_process_group "$DRIVER_PGID" "$DRIVER_PID"
    TRACE_PID=""; TRACE_PGID=""
    RSS_PID=""; RSS_PGID=""
    DRIVER_PID=""; DRIVER_PGID=""
}

restore_termios() {
    [[ -z "$SAVED_TERMIOS" ]] || stty "$SAVED_TERMIOS" 2>/dev/null || true
}

cleanup_temp() {
    if [[ -n "$TEMP_WINDOW_PROOF" && -f "$TEMP_WINDOW_PROOF" ]]; then
        rm -f -- "$TEMP_WINDOW_PROOF"
    fi
}

on_signal() {
    EXIT_TRAP_ACTIVE=false
    trap - EXIT INT TERM HUP
    cleanup_children
    cleanup_temp
    restore_termios
    publish_result incomplete interrupted || true
    FINALIZED=true
    exit 130
}

on_exit() {
    local status="$1"
    [[ "$EXIT_TRAP_ACTIVE" == true && "$FINALIZED" == false ]] || return 0
    EXIT_TRAP_ACTIVE=false
    trap - EXIT INT TERM HUP
    cleanup_children
    cleanup_temp
    restore_termios
    publish_result incomplete unexpected-controller-exit || true
    exit "$status"
}

publish_result() {
    local status="$1" reason="$2" temporary test_mode=false
    [[ -n "$RESULT_OUTPUT" && ! -e "$RESULT_OUTPUT" ]] || return 1
    [[ "${SPACETERM_PERFORMANCE_TEST_MODE:-0}" != 1 ]] || test_mode=true
    [[ "$status" != complete || "$test_mode" == false ]] || status=test-only
    temporary="${RESULT_OUTPUT}.tmp.$$"
    {
        printf 'format_version\t1\n'
        printf 'scenario\t%s\n' "$SCENARIO"
        printf 'subject_identity_sha256\t%s\n' "$(sha256 "$SUBJECT_IDENTITY" 2>/dev/null || printf unavailable)"
        printf 'window_identity_sha256\t%s\n' "$(sha256 "$WINDOW_IDENTITY" 2>/dev/null || printf unavailable)"
        printf 'run_metadata_sha256\t%s\n' "$(sha256 "$RUN_METADATA" 2>/dev/null || printf unavailable)"
        printf 'scenario_plan_sha256\t%s\n' "$(sha256 "$SCENARIO_PLAN" 2>/dev/null || printf unavailable)"
        printf 'plan_start_gate_sha256\t%s\n' "$(sha256 "$PLAN_START_GATE" 2>/dev/null || printf unavailable)"
        printf 'workload_events_sha256\t%s\n' "$(sha256 "$WORKLOAD_EVENTS" 2>/dev/null || printf unavailable)"
        printf 'workload_metadata_sha256\t%s\n' "$(sha256 "$WORKLOAD_METADATA" 2>/dev/null || printf unavailable)"
        printf 'workload_ready_receipt_sha256\t%s\n' "$(sha256 "$WORKLOAD_READY_RECEIPT" 2>/dev/null || printf unavailable)"
        printf 'tools_metadata_sha256\t%s\n' \
            "$(sha256 "$TOOLS_DIRECTORY/native-performance-tools.tsv" 2>/dev/null || printf unavailable)"
        printf 'trace_recorder_sha256\t%s\n' "$TRACE_RECORDER_SHA256"
        printf 'trace_inspector_sha256\t%s\n' "$TRACE_INSPECTOR_SHA256"
        printf 'trace_verifier_sha256\t%s\n' "$TRACE_VERIFIER_SHA256"
        printf 'trace_command_runner_sha256\t%s\n' "$TRACE_COMMAND_RUNNER_SHA256"
        printf 'scenario_controller_sha256\t%s\n' "$CONTROLLER_SHA256"
        printf 'process_group_runner_sha256\t%s\n' "$PROCESS_GROUP_RUNNER_SHA256"
        printf 'workload_ready_verifier_sha256\t%s\n' "$WORKLOAD_READY_VERIFIER_SHA256"
        printf 'workload_auth_verifier_sha256\t%s\n' "$WORKLOAD_AUTH_VERIFIER_SHA256"
        printf 'driver_events_sha256\t%s\n' "$(sha256 "$DRIVER_OUTPUT" 2>/dev/null || printf unavailable)"
        printf 'rss_samples_sha256\t%s\n' "$(sha256 "$RSS_OUTPUT" 2>/dev/null || printf unavailable)"
        printf 'trace_metadata_sha256\t%s\n' "$(sha256 "${TRACE_OUTPUT_DIRECTORY:-/nonexistent}/$SUBJECT-$SCENARIO-trace-metadata.tsv" 2>/dev/null || printf unavailable)"
        printf 'campaign_id\t%s\n' "$CAMPAIGN_ID"
        printf 'session_id\t%s\n' "$SESSION_ID"
        printf 'nonce\t%s\n' "$NONCE"
        printf 'seed_complete_continuous_ns\t%s\n' "$SEED_CONTINUOUS_NS"
        printf 'measurement_ready_continuous_ns\t%s\n' "$MEASUREMENT_READY_CONTINUOUS_NS"
        printf 'plan_start_continuous_ns\t%s\n' "$PLAN_START_CONTINUOUS_NS"
        printf 'children_started_continuous_ns\t%s\n' "$CHILDREN_STARTED_CONTINUOUS_NS"
        printf 'trace_boundary_continuous_ns\t%s\n' "$TRACE_BOUNDARY_CONTINUOUS_NS"
        printf 'trace_invoked_continuous_ns\t%s\n' "$TRACE_INVOKED_CONTINUOUS_NS"
        printf 'producer_ended_continuous_ns\t%s\n' "$PRODUCER_ENDED_CONTINUOUS_NS"
        printf 'tail_verified_continuous_ns\t%s\n' "$TAIL_VERIFIED_CONTINUOUS_NS"
        printf 'driver_exit_status\t%s\n' "$DRIVER_STATUS"
        printf 'rss_exit_status\t%s\n' "$RSS_STATUS"
        printf 'trace_exit_status\t%s\n' "$TRACE_STATUS"
        printf 'trace_protocol\t%s\n' 'subject-identity-v3'
        printf 'post_producer_tail_ms\t%s\n' "${SPACETERM_PERFORMANCE_TAIL_MS:-5000}"
        printf 'test_overrides_active\t%s\n' "$test_mode"
        printf 'result_reason\t%s\n' "$reason"
        printf 'status\t%s\n' "$status"
    } > "$temporary"
    chmod 0400 "$temporary"
    ln "$temporary" "$RESULT_OUTPUT" || {
        rm -f -- "$temporary"
        return 1
    }
    rm -f -- "$temporary"
}

abort_run() {
    local reason="$1"
    cleanup_children
    cleanup_temp
    restore_termios
    publish_result incomplete "$reason" || true
    FINALIZED=true
    die "$reason"
}

spawn_process_group() {
    local pid pgid attempts=0
    SPAWNED_PID=""
    python3 "$PROCESS_GROUP_RUNNER" "$@" &
    pid=$!
    while (( attempts < 100 )); do
        pgid="$(ps -o pgid= -p "$pid" 2>/dev/null | tr -d '[:space:]')"
        if [[ "$pgid" == "$pid" ]]; then
            SPAWNED_PID="$pid"
            return 0
        fi
        kill -0 "$pid" 2>/dev/null || break
        attempts=$((attempts + 1))
        sleep 0.01
    done
    terminate_process_group "$pid" "$pid"
    return 1
}

verify_controller_toolchain() {
    [[ "$(sha256 "$SCRIPT_DIRECTORY/$(basename -- "$0")")" == "$CONTROLLER_SHA256" \
        && "$(sha256 "$PROCESS_GROUP_RUNNER")" == "$PROCESS_GROUP_RUNNER_SHA256" \
        && "$(sha256 "$TRACE_RECORDER")" == "$TRACE_RECORDER_SHA256" \
        && "$(sha256 "$TRACE_INSPECTOR")" == "$TRACE_INSPECTOR_SHA256" \
        && "$(sha256 "$TRACE_VERIFIER")" == "$TRACE_VERIFIER_SHA256" \
        && "$(sha256 "$TRACE_COMMAND_RUNNER")" == "$TRACE_COMMAND_RUNNER_SHA256" \
        && "$(sha256 "$WORKLOAD_READY_VERIFIER")" == "$WORKLOAD_READY_VERIFIER_SHA256" \
        && "$(sha256 "$WORKLOAD_AUTH_VERIFIER")" == "$WORKLOAD_AUTH_VERIFIER_SHA256" ]]
}

workload_event_time() {
    local kind="$1"
    [[ -f "$WORKLOAD_EVENTS" && ! -L "$WORKLOAD_EVENTS" ]] || return 1
    [[ "$(tail -c 1 "$WORKLOAD_EVENTS" 2>/dev/null || true)" == "" ]] || return 1
    awk -F '\t' -v wanted="$kind" '
        NR == 1 {
            if ($0 != "sequence\tcontinuous_ns\tkind\tevent_id\tbyte_count\trows\tcolumns\tpixel_width\tpixel_height\tstatus") { bad = 1; exit }
            next
        }
        NF != 10 || $1 !~ /^[0-9]+$/ || $2 !~ /^[0-9]+$/ || $5 !~ /^[0-9]+$/ { bad = 1; exit }
        $1 + 0 != NR - 2 || (NR > 2 && $2 + 0 <= prior) { bad = 1; exit }
        { prior = $2 + 0 }
        $3 == wanted {
            count += 1; value = $2
            if ($10 != "ok" || (wanted == "seed-complete" && $5 + 0 <= 0)) {
                bad = 1; exit
            }
        }
        END { if (bad) exit 2; if (count == 1) print value; else exit 1 }
    ' "$WORKLOAD_EVENTS"
}

publish_plan_start_gate() {
    local secret_before secret_after ready_receipt_sha256
    secret_before="$(sha256 "$CAMPAIGN_SECRET_FILE")"
    ready_receipt_sha256="$(sha256 "$WORKLOAD_READY_RECEIPT")"
    python3 - "$PLAN_START_GATE" "$CAMPAIGN_SECRET_FILE" "$CAMPAIGN_ID" \
        "$SESSION_ID" "$NONCE" "$ready_receipt_sha256" "$PLAN_START_CONTINUOUS_NS" <<'PY'
import hashlib
import hmac
import os
import struct
import sys

target, secret_path, campaign, session, nonce, ready_hash, start = sys.argv[1:]
with open(secret_path, "rb") as source:
    secret = source.read()
unsigned = (
    "format_version\t1\n"
    f"campaign_id\t{campaign}\n"
    f"session_id\t{session}\n"
    f"nonce\t{nonce}\n"
    f"ready_receipt_sha256\t{ready_hash}\n"
    f"plan_start_continuous_ns\t{start}\n"
).encode("ascii")
authenticated = (
    b"spaceterm.performance.plan-start-gate/v1\0"
    + struct.pack(">Q", len(unsigned))
    + unsigned
)
signature = hmac.new(secret, authenticated, hashlib.sha256).hexdigest()
contents = unsigned + f"start_gate_hmac_sha256\t{signature}\n".encode("ascii")
descriptor = os.open(
    target, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o400
)
try:
    if os.write(descriptor, contents) != len(contents):
        raise OSError("short gate write")
    os.fsync(descriptor)
finally:
    os.close(descriptor)
PY
    secret_after="$(sha256 "$CAMPAIGN_SECRET_FILE")"
    [[ "$secret_before" == "$secret_after" ]] || abort_run campaign-secret-changed
    require_immutable_file "$PLAN_START_GATE" plan-start-gate
}

wait_for_authenticated_ready() {
    local deadline now
    deadline=$(( $(continuous_ns) + SEED_TIMEOUT_SECONDS * 1000000000 ))
    while [[ ! -f "$WORKLOAD_READY_RECEIPT" ]]; do
        [[ ! -L "$WORKLOAD_READY_RECEIPT" ]] || abort_run workload-ready-receipt-is-symlink
        now="$(continuous_ns)"
        (( now < deadline )) || abort_run workload-ready-receipt-timeout
        sleep 0.02
    done
    "$WORKLOAD_READY_VERIFIER" --ready-receipt "$WORKLOAD_READY_RECEIPT" \
        --events "$WORKLOAD_EVENTS" --subject-identity "$SUBJECT_IDENTITY" \
        --campaign-secret-file "$CAMPAIGN_SECRET_FILE" --campaign-id "$CAMPAIGN_ID" \
        --session-id "$SESSION_ID" --nonce "$NONCE" \
        || abort_run workload-ready-receipt-authentication-failed
    SEED_CONTINUOUS_NS="$(workload_event_time seed-complete)" \
        || abort_run authenticated-seed-complete-missing
    MEASUREMENT_READY_CONTINUOUS_NS="$(kv "$WORKLOAD_READY_RECEIPT" measurement_ready_continuous_ns)"
    [[ "$(workload_event_time measurement-ready)" == "$MEASUREMENT_READY_CONTINUOUS_NS" ]] \
        || abort_run authenticated-measurement-ready-mismatch
    now="$(continuous_ns)"
    (( now >= MEASUREMENT_READY_CONTINUOUS_NS \
        && now - MEASUREMENT_READY_CONTINUOUS_NS <= 2000000000 )) \
        || abort_run stale-authenticated-workload-ready
}

verify_window_proof() {
    local phase="$1" expected_hash expected_number expected_pid proof_hash proof_number proof_pid
    if [[ "$TEST_MODE" == 1 ]]; then
        return 0
    fi
    TEMP_WINDOW_PROOF="$(dirname -- "$RESULT_OUTPUT")/.window-proof-${phase}.$$.tsv"
    [[ ! -e "$TEMP_WINDOW_PROOF" ]] || abort_run window-proof-temporary-path-exists
    "$WINDOW_RESOLVER" --subject-identity "$SUBJECT_IDENTITY" \
        --window-number "$WINDOW_NUMBER" --output "$TEMP_WINDOW_PROOF" \
        || abort_run "window-${phase}-authentication-failed"
    expected_hash="$(sha256 "$SUBJECT_IDENTITY")"
    expected_number="$(kv "$WINDOW_IDENTITY" window_number)"
    expected_pid="$(kv "$SUBJECT_IDENTITY" process_pid)"
    proof_hash="$(kv "$TEMP_WINDOW_PROOF" subject_identity_sha256)"
    proof_number="$(kv "$TEMP_WINDOW_PROOF" window_number)"
    proof_pid="$(kv "$TEMP_WINDOW_PROOF" process_pid)"
    [[ "$proof_hash" == "$expected_hash" && "$proof_number" == "$expected_number" \
        && "$proof_pid" == "$expected_pid" && "$(kv "$TEMP_WINDOW_PROOF" status)" == frozen ]] \
        || abort_run "window-${phase}-proof-mismatch"
    rm -f -- "$TEMP_WINDOW_PROOF"
    TEMP_WINDOW_PROOF=""
}

wait_for_workload_completion() {
    local deadline now
    deadline=$(( $(continuous_ns) + 10000000000 ))
    while [[ ! -f "$WORKLOAD_METADATA" ]]; do
        now="$(continuous_ns)"
        (( now < deadline )) || abort_run workload-metadata-timeout
        sleep 0.05
    done
    [[ ! -L "$WORKLOAD_METADATA" ]] || abort_run workload-metadata-is-symlink
}

while (( $# > 0 )); do
    case "$1" in
        --run-directory) RUN_DIRECTORY="${2:-}"; shift ;;
        --tools-directory) TOOLS_DIRECTORY="${2:-}"; shift ;;
        --subject-identity) SUBJECT_IDENTITY="${2:-}"; shift ;;
        --window-identity) WINDOW_IDENTITY="${2:-}"; shift ;;
        --scenario) SCENARIO="${2:-}"; shift ;;
        --scenario-plan) SCENARIO_PLAN="${2:-}"; shift ;;
        --plan-metadata) PLAN_METADATA="${2:-}"; shift ;;
        --run-metadata) RUN_METADATA="${2:-}"; shift ;;
        --workload-events) WORKLOAD_EVENTS="${2:-}"; shift ;;
        --workload-metadata) WORKLOAD_METADATA="${2:-}"; shift ;;
        --workload-ready-receipt) WORKLOAD_READY_RECEIPT="${2:-}"; shift ;;
        --plan-start-gate) PLAN_START_GATE="${2:-}"; shift ;;
        --driver-output) DRIVER_OUTPUT="${2:-}"; shift ;;
        --rss-output) RSS_OUTPUT="${2:-}"; shift ;;
        --trace-output-directory) TRACE_OUTPUT_DIRECTORY="${2:-}"; shift ;;
        --result-output) RESULT_OUTPUT="${2:-}"; shift ;;
        --trace-recorder) TRACE_RECORDER="${2:-}"; shift ;;
        --campaign-secret-file) CAMPAIGN_SECRET_FILE="${2:-}"; shift ;;
        --campaign-id) CAMPAIGN_ID="${2:-}"; shift ;;
        --session-id) SESSION_ID="${2:-}"; shift ;;
        --nonce) NONCE="${2:-}"; shift ;;
        --seed-timeout-seconds) SEED_TIMEOUT_SECONDS="${2:-}"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument: $1" ;;
    esac
    shift
done

for command in awk chmod dirname kill ln ps python3 realpath rm shasum sleep stat stty tail tr; do
    command -v "$command" >/dev/null 2>&1 || die "required command not found: $command"
done
[[ "$RUN_DIRECTORY" == /* && -d "$RUN_DIRECTORY" && ! -L "$RUN_DIRECTORY" ]] \
    || die "--run-directory must be an absolute non-symlink directory"
RUN_DIRECTORY="$(realpath "$RUN_DIRECTORY")"
is_label "$SCENARIO" || die "invalid scenario"
is_label "$CAMPAIGN_ID" || die "invalid campaign ID"
is_label "$SESSION_ID" || die "invalid session ID"
is_hash "$NONCE" || die "invalid nonce"
is_positive_uint "$SEED_TIMEOUT_SECONDS" || die "invalid seed timeout"
[[ -d "$TOOLS_DIRECTORY" && ! -L "$TOOLS_DIRECTORY" ]] || die "tools directory is unavailable"
TOOLS_DIRECTORY="$(realpath "$TOOLS_DIRECTORY")"
[[ "$TOOLS_DIRECTORY" == "$RUN_DIRECTORY/"* ]] || die "tools directory is outside the run"
for input in "$SUBJECT_IDENTITY" "$WINDOW_IDENTITY" "$SCENARIO_PLAN" \
    "$PLAN_METADATA" "$RUN_METADATA" "$CAMPAIGN_SECRET_FILE"; do
    path_is_within_run "$input" || die "input is outside the run: $input"
done
require_immutable_file "$SUBJECT_IDENTITY" subject-identity
require_immutable_file "$WINDOW_IDENTITY" window-identity
require_immutable_file "$SCENARIO_PLAN" scenario-plan
require_immutable_file "$PLAN_METADATA" plan-metadata
require_immutable_file "$RUN_METADATA" run-metadata
require_immutable_file "$TOOLS_DIRECTORY/native-performance-tools.tsv" tools-metadata
[[ -f "$CAMPAIGN_SECRET_FILE" && ! -L "$CAMPAIGN_SECRET_FILE" ]] \
    || die "campaign secret must be a non-symlink regular file"
secret_mode="$(stat -f '%Lp' "$CAMPAIGN_SECRET_FILE")"
[[ "$secret_mode" =~ ^[0-7]{3,4}$ && $((8#$secret_mode & 077)) == 0 ]] \
    || die "campaign secret must be owner-private"
(( $(stat -f '%z' "$CAMPAIGN_SECRET_FILE") >= 32 )) \
    || die "campaign secret is too short"
for output in "$WORKLOAD_EVENTS" "$WORKLOAD_METADATA" "$WORKLOAD_READY_RECEIPT" \
    "$PLAN_START_GATE" "$DRIVER_OUTPUT" "$RSS_OUTPUT" \
    "$RESULT_OUTPUT"; do
    path_is_within_run "$output" || die "output is outside the run: $output"
done
[[ ! -e "$WORKLOAD_EVENTS" && ! -L "$WORKLOAD_EVENTS" ]] \
    || die "workload events must be absent before the controller starts"
[[ ! -e "$WORKLOAD_METADATA" && ! -L "$WORKLOAD_METADATA" ]] \
    || die "workload metadata must be absent before the controller starts"
require_absent_output "$WORKLOAD_READY_RECEIPT" workload-ready-receipt
require_absent_output "$PLAN_START_GATE" plan-start-gate
require_absent_output "$DRIVER_OUTPUT" driver-output
require_absent_output "$RSS_OUTPUT" rss-output
require_absent_output "$RESULT_OUTPUT" result-output
require_absent_output "$TRACE_OUTPUT_DIRECTORY" trace-output-directory
[[ -f "$TRACE_RECORDER" && -x "$TRACE_RECORDER" && ! -L "$TRACE_RECORDER" ]] \
    || die "trace recorder must be an executable non-symlink file"

if [[ -n "${SPACETERM_PERFORMANCE_TAIL_MS:-}" \
    || -n "${SPACETERM_PERFORMANCE_TEST_MODE:-}" ]]; then
    [[ "${SPACETERM_PERFORMANCE_TEST_MODE:-0}" == 1 \
        && "${SPACETERM_PERFORMANCE_TAIL_MS:-0}" =~ ^[0-9]+$ ]] \
        || die "performance test overrides require explicit test mode"
fi
readonly TEST_MODE="${SPACETERM_PERFORMANCE_TEST_MODE:-0}"
readonly TAIL_MS="${SPACETERM_PERFORMANCE_TAIL_MS:-5000}"

readonly TOOLS_METADATA="$TOOLS_DIRECTORY/native-performance-tools.tsv"
readonly WORKLOAD_BINARY="$TOOLS_DIRECTORY/performance-workload"
readonly DRIVER_BINARY="$TOOLS_DIRECTORY/performance-driver"
readonly RSS_BINARY="$TOOLS_DIRECTORY/performance-rss-sampler"
readonly WINDOW_RESOLVER="$TOOLS_DIRECTORY/performance-window-resolver"
for binary in "$WORKLOAD_BINARY" "$DRIVER_BINARY" "$RSS_BINARY" "$WINDOW_RESOLVER"; do
    [[ -f "$binary" && -x "$binary" && ! -L "$binary" ]] \
        || die "run-built native tool is unavailable: $binary"
done

SCRIPT_DIRECTORY="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIRECTORY
readonly PROCESS_GROUP_RUNNER="$SCRIPT_DIRECTORY/run-performance-process-group.py"
[[ -f "$PROCESS_GROUP_RUNNER" && -x "$PROCESS_GROUP_RUNNER" && ! -L "$PROCESS_GROUP_RUNNER" ]] \
    || die "process-group runner is unavailable"
readonly WORKLOAD_READY_VERIFIER="$SCRIPT_DIRECTORY/verify-performance-workload-ready.py"
[[ -f "$WORKLOAD_READY_VERIFIER" && -x "$WORKLOAD_READY_VERIFIER" \
    && ! -L "$WORKLOAD_READY_VERIFIER" ]] \
    || die "workload readiness verifier is unavailable"
WORKLOAD_AUTH_VERIFIER="$SCRIPT_DIRECTORY/verify-performance-workload-auth.py"
readonly WORKLOAD_AUTH_VERIFIER
[[ -f "$WORKLOAD_AUTH_VERIFIER" && -x "$WORKLOAD_AUTH_VERIFIER" \
    && ! -L "$WORKLOAD_AUTH_VERIFIER" ]] \
    || die "workload authentication verifier is unavailable"
TRACE_RECORDER="$(realpath "$TRACE_RECORDER")"
readonly TRACE_RECORDER
CANONICAL_TRACE_RECORDER="$(realpath "$SCRIPT_DIRECTORY/../record-release-performance-trace.sh")"
readonly CANONICAL_TRACE_RECORDER
[[ "$TRACE_RECORDER" == "$CANONICAL_TRACE_RECORDER" ]] \
    || die "trace recorder is not the canonical frozen repository driver"
TRACE_INSPECTOR="$(realpath "$SCRIPT_DIRECTORY/../inspect-release-performance-process.py")"
TRACE_VERIFIER="$(realpath "$SCRIPT_DIRECTORY/../verify-release-performance-trace.py")"
TRACE_COMMAND_RUNNER="$(realpath "$SCRIPT_DIRECTORY/../run-release-performance-command.py")"
readonly TRACE_INSPECTOR TRACE_VERIFIER TRACE_COMMAND_RUNNER
for dependency in "$TRACE_INSPECTOR" "$TRACE_VERIFIER" "$TRACE_COMMAND_RUNNER"; do
    [[ -f "$dependency" && ! -L "$dependency" ]] \
        || die "trace recorder dependency is unavailable: $dependency"
done
CONTROLLER_SHA256="$(sha256 "$SCRIPT_DIRECTORY/$(basename -- "$0")")"
PROCESS_GROUP_RUNNER_SHA256="$(sha256 "$PROCESS_GROUP_RUNNER")"
TRACE_RECORDER_SHA256="$(sha256 "$TRACE_RECORDER")"
TRACE_INSPECTOR_SHA256="$(sha256 "$TRACE_INSPECTOR")"
TRACE_VERIFIER_SHA256="$(sha256 "$TRACE_VERIFIER")"
TRACE_COMMAND_RUNNER_SHA256="$(sha256 "$TRACE_COMMAND_RUNNER")"
WORKLOAD_READY_VERIFIER_SHA256="$(sha256 "$WORKLOAD_READY_VERIFIER")"
WORKLOAD_AUTH_VERIFIER_SHA256="$(sha256 "$WORKLOAD_AUTH_VERIFIER")"
readonly CONTROLLER_SHA256 PROCESS_GROUP_RUNNER_SHA256 TRACE_RECORDER_SHA256
readonly TRACE_INSPECTOR_SHA256 TRACE_VERIFIER_SHA256 TRACE_COMMAND_RUNNER_SHA256
readonly WORKLOAD_READY_VERIFIER_SHA256 WORKLOAD_AUTH_VERIFIER_SHA256
verify_controller_toolchain || die "controller toolchain changed during startup"
[[ "$(kv "$TOOLS_METADATA" format_version)" == 1 \
    && "$(kv "$TOOLS_METADATA" status)" == complete ]] || die "tools metadata is invalid"
declare -a tool_names=(performance_workload performance_driver performance_rss_sampler performance_window_resolver)
declare -a tool_sources=(performance-workload.c performance-driver.m performance-rss-sampler.m performance-window-resolver.m)
declare -a tool_binaries=(performance-workload performance-driver performance-rss-sampler performance-window-resolver)
for ((index = 0; index < ${#tool_names[@]}; index += 1)); do
    source_path="$SCRIPT_DIRECTORY/${tool_sources[index]}"
    binary_path="$TOOLS_DIRECTORY/${tool_binaries[index]}"
    [[ "$(kv "$TOOLS_METADATA" "${tool_names[index]}_source_sha256")" == "$(sha256 "$source_path")" \
        && "$(kv "$TOOLS_METADATA" "${tool_names[index]}_binary_sha256")" == "$(sha256 "$binary_path")" ]] \
        || die "run-built tool source or binary hash mismatch: ${tool_binaries[index]}"
done
compiler_path="$(kv "$TOOLS_METADATA" compiler_path)"
compiler_sha256="$(kv "$TOOLS_METADATA" compiler_sha256)"
[[ "$compiler_path" == /* && -f "$compiler_path" && -x "$compiler_path" \
    && ! -L "$compiler_path" && "$compiler_sha256" =~ ^[0-9a-f]{64}$ \
    && "$(sha256 "$compiler_path")" == "$compiler_sha256" ]] \
    || die "native tools compiler provenance is invalid"
[[ "$(kv "$TOOLS_METADATA" builder_sha256)" == \
    "$(sha256 "$SCRIPT_DIRECTORY/build-native-performance-tools.sh")" ]] \
    || die "tools builder hash mismatch"
if [[ "$TEST_MODE" != 1 ]]; then
    architecture="$(kv "$TOOLS_METADATA" architecture)"
    [[ "$architecture" == arm64 || "$architecture" == x86_64 ]] \
        || die "tools architecture is invalid"
    for binary in "$WORKLOAD_BINARY" "$DRIVER_BINARY" "$RSS_BINARY" "$WINDOW_RESOLVER"; do
        [[ "$(lipo -archs "$binary")" == "$architecture" ]] \
            || die "tool is not an exact-architecture Mach-O binary: $binary"
    done
fi

SUBJECT_HASH="$(sha256 "$SUBJECT_IDENTITY")"
PLAN_HASH="$(sha256 "$SCENARIO_PLAN")"
WORKLOAD_HASH="$(sha256 "$WORKLOAD_BINARY")"
SUBJECT="$(kv "$SUBJECT_IDENTITY" subject)"
SUBJECT_PID="$(kv "$SUBJECT_IDENTITY" process_pid)"
START_IDENTITY="$(kv "$SUBJECT_IDENTITY" process_start_identity)"
EXECUTABLE="$(kv "$SUBJECT_IDENTITY" executable_path)"
EXECUTABLE_SHA256="$(kv "$SUBJECT_IDENTITY" executable_sha256)"
APP_BUNDLE="$(kv "$SUBJECT_IDENTITY" app_bundle_path)"
BUNDLE_IDENTIFIER="$(kv "$SUBJECT_IDENTITY" bundle_identifier)"
SIGNING_IDENTIFIER="$(kv "$SUBJECT_IDENTITY" signing_identifier)"
TEAM_IDENTIFIER="$(kv "$SUBJECT_IDENTITY" team_identifier)"
CDHASH="$(kv "$SUBJECT_IDENTITY" cdhash)"
WINDOW_NUMBER="$(kv "$WINDOW_IDENTITY" window_number)"
WARMUP_MS="$(kv "$PLAN_METADATA" warmup_ms)"
DURATION_MS="$(kv "$PLAN_METADATA" measured_duration_ms)"
readonly SUBJECT_HASH PLAN_HASH WORKLOAD_HASH SUBJECT SUBJECT_PID START_IDENTITY
readonly EXECUTABLE EXECUTABLE_SHA256 APP_BUNDLE BUNDLE_IDENTIFIER
readonly SIGNING_IDENTIFIER TEAM_IDENTIFIER CDHASH WINDOW_NUMBER WARMUP_MS DURATION_MS
[[ "$SUBJECT" == spaceterm || "$SUBJECT" == ghostty ]] || die "subject identity is invalid"
is_positive_uint "$SUBJECT_PID" || die "subject PID is invalid"
is_positive_uint "$WINDOW_NUMBER" || die "window number is invalid"
if ! is_uint "$WARMUP_MS" || ! is_positive_uint "$DURATION_MS"; then
    die "plan timing metadata is invalid"
fi
(( DURATION_MS % 10000 == 0 )) || die "duration must be a 10-second multiple"
[[ "$(kv "$WINDOW_IDENTITY" format_version)" == 1 \
    && "$(kv "$WINDOW_IDENTITY" subject_identity_sha256)" == "$SUBJECT_HASH" \
    && "$(kv "$WINDOW_IDENTITY" process_pid)" == "$SUBJECT_PID" \
    && "$(kv "$WINDOW_IDENTITY" process_start_identity)" == "$START_IDENTITY" \
    && "$(kv "$WINDOW_IDENTITY" executable_sha256)" == "$EXECUTABLE_SHA256" \
    && "$(kv "$WINDOW_IDENTITY" window_owner_pid_verified)" == true \
    && "$(kv "$WINDOW_IDENTITY" window_layer)" == 0 \
    && "$(kv "$WINDOW_IDENTITY" window_onscreen)" == true \
    && "$(kv "$WINDOW_IDENTITY" window_minimized)" == false \
    && "$(kv "$WINDOW_IDENTITY" status)" == frozen ]] \
    || die "window identity does not bind the frozen subject"
[[ "$(kv "$PLAN_METADATA" format_version)" == 1 \
    && "$(kv "$PLAN_METADATA" scenario)" == "$SCENARIO" \
    && "$(kv "$PLAN_METADATA" plan_sha256)" == "$PLAN_HASH" ]] \
    || die "plan metadata does not bind the scenario plan"
[[ "$(kv "$RUN_METADATA" format_version)" == 1 \
    && "$(kv "$RUN_METADATA" subject)" == "$SUBJECT" \
    && "$(kv "$RUN_METADATA" subject_identity_sha256)" == "$SUBJECT_HASH" \
    && "$(kv "$RUN_METADATA" scenario)" == "$SCENARIO" \
    && "$(kv "$RUN_METADATA" scenario_plan_sha256)" == "$PLAN_HASH" \
    && "$(kv "$RUN_METADATA" workload_sha256)" == "$WORKLOAD_HASH" \
    && "$(kv "$RUN_METADATA" measured_duration_ms)" == "$DURATION_MS" \
    && "$(kv "$RUN_METADATA" process_pid)" == "$SUBJECT_PID" \
    && "$(kv "$RUN_METADATA" process_start_identity)" == "$START_IDENTITY" \
    && "$(kv "$RUN_METADATA" status)" == complete ]] \
    || die "run metadata does not bind the frozen process and inputs"

trace_help="$($TRACE_RECORDER --help 2>&1)"
for feature in --subject-identity --run-metadata --workload-metadata \
    --workload-events --workload-ready-receipt --campaign-secret-file \
    --campaign-id --session-id --nonce \
    --scenario --warmup-ms --duration-ms --output-directory; do
    grep -Fq -- "$feature" <<< "$trace_help" \
        || die "trace recorder lacks required v3 interface: $feature"
done

if [[ -t 0 ]]; then SAVED_TERMIOS="$(stty -g)"; fi
EXIT_TRAP_ACTIVE=true
trap 'on_exit $?' EXIT
trap on_signal INT TERM HUP
verify_window_proof preflight
wait_for_authenticated_ready
(( MEASUREMENT_READY_CONTINUOUS_NS >= SEED_CONTINUOUS_NS \
    && MEASUREMENT_READY_CONTINUOUS_NS - SEED_CONTINUOUS_NS <= 2000000000 )) \
    || abort_run measurement-ready-missed-seed-window

CHILDREN_STARTED_CONTINUOUS_NS="$(continuous_ns)"
(( CHILDREN_STARTED_CONTINUOUS_NS >= MEASUREMENT_READY_CONTINUOUS_NS \
    && CHILDREN_STARTED_CONTINUOUS_NS - MEASUREMENT_READY_CONTINUOUS_NS <= 2000000000 )) \
    || abort_run child-launch-missed-seed-window

PLAN_START_CONTINUOUS_NS=$((CHILDREN_STARTED_CONTINUOUS_NS + 5000000000))
TRACE_BOUNDARY_CONTINUOUS_NS=$((PLAN_START_CONTINUOUS_NS + WARMUP_MS * 1000000))
publish_plan_start_gate

spawn_process_group "$DRIVER_BINARY" --pid "$SUBJECT_PID" --start-identity "$START_IDENTITY" \
    --executable "$EXECUTABLE" --executable-sha256 "$EXECUTABLE_SHA256" \
    --app-bundle "$APP_BUNDLE" --bundle-identifier "$BUNDLE_IDENTIFIER" \
    --signing-identifier "$SIGNING_IDENTIFIER" --team-identifier "$TEAM_IDENTIFIER" \
    --cdhash "$CDHASH" --window-number "$WINDOW_NUMBER" \
    --scenario-plan "$SCENARIO_PLAN" --plan-start-continuous-ns "$PLAN_START_CONTINUOUS_NS" \
    --output "$DRIVER_OUTPUT" || abort_run driver-process-group-launch-failed
DRIVER_PID="$SPAWNED_PID"
DRIVER_PGID="$DRIVER_PID"
spawn_process_group "$RSS_BINARY" --subject-identity "$SUBJECT_IDENTITY" \
    --plan-start-continuous-ns "$PLAN_START_CONTINUOUS_NS" \
    --warmup-ms "$WARMUP_MS" \
    --plan-start-gate-sha256 "$(sha256 "$PLAN_START_GATE")" \
    --ready-receipt-sha256 "$(sha256 "$WORKLOAD_READY_RECEIPT")" \
    --duration-ms "$DURATION_MS" --output "$RSS_OUTPUT" \
    || abort_run rss-process-group-launch-failed
RSS_PID="$SPAWNED_PID"
RSS_PGID="$RSS_PID"

trace_launch_deadline=$((TRACE_BOUNDARY_CONTINUOUS_NS - 2000000000))
sleep_until "$trace_launch_deadline"
TRACE_INVOKED_CONTINUOUS_NS="$(continuous_ns)"
verify_controller_toolchain || abort_run controller-toolchain-changed-before-trace
spawn_process_group "$TRACE_RECORDER" --subject-identity "$SUBJECT_IDENTITY" \
    --run-metadata "$RUN_METADATA" --workload-metadata "$WORKLOAD_METADATA" \
    --workload-events "$WORKLOAD_EVENTS" \
    --workload-ready-receipt "$WORKLOAD_READY_RECEIPT" \
    --supplemental-evidence "$PLAN_START_GATE" \
    --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
    --campaign-id "$CAMPAIGN_ID" --session-id "$SESSION_ID" --nonce "$NONCE" \
    --scenario "$SCENARIO" --warmup-ms "$WARMUP_MS" --duration-ms "$DURATION_MS" \
    --output-directory "$TRACE_OUTPUT_DIRECTORY" \
    || abort_run trace-process-group-launch-failed
TRACE_PID="$SPAWNED_PID"
TRACE_PGID="$TRACE_PID"

driver_done=false
rss_done=false
trace_done=false
while [[ "$driver_done" == false || "$rss_done" == false || "$trace_done" == false ]]; do
    if [[ "$driver_done" == false ]] && process_has_exited "$DRIVER_PID"; then
        set +e; wait "$DRIVER_PID"; DRIVER_STATUS=$?; set -e
        if process_group_exists "$DRIVER_PGID"; then
            terminate_process_group "$DRIVER_PGID" ""
            DRIVER_STATUS=70
        fi
        DRIVER_PID=""; DRIVER_PGID=""; driver_done=true
        [[ "$DRIVER_STATUS" == 0 ]] || abort_run driver-failed
    fi
    if [[ "$rss_done" == false ]] && process_has_exited "$RSS_PID"; then
        set +e; wait "$RSS_PID"; RSS_STATUS=$?; set -e
        if process_group_exists "$RSS_PGID"; then
            terminate_process_group "$RSS_PGID" ""
            RSS_STATUS=70
        fi
        RSS_PID=""; RSS_PGID=""; rss_done=true
        [[ "$RSS_STATUS" == 0 ]] || abort_run rss-sampler-failed
    fi
    if [[ "$trace_done" == false ]] && process_has_exited "$TRACE_PID"; then
        set +e; wait "$TRACE_PID"; TRACE_STATUS=$?; set -e
        if process_group_exists "$TRACE_PGID"; then
            terminate_process_group "$TRACE_PGID" ""
            TRACE_STATUS=70
        fi
        TRACE_PID=""; TRACE_PGID=""; trace_done=true
        [[ "$TRACE_STATUS" == 0 ]] || abort_run trace-recorder-failed
    fi
    [[ "$driver_done" == false || "$rss_done" == false || "$trace_done" == false ]] \
        && sleep 0.02
done

wait_for_workload_completion
"$WORKLOAD_AUTH_VERIFIER" --metadata "$WORKLOAD_METADATA" \
    --events "$WORKLOAD_EVENTS" --subject-identity "$SUBJECT_IDENTITY" \
    --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
    --ready-receipt "$WORKLOAD_READY_RECEIPT" --campaign-id "$CAMPAIGN_ID" \
    --session-id "$SESSION_ID" --nonce "$NONCE" --scenario "$SCENARIO" \
    --requested-warmup-ms "$WARMUP_MS" --requested-duration-ms "$DURATION_MS" \
    || abort_run workload-metadata-authentication-failed
[[ "$(kv "$WORKLOAD_METADATA" format_version)" == 3 \
    && "$(kv "$WORKLOAD_METADATA" scenario)" == "$SCENARIO" \
    && "$(kv "$WORKLOAD_METADATA" campaign_id)" == "$CAMPAIGN_ID" \
    && "$(kv "$WORKLOAD_METADATA" session_id)" == "$SESSION_ID" \
    && "$(kv "$WORKLOAD_METADATA" nonce)" == "$NONCE" \
    && "$(kv "$WORKLOAD_METADATA" subject_identity_sha256)" == "$SUBJECT_HASH" \
    && "$(kv "$WORKLOAD_METADATA" subject_process_pid)" == "$SUBJECT_PID" \
    && "$(kv "$WORKLOAD_METADATA" subject_process_start_identity)" == "$START_IDENTITY" \
    && "$(kv "$WORKLOAD_METADATA" producer_sha256)" == "$WORKLOAD_HASH" \
    && "$(kv "$WORKLOAD_METADATA" events_sha256)" == "$(sha256 "$WORKLOAD_EVENTS")" \
    && "$(kv "$WORKLOAD_METADATA" auth_algorithm)" == hmac-sha256 \
    && "$(kv "$WORKLOAD_METADATA" requested_duration_ms)" == "$DURATION_MS" \
    && "$(kv "$WORKLOAD_METADATA" warmup_ms)" == "$WARMUP_MS" \
    && "$(kv "$WORKLOAD_METADATA" plan_start_continuous_ns)" == "$PLAN_START_CONTINUOUS_NS" \
    && "$(kv "$WORKLOAD_METADATA" status)" == complete ]] \
    || abort_run workload-metadata-authentication-failed
workload_started="$(kv "$WORKLOAD_METADATA" started_continuous_ns)"
is_positive_uint "$workload_started" || abort_run workload-start-time-invalid
(( workload_started >= TRACE_BOUNDARY_CONTINUOUS_NS \
    && workload_started - TRACE_BOUNDARY_CONTINUOUS_NS <= 100000000 )) \
    || abort_run workload-missed-shared-measurement-clock
PRODUCER_ENDED_CONTINUOUS_NS="$(kv "$WORKLOAD_METADATA" ended_continuous_ns)"
is_positive_uint "$PRODUCER_ENDED_CONTINUOUS_NS" || abort_run workload-end-time-invalid
event_end="$(awk -F '\t' '$3 == "producer-end" && $10 == "success" {count += 1; value = $2} \
    END {if (count == 1) print value}' "$WORKLOAD_EVENTS")"
[[ "$event_end" == "$PRODUCER_ENDED_CONTINUOUS_NS" ]] \
    || abort_run workload-end-event-mismatch

tail_deadline=$((PRODUCER_ENDED_CONTINUOUS_NS + TAIL_MS * 1000000))
sleep_until "$tail_deadline"
verify_window_proof postflight
TAIL_VERIFIED_CONTINUOUS_NS="$(continuous_ns)"
(( TAIL_VERIFIED_CONTINUOUS_NS >= tail_deadline )) \
    || abort_run post-producer-tail-not-preserved

[[ -f "$DRIVER_OUTPUT" && ! -L "$DRIVER_OUTPUT" \
    && -f "$RSS_OUTPUT" && ! -L "$RSS_OUTPUT" \
    && -d "$TRACE_OUTPUT_DIRECTORY" && ! -L "$TRACE_OUTPUT_DIRECTORY" ]] \
    || abort_run expected-child-artifact-missing
trace_metadata="$TRACE_OUTPUT_DIRECTORY/$SUBJECT-$SCENARIO-trace-metadata.tsv"
[[ -f "$trace_metadata" && ! -L "$trace_metadata" ]] \
    || abort_run trace-v3-metadata-missing
trace_started="$(kv "$trace_metadata" capture_started_continuous_ns)"
trace_ended="$(kv "$trace_metadata" capture_ended_continuous_ns)"
trace_actual_ms="$(kv "$trace_metadata" actual_duration_ms)"
is_positive_uint "$trace_started" || abort_run trace-start-time-missing
is_positive_uint "$trace_ended" || abort_run trace-end-time-missing
is_positive_uint "$trace_actual_ms" || abort_run trace-duration-missing
trace_skew=$((trace_started - TRACE_BOUNDARY_CONTINUOUS_NS))
(( trace_skew >= -2000000000 && trace_skew <= 0 \
    && trace_ended >= PRODUCER_ENDED_CONTINUOUS_NS \
    && trace_ended - PRODUCER_ENDED_CONTINUOUS_NS <= 2000000000 \
    && trace_actual_ms >= DURATION_MS \
    && trace_actual_ms <= DURATION_MS + 3250 )) \
    || abort_run trace-did-not-cover-warmup-boundary
driver_first="$(awk -F '\t' 'NR == 2 {print $2}' "$DRIVER_OUTPUT")"
rss_first="$(awk -F '\t' '$1 ~ /^[0-9]+$/ {print $2; exit}' "$RSS_OUTPUT")"
is_positive_uint "$driver_first" || abort_run driver-start-time-missing
is_positive_uint "$rss_first" || abort_run rss-start-time-missing
driver_skew=$((driver_first - PLAN_START_CONTINUOUS_NS))
rss_skew=$((rss_first - TRACE_BOUNDARY_CONTINUOUS_NS))
(( driver_skew >= 0 && driver_skew <= 250000000 )) \
    || abort_run driver-did-not-use-authoritative-plan-clock
(( rss_skew >= 0 && rss_skew <= 1000000000 )) \
    || abort_run rss-did-not-use-authoritative-measurement-clock
[[ "$(kv "$trace_metadata" run_metadata_sha256)" == "$(sha256 "$RUN_METADATA")" \
    && "$(kv "$trace_metadata" workload_metadata_sha256)" == "$(sha256 "$WORKLOAD_METADATA")" \
    && "$(kv "$trace_metadata" workload_ready_receipt_sha256)" == "$(sha256 "$WORKLOAD_READY_RECEIPT")" \
    && "$(kv "$trace_metadata" supplemental_evidence_sha256)" == "$(sha256 "$PLAN_START_GATE")" ]] \
    || abort_run trace-metadata-input-binding-failed
verify_controller_toolchain || abort_run controller-toolchain-changed-during-run

cleanup_temp
restore_termios
publish_result complete none || die "cannot publish scenario result"
FINALIZED=true
trap - EXIT INT TERM HUP
printf 'scenario_result_sha256\t%s\n' "$(sha256 "$RESULT_OUTPUT")"
