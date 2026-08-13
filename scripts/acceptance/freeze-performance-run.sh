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
DRIVER_INTENT=""
DRIVER_RECEIPT=""
DRIVER_EVENTS=""
WINDOW_IDENTITY=""
DRIVER_BINARY=""
DRIVER_SOURCE=""
DRIVER_CONTROLLER=""
SCENARIO_PLAN=""
PLAN_START_GATE=""
WORKLOAD_METADATA=""
WORKLOAD_EVENTS=""
WORKLOAD_READY_RECEIPT=""
RSS_SAMPLES=""
PERFORMANCE_LIFECYCLE_READY_RECEIPT=""
PERFORMANCE_LIFECYCLE_REGISTRATION=""
SUBJECT_LIFECYCLE_HELPER=""
APPKIT_TERMINATOR_SOURCE=""
APPKIT_TERMINATOR_BINARY=""
OUTPUT=""
TEMP=""
readonly EVIDENCE_MODE=production

SCRIPT_DIRECTORY="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIRECTORY

usage() {
    cat <<EOF
Usage: $(basename -- "$0") --run-intent FILE --subject-identity FILE \\
  --campaign-secret-file FILE --trace-provisional-receipt FILE \\
  --performance-tail-receipt FILE --performance-quit-receipt FILE \\
  --subject-exit-receipt FILE --driver-intent FILE --driver-receipt FILE \\
  --driver-events FILE --window-identity FILE --driver-binary FILE \\
  --driver-source FILE --driver-controller FILE --scenario-plan FILE \\
  --plan-start-gate FILE \\
  --workload-metadata FILE --workload-events FILE --rss-samples FILE \\
  --workload-ready-receipt FILE \\
  --performance-lifecycle-ready-receipt FILE \\
  --performance-lifecycle-registration FILE --subject-lifecycle-helper FILE \\
  --appkit-terminator-source FILE --appkit-terminator-binary FILE \\
  --output ABSENT_FILE [SPACETERM NATIVE CLOSURE]

SpaceTerm native closure:
  --native-provisional-observation FILE --native-observation FILE
  --native-runtime-metadata FILE --native-runtime-samples FILE
  --native-runtime-events FILE --native-failure-actions FILE

Finalize one exact 35-record performance-run-metadata/v4 after subject exit.
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
        --driver-intent) DRIVER_INTENT="${2:-}"; shift ;;
        --driver-receipt) DRIVER_RECEIPT="${2:-}"; shift ;;
        --driver-events) DRIVER_EVENTS="${2:-}"; shift ;;
        --window-identity) WINDOW_IDENTITY="${2:-}"; shift ;;
        --driver-binary) DRIVER_BINARY="${2:-}"; shift ;;
        --driver-source) DRIVER_SOURCE="${2:-}"; shift ;;
        --driver-controller) DRIVER_CONTROLLER="${2:-}"; shift ;;
        --scenario-plan) SCENARIO_PLAN="${2:-}"; shift ;;
        --plan-start-gate) PLAN_START_GATE="${2:-}"; shift ;;
        --workload-metadata) WORKLOAD_METADATA="${2:-}"; shift ;;
        --workload-events) WORKLOAD_EVENTS="${2:-}"; shift ;;
        --workload-ready-receipt) WORKLOAD_READY_RECEIPT="${2:-}"; shift ;;
        --rss-samples) RSS_SAMPLES="${2:-}"; shift ;;
        --performance-lifecycle-ready-receipt) PERFORMANCE_LIFECYCLE_READY_RECEIPT="${2:-}"; shift ;;
        --performance-lifecycle-registration) PERFORMANCE_LIFECYCLE_REGISTRATION="${2:-}"; shift ;;
        --subject-lifecycle-helper) SUBJECT_LIFECYCLE_HELPER="${2:-}"; shift ;;
        --appkit-terminator-source) APPKIT_TERMINATOR_SOURCE="${2:-}"; shift ;;
        --appkit-terminator-binary) APPKIT_TERMINATOR_BINARY="${2:-}"; shift ;;
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
    "$SUBJECT_EXIT_RECEIPT" "$DRIVER_INTENT" "$DRIVER_RECEIPT" "$DRIVER_EVENTS" \
    "$WINDOW_IDENTITY" "$DRIVER_BINARY" "$DRIVER_SOURCE" "$DRIVER_CONTROLLER" \
    "$SCENARIO_PLAN" "$PLAN_START_GATE" \
    "$WORKLOAD_METADATA" "$WORKLOAD_EVENTS" "$WORKLOAD_READY_RECEIPT" "$RSS_SAMPLES" \
    "$PERFORMANCE_LIFECYCLE_READY_RECEIPT" "$PERFORMANCE_LIFECYCLE_REGISTRATION" \
    "$SUBJECT_LIFECYCLE_HELPER" "$APPKIT_TERMINATOR_SOURCE" "$APPKIT_TERMINATOR_BINARY"; do
    [[ -f "$input" && ! -L "$input" ]] || die "causal run-closure artifact is unavailable"
done
[[ -n "$OUTPUT" && ! -e "$OUTPUT" && ! -L "$OUTPUT" ]] \
    || die "output path is missing or exists"
readonly INTENT_KEYS="format_version subject subject_identity_sha256 scenario scenario_plan_sha256 workload_sha256 command_sha256 environment_sha256 font_sha256 initial_grid_sha256 measured_duration_ms process_pid process_start_identity campaign_id session_id nonce native_provisional_observation_sha256 evidence_mode status"
exact_schema "$RUN_INTENT" "$INTENT_KEYS" 19 || die "run intent schema is invalid"
subject="$(kv "$RUN_INTENT" subject)"
[[ "$(kv "$RUN_INTENT" format_version)" == 1 \
    && "$(kv "$RUN_INTENT" status)" == prepared \
    && "$(kv "$RUN_INTENT" evidence_mode)" == "$EVIDENCE_MODE" \
    && ( "$subject" == spaceterm || "$subject" == ghostty ) \
    && "$(kv "$RUN_INTENT" subject_identity_sha256)" == "$(sha256 "$SUBJECT_IDENTITY")" \
    && "$(kv "$SUBJECT_IDENTITY" subject)" == "$subject" \
    && "$(kv "$SUBJECT_IDENTITY" process_pid)" == "$(kv "$RUN_INTENT" process_pid)" \
    && "$(kv "$SUBJECT_IDENTITY" process_start_identity)" \
        == "$(kv "$RUN_INTENT" process_start_identity)" ]] \
    || die "run intent does not bind the frozen subject"

canonical_path() {
    local directory base
    directory="$(cd -- "$(dirname -- "$1")" && pwd -P)" || return 1
    base="$(basename -- "$1")"
    printf '%s/%s\n' "$directory" "$base"
}
[[ "$(canonical_path "$DRIVER_SOURCE")" == "$SCRIPT_DIRECTORY/performance-driver.m" \
    && "$(canonical_path "$DRIVER_CONTROLLER")" \
        == "$SCRIPT_DIRECTORY/run-native-performance-scenario.sh" \
    && "$(canonical_path "$SUBJECT_LIFECYCLE_HELPER")" \
        == "$SCRIPT_DIRECTORY/performance-subject-lifecycle.py" \
    && "$(canonical_path "$APPKIT_TERMINATOR_SOURCE")" \
        == "$SCRIPT_DIRECTORY/performance-appkit-terminate.m" ]] \
    || die "driver or lifecycle source is not canonical"
readonly GATE_KEYS="format_version campaign_id session_id nonce ready_receipt_sha256 plan_start_continuous_ns start_gate_hmac_sha256"
exact_schema "$PLAN_START_GATE" "$GATE_KEYS" 7 || die "plan start gate schema is invalid"
plan_start_continuous_ns="$(kv "$PLAN_START_GATE" plan_start_continuous_ns)"
[[ "$plan_start_continuous_ns" =~ ^[1-9][0-9]*$ \
    && "$(sha256 "$SCENARIO_PLAN")" == "$(kv "$RUN_INTENT" scenario_plan_sha256)" ]] \
    || die "driver plan does not bind the run intent"
"$SCRIPT_DIRECTORY/verify-performance-workload-ready.py" \
    --ready-receipt "$WORKLOAD_READY_RECEIPT" --events "$WORKLOAD_EVENTS" \
    --subject-identity "$SUBJECT_IDENTITY" \
    --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
    --campaign-id "$(kv "$RUN_INTENT" campaign_id)" \
    --session-id "$(kv "$RUN_INTENT" session_id)" --nonce "$(kv "$RUN_INTENT" nonce)" \
    --plan-start-gate "$PLAN_START_GATE" \
    --expected-plan-start-continuous-ns "$plan_start_continuous_ns" >/dev/null \
    || die "plan start gate is invalid"
"$SCRIPT_DIRECTORY/performance-driver-receipt.py" verify \
    --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
    --campaign-id "$(kv "$RUN_INTENT" campaign_id)" \
    --session-id "$(kv "$RUN_INTENT" session_id)" --nonce "$(kv "$RUN_INTENT" nonce)" \
    --driver-output "$DRIVER_EVENTS" --driver-binary "$DRIVER_BINARY" \
    --driver-source "$DRIVER_SOURCE" --controller "$DRIVER_CONTROLLER" \
    --scenario-plan "$SCENARIO_PLAN" \
    --plan-start-continuous-ns "$plan_start_continuous_ns" \
    --subject-identity "$SUBJECT_IDENTITY" --window-identity "$WINDOW_IDENTITY" \
    --intent "$DRIVER_INTENT" --receipt "$DRIVER_RECEIPT" >/dev/null \
    || die "public driver evidence is invalid"

readonly TRACE_PROVISIONAL_KEYS="format_version subject_identity_sha256 run_intent_sha256 workload_metadata_sha256 workload_ready_receipt_sha256 supplemental_evidence_sha256 capture_status requested_duration_ms actual_duration_ms capture_started_continuous_ns capture_ended_continuous_ns trace_bundle_tree_sha256 toc_sha256 time_profile_export_sha256 allocations_export_sha256 hangs_export_sha256 trace_verification_sha256 verifier_sha256 evidence_mode status auth_algorithm provisional_hmac_sha256"
exact_schema "$TRACE_PROVISIONAL_RECEIPT" "$TRACE_PROVISIONAL_KEYS" 22 \
    || die "trace provisional receipt schema is invalid"
[[ "$(kv "$TRACE_PROVISIONAL_RECEIPT" format_version)" == 1 \
    && "$(kv "$TRACE_PROVISIONAL_RECEIPT" run_intent_sha256)" == "$(sha256 "$RUN_INTENT")" \
    && "$(kv "$TRACE_PROVISIONAL_RECEIPT" subject_identity_sha256)" \
        == "$(sha256 "$SUBJECT_IDENTITY")" \
    && "$(kv "$TRACE_PROVISIONAL_RECEIPT" supplemental_evidence_sha256)" \
        == "$(sha256 "$PLAN_START_GATE")" \
    && "$(kv "$TRACE_PROVISIONAL_RECEIPT" capture_status)" == CAPTURED \
    && "$(kv "$TRACE_PROVISIONAL_RECEIPT" evidence_mode)" == "$EVIDENCE_MODE" \
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
    --lifecycle-ready-receipt "$PERFORMANCE_LIFECYCLE_READY_RECEIPT" \
    --appkit-terminator-source "$APPKIT_TERMINATOR_SOURCE" \
    --appkit-terminator-binary "$APPKIT_TERMINATOR_BINARY" \
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
    --appkit-terminator-source "$APPKIT_TERMINATOR_SOURCE"
    --appkit-terminator-binary "$APPKIT_TERMINATOR_BINARY"
)
lifecycle_arguments=(
    --campaign-secret-file "$CAMPAIGN_SECRET_FILE"
    --ready-receipt "$PERFORMANCE_LIFECYCLE_READY_RECEIPT"
    --registration-receipt "$PERFORMANCE_LIFECYCLE_REGISTRATION"
    --run-intent "$RUN_INTENT" --subject-identity "$SUBJECT_IDENTITY"
    --tail-receipt "$PERFORMANCE_TAIL_RECEIPT"
    --workload-metadata "$WORKLOAD_METADATA" --workload-events "$WORKLOAD_EVENTS"
    --workload-ready-receipt "$WORKLOAD_READY_RECEIPT"
    --quit-receipt "$PERFORMANCE_QUIT_RECEIPT"
    --subject-exit-receipt "$SUBJECT_EXIT_RECEIPT"
    --subject-lifecycle-helper "$SUBJECT_LIFECYCLE_HELPER"
    --appkit-terminator-source "$APPKIT_TERMINATOR_SOURCE"
    --appkit-terminator-binary "$APPKIT_TERMINATOR_BINARY"
)
if [[ "$subject" == spaceterm ]]; then
    exit_arguments+=(--native-observation "$NATIVE_OBSERVATION")
    lifecycle_arguments+=(--native-observation "$NATIVE_OBSERVATION")
fi
"$SCRIPT_DIRECTORY/verify-performance-lifecycle-receipts.py" \
    "${lifecycle_arguments[@]}" >/dev/null \
    || die "performance lifecycle receipts are invalid"
"$SCRIPT_DIRECTORY/verify-performance-subject-exit.py" "${exit_arguments[@]}" >/dev/null \
    || die "performance subject exit receipt is invalid"

mkdir -p -- "$(dirname -- "$OUTPUT")"
TEMP="${OUTPUT}.tmp.$$"
trap cleanup EXIT INT TERM
{
    printf 'format_version\t4\n'
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
    printf 'lifecycle_ready_receipt_sha256\t%s\n' "$(sha256 "$PERFORMANCE_LIFECYCLE_READY_RECEIPT")"
    printf 'lifecycle_registration_receipt_sha256\t%s\n' "$(sha256 "$PERFORMANCE_LIFECYCLE_REGISTRATION")"
    printf 'lifecycle_helper_sha256\t%s\n' "$(sha256 "$SUBJECT_LIFECYCLE_HELPER")"
    printf 'terminator_source_sha256\t%s\n' "$(sha256 "$APPKIT_TERMINATOR_SOURCE")"
    printf 'terminator_binary_sha256\t%s\n' "$(sha256 "$APPKIT_TERMINATOR_BINARY")"
    printf 'evidence_mode\t%s\n' "$EVIDENCE_MODE"
    printf 'status\tcomplete\n'
} > "$TEMP"
chmod 0444 "$TEMP"
ln "$TEMP" "$OUTPUT" || die "output path was created concurrently"
rm -f -- "$TEMP"
TEMP=""
trap - EXIT INT TERM
printf 'run_metadata_sha256\t%s\n' "$(sha256 "$OUTPUT")"
