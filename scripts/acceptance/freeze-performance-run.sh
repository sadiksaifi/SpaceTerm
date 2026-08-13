#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
umask 077

RUN_INTENT=""
SUBJECT_IDENTITY=""
NATIVE_PROVISIONAL_OBSERVATION=""
NATIVE_OBSERVATION=""
NATIVE_RUNTIME_METADATA=""
NATIVE_RUNTIME_SAMPLES=""
NATIVE_RUNTIME_EVENTS=""
NATIVE_FAILURE_ACTIONS=""
CAMPAIGN_SECRET_FILE=""
TRACE_PROVISIONAL_RECEIPT=""
PERFORMANCE_TAIL_RECEIPT=""
PERFORMANCE_QUIT_RECEIPT=""
SUBJECT_EXIT_RECEIPT=""
DRIVER_RECEIPT=""
DRIVER_EVENTS=""
WORKLOAD_METADATA=""
WORKLOAD_EVENTS=""
WORKLOAD_READY_RECEIPT=""
RSS_SAMPLES=""
OUTPUT=""
TEMP=""

SCRIPT_DIRECTORY="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIRECTORY

usage() {
    cat <<EOF
Usage: $(basename -- "$0") --run-intent FILE --subject-identity FILE \\
  --campaign-secret-file FILE --trace-provisional-receipt FILE \\
  --performance-tail-receipt FILE --performance-quit-receipt FILE \\
  --subject-exit-receipt FILE --driver-receipt FILE --driver-events FILE \\
  --workload-metadata FILE --workload-events FILE --rss-samples FILE \\
  --workload-ready-receipt FILE \\
  --output ABSENT_FILE [SPACETERM NATIVE CLOSURE]

SpaceTerm native closure:
  --native-provisional-observation FILE --native-observation FILE
  --native-runtime-metadata FILE --native-runtime-samples FILE
  --native-runtime-events FILE --native-failure-actions FILE

Finalize one exact 29-record performance-run-metadata/v3 after subject exit.
SpaceTerm requires the authenticated v5/v3/v2 native closure. Ghostty rejects
SpaceTerm-only artifacts and records ten literal not-applicable values.
EOF
}

die() { echo "error: $*" >&2; exit 1; }
cleanup() { [[ -z "$TEMP" ]] || rm -f -- "$TEMP"; }
sha256() { shasum -a 256 "$1" | awk '{ print $1 }'; }
kv() {
    awk -F '\t' -v wanted="$2" '
        NF != 2 { bad = 1 }
        $1 == wanted { count += 1; value = $2 }
        END { if (!bad && count == 1) print value }
    ' "$1"
}
exact_schema() {
    local file="$1" keys="$2" count="$3"
    awk -F '\t' -v keys="$keys" -v count="$count" '
        BEGIN { split(keys, wanted, " ") }
        NF != 2 || NR > count || $1 != wanted[NR] { exit 1 }
        END { if (NR != count) exit 1 }
    ' "$file"
}

while (( $# > 0 )); do
    case "$1" in
        --run-intent) RUN_INTENT="${2:-}"; shift ;;
        --subject-identity) SUBJECT_IDENTITY="${2:-}"; shift ;;
        --native-provisional-observation) NATIVE_PROVISIONAL_OBSERVATION="${2:-}"; shift ;;
        --native-observation) NATIVE_OBSERVATION="${2:-}"; shift ;;
        --native-runtime-metadata) NATIVE_RUNTIME_METADATA="${2:-}"; shift ;;
        --native-runtime-samples) NATIVE_RUNTIME_SAMPLES="${2:-}"; shift ;;
        --native-runtime-events) NATIVE_RUNTIME_EVENTS="${2:-}"; shift ;;
        --native-failure-actions) NATIVE_FAILURE_ACTIONS="${2:-}"; shift ;;
        --campaign-secret-file) CAMPAIGN_SECRET_FILE="${2:-}"; shift ;;
        --trace-provisional-receipt) TRACE_PROVISIONAL_RECEIPT="${2:-}"; shift ;;
        --performance-tail-receipt) PERFORMANCE_TAIL_RECEIPT="${2:-}"; shift ;;
        --performance-quit-receipt) PERFORMANCE_QUIT_RECEIPT="${2:-}"; shift ;;
        --subject-exit-receipt) SUBJECT_EXIT_RECEIPT="${2:-}"; shift ;;
        --driver-receipt) DRIVER_RECEIPT="${2:-}"; shift ;;
        --driver-events) DRIVER_EVENTS="${2:-}"; shift ;;
        --workload-metadata) WORKLOAD_METADATA="${2:-}"; shift ;;
        --workload-events) WORKLOAD_EVENTS="${2:-}"; shift ;;
        --workload-ready-receipt) WORKLOAD_READY_RECEIPT="${2:-}"; shift ;;
        --rss-samples) RSS_SAMPLES="${2:-}"; shift ;;
        --output) OUTPUT="${2:-}"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument: $1" ;;
    esac
    shift
done

for command in awk chmod ln mkdir rm shasum; do
    command -v "$command" >/dev/null 2>&1 || die "required command not found: $command"
done
[[ -f "$RUN_INTENT" && ! -L "$RUN_INTENT" \
    && -f "$SUBJECT_IDENTITY" && ! -L "$SUBJECT_IDENTITY" ]] \
    || die "run intent or subject identity is unavailable"
for input in "$CAMPAIGN_SECRET_FILE" "$TRACE_PROVISIONAL_RECEIPT" \
    "$PERFORMANCE_TAIL_RECEIPT" "$PERFORMANCE_QUIT_RECEIPT" \
    "$SUBJECT_EXIT_RECEIPT" "$DRIVER_RECEIPT" "$DRIVER_EVENTS" \
    "$WORKLOAD_METADATA" "$WORKLOAD_EVENTS" "$WORKLOAD_READY_RECEIPT" "$RSS_SAMPLES"; do
    [[ -f "$input" && ! -L "$input" ]] || die "causal run-closure artifact is unavailable"
done
[[ -n "$OUTPUT" && ! -e "$OUTPUT" && ! -L "$OUTPUT" ]] \
    || die "output path is missing or exists"
readonly INTENT_KEYS="format_version subject subject_identity_sha256 scenario scenario_plan_sha256 workload_sha256 command_sha256 environment_sha256 font_sha256 initial_grid_sha256 measured_duration_ms process_pid process_start_identity campaign_id session_id nonce native_provisional_observation_sha256 status"
exact_schema "$RUN_INTENT" "$INTENT_KEYS" 18 || die "run intent schema is invalid"
subject="$(kv "$RUN_INTENT" subject)"
[[ "$(kv "$RUN_INTENT" format_version)" == 1 \
    && "$(kv "$RUN_INTENT" status)" == prepared \
    && ( "$subject" == spaceterm || "$subject" == ghostty ) \
    && "$(kv "$RUN_INTENT" subject_identity_sha256)" == "$(sha256 "$SUBJECT_IDENTITY")" \
    && "$(kv "$SUBJECT_IDENTITY" subject)" == "$subject" \
    && "$(kv "$SUBJECT_IDENTITY" process_pid)" == "$(kv "$RUN_INTENT" process_pid)" \
    && "$(kv "$SUBJECT_IDENTITY" process_start_identity)" \
        == "$(kv "$RUN_INTENT" process_start_identity)" ]] \
    || die "run intent does not bind the frozen subject"

readonly TRACE_PROVISIONAL_KEYS="format_version subject_identity_sha256 run_intent_sha256 workload_metadata_sha256 workload_ready_receipt_sha256 supplemental_evidence_sha256 capture_status requested_duration_ms actual_duration_ms capture_started_continuous_ns capture_ended_continuous_ns trace_bundle_tree_sha256 toc_sha256 time_profile_export_sha256 allocations_export_sha256 hangs_export_sha256 trace_verification_sha256 verifier_sha256 status auth_algorithm provisional_hmac_sha256"
exact_schema "$TRACE_PROVISIONAL_RECEIPT" "$TRACE_PROVISIONAL_KEYS" 21 \
    || die "trace provisional receipt schema is invalid"
[[ "$(kv "$TRACE_PROVISIONAL_RECEIPT" format_version)" == 1 \
    && "$(kv "$TRACE_PROVISIONAL_RECEIPT" run_intent_sha256)" == "$(sha256 "$RUN_INTENT")" \
    && "$(kv "$TRACE_PROVISIONAL_RECEIPT" subject_identity_sha256)" \
        == "$(sha256 "$SUBJECT_IDENTITY")" \
    && "$(kv "$TRACE_PROVISIONAL_RECEIPT" capture_status)" == CAPTURED \
    && "$(kv "$TRACE_PROVISIONAL_RECEIPT" status)" == complete ]] \
    || die "trace provisional receipt does not bind the run intent"

quit_token="$(kv "$PERFORMANCE_TAIL_RECEIPT" quit_token)"
tail_completed_ns="$(kv "$PERFORMANCE_TAIL_RECEIPT" tail_completed_continuous_ns)"
"$SCRIPT_DIRECTORY/performance-tail-receipt.py" verify \
    --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
    --campaign-id "$(kv "$RUN_INTENT" campaign_id)" \
    --session-id "$(kv "$RUN_INTENT" session_id)" --nonce "$(kv "$RUN_INTENT" nonce)" \
    --quit-token "$quit_token" --run-intent "$RUN_INTENT" \
    --subject-identity "$SUBJECT_IDENTITY" --driver-receipt "$DRIVER_RECEIPT" \
    --driver-events "$DRIVER_EVENTS" --workload-metadata "$WORKLOAD_METADATA" \
    --workload-events "$WORKLOAD_EVENTS" --workload-ready-receipt "$WORKLOAD_READY_RECEIPT" \
    --rss-samples "$RSS_SAMPLES" \
    --trace-provisional-receipt "$TRACE_PROVISIONAL_RECEIPT" \
    --tail-completed-continuous-ns "$tail_completed_ns" \
    --receipt "$PERFORMANCE_TAIL_RECEIPT" >/dev/null \
    || die "performance tail receipt is invalid"

if [[ "$subject" == spaceterm ]]; then
    for input in "$NATIVE_PROVISIONAL_OBSERVATION" "$NATIVE_OBSERVATION" \
        "$NATIVE_RUNTIME_METADATA" "$NATIVE_RUNTIME_SAMPLES" \
        "$NATIVE_RUNTIME_EVENTS" "$NATIVE_FAILURE_ACTIONS"; do
        [[ -f "$input" && ! -L "$input" ]] \
            || die "SpaceTerm native closure artifact is missing or symlinked"
    done
    [[ "$(sha256 "$NATIVE_PROVISIONAL_OBSERVATION")" \
        == "$(kv "$RUN_INTENT" native_provisional_observation_sha256)" ]] \
        || die "SpaceTerm provisional observation does not bind the run intent"
    "$SCRIPT_DIRECTORY/verify-performance-native-closure.py" \
        --subject-identity "$SUBJECT_IDENTITY" \
        --provisional-observation "$NATIVE_PROVISIONAL_OBSERVATION" \
        --native-observation "$NATIVE_OBSERVATION" \
        --runtime-metadata "$NATIVE_RUNTIME_METADATA" \
        --runtime-samples "$NATIVE_RUNTIME_SAMPLES" \
        --runtime-events "$NATIVE_RUNTIME_EVENTS" \
        --failure-actions "$NATIVE_FAILURE_ACTIONS" >/dev/null \
        || die "SpaceTerm native performance closure is invalid"
    native_observation_sha256="$(sha256 "$NATIVE_OBSERVATION")"
    native_runtime_metadata_sha256="$(sha256 "$NATIVE_RUNTIME_METADATA")"
    native_failure_actions_sha256="$(sha256 "$NATIVE_FAILURE_ACTIONS")"
    native_failure_action_enabled=false
    native_failure_request_count=0
    native_failure_result_count=0
    native_failure_resource_staged_count=0
    native_failure_resource_staged_bytes=0
    native_failure_resource_rolled_back_count=0
    native_failure_resource_rolled_back_bytes=0
else
    [[ "$(kv "$RUN_INTENT" native_provisional_observation_sha256)" == not-applicable ]] \
        || die "Ghostty run intent has a SpaceTerm provisional observation"
    [[ -z "$NATIVE_PROVISIONAL_OBSERVATION$NATIVE_OBSERVATION$NATIVE_RUNTIME_METADATA" \
        && -z "$NATIVE_RUNTIME_SAMPLES$NATIVE_RUNTIME_EVENTS$NATIVE_FAILURE_ACTIONS" ]] \
        || die "Ghostty must not receive SpaceTerm native closure artifacts"
    native_observation_sha256=not-applicable
    native_runtime_metadata_sha256=not-applicable
    native_failure_actions_sha256=not-applicable
    native_failure_action_enabled=not-applicable
    native_failure_request_count=not-applicable
    native_failure_result_count=not-applicable
    native_failure_resource_staged_count=not-applicable
    native_failure_resource_staged_bytes=not-applicable
    native_failure_resource_rolled_back_count=not-applicable
    native_failure_resource_rolled_back_bytes=not-applicable
fi

exit_arguments=(
    --campaign-secret-file "$CAMPAIGN_SECRET_FILE" --run-intent "$RUN_INTENT"
    --subject-identity "$SUBJECT_IDENTITY" --tail-receipt "$PERFORMANCE_TAIL_RECEIPT"
    --quit-receipt "$PERFORMANCE_QUIT_RECEIPT"
    --subject-exit-receipt "$SUBJECT_EXIT_RECEIPT"
)
if [[ "$subject" == spaceterm ]]; then
    exit_arguments+=(--native-observation "$NATIVE_OBSERVATION")
fi
"$SCRIPT_DIRECTORY/verify-performance-subject-exit.py" "${exit_arguments[@]}" >/dev/null \
    || die "performance subject exit receipt is invalid"

mkdir -p -- "$(dirname -- "$OUTPUT")"
TEMP="${OUTPUT}.tmp.$$"
trap cleanup EXIT INT TERM
{
    printf 'format_version\t3\n'
    for key in subject subject_identity_sha256 scenario scenario_plan_sha256 workload_sha256 \
        command_sha256 environment_sha256 font_sha256 initial_grid_sha256 measured_duration_ms \
        process_pid process_start_identity; do
        printf '%s\t%s\n' "$key" "$(kv "$RUN_INTENT" "$key")"
    done
    printf 'run_intent_sha256\t%s\n' "$(sha256 "$RUN_INTENT")"
    printf 'native_observation_sha256\t%s\n' "$native_observation_sha256"
    printf 'native_runtime_metadata_sha256\t%s\n' "$native_runtime_metadata_sha256"
    printf 'native_failure_actions_sha256\t%s\n' "$native_failure_actions_sha256"
    printf 'native_failure_action_enabled\t%s\n' "$native_failure_action_enabled"
    printf 'native_failure_request_count\t%s\n' "$native_failure_request_count"
    printf 'native_failure_result_count\t%s\n' "$native_failure_result_count"
    printf 'native_failure_resource_staged_count\t%s\n' "$native_failure_resource_staged_count"
    printf 'native_failure_resource_staged_bytes\t%s\n' "$native_failure_resource_staged_bytes"
    printf 'native_failure_resource_rolled_back_count\t%s\n' "$native_failure_resource_rolled_back_count"
    printf 'native_failure_resource_rolled_back_bytes\t%s\n' "$native_failure_resource_rolled_back_bytes"
    printf 'trace_provisional_receipt_sha256\t%s\n' "$(sha256 "$TRACE_PROVISIONAL_RECEIPT")"
    printf 'performance_tail_receipt_sha256\t%s\n' "$(sha256 "$PERFORMANCE_TAIL_RECEIPT")"
    printf 'performance_quit_receipt_sha256\t%s\n' "$(sha256 "$PERFORMANCE_QUIT_RECEIPT")"
    printf 'subject_exit_receipt_sha256\t%s\n' "$(sha256 "$SUBJECT_EXIT_RECEIPT")"
    printf 'status\tcomplete\n'
} > "$TEMP"
chmod 0444 "$TEMP"
ln "$TEMP" "$OUTPUT" || die "output path was created concurrently"
rm -f -- "$TEMP"
TEMP=""
trap - EXIT INT TERM
printf 'run_metadata_sha256\t%s\n' "$(sha256 "$OUTPUT")"
