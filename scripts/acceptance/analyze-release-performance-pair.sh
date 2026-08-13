#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
umask 077

SCRIPT_DIRECTORY="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIRECTORY

CAMPAIGN_ID=""
CAMPAIGN_SECRET_FILE=""
SCENARIO=""
PLAN=""
PLAN_METADATA=""
PAIR_METADATA=""
PAIR_RESULT=""
TEMP_ROOT=""

SPACETERM_SUBJECT_IDENTITY=""
SPACETERM_RUN_INTENT=""
SPACETERM_RUN_METADATA=""
SPACETERM_WORKLOAD_METADATA=""
SPACETERM_WORKLOAD_EVENTS=""
SPACETERM_READY_RECEIPT=""
SPACETERM_SESSION_ID=""
SPACETERM_NONCE=""
SPACETERM_DRIVER_EVENTS=""
SPACETERM_DRIVER_INTENT=""
SPACETERM_DRIVER_RECEIPT=""
SPACETERM_WINDOW_IDENTITY=""
SPACETERM_DRIVER_BINARY=""
SPACETERM_DRIVER_SOURCE=""
SPACETERM_DRIVER_CONTROLLER=""
SPACETERM_RSS_SAMPLES=""
SPACETERM_TRACE_METADATA=""
SPACETERM_TRACE_PROVISIONAL_RECEIPT=""
SPACETERM_PERFORMANCE_TAIL_RECEIPT=""
SPACETERM_PERFORMANCE_QUIT_RECEIPT=""
SPACETERM_SUBJECT_EXIT_RECEIPT=""
SPACETERM_PLAN_START_GATE=""
SPACETERM_MANUAL_ARTIFACTS=""
SPACETERM_MANUAL_SCREENSHOT=""
SPACETERM_MANUAL_VIDEO=""
SPACETERM_RUNTIME_SAMPLES=""
SPACETERM_RUNTIME_EVENTS=""
SPACETERM_RUNTIME_METADATA=""
SPACETERM_FAILURE_ACTIONS=""
SPACETERM_NATIVE_LAUNCH_OBSERVATION=""
SPACETERM_NATIVE_PROVISIONAL_OBSERVATION=""
SPACETERM_LIFECYCLE_READY_RECEIPT=""
SPACETERM_LIFECYCLE_REGISTRATION=""
SPACETERM_CASE_REPORT=""

GHOSTTY_SUBJECT_IDENTITY=""
GHOSTTY_RUN_INTENT=""
GHOSTTY_RUN_METADATA=""
GHOSTTY_WORKLOAD_METADATA=""
GHOSTTY_WORKLOAD_EVENTS=""
GHOSTTY_READY_RECEIPT=""
GHOSTTY_SESSION_ID=""
GHOSTTY_NONCE=""
GHOSTTY_DRIVER_EVENTS=""
GHOSTTY_DRIVER_INTENT=""
GHOSTTY_DRIVER_RECEIPT=""
GHOSTTY_WINDOW_IDENTITY=""
GHOSTTY_DRIVER_BINARY=""
GHOSTTY_DRIVER_SOURCE=""
GHOSTTY_DRIVER_CONTROLLER=""
GHOSTTY_RSS_SAMPLES=""
GHOSTTY_TRACE_METADATA=""
GHOSTTY_TRACE_PROVISIONAL_RECEIPT=""
GHOSTTY_PERFORMANCE_TAIL_RECEIPT=""
GHOSTTY_PERFORMANCE_QUIT_RECEIPT=""
GHOSTTY_SUBJECT_EXIT_RECEIPT=""
GHOSTTY_PLAN_START_GATE=""
GHOSTTY_MANUAL_ARTIFACTS=""
GHOSTTY_MANUAL_SCREENSHOT=""
GHOSTTY_MANUAL_VIDEO=""
GHOSTTY_LIFECYCLE_READY_RECEIPT=""
GHOSTTY_LIFECYCLE_REGISTRATION=""
GHOSTTY_CASE_REPORT=""
COMMON_LIFECYCLE_HELPER=""
APPKIT_TERMINATOR_SOURCE=""
APPKIT_TERMINATOR_BINARY=""

usage() {
    cat <<EOF
Usage: $(basename -- "$0") --campaign-id ID --campaign-secret-file FILE \\
  --scenario NAME --plan FILE --plan-metadata FILE --pair-metadata FILE \\
  --pair-result FILE [SPACETERM EVIDENCE] [GHOSTTY EVIDENCE]

Analyze both already-complete production evidence sets and print the only
release-performance PASS verdict. Every single-subject analyzer must first
return CASE-COMPLETE, and the authenticated pair result is independently
replayed before and after those analyses.

Each subject requires prefixed forms of: --subject-identity, --run-intent,
--run-metadata, --workload-metadata, --workload-events, --ready-receipt,
--session-id, --nonce, --driver-events, --driver-intent, --driver-receipt,
--window-identity, --driver-binary, --driver-source, --driver-controller,
--rss-samples, --trace-metadata, --trace-provisional-receipt,
--performance-tail-receipt, --performance-quit-receipt,
--subject-exit-receipt, --plan-start-gate, --manual-artifacts,
--manual-screenshot, and --manual-video.

SpaceTerm additionally requires prefixed --runtime-samples, --runtime-events,
--runtime-metadata, --failure-actions, --native-launch-observation, and
--native-provisional-observation.

Both subjects also require prefixed --lifecycle-ready-receipt and
--lifecycle-registration. The pair requires --common-lifecycle-helper,
--appkit-terminator-source, and --appkit-terminator-binary.
EOF
}

verdict() {
    local result="$1" reason="$2"
    printf 'format_version\t1\n'
    printf 'campaign_id\t%s\n' "${CAMPAIGN_ID:-unknown}"
    printf 'scenario\t%s\n' "${SCENARIO:-unknown}"
    printf 'result\t%s\n' "$result"
    printf 'reason\t%s\n' "$reason"
    case "$result" in
        FAIL) exit 1 ;;
        NOT-RUN) exit 2 ;;
        *) exit 3 ;;
    esac
}

fail() { verdict FAIL "$1"; }
not_run() { verdict NOT-RUN "$1"; }
sha256() { shasum -a 256 "$1" | awk '{ print $1 }'; }
kv() {
    awk -F '\t' -v wanted="$2" '
        NF != 2 { bad = 1 }
        $1 == wanted { count += 1; value = $2 }
        END { if (!bad && count == 1) print value }
    ' "$1"
}
cleanup() { [[ -z "$TEMP_ROOT" ]] || rm -rf -- "$TEMP_ROOT"; }

while (( $# > 0 )); do
    case "$1" in
        --campaign-id) CAMPAIGN_ID="${2:-}"; shift ;;
        --campaign-secret-file) CAMPAIGN_SECRET_FILE="${2:-}"; shift ;;
        --scenario) SCENARIO="${2:-}"; shift ;;
        --plan) PLAN="${2:-}"; shift ;;
        --plan-metadata) PLAN_METADATA="${2:-}"; shift ;;
        --pair-metadata) PAIR_METADATA="${2:-}"; shift ;;
        --pair-result) PAIR_RESULT="${2:-}"; shift ;;
        --spaceterm-subject-identity) SPACETERM_SUBJECT_IDENTITY="${2:-}"; shift ;;
        --spaceterm-run-intent) SPACETERM_RUN_INTENT="${2:-}"; shift ;;
        --spaceterm-run-metadata) SPACETERM_RUN_METADATA="${2:-}"; shift ;;
        --spaceterm-workload-metadata) SPACETERM_WORKLOAD_METADATA="${2:-}"; shift ;;
        --spaceterm-workload-events) SPACETERM_WORKLOAD_EVENTS="${2:-}"; shift ;;
        --spaceterm-ready-receipt) SPACETERM_READY_RECEIPT="${2:-}"; shift ;;
        --spaceterm-session-id) SPACETERM_SESSION_ID="${2:-}"; shift ;;
        --spaceterm-nonce) SPACETERM_NONCE="${2:-}"; shift ;;
        --spaceterm-driver-events) SPACETERM_DRIVER_EVENTS="${2:-}"; shift ;;
        --spaceterm-driver-intent) SPACETERM_DRIVER_INTENT="${2:-}"; shift ;;
        --spaceterm-driver-receipt) SPACETERM_DRIVER_RECEIPT="${2:-}"; shift ;;
        --spaceterm-window-identity) SPACETERM_WINDOW_IDENTITY="${2:-}"; shift ;;
        --spaceterm-driver-binary) SPACETERM_DRIVER_BINARY="${2:-}"; shift ;;
        --spaceterm-driver-source) SPACETERM_DRIVER_SOURCE="${2:-}"; shift ;;
        --spaceterm-driver-controller) SPACETERM_DRIVER_CONTROLLER="${2:-}"; shift ;;
        --spaceterm-rss-samples) SPACETERM_RSS_SAMPLES="${2:-}"; shift ;;
        --spaceterm-trace-metadata) SPACETERM_TRACE_METADATA="${2:-}"; shift ;;
        --spaceterm-trace-provisional-receipt) SPACETERM_TRACE_PROVISIONAL_RECEIPT="${2:-}"; shift ;;
        --spaceterm-performance-tail-receipt) SPACETERM_PERFORMANCE_TAIL_RECEIPT="${2:-}"; shift ;;
        --spaceterm-performance-quit-receipt) SPACETERM_PERFORMANCE_QUIT_RECEIPT="${2:-}"; shift ;;
        --spaceterm-subject-exit-receipt) SPACETERM_SUBJECT_EXIT_RECEIPT="${2:-}"; shift ;;
        --spaceterm-plan-start-gate) SPACETERM_PLAN_START_GATE="${2:-}"; shift ;;
        --spaceterm-manual-artifacts) SPACETERM_MANUAL_ARTIFACTS="${2:-}"; shift ;;
        --spaceterm-manual-screenshot) SPACETERM_MANUAL_SCREENSHOT="${2:-}"; shift ;;
        --spaceterm-manual-video) SPACETERM_MANUAL_VIDEO="${2:-}"; shift ;;
        --spaceterm-runtime-samples) SPACETERM_RUNTIME_SAMPLES="${2:-}"; shift ;;
        --spaceterm-runtime-events) SPACETERM_RUNTIME_EVENTS="${2:-}"; shift ;;
        --spaceterm-runtime-metadata) SPACETERM_RUNTIME_METADATA="${2:-}"; shift ;;
        --spaceterm-failure-actions) SPACETERM_FAILURE_ACTIONS="${2:-}"; shift ;;
        --spaceterm-native-launch-observation) SPACETERM_NATIVE_LAUNCH_OBSERVATION="${2:-}"; shift ;;
        --spaceterm-native-provisional-observation) SPACETERM_NATIVE_PROVISIONAL_OBSERVATION="${2:-}"; shift ;;
        --spaceterm-lifecycle-ready-receipt) SPACETERM_LIFECYCLE_READY_RECEIPT="${2:-}"; shift ;;
        --spaceterm-lifecycle-registration) SPACETERM_LIFECYCLE_REGISTRATION="${2:-}"; shift ;;
        --spaceterm-case-report) SPACETERM_CASE_REPORT="${2:-}"; shift ;;
        --ghostty-subject-identity) GHOSTTY_SUBJECT_IDENTITY="${2:-}"; shift ;;
        --ghostty-run-intent) GHOSTTY_RUN_INTENT="${2:-}"; shift ;;
        --ghostty-run-metadata) GHOSTTY_RUN_METADATA="${2:-}"; shift ;;
        --ghostty-workload-metadata) GHOSTTY_WORKLOAD_METADATA="${2:-}"; shift ;;
        --ghostty-workload-events) GHOSTTY_WORKLOAD_EVENTS="${2:-}"; shift ;;
        --ghostty-ready-receipt) GHOSTTY_READY_RECEIPT="${2:-}"; shift ;;
        --ghostty-session-id) GHOSTTY_SESSION_ID="${2:-}"; shift ;;
        --ghostty-nonce) GHOSTTY_NONCE="${2:-}"; shift ;;
        --ghostty-driver-events) GHOSTTY_DRIVER_EVENTS="${2:-}"; shift ;;
        --ghostty-driver-intent) GHOSTTY_DRIVER_INTENT="${2:-}"; shift ;;
        --ghostty-driver-receipt) GHOSTTY_DRIVER_RECEIPT="${2:-}"; shift ;;
        --ghostty-window-identity) GHOSTTY_WINDOW_IDENTITY="${2:-}"; shift ;;
        --ghostty-driver-binary) GHOSTTY_DRIVER_BINARY="${2:-}"; shift ;;
        --ghostty-driver-source) GHOSTTY_DRIVER_SOURCE="${2:-}"; shift ;;
        --ghostty-driver-controller) GHOSTTY_DRIVER_CONTROLLER="${2:-}"; shift ;;
        --ghostty-rss-samples) GHOSTTY_RSS_SAMPLES="${2:-}"; shift ;;
        --ghostty-trace-metadata) GHOSTTY_TRACE_METADATA="${2:-}"; shift ;;
        --ghostty-trace-provisional-receipt) GHOSTTY_TRACE_PROVISIONAL_RECEIPT="${2:-}"; shift ;;
        --ghostty-performance-tail-receipt) GHOSTTY_PERFORMANCE_TAIL_RECEIPT="${2:-}"; shift ;;
        --ghostty-performance-quit-receipt) GHOSTTY_PERFORMANCE_QUIT_RECEIPT="${2:-}"; shift ;;
        --ghostty-subject-exit-receipt) GHOSTTY_SUBJECT_EXIT_RECEIPT="${2:-}"; shift ;;
        --ghostty-plan-start-gate) GHOSTTY_PLAN_START_GATE="${2:-}"; shift ;;
        --ghostty-manual-artifacts) GHOSTTY_MANUAL_ARTIFACTS="${2:-}"; shift ;;
        --ghostty-manual-screenshot) GHOSTTY_MANUAL_SCREENSHOT="${2:-}"; shift ;;
        --ghostty-manual-video) GHOSTTY_MANUAL_VIDEO="${2:-}"; shift ;;
        --ghostty-lifecycle-ready-receipt) GHOSTTY_LIFECYCLE_READY_RECEIPT="${2:-}"; shift ;;
        --ghostty-lifecycle-registration) GHOSTTY_LIFECYCLE_REGISTRATION="${2:-}"; shift ;;
        --ghostty-case-report) GHOSTTY_CASE_REPORT="${2:-}"; shift ;;
        --common-lifecycle-helper) COMMON_LIFECYCLE_HELPER="${2:-}"; shift ;;
        --appkit-terminator-source) APPKIT_TERMINATOR_SOURCE="${2:-}"; shift ;;
        --appkit-terminator-binary) APPKIT_TERMINATOR_BINARY="${2:-}"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; not_run unknown-argument ;;
    esac
    shift
done

for command in awk chmod cp mktemp python3 rm shasum; do
    command -v "$command" >/dev/null 2>&1 || not_run "${command}-unavailable"
done
[[ "$CAMPAIGN_ID" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,79}$ ]] \
    || not_run invalid-campaign-id
case "$SCENARIO" in
    ascii|unicode-styles|scrolled|hidden-occluded|resize) ;;
    *) not_run invalid-scenario ;;
esac

readonly CASE_ANALYZER="$SCRIPT_DIRECTORY/analyze-release-performance-case.sh"
readonly PAIR_TOOL="$SCRIPT_DIRECTORY/performance-pair-result.py"
[[ -x "$CASE_ANALYZER" && -x "$PAIR_TOOL" ]] || not_run pair-analyzer-tool-missing

pair_arguments=(
    --campaign-secret-file "$CAMPAIGN_SECRET_FILE" --campaign-id "$CAMPAIGN_ID"
    --pair-metadata "$PAIR_METADATA" --scenario-plan "$PLAN"
    --spaceterm-subject-identity "$SPACETERM_SUBJECT_IDENTITY"
    --spaceterm-run-intent "$SPACETERM_RUN_INTENT"
    --spaceterm-run-metadata "$SPACETERM_RUN_METADATA"
    --spaceterm-window-identity "$SPACETERM_WINDOW_IDENTITY"
    --spaceterm-driver-intent "$SPACETERM_DRIVER_INTENT"
    --spaceterm-driver-events "$SPACETERM_DRIVER_EVENTS"
    --spaceterm-driver-receipt "$SPACETERM_DRIVER_RECEIPT"
    --spaceterm-driver-binary "$SPACETERM_DRIVER_BINARY"
    --spaceterm-driver-source "$SPACETERM_DRIVER_SOURCE"
    --spaceterm-driver-controller "$SPACETERM_DRIVER_CONTROLLER"
    --spaceterm-plan-start-gate "$SPACETERM_PLAN_START_GATE"
    --spaceterm-trace-provisional-receipt "$SPACETERM_TRACE_PROVISIONAL_RECEIPT"
    --spaceterm-workload-metadata "$SPACETERM_WORKLOAD_METADATA"
    --spaceterm-workload-events "$SPACETERM_WORKLOAD_EVENTS"
    --spaceterm-workload-ready-receipt "$SPACETERM_READY_RECEIPT"
    --spaceterm-lifecycle-ready-receipt "$SPACETERM_LIFECYCLE_READY_RECEIPT"
    --spaceterm-lifecycle-registration "$SPACETERM_LIFECYCLE_REGISTRATION"
    --spaceterm-case-report "$SPACETERM_CASE_REPORT"
    --spaceterm-trace-metadata "$SPACETERM_TRACE_METADATA"
    --spaceterm-trace-archive "$(dirname -- "$SPACETERM_TRACE_METADATA")/spaceterm-$SCENARIO.trace"
    --spaceterm-manual-artifacts "$SPACETERM_MANUAL_ARTIFACTS"
    --spaceterm-manual-screenshot "$SPACETERM_MANUAL_SCREENSHOT"
    --spaceterm-manual-video "$SPACETERM_MANUAL_VIDEO"
    --spaceterm-tail-receipt "$SPACETERM_PERFORMANCE_TAIL_RECEIPT"
    --spaceterm-quit-receipt "$SPACETERM_PERFORMANCE_QUIT_RECEIPT"
    --spaceterm-exit-receipt "$SPACETERM_SUBJECT_EXIT_RECEIPT"
    --ghostty-subject-identity "$GHOSTTY_SUBJECT_IDENTITY"
    --ghostty-run-intent "$GHOSTTY_RUN_INTENT"
    --ghostty-run-metadata "$GHOSTTY_RUN_METADATA"
    --ghostty-window-identity "$GHOSTTY_WINDOW_IDENTITY"
    --ghostty-driver-intent "$GHOSTTY_DRIVER_INTENT"
    --ghostty-driver-events "$GHOSTTY_DRIVER_EVENTS"
    --ghostty-driver-receipt "$GHOSTTY_DRIVER_RECEIPT"
    --ghostty-driver-binary "$GHOSTTY_DRIVER_BINARY"
    --ghostty-driver-source "$GHOSTTY_DRIVER_SOURCE"
    --ghostty-driver-controller "$GHOSTTY_DRIVER_CONTROLLER"
    --ghostty-plan-start-gate "$GHOSTTY_PLAN_START_GATE"
    --ghostty-trace-provisional-receipt "$GHOSTTY_TRACE_PROVISIONAL_RECEIPT"
    --ghostty-workload-metadata "$GHOSTTY_WORKLOAD_METADATA"
    --ghostty-workload-events "$GHOSTTY_WORKLOAD_EVENTS"
    --ghostty-workload-ready-receipt "$GHOSTTY_READY_RECEIPT"
    --ghostty-lifecycle-ready-receipt "$GHOSTTY_LIFECYCLE_READY_RECEIPT"
    --ghostty-lifecycle-registration "$GHOSTTY_LIFECYCLE_REGISTRATION"
    --ghostty-case-report "$GHOSTTY_CASE_REPORT"
    --ghostty-trace-metadata "$GHOSTTY_TRACE_METADATA"
    --ghostty-trace-archive "$(dirname -- "$GHOSTTY_TRACE_METADATA")/ghostty-$SCENARIO.trace"
    --ghostty-manual-artifacts "$GHOSTTY_MANUAL_ARTIFACTS"
    --ghostty-manual-screenshot "$GHOSTTY_MANUAL_SCREENSHOT"
    --ghostty-manual-video "$GHOSTTY_MANUAL_VIDEO"
    --ghostty-tail-receipt "$GHOSTTY_PERFORMANCE_TAIL_RECEIPT"
    --ghostty-quit-receipt "$GHOSTTY_PERFORMANCE_QUIT_RECEIPT"
    --ghostty-exit-receipt "$GHOSTTY_SUBJECT_EXIT_RECEIPT"
    --spaceterm-native-provisional-observation "$SPACETERM_NATIVE_PROVISIONAL_OBSERVATION"
    --spaceterm-native-observation "$SPACETERM_NATIVE_LAUNCH_OBSERVATION"
    --spaceterm-native-runtime-metadata "$SPACETERM_RUNTIME_METADATA"
    --spaceterm-native-runtime-samples "$SPACETERM_RUNTIME_SAMPLES"
    --spaceterm-native-runtime-events "$SPACETERM_RUNTIME_EVENTS"
    --spaceterm-native-failure-actions "$SPACETERM_FAILURE_ACTIONS"
    --common-lifecycle-helper "$COMMON_LIFECYCLE_HELPER"
    --appkit-terminator-source "$APPKIT_TERMINATOR_SOURCE"
    --appkit-terminator-binary "$APPKIT_TERMINATOR_BINARY"
)

TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-pair-analysis.XXXXXX")"
readonly TEMP_ROOT
trap cleanup EXIT INT TERM
PAIR_RESULT_SNAPSHOT="$TEMP_ROOT/pair-result.tsv"
pair_result_hash="$(sha256 "$PAIR_RESULT" 2>/dev/null)" \
    || not_run pair-result-unavailable
cp -- "$PAIR_RESULT" "$PAIR_RESULT_SNAPSHOT" 2>/dev/null \
    || not_run pair-result-snapshot-failed
chmod 0400 "$PAIR_RESULT_SNAPSHOT"
[[ "$(sha256 "$PAIR_RESULT_SNAPSHOT")" == "$pair_result_hash" \
    && "$(sha256 "$PAIR_RESULT")" == "$pair_result_hash" ]] \
    || not_run pair-result-changed-during-snapshot

python3 "$PAIR_TOOL" verify "${pair_arguments[@]}" --receipt "$PAIR_RESULT_SNAPSHOT" \
    >/dev/null 2>&1 || not_run pair-result-verification-failed
[[ "$(kv "$PAIR_RESULT_SNAPSHOT" evidence_mode)" == production \
    && "$(kv "$PAIR_RESULT_SNAPSHOT" status)" == complete \
    && "$(kv "$PAIR_RESULT_SNAPSHOT" campaign_id)" == "$CAMPAIGN_ID" \
    && "$(kv "$PAIR_RESULT_SNAPSHOT" pair_metadata_sha256)" == "$(sha256 "$PAIR_METADATA")" \
    && "$(kv "$PAIR_RESULT_SNAPSHOT" scenario_plan_sha256)" == "$(sha256 "$PLAN")" ]] \
    || not_run pair-result-production-binding-failed

run_case() {
    local subject="$1" output="$2" status=0
    local identity intent metadata workload_metadata workload_events ready session nonce
    local driver_events driver_intent driver_receipt window driver_binary driver_source
    local driver_controller rss trace trace_provisional tail quit exit_receipt gate manual
    local screenshot video
    local expected_report
    local expected_trace_hash expected_manual_hash expected_screenshot_hash expected_video_hash
    if [[ "$subject" == spaceterm ]]; then
        identity="$SPACETERM_SUBJECT_IDENTITY"; intent="$SPACETERM_RUN_INTENT"
        metadata="$SPACETERM_RUN_METADATA"; workload_metadata="$SPACETERM_WORKLOAD_METADATA"
        workload_events="$SPACETERM_WORKLOAD_EVENTS"; ready="$SPACETERM_READY_RECEIPT"
        session="$SPACETERM_SESSION_ID"; nonce="$SPACETERM_NONCE"
        driver_events="$SPACETERM_DRIVER_EVENTS"; driver_intent="$SPACETERM_DRIVER_INTENT"
        driver_receipt="$SPACETERM_DRIVER_RECEIPT"; window="$SPACETERM_WINDOW_IDENTITY"
        driver_binary="$SPACETERM_DRIVER_BINARY"; driver_source="$SPACETERM_DRIVER_SOURCE"
        driver_controller="$SPACETERM_DRIVER_CONTROLLER"; rss="$SPACETERM_RSS_SAMPLES"
        trace="$SPACETERM_TRACE_METADATA"
        trace_provisional="$SPACETERM_TRACE_PROVISIONAL_RECEIPT"
        tail="$SPACETERM_PERFORMANCE_TAIL_RECEIPT"; quit="$SPACETERM_PERFORMANCE_QUIT_RECEIPT"
        exit_receipt="$SPACETERM_SUBJECT_EXIT_RECEIPT"; gate="$SPACETERM_PLAN_START_GATE"
        manual="$SPACETERM_MANUAL_ARTIFACTS"; screenshot="$SPACETERM_MANUAL_SCREENSHOT"
        video="$SPACETERM_MANUAL_VIDEO"
        expected_report="$SPACETERM_CASE_REPORT"
    else
        identity="$GHOSTTY_SUBJECT_IDENTITY"; intent="$GHOSTTY_RUN_INTENT"
        metadata="$GHOSTTY_RUN_METADATA"; workload_metadata="$GHOSTTY_WORKLOAD_METADATA"
        workload_events="$GHOSTTY_WORKLOAD_EVENTS"; ready="$GHOSTTY_READY_RECEIPT"
        session="$GHOSTTY_SESSION_ID"; nonce="$GHOSTTY_NONCE"
        driver_events="$GHOSTTY_DRIVER_EVENTS"; driver_intent="$GHOSTTY_DRIVER_INTENT"
        driver_receipt="$GHOSTTY_DRIVER_RECEIPT"; window="$GHOSTTY_WINDOW_IDENTITY"
        driver_binary="$GHOSTTY_DRIVER_BINARY"; driver_source="$GHOSTTY_DRIVER_SOURCE"
        driver_controller="$GHOSTTY_DRIVER_CONTROLLER"; rss="$GHOSTTY_RSS_SAMPLES"
        trace="$GHOSTTY_TRACE_METADATA"; trace_provisional="$GHOSTTY_TRACE_PROVISIONAL_RECEIPT"
        tail="$GHOSTTY_PERFORMANCE_TAIL_RECEIPT"; quit="$GHOSTTY_PERFORMANCE_QUIT_RECEIPT"
        exit_receipt="$GHOSTTY_SUBJECT_EXIT_RECEIPT"; gate="$GHOSTTY_PLAN_START_GATE"
        manual="$GHOSTTY_MANUAL_ARTIFACTS"; screenshot="$GHOSTTY_MANUAL_SCREENSHOT"
        video="$GHOSTTY_MANUAL_VIDEO"
        expected_report="$GHOSTTY_CASE_REPORT"
    fi
    expected_trace_hash="$(sha256 "$trace")" || not_run "$subject-trace-unavailable"
    expected_manual_hash="$(sha256 "$manual")" || not_run "$subject-manual-unavailable"
    expected_screenshot_hash="$(sha256 "$screenshot")" \
        || not_run "$subject-screenshot-unavailable"
    expected_video_hash="$(sha256 "$video")" || not_run "$subject-video-unavailable"
    local -a arguments=(
        --subject "$subject" --scenario "$SCENARIO" --plan "$PLAN"
        --plan-metadata "$PLAN_METADATA" --pair-metadata "$PAIR_METADATA"
        --subject-identity "$identity" --run-intent "$intent" --run-metadata "$metadata"
        --workload-metadata "$workload_metadata" --workload-events "$workload_events"
        --ready-receipt "$ready" --campaign-id "$CAMPAIGN_ID" --session-id "$session"
        --nonce "$nonce" --campaign-secret-file "$CAMPAIGN_SECRET_FILE"
        --driver-events "$driver_events" --driver-intent "$driver_intent"
        --driver-receipt "$driver_receipt" --window-identity "$window"
        --driver-binary "$driver_binary" --driver-source "$driver_source"
        --driver-controller "$driver_controller" --rss-samples "$rss"
        --trace-metadata "$trace" --trace-provisional-receipt "$trace_provisional"
        --performance-tail-receipt "$tail" --performance-quit-receipt "$quit"
        --subject-exit-receipt "$exit_receipt" --plan-start-gate "$gate"
        --performance-lifecycle-ready-receipt \
            "$([[ "$subject" == spaceterm ]] && printf '%s' "$SPACETERM_LIFECYCLE_READY_RECEIPT" || printf '%s' "$GHOSTTY_LIFECYCLE_READY_RECEIPT")"
        --performance-lifecycle-registration \
            "$([[ "$subject" == spaceterm ]] && printf '%s' "$SPACETERM_LIFECYCLE_REGISTRATION" || printf '%s' "$GHOSTTY_LIFECYCLE_REGISTRATION")"
        --subject-lifecycle-helper "$COMMON_LIFECYCLE_HELPER"
        --appkit-terminator-source "$APPKIT_TERMINATOR_SOURCE"
        --appkit-terminator-binary "$APPKIT_TERMINATOR_BINARY"
        --manual-artifacts "$manual" --manual-screenshot "$screenshot" --manual-video "$video"
    )
    if [[ "$subject" == spaceterm ]]; then
        arguments+=(
            --runtime-samples "$SPACETERM_RUNTIME_SAMPLES"
            --runtime-events "$SPACETERM_RUNTIME_EVENTS"
            --runtime-metadata "$SPACETERM_RUNTIME_METADATA"
            --failure-actions "$SPACETERM_FAILURE_ACTIONS"
            --native-launch-observation "$SPACETERM_NATIVE_LAUNCH_OBSERVATION"
            --native-provisional-observation "$SPACETERM_NATIVE_PROVISIONAL_OBSERVATION"
        )
    fi
    "$CASE_ANALYZER" "${arguments[@]}" > "$output" || status=$?
    case "$status" in
        0)
            [[ "$(wc -l < "$output" | tr -d ' ')" == 14 \
                && "$(kv "$output" format_version)" == 2 \
                && "$(kv "$output" subject)" == "$subject" \
                && "$(kv "$output" scenario)" == "$SCENARIO" \
                && "$(kv "$output" session_id)" \
                    == "$(kv "$PAIR_RESULT_SNAPSHOT" "${subject}_session_id")" \
                && "$(kv "$output" nonce)" \
                    == "$(kv "$PAIR_RESULT_SNAPSHOT" "${subject}_nonce")" \
                && "$(kv "$output" run_intent_sha256)" \
                    == "$(kv "$PAIR_RESULT_SNAPSHOT" "${subject}_run_intent_sha256")" \
                && "$(kv "$output" run_metadata_sha256)" \
                    == "$(kv "$PAIR_RESULT_SNAPSHOT" "${subject}_run_metadata_sha256")" \
                && "$(kv "$output" trace_metadata_sha256)" == "$expected_trace_hash" \
                && "$(kv "$output" manual_artifacts_sha256)" == "$expected_manual_hash" \
                && "$(kv "$output" manual_screenshot_sha256)" == "$expected_screenshot_hash" \
                && "$(kv "$output" manual_video_sha256)" == "$expected_video_hash" \
                && "$(sha256 "$trace")" == "$expected_trace_hash" \
                && "$(sha256 "$manual")" == "$expected_manual_hash" \
                && "$(sha256 "$screenshot")" == "$expected_screenshot_hash" \
                && "$(sha256 "$video")" == "$expected_video_hash" \
                && "$(kv "$output" result)" == CASE-COMPLETE \
                && "$(kv "$output" reason)" == all-required-evidence-complete ]] \
                || not_run "$subject-case-report-invalid"
            [[ -f "$expected_report" && ! -L "$expected_report" \
                && "$(stat -f '%Lp' "$expected_report")" == 400 \
                && "$(sha256 "$output")" == "$(sha256 "$expected_report")" ]] \
                || not_run "$subject-frozen-case-report-mismatch"
            ;;
        1) fail "$subject-case-failed" ;;
        2) not_run "$subject-case-not-runnable" ;;
        *) not_run "$subject-case-analyzer-error" ;;
    esac
}

SPACETERM_REPORT="$TEMP_ROOT/spaceterm-report.tsv"
GHOSTTY_REPORT="$TEMP_ROOT/ghostty-report.tsv"
run_case spaceterm "$SPACETERM_REPORT"
run_case ghostty "$GHOSTTY_REPORT"

# Replay after analysis so no core evidence replacement can bridge analysis and PASS.
python3 "$PAIR_TOOL" verify "${pair_arguments[@]}" --receipt "$PAIR_RESULT_SNAPSHOT" \
    >/dev/null 2>&1 || not_run pair-result-changed-during-analysis

printf 'format_version\t1\n'
printf 'campaign_id\t%s\n' "$CAMPAIGN_ID"
printf 'scenario\t%s\n' "$SCENARIO"
printf 'pair_metadata_sha256\t%s\n' "$(sha256 "$PAIR_METADATA")"
printf 'pair_result_sha256\t%s\n' "$pair_result_hash"
printf 'spaceterm_case_report_sha256\t%s\n' "$(sha256 "$SPACETERM_REPORT")"
printf 'ghostty_case_report_sha256\t%s\n' "$(sha256 "$GHOSTTY_REPORT")"
printf 'result\tPASS\n'
printf 'reason\tall-required-paired-evidence-passed\n'
