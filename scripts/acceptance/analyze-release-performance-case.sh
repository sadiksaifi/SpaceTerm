#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C

SCRIPT_DIRECTORY="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIRECTORY

SUBJECT=""
SCENARIO=""
PLAN=""
PLAN_METADATA=""
PAIR_METADATA=""
SUBJECT_IDENTITY=""
RUN_INTENT=""
RUN_METADATA=""
WORKLOAD_METADATA=""
WORKLOAD_EVENTS=""
READY_RECEIPT=""
DRIVER_EVENTS=""
DRIVER_INTENT=""
DRIVER_RECEIPT=""
WINDOW_IDENTITY=""
DRIVER_BINARY=""
DRIVER_SOURCE=""
DRIVER_CONTROLLER=""
RSS_SAMPLES=""
RUNTIME_SAMPLES=""
RUNTIME_EVENTS=""
RUNTIME_METADATA=""
FAILURE_ACTIONS=""
NATIVE_LAUNCH_OBSERVATION=""
NATIVE_PROVISIONAL_OBSERVATION=""
TRACE_METADATA=""
TRACE_PROVISIONAL_RECEIPT=""
PERFORMANCE_TAIL_RECEIPT=""
PERFORMANCE_QUIT_RECEIPT=""
SUBJECT_EXIT_RECEIPT=""
PERFORMANCE_LIFECYCLE_READY_RECEIPT=""
PERFORMANCE_LIFECYCLE_REGISTRATION=""
SUBJECT_LIFECYCLE_HELPER=""
COMMON_LIFECYCLE_HELPER=""
APPKIT_TERMINATOR_SOURCE=""
APPKIT_TERMINATOR_BINARY=""
PLAN_START_GATE=""
MANUAL_ARTIFACTS=""
MANUAL_SCREENSHOT=""
MANUAL_VIDEO=""
CAMPAIGN_ID=""
SESSION_ID=""
NONCE=""
CAMPAIGN_SECRET_FILE=""
AUTH_SNAPSHOT_DIR=""
TRACE_METADATA_SHA256="unavailable"
TRACE_ARCHIVE_SHA256="unavailable"
MANUAL_ARTIFACTS_SHA256="unavailable"
MANUAL_SCREENSHOT_SHA256="unavailable"
MANUAL_VIDEO_SHA256="unavailable"

readonly PLAN_HEADER=$'event_id\toffset_ms\taction\targ0\targ1'
readonly WORKLOAD_HEADER=$'sequence\tcontinuous_ns\tkind\tevent_id\tbyte_count\trows\tcolumns\tpixel_width\tpixel_height\tstatus'
readonly DRIVER_HEADER=$'sequence\tcontinuous_ns\tevent_id\taction\ttarget_pid\twindow_number\trequested_a\trequested_b\tobserved_a\tobserved_b\tresult'
readonly RSS_HEADER=$'elapsed_ms\tcontinuous_ns\trss_kib\tworkload_bytes\tresize_count'
readonly RUNTIME_SAMPLE_HEADER=$'sequence\tcontinuous_ns\tworker_generation\tscreens_published\tscreens_enqueued\tscreens_superseded\tevent_queue_length\tevent_queue_high_water\tui_dispatches\tui_screen_events\tui_drain_high_water\tui_latest_generation\trender_latest_generation\tnext_frame_generation\tnext_frame_count\tpresentable\tminimized\toccluded\tworkspace_visible\tpane_visible\tlive_resize\tviewport_total_rows\tviewport_visible_rows\tviewport_offset_rows\tselection_present\tresize_requests\tresize_notifications\tresize_applied\tresize_coalesced\tpty_rows\tpty_columns\tpty_pixel_width\tpty_pixel_height\tterminal_inputs_accepted\tlifecycle\tobserver_drops'
readonly RUNTIME_EVENT_HEADER=$'sequence\tcontinuous_ns\tkind\tgeneration\taux0\taux1'

usage() {
    cat <<EOF
Usage: $(basename -- "$0") --subject spaceterm|ghostty --scenario NAME \\
  --plan FILE --plan-metadata FILE --pair-metadata FILE \\
  --subject-identity FILE --run-intent FILE --run-metadata FILE \\
  --workload-metadata FILE --workload-events FILE --ready-receipt FILE \\
  --campaign-id ID --session-id ID --nonce 64_LOWER_HEX \\
  --campaign-secret-file FILE \\
  --driver-events FILE --driver-intent FILE --driver-receipt FILE \\
  --window-identity FILE --driver-binary FILE --driver-source FILE \\
  --driver-controller FILE --rss-samples FILE \\
  --trace-metadata FILE --trace-provisional-receipt FILE \\
  --performance-tail-receipt FILE --performance-quit-receipt FILE \\
  --subject-exit-receipt FILE \\
  --performance-lifecycle-ready-receipt FILE \\
  --performance-lifecycle-registration FILE --subject-lifecycle-helper FILE \\
  --common-lifecycle-helper FILE \\
  --appkit-terminator-source FILE --appkit-terminator-binary FILE \\
  --plan-start-gate FILE \\
  --manual-artifacts FILE --manual-screenshot FILE --manual-video FILE \
  [SPACETERM RUNTIME FILES]

SpaceTerm runtime files:
  --runtime-samples FILE --runtime-events FILE --runtime-metadata FILE
  --failure-actions FILE
  --native-launch-observation FILE
  --native-provisional-observation FILE

Print a content-free CASE-COMPLETE, FAIL, or NOT-RUN verdict for one native
release-performance case. CASE-COMPLETE is non-final; only the paired analyzer
may emit PASS after both subjects and the authenticated pair closure verify.
EOF
}

verdict() {
    local result="$1"
    local reason="$2"
    printf 'format_version\t2\n'
    printf 'subject\t%s\n' "${SUBJECT:-unknown}"
    printf 'scenario\t%s\n' "${SCENARIO:-unknown}"
    printf 'session_id\t%s\n' "${SESSION_ID:-unknown}"
    printf 'nonce\t%s\n' "${NONCE:-unknown}"
    if [[ -f "${RUN_INTENT:-}" && ! -L "${RUN_INTENT:-}" ]]; then
        printf 'run_intent_sha256\t%s\n' "$(shasum -a 256 "$RUN_INTENT" | awk '{print $1}')"
    else
        printf 'run_intent_sha256\tunavailable\n'
    fi
    if [[ -f "${RUN_METADATA:-}" && ! -L "${RUN_METADATA:-}" ]]; then
        printf 'run_metadata_sha256\t%s\n' "$(shasum -a 256 "$RUN_METADATA" | awk '{print $1}')"
    else
        printf 'run_metadata_sha256\tunavailable\n'
    fi
    printf 'trace_metadata_sha256\t%s\n' "$TRACE_METADATA_SHA256"
    printf 'trace_archive_sha256\t%s\n' "$TRACE_ARCHIVE_SHA256"
    printf 'manual_artifacts_sha256\t%s\n' "$MANUAL_ARTIFACTS_SHA256"
    printf 'manual_screenshot_sha256\t%s\n' "$MANUAL_SCREENSHOT_SHA256"
    printf 'manual_video_sha256\t%s\n' "$MANUAL_VIDEO_SHA256"
    printf 'result\t%s\n' "$result"
    printf 'reason\t%s\n' "$reason"
    case "$result" in
        CASE-COMPLETE) exit 0 ;;
        FAIL) exit 1 ;;
        NOT-RUN) exit 2 ;;
        *) exit 3 ;;
    esac
}

not_run() { verdict NOT-RUN "$1"; }
fail() { verdict FAIL "$1"; }

# shellcheck disable=SC2329  # invoked by the authentication snapshot trap
cleanup() {
    [[ -z "$AUTH_SNAPSHOT_DIR" ]] || rm -rf -- "$AUTH_SNAPSHOT_DIR"
}

require_file() {
    [[ -f "$2" && -r "$2" ]] || not_run "missing-$1"
}

sha256() {
    shasum -a 256 "$1" | awk '{ print $1 }'
}

trace_tree_sha256() {
    python3 - "$1" <<'PY'
import hashlib, pathlib, struct, sys, unicodedata
root = pathlib.Path(sys.argv[1])
if not root.is_dir() or root.is_symlink(): raise SystemExit(1)
digest = hashlib.sha256(b"spaceterm.performance.trace-tree/v1\0")
entries = []
for path in root.rglob("*"):
    if path.is_symlink() or (path.exists() and not path.is_file() and not path.is_dir()):
        raise SystemExit(1)
    if path.is_file():
        relative = unicodedata.normalize("NFC", path.relative_to(root).as_posix())
        if relative != path.relative_to(root).as_posix(): raise SystemExit(1)
        entries.append((relative.encode(), path))
for encoded, path in sorted(entries):
    data = path.read_bytes()
    digest.update(struct.pack(">Q", len(encoded))); digest.update(encoded)
    digest.update(struct.pack(">Q", len(data))); digest.update(data)
print(digest.hexdigest())
PY
}

kv() {
    local file="$1"
    local key="$2"
    awk -F '\t' -v wanted="$key" '
        NF != 2 { bad = 1 }
        $1 == wanted { count += 1; value = $2 }
        END { if (!bad && count == 1) print value }
    ' "$file"
}

require_kv() {
    local value
    value="$(kv "$1" "$2")"
    if [[ -z "$value" ]]; then
        printf '__MISSING_OR_DUPLICATE__%s__%s' "$3" "$2"
        return 0
    fi
    printf '%s' "$value"
}

reject_missing_marker() {
    [[ "$1" != __MISSING_OR_DUPLICATE__* ]] \
        || not_run "missing-or-duplicate-${1#__MISSING_OR_DUPLICATE__}"
}

comment_kv() {
    local file="$1"
    local key="$2"
    awk -F '\t' -v wanted="$key" '
        substr($1, 1, 2) == "# " && substr($1, 3) == wanted {
            count += 1
            value = $2
        }
        END { if (count == 1) print value }
    ' "$file"
}

require_uint() {
    reject_missing_marker "$1"
    [[ "$1" =~ ^[0-9]+$ ]] || not_run "invalid-$2"
}

require_hash() {
    reject_missing_marker "$1"
    [[ ${#1} -eq 64 && "$1" =~ ^[0-9a-f]+$ ]] || not_run "invalid-$2"
}

reject_unknown_kv() {
    local file="$1"
    local allowed="$2"
    local label="$3"
    awk -F '\t' -v allowed="$allowed" '
        BEGIN { count = split(allowed, keys, " "); for (i = 1; i <= count; i++) ok[keys[i]] = 1 }
        NF != 2 || !($1 in ok) || seen[$1]++ { exit 1 }
    ' "$file" || not_run "invalid-$label-schema"
}

require_exact_kv_schema() {
    local file="$1"
    local allowed="$2"
    local label="$3"
    awk -F '\t' -v allowed="$allowed" '
        BEGIN {
            required_count = split(allowed, keys, " ")
            for (i = 1; i <= required_count; i++) required[keys[i]] = 1
        }
        NF != 2 || !($1 in required) || seen[$1]++ { exit 1 }
        END {
            if (NR != required_count) exit 1
            for (key in required) if (!(key in seen)) exit 1
        }
    ' "$file" || not_run "invalid-$label-schema"
}

require_ordered_kv_schema() {
    local file="$1"
    local ordered="$2"
    local label="$3"
    awk -F '\t' -v ordered="$ordered" '
        BEGIN { required_count = split(ordered, keys, " ") }
        NF != 2 || NR > required_count || $1 != keys[NR] { exit 1 }
        END { if (NR != required_count) exit 1 }
    ' "$file" || not_run "invalid-$label-schema"
}

while (( $# > 0 )); do
    case "$1" in
        --subject) SUBJECT="${2:-}"; shift ;;
        --scenario) SCENARIO="${2:-}"; shift ;;
        --plan) PLAN="${2:-}"; shift ;;
        --plan-metadata) PLAN_METADATA="${2:-}"; shift ;;
        --pair-metadata) PAIR_METADATA="${2:-}"; shift ;;
        --subject-identity) SUBJECT_IDENTITY="${2:-}"; shift ;;
        --run-intent) RUN_INTENT="${2:-}"; shift ;;
        --run-metadata) RUN_METADATA="${2:-}"; shift ;;
        --workload-metadata) WORKLOAD_METADATA="${2:-}"; shift ;;
        --workload-events) WORKLOAD_EVENTS="${2:-}"; shift ;;
        --ready-receipt) READY_RECEIPT="${2:-}"; shift ;;
        --campaign-id) CAMPAIGN_ID="${2:-}"; shift ;;
        --session-id) SESSION_ID="${2:-}"; shift ;;
        --nonce) NONCE="${2:-}"; shift ;;
        --campaign-secret-file) CAMPAIGN_SECRET_FILE="${2:-}"; shift ;;
        --driver-events) DRIVER_EVENTS="${2:-}"; shift ;;
        --driver-intent) DRIVER_INTENT="${2:-}"; shift ;;
        --driver-receipt) DRIVER_RECEIPT="${2:-}"; shift ;;
        --window-identity) WINDOW_IDENTITY="${2:-}"; shift ;;
        --driver-binary) DRIVER_BINARY="${2:-}"; shift ;;
        --driver-source) DRIVER_SOURCE="${2:-}"; shift ;;
        --driver-controller) DRIVER_CONTROLLER="${2:-}"; shift ;;
        --rss-samples) RSS_SAMPLES="${2:-}"; shift ;;
        --runtime-samples) RUNTIME_SAMPLES="${2:-}"; shift ;;
        --runtime-events) RUNTIME_EVENTS="${2:-}"; shift ;;
        --runtime-metadata) RUNTIME_METADATA="${2:-}"; shift ;;
        --failure-actions) FAILURE_ACTIONS="${2:-}"; shift ;;
        --native-launch-observation) NATIVE_LAUNCH_OBSERVATION="${2:-}"; shift ;;
        --native-provisional-observation) NATIVE_PROVISIONAL_OBSERVATION="${2:-}"; shift ;;
        --trace-metadata) TRACE_METADATA="${2:-}"; shift ;;
        --trace-provisional-receipt) TRACE_PROVISIONAL_RECEIPT="${2:-}"; shift ;;
        --performance-tail-receipt) PERFORMANCE_TAIL_RECEIPT="${2:-}"; shift ;;
        --performance-quit-receipt) PERFORMANCE_QUIT_RECEIPT="${2:-}"; shift ;;
        --subject-exit-receipt) SUBJECT_EXIT_RECEIPT="${2:-}"; shift ;;
        --performance-lifecycle-ready-receipt) PERFORMANCE_LIFECYCLE_READY_RECEIPT="${2:-}"; shift ;;
        --performance-lifecycle-registration) PERFORMANCE_LIFECYCLE_REGISTRATION="${2:-}"; shift ;;
        --subject-lifecycle-helper) SUBJECT_LIFECYCLE_HELPER="${2:-}"; shift ;;
        --common-lifecycle-helper) COMMON_LIFECYCLE_HELPER="${2:-}"; shift ;;
        --appkit-terminator-source) APPKIT_TERMINATOR_SOURCE="${2:-}"; shift ;;
        --appkit-terminator-binary) APPKIT_TERMINATOR_BINARY="${2:-}"; shift ;;
        --plan-start-gate) PLAN_START_GATE="${2:-}"; shift ;;
        --manual-artifacts) MANUAL_ARTIFACTS="${2:-}"; shift ;;
        --manual-screenshot) MANUAL_SCREENSHOT="${2:-}"; shift ;;
        --manual-video) MANUAL_VIDEO="${2:-}"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; not_run "unknown-argument" ;;
    esac
    shift
done

[[ "$SUBJECT" == spaceterm || "$SUBJECT" == ghostty ]] || not_run "invalid-subject"
case "$SCENARIO" in
    ascii|unicode-styles|scrolled|hidden-occluded|resize) ;;
    *) not_run "invalid-scenario" ;;
esac
command -v awk >/dev/null 2>&1 || not_run "awk-unavailable"
command -v mktemp >/dev/null 2>&1 || not_run "mktemp-unavailable"
command -v python3 >/dev/null 2>&1 || not_run "python3-unavailable"
command -v rm >/dev/null 2>&1 || not_run "rm-unavailable"
command -v shasum >/dev/null 2>&1 || not_run "shasum-unavailable"
[[ "$CAMPAIGN_ID" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,79}$ ]] \
    || not_run "invalid-campaign-id"
[[ "$SESSION_ID" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,79}$ ]] \
    || not_run "invalid-session-id"
[[ "$NONCE" =~ ^[0-9a-f]{64}$ ]] || not_run "invalid-workload-nonce"

require_file scenario-plan "$PLAN"
require_file plan-metadata "$PLAN_METADATA"
require_file pair-metadata "$PAIR_METADATA"
require_file subject-identity "$SUBJECT_IDENTITY"
require_file run-intent "$RUN_INTENT"
require_file run-metadata "$RUN_METADATA"
require_file workload-metadata "$WORKLOAD_METADATA"
require_file workload-events "$WORKLOAD_EVENTS"
require_file ready-receipt "$READY_RECEIPT"
require_file campaign-secret "$CAMPAIGN_SECRET_FILE"
require_file driver-events "$DRIVER_EVENTS"
require_file driver-intent "$DRIVER_INTENT"
require_file driver-receipt "$DRIVER_RECEIPT"
require_file window-identity "$WINDOW_IDENTITY"
require_file driver-binary "$DRIVER_BINARY"
require_file driver-source "$DRIVER_SOURCE"
require_file driver-controller "$DRIVER_CONTROLLER"
require_file rss-samples "$RSS_SAMPLES"
require_file trace-metadata "$TRACE_METADATA"
require_file trace-provisional-receipt "$TRACE_PROVISIONAL_RECEIPT"
require_file performance-tail-receipt "$PERFORMANCE_TAIL_RECEIPT"
require_file performance-quit-receipt "$PERFORMANCE_QUIT_RECEIPT"
require_file subject-exit-receipt "$SUBJECT_EXIT_RECEIPT"
require_file performance-lifecycle-ready-receipt "$PERFORMANCE_LIFECYCLE_READY_RECEIPT"
require_file performance-lifecycle-registration "$PERFORMANCE_LIFECYCLE_REGISTRATION"
require_file subject-lifecycle-helper "$SUBJECT_LIFECYCLE_HELPER"
require_file common-lifecycle-helper "$COMMON_LIFECYCLE_HELPER"
require_file appkit-terminator-source "$APPKIT_TERMINATOR_SOURCE"
require_file appkit-terminator-binary "$APPKIT_TERMINATOR_BINARY"
require_file plan-start-gate "$PLAN_START_GATE"
require_file manual-artifacts "$MANUAL_ARTIFACTS"
require_file manual-screenshot "$MANUAL_SCREENSHOT"
require_file manual-video "$MANUAL_VIDEO"
TRACE_ARCHIVE="$(dirname -- "$TRACE_METADATA")/$SUBJECT-$SCENARIO.trace"
TRACE_ARCHIVE_SHA256="$(trace_tree_sha256 "$TRACE_ARCHIVE")" \
    || not_run "trace-archive-invalid"

canonical_path() {
    local directory base
    directory="$(cd -- "$(dirname -- "$1")" && pwd -P)" || return 1
    base="$(basename -- "$1")"
    printf '%s/%s\n' "$directory" "$base"
}
[[ "$(canonical_path "$DRIVER_SOURCE")" == "$SCRIPT_DIRECTORY/performance-driver.m" \
    && "$(canonical_path "$DRIVER_CONTROLLER")" \
        == "$SCRIPT_DIRECTORY/run-native-performance-scenario.sh" \
    && "$(canonical_path "$COMMON_LIFECYCLE_HELPER")" \
        == "$SCRIPT_DIRECTORY/performance-subject-lifecycle.py" \
    && "$(canonical_path "$APPKIT_TERMINATOR_SOURCE")" \
        == "$SCRIPT_DIRECTORY/performance-appkit-terminate.m" ]] \
    || not_run "noncanonical-performance-toolchain"
run_directory="$(cd -- "$(dirname -- "$RUN_INTENT")" && pwd -P)" \
    || not_run "run-directory-unavailable"
subject_lifecycle_helper_path="$(canonical_path "$SUBJECT_LIFECYCLE_HELPER")" \
    || not_run "run-owned-lifecycle-helper-unavailable"
[[ "$(dirname -- "$subject_lifecycle_helper_path")" == "$run_directory" \
    && "$subject_lifecycle_helper_path" != "$(canonical_path "$COMMON_LIFECYCLE_HELPER")" \
    && "$(sha256 "$SUBJECT_LIFECYCLE_HELPER")" == "$(sha256 "$COMMON_LIFECYCLE_HELPER")" ]] \
    || not_run "run-owned-lifecycle-helper-invalid"
plan_start_continuous_ns="$(kv "$PLAN_START_GATE" plan_start_continuous_ns)"
[[ "$plan_start_continuous_ns" =~ ^[1-9][0-9]*$ ]] \
    || not_run "invalid-driver-plan-start-gate"
python3 "$SCRIPT_DIRECTORY/performance-driver-receipt.py" verify \
    --campaign-secret-file "$CAMPAIGN_SECRET_FILE" --campaign-id "$CAMPAIGN_ID" \
    --session-id "$SESSION_ID" --nonce "$NONCE" --driver-output "$DRIVER_EVENTS" \
    --driver-binary "$DRIVER_BINARY" --driver-source "$DRIVER_SOURCE" \
    --controller "$DRIVER_CONTROLLER" --scenario-plan "$PLAN" \
    --plan-start-continuous-ns "$plan_start_continuous_ns" \
    --subject-identity "$SUBJECT_IDENTITY" --window-identity "$WINDOW_IDENTITY" \
    --intent "$DRIVER_INTENT" --receipt "$DRIVER_RECEIPT" >/dev/null 2>&1 \
    || not_run "public-driver-evidence-invalid"
if [[ "$SUBJECT" == spaceterm ]]; then
    require_file runtime-samples "$RUNTIME_SAMPLES"
    require_file runtime-events "$RUNTIME_EVENTS"
    require_file runtime-metadata "$RUNTIME_METADATA"
    require_file failure-actions "$FAILURE_ACTIONS"
    require_file native-launch-observation "$NATIVE_LAUNCH_OBSERVATION"
    require_file native-provisional-observation "$NATIVE_PROVISIONAL_OBSERVATION"
elif [[ -n "$RUNTIME_SAMPLES$RUNTIME_EVENTS$RUNTIME_METADATA$FAILURE_ACTIONS$NATIVE_LAUNCH_OBSERVATION$NATIVE_PROVISIONAL_OBSERVATION" ]]; then
    not_run "ghostty-must-not-claim-spaceterm-runtime-observations"
fi

tail_token="$(kv "$PERFORMANCE_TAIL_RECEIPT" quit_token)"
tail_completed_ns="$(kv "$PERFORMANCE_TAIL_RECEIPT" tail_completed_continuous_ns)"
python3 "$SCRIPT_DIRECTORY/performance-tail-receipt.py" verify \
    --campaign-secret-file "$CAMPAIGN_SECRET_FILE" --campaign-id "$CAMPAIGN_ID" \
    --session-id "$SESSION_ID" --nonce "$NONCE" --quit-token "$tail_token" \
    --run-intent "$RUN_INTENT" --subject-identity "$SUBJECT_IDENTITY" \
    --driver-receipt "$DRIVER_RECEIPT" --driver-events "$DRIVER_EVENTS" \
    --workload-metadata "$WORKLOAD_METADATA" --workload-events "$WORKLOAD_EVENTS" \
    --workload-ready-receipt "$READY_RECEIPT" \
    --rss-samples "$RSS_SAMPLES" --trace-provisional-receipt "$TRACE_PROVISIONAL_RECEIPT" \
    --lifecycle-ready-receipt "$PERFORMANCE_LIFECYCLE_READY_RECEIPT" \
    --tail-completed-continuous-ns "$tail_completed_ns" \
    --appkit-terminator-source "$APPKIT_TERMINATOR_SOURCE" \
    --appkit-terminator-binary "$APPKIT_TERMINATOR_BINARY" \
    --receipt "$PERFORMANCE_TAIL_RECEIPT" >/dev/null 2>&1 \
    || not_run "performance-tail-receipt-invalid"
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
    --workload-ready-receipt "$READY_RECEIPT"
    --quit-receipt "$PERFORMANCE_QUIT_RECEIPT"
    --subject-exit-receipt "$SUBJECT_EXIT_RECEIPT"
    --subject-lifecycle-helper "$SUBJECT_LIFECYCLE_HELPER"
    --appkit-terminator-source "$APPKIT_TERMINATOR_SOURCE"
    --appkit-terminator-binary "$APPKIT_TERMINATOR_BINARY"
)
if [[ "$SUBJECT" == spaceterm ]]; then
    exit_arguments+=(--native-observation "$NATIVE_LAUNCH_OBSERVATION")
    lifecycle_arguments+=(--native-observation "$NATIVE_LAUNCH_OBSERVATION")
fi
python3 "$SCRIPT_DIRECTORY/verify-performance-lifecycle-receipts.py" \
    "${lifecycle_arguments[@]}" >/dev/null 2>&1 \
    || not_run "performance-lifecycle-receipts-invalid"
python3 "$SCRIPT_DIRECTORY/verify-performance-subject-exit.py" "${exit_arguments[@]}" \
    >/dev/null 2>&1 || not_run "performance-subject-exit-invalid"

early_warmup_ms="$(kv "$PLAN_METADATA" warmup_ms)"
early_duration_ms="$(kv "$PLAN_METADATA" measured_duration_ms)"
[[ "$early_warmup_ms" =~ ^[0-9]+$ && "$early_duration_ms" =~ ^[1-9][0-9]*$ ]] \
    || not_run "invalid-plan-duration-before-authentication"
AUTH_SNAPSHOT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-case-auth.XXXXXX")"
trap cleanup EXIT INT TERM
verified_metadata="$AUTH_SNAPSHOT_DIR/workload-metadata.tsv"
verified_events="$AUTH_SNAPSHOT_DIR/workload-events.tsv"
verified_subject="$AUTH_SNAPSHOT_DIR/subject-identity.tsv"
verified_trace="$AUTH_SNAPSHOT_DIR/trace-metadata.tsv"
verified_manual="$AUTH_SNAPSHOT_DIR/manual-artifacts.tsv"
trace_source_hash="$(sha256 "$TRACE_METADATA")"
manual_source_hash="$(sha256 "$MANUAL_ARTIFACTS")"
cp -- "$TRACE_METADATA" "$verified_trace" \
    || not_run "trace-metadata-snapshot-failed"
cp -- "$MANUAL_ARTIFACTS" "$verified_manual" \
    || not_run "manual-artifacts-snapshot-failed"
TRACE_METADATA_SHA256="$(sha256 "$verified_trace")"
MANUAL_ARTIFACTS_SHA256="$(sha256 "$verified_manual")"
[[ "$TRACE_METADATA_SHA256" == "$trace_source_hash" \
    && "$TRACE_METADATA_SHA256" == "$(sha256 "$TRACE_METADATA")" ]] \
    || not_run "trace-metadata-changed-during-snapshot"
[[ "$MANUAL_ARTIFACTS_SHA256" == "$manual_source_hash" \
    && "$MANUAL_ARTIFACTS_SHA256" == "$(sha256 "$MANUAL_ARTIFACTS")" ]] \
    || not_run "manual-artifacts-changed-during-snapshot"
MANUAL_SCREENSHOT_SHA256="$(sha256 "$MANUAL_SCREENSHOT")"
MANUAL_VIDEO_SHA256="$(sha256 "$MANUAL_VIDEO")"
TRACE_METADATA="$verified_trace"
MANUAL_ARTIFACTS="$verified_manual"
python3 "$SCRIPT_DIRECTORY/verify-performance-workload-ready.py" \
    --ready-receipt "$READY_RECEIPT" --events "$WORKLOAD_EVENTS" \
    --subject-identity "$SUBJECT_IDENTITY" \
    --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
    --campaign-id "$CAMPAIGN_ID" --session-id "$SESSION_ID" --nonce "$NONCE" \
    --plan-start-gate "$PLAN_START_GATE" \
    --expected-plan-start-continuous-ns \
        "$(awk -F '\t' '$1 == "plan_start_continuous_ns" { print $2 }' "$WORKLOAD_METADATA")" \
    >/dev/null 2>&1 || not_run "original-workload-readiness-invalid"
python3 "$SCRIPT_DIRECTORY/verify-performance-workload-auth.py" \
    --metadata "$WORKLOAD_METADATA" \
    --events "$WORKLOAD_EVENTS" \
    --subject-identity "$SUBJECT_IDENTITY" \
    --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
    --ready-receipt "$READY_RECEIPT" \
    --campaign-id "$CAMPAIGN_ID" \
    --session-id "$SESSION_ID" \
    --nonce "$NONCE" \
    --scenario "$SCENARIO" \
    --requested-warmup-ms "$early_warmup_ms" \
    --requested-duration-ms "$early_duration_ms" \
    --verified-metadata-output "$verified_metadata" \
    --verified-events-output "$verified_events" \
    --verified-subject-identity-output "$verified_subject" \
    >/dev/null 2>&1 || not_run "workload-authentication-invalid"
python3 "$SCRIPT_DIRECTORY/verify-performance-workload-ready.py" \
    --ready-receipt "$READY_RECEIPT" \
    --events "$verified_events" \
    --subject-identity "$verified_subject" \
    --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
    --campaign-id "$CAMPAIGN_ID" --session-id "$SESSION_ID" --nonce "$NONCE" \
    --plan-start-gate "$PLAN_START_GATE" \
    --expected-plan-start-continuous-ns \
        "$(awk -F '\t' '$1 == "plan_start_continuous_ns" { print $2 }' "$verified_metadata")" \
    --ignore-events-file-identity \
    >/dev/null 2>&1 || not_run "workload-readiness-authentication-invalid"
WORKLOAD_METADATA="$verified_metadata"
WORKLOAD_EVENTS="$verified_events"
SUBJECT_IDENTITY="$verified_subject"

[[ "$(head -n 1 "$PLAN")" == "$PLAN_HEADER" ]] || not_run "invalid-plan-header"
[[ "$(head -n 1 "$WORKLOAD_EVENTS")" == "$WORKLOAD_HEADER" ]] \
    || not_run "invalid-workload-events-header"
[[ "$(head -n 1 "$DRIVER_EVENTS")" == "$DRIVER_HEADER" ]] \
    || not_run "invalid-driver-events-header"
[[ "$(head -n 1 "$RSS_SAMPLES")" == "$RSS_HEADER" ]] \
    || not_run "invalid-rss-header"
if [[ "$SUBJECT" == spaceterm ]]; then
    [[ "$(head -n 1 "$RUNTIME_SAMPLES")" == "$RUNTIME_SAMPLE_HEADER" ]] \
        || not_run "invalid-runtime-samples-header"
    [[ "$(head -n 1 "$RUNTIME_EVENTS")" == "$RUNTIME_EVENT_HEADER" ]] \
        || not_run "invalid-runtime-events-header"
fi

[[ "$(comment_kv "$RSS_SAMPLES" format_version)" == 4 \
    && "$(comment_kv "$RSS_SAMPLES" scenario)" == "$SCENARIO" \
    && "$(comment_kv "$RSS_SAMPLES" subject_identity_sha256)" == "$(sha256 "$SUBJECT_IDENTITY")" \
    && "$(comment_kv "$RSS_SAMPLES" workload_events_sha256)" == "$(sha256 "$WORKLOAD_EVENTS")" \
    && "$(comment_kv "$RSS_SAMPLES" workload_metadata_sha256)" == "$(sha256 "$WORKLOAD_METADATA")" \
    && "$(comment_kv "$RSS_SAMPLES" ready_receipt_sha256)" == "$(sha256 "$READY_RECEIPT")" \
    && "$(comment_kv "$RSS_SAMPLES" plan_start_gate_sha256)" == "$(sha256 "$PLAN_START_GATE")" \
    && "$(comment_kv "$RSS_SAMPLES" workload_authentication)" == hmac-sha256 \
    && "$(comment_kv "$RSS_SAMPLES" progress_interval_ms)" == 1000 \
    && "$(comment_kv "$RSS_SAMPLES" maximum_progress_age_ms)" == 2000 \
    && "$(comment_kv "$RSS_SAMPLES" driver_events_sha256)" == "$(sha256 "$DRIVER_EVENTS")" ]] \
    || not_run "rss-evidence-binding-mismatch"

reject_unknown_kv "$PLAN_METADATA" \
    "format_version scenario plan_sha256 input_schedule_sha256 warmup_ms measured_duration_ms input_interval_ms required_seed_rows required_resize_cycles geometry_authority native_resize_arguments" \
    plan-metadata
reject_unknown_kv "$PAIR_METADATA" \
    "format_version pair_id scenario plan_sha256 workload_sha256 command_sha256 environment_sha256 font_sha256 initial_grid_sha256 duration_ms spaceterm_subject_identity_sha256 ghostty_subject_identity_sha256" \
    pair-metadata
reject_unknown_kv "$WORKLOAD_METADATA" \
    "format_version scenario campaign_id session_id nonce subject_identity_sha256 subject_process_pid subject_process_start_identity producer_sha256 producer_pid producer_started_continuous_ns producer_session_id producer_process_group tty_device tty_inode tty_rdev ready_receipt_sha256 events_sha256 auth_algorithm seed_sha256 seed_bytes requested_duration_ms warmup_ms requested_iterations requested_seed_rows emitted_bytes input_events plan_start_continuous_ns started_continuous_ns ended_continuous_ns status events_hmac_sha256" \
    workload-metadata
reject_unknown_kv "$SUBJECT_IDENTITY" \
    "format_version subject app_bundle_path bundle_identifier bundle_version executable_path executable_sha256 executable_device executable_inode executable_fsid signature_valid signing_identifier team_identifier cdhash process_pid process_start_identity identity_status" \
    subject-identity
require_ordered_kv_schema "$RUN_INTENT" \
    "format_version subject subject_identity_sha256 scenario scenario_plan_sha256 workload_sha256 command_sha256 environment_sha256 font_sha256 initial_grid_sha256 measured_duration_ms process_pid process_start_identity campaign_id session_id nonce native_provisional_observation_sha256 evidence_mode status" \
    run-intent
require_ordered_kv_schema "$RUN_METADATA" \
    "format_version subject subject_identity_sha256 scenario scenario_plan_sha256 workload_sha256 command_sha256 environment_sha256 font_sha256 initial_grid_sha256 measured_duration_ms process_pid process_start_identity run_intent_sha256 native_observation_sha256 native_runtime_metadata_sha256 native_failure_actions_sha256 native_failure_action_enabled native_failure_request_count native_failure_result_count native_failure_resource_staged_count native_failure_resource_staged_bytes native_failure_resource_rolled_back_count native_failure_resource_rolled_back_bytes trace_provisional_receipt_sha256 performance_tail_receipt_sha256 performance_quit_receipt_sha256 subject_exit_receipt_sha256 lifecycle_ready_receipt_sha256 lifecycle_registration_receipt_sha256 lifecycle_helper_sha256 terminator_source_sha256 terminator_binary_sha256 evidence_mode status" \
    run-metadata
require_exact_kv_schema "$TRACE_METADATA" \
    "format_version capture_status incomplete_reason subject_identity_sha256 run_metadata_sha256 workload_metadata_sha256 workload_ready_receipt_sha256 supplemental_evidence_sha256 requested_duration_ms actual_duration_ms capture_started_continuous_ns capture_ended_continuous_ns target_identity_verified trace_target_pid_verified time_profiler_instrument allocations_instrument hangs_instrument time_profiler_target_verified allocations_target_verified hangs_target_verified time_profiler_rows allocations_rows hangs_rows maximum_main_thread_hang_ms status" \
    trace-metadata

plan_format="$(require_kv "$PLAN_METADATA" format_version plan)"
[[ "$plan_format" == 1 ]] || not_run "unsupported-plan-format"
[[ "$(require_kv "$PLAN_METADATA" scenario plan)" == "$SCENARIO" ]] \
    || not_run "plan-scenario-mismatch"
plan_hash="$(require_kv "$PLAN_METADATA" plan_sha256 plan)"
require_hash "$plan_hash" plan-sha256
[[ "$(sha256 "$PLAN")" == "$plan_hash" ]] || not_run "plan-hash-mismatch"
measured_duration_ms="$(require_kv "$PLAN_METADATA" measured_duration_ms plan)"
require_uint "$measured_duration_ms" measured-duration-ms
plan_warmup_ms="$(require_kv "$PLAN_METADATA" warmup_ms plan)"
require_uint "$plan_warmup_ms" plan-warmup-ms

[[ "$(require_kv "$PAIR_METADATA" format_version pair)" == 1 ]] \
    || not_run "unsupported-pair-format"
[[ "$(require_kv "$PAIR_METADATA" scenario pair)" == "$SCENARIO" ]] \
    || not_run "pair-scenario-mismatch"
[[ "$(require_kv "$PAIR_METADATA" plan_sha256 pair)" == "$plan_hash" ]] \
    || not_run "paired-plan-mismatch"
[[ "$(require_kv "$PAIR_METADATA" duration_ms pair)" == "$measured_duration_ms" ]] \
    || not_run "paired-duration-mismatch"
for pair_key in workload_sha256 command_sha256 environment_sha256 font_sha256 initial_grid_sha256; do
    pair_hash="$(require_kv "$PAIR_METADATA" "$pair_key" pair)"
    require_hash "$pair_hash" "pair-$pair_key"
done

[[ "$(require_kv "$SUBJECT_IDENTITY" format_version subject)" == 1 ]] \
    || not_run "unsupported-subject-identity-format"
[[ "$(require_kv "$SUBJECT_IDENTITY" subject subject)" == "$SUBJECT" ]] \
    || not_run "subject-identity-mismatch"
[[ "$(require_kv "$SUBJECT_IDENTITY" signature_valid subject)" == true \
    && "$(require_kv "$SUBJECT_IDENTITY" identity_status subject)" == frozen ]] \
    || not_run "subject-identity-not-frozen"
subject_hash="$(sha256 "$SUBJECT_IDENTITY")"
pair_subject_key="${SUBJECT}_subject_identity_sha256"
[[ "$(require_kv "$PAIR_METADATA" "$pair_subject_key" pair)" == "$subject_hash" ]] \
    || not_run "paired-subject-identity-mismatch"
for identity_key in executable_sha256 cdhash; do
    identity_hash="$(require_kv "$SUBJECT_IDENTITY" "$identity_key" subject)"
    [[ -n "$identity_hash" ]] || not_run "missing-subject-$identity_key"
done
for identity_path_key in app_bundle_path executable_path; do
    identity_path="$(require_kv "$SUBJECT_IDENTITY" "$identity_path_key" subject)"
    reject_missing_marker "$identity_path"
    [[ "$identity_path" == /* && "$identity_path" != *$'\t'* \
        && "$identity_path" != *$'\n'* ]] \
        || not_run "invalid-subject-$identity_path_key"
done

[[ "$(require_kv "$RUN_INTENT" format_version intent)" == 1 \
    && "$(require_kv "$RUN_INTENT" status intent)" == prepared \
    && "$(require_kv "$RUN_INTENT" evidence_mode intent)" == production \
    && "$(require_kv "$RUN_INTENT" subject intent)" == "$SUBJECT" \
    && "$(require_kv "$RUN_INTENT" campaign_id intent)" == "$CAMPAIGN_ID" \
    && "$(require_kv "$RUN_INTENT" session_id intent)" == "$SESSION_ID" \
    && "$(require_kv "$RUN_INTENT" nonce intent)" == "$NONCE" \
    && "$(require_kv "$RUN_INTENT" subject_identity_sha256 intent)" == "$subject_hash" ]] \
    || not_run "run-intent-binding-mismatch"
[[ "$(require_kv "$RUN_METADATA" format_version run)" == 4 \
    && "$(require_kv "$RUN_METADATA" subject run)" == "$SUBJECT" \
    && "$(require_kv "$RUN_METADATA" scenario run)" == "$SCENARIO" \
    && "$(require_kv "$RUN_METADATA" subject_identity_sha256 run)" == "$subject_hash" \
    && "$(require_kv "$RUN_METADATA" scenario_plan_sha256 run)" == "$plan_hash" \
    && "$(require_kv "$RUN_METADATA" measured_duration_ms run)" == "$measured_duration_ms" \
    && "$(require_kv "$RUN_METADATA" run_intent_sha256 run)" == "$(sha256 "$RUN_INTENT")" \
    && "$(require_kv "$RUN_METADATA" evidence_mode run)" == production \
    && "$(require_kv "$RUN_METADATA" status run)" == complete ]] \
    || not_run "run-metadata-binding-mismatch"
for parity_key in workload_sha256 command_sha256 environment_sha256 font_sha256 initial_grid_sha256; do
    run_hash="$(require_kv "$RUN_METADATA" "$parity_key" run)"
    require_hash "$run_hash" "run-$parity_key"
    [[ "$run_hash" == "$(require_kv "$PAIR_METADATA" "$parity_key" pair)" ]] \
        || not_run "paired-$parity_key-mismatch"
done
for common_key in subject subject_identity_sha256 scenario scenario_plan_sha256 workload_sha256 \
    command_sha256 environment_sha256 font_sha256 initial_grid_sha256 measured_duration_ms \
    process_pid process_start_identity; do
    [[ "$(require_kv "$RUN_METADATA" "$common_key" run)" \
        == "$(require_kv "$RUN_INTENT" "$common_key" intent)" ]] \
        || not_run "run-intent-final-$common_key-mismatch"
done
[[ "$(require_kv "$RUN_METADATA" process_pid run)" \
        == "$(require_kv "$SUBJECT_IDENTITY" process_pid subject)" \
    && "$(require_kv "$RUN_METADATA" process_start_identity run)" \
        == "$(require_kv "$SUBJECT_IDENTITY" process_start_identity subject)" ]] \
    || not_run "run-process-identity-mismatch"
[[ "$(require_kv "$RUN_METADATA" trace_provisional_receipt_sha256 run)" \
        == "$(sha256 "$TRACE_PROVISIONAL_RECEIPT")" \
    && "$(require_kv "$RUN_METADATA" performance_tail_receipt_sha256 run)" \
        == "$(sha256 "$PERFORMANCE_TAIL_RECEIPT")" \
    && "$(require_kv "$RUN_METADATA" performance_quit_receipt_sha256 run)" \
        == "$(sha256 "$PERFORMANCE_QUIT_RECEIPT")" \
    && "$(require_kv "$RUN_METADATA" subject_exit_receipt_sha256 run)" \
        == "$(sha256 "$SUBJECT_EXIT_RECEIPT")" ]] \
    || not_run "run-causal-closure-mismatch"
[[ "$(require_kv "$RUN_METADATA" lifecycle_ready_receipt_sha256 run)" \
        == "$(sha256 "$PERFORMANCE_LIFECYCLE_READY_RECEIPT")" \
    && "$(require_kv "$RUN_METADATA" lifecycle_registration_receipt_sha256 run)" \
        == "$(sha256 "$PERFORMANCE_LIFECYCLE_REGISTRATION")" \
    && "$(require_kv "$RUN_METADATA" lifecycle_helper_sha256 run)" \
        == "$(sha256 "$SUBJECT_LIFECYCLE_HELPER")" \
    && "$(require_kv "$RUN_METADATA" terminator_source_sha256 run)" \
        == "$(sha256 "$APPKIT_TERMINATOR_SOURCE")" \
    && "$(require_kv "$RUN_METADATA" terminator_binary_sha256 run)" \
        == "$(sha256 "$APPKIT_TERMINATOR_BINARY")" ]] \
    || not_run "run-lifecycle-provenance-mismatch"
if [[ "$SUBJECT" == spaceterm ]]; then
    [[ "$(require_kv "$RUN_METADATA" native_observation_sha256 run)" \
            == "$(sha256 "$NATIVE_LAUNCH_OBSERVATION")" \
        && "$(require_kv "$RUN_METADATA" native_runtime_metadata_sha256 run)" \
            == "$(sha256 "$RUNTIME_METADATA")" \
        && "$(require_kv "$RUN_METADATA" native_failure_actions_sha256 run)" \
            == "$(sha256 "$FAILURE_ACTIONS")" \
        && "$(require_kv "$RUN_METADATA" native_failure_action_enabled run)" == false \
        && "$(require_kv "$RUN_METADATA" native_failure_request_count run)" == 0 \
        && "$(require_kv "$RUN_METADATA" native_failure_result_count run)" == 0 \
        && "$(require_kv "$RUN_METADATA" native_failure_resource_staged_count run)" == 0 \
        && "$(require_kv "$RUN_METADATA" native_failure_resource_staged_bytes run)" == 0 \
        && "$(require_kv "$RUN_METADATA" native_failure_resource_rolled_back_count run)" == 0 \
        && "$(require_kv "$RUN_METADATA" native_failure_resource_rolled_back_bytes run)" == 0 ]] \
        || not_run "spaceterm-run-native-closure-mismatch"
    [[ "$(require_kv "$RUN_INTENT" native_provisional_observation_sha256 intent)" \
        == "$(sha256 "$NATIVE_PROVISIONAL_OBSERVATION")" ]] \
        || not_run "spaceterm-intent-provisional-mismatch"
    python3 "$SCRIPT_DIRECTORY/verify-performance-native-closure.py" \
        --subject-identity "$SUBJECT_IDENTITY" \
        --provisional-observation "$NATIVE_PROVISIONAL_OBSERVATION" \
        --native-observation "$NATIVE_LAUNCH_OBSERVATION" \
        --runtime-metadata "$RUNTIME_METADATA" --runtime-samples "$RUNTIME_SAMPLES" \
        --runtime-events "$RUNTIME_EVENTS" --failure-actions "$FAILURE_ACTIONS" \
        >/dev/null 2>&1 || not_run "spaceterm-native-closure-invalid"
else
    [[ "$(require_kv "$RUN_INTENT" native_provisional_observation_sha256 intent)" \
        == not-applicable ]] || not_run "ghostty-intent-provisional-not-applicable-mismatch"
    for key in native_observation_sha256 native_runtime_metadata_sha256 \
        native_failure_actions_sha256 native_failure_action_enabled \
        native_failure_request_count native_failure_result_count \
        native_failure_resource_staged_count native_failure_resource_staged_bytes \
        native_failure_resource_rolled_back_count native_failure_resource_rolled_back_bytes; do
        [[ "$(require_kv "$RUN_METADATA" "$key" run)" == not-applicable ]] \
            || not_run "ghostty-native-closure-not-applicable-mismatch"
    done
fi
for identity_key in executable_device executable_inode executable_fsid process_pid; do
    identity_value="$(require_kv "$SUBJECT_IDENTITY" "$identity_key" subject)"
    require_uint "$identity_value" "subject-$identity_key"
done

[[ "$(require_kv "$WORKLOAD_METADATA" format_version workload)" == 3 ]] \
    || not_run "unsupported-workload-format"
[[ "$(require_kv "$WORKLOAD_METADATA" scenario workload)" == "$SCENARIO" ]] \
    || not_run "workload-scenario-mismatch"
producer_hash="$(require_kv "$WORKLOAD_METADATA" producer_sha256 workload)"
require_hash "$producer_hash" producer-sha256
[[ "$producer_hash" == "$(require_kv "$PAIR_METADATA" workload_sha256 pair)" ]] \
    || not_run "paired-workload-mismatch"
[[ "$(require_kv "$WORKLOAD_METADATA" requested_duration_ms workload)" \
        == "$measured_duration_ms" ]] \
    || not_run "workload-duration-mismatch"
[[ "$(require_kv "$WORKLOAD_METADATA" warmup_ms workload)" == "$plan_warmup_ms" ]] \
    || not_run "workload-warmup-mismatch"
[[ "$(require_kv "$WORKLOAD_METADATA" status workload)" == complete ]] \
    || not_run "workload-incomplete"
workload_started="$(require_kv "$WORKLOAD_METADATA" started_continuous_ns workload)"
workload_ended="$(require_kv "$WORKLOAD_METADATA" ended_continuous_ns workload)"
workload_emitted="$(require_kv "$WORKLOAD_METADATA" emitted_bytes workload)"
ready_receipt_hash="$(require_kv "$WORKLOAD_METADATA" ready_receipt_sha256 workload)"
require_uint "$workload_started" workload-started-continuous-ns
require_uint "$workload_ended" workload-ended-continuous-ns
require_uint "$workload_emitted" workload-emitted-bytes
require_hash "$ready_receipt_hash" workload-ready-receipt-sha256
[[ "$ready_receipt_hash" == "$(sha256 "$READY_RECEIPT")" ]] \
    || not_run "workload-ready-receipt-mismatch"
(( workload_ended > workload_started )) || not_run "invalid-workload-duration"
actual_workload_ms=$(((workload_ended - workload_started) / 1000000))
(( actual_workload_ms >= measured_duration_ms \
    && actual_workload_ms <= measured_duration_ms + 2000 )) \
    || not_run "workload-does-not-cover-duration"

# All event artifacts are append-only, exact-schema, sequence- and time-ordered.
awk -F '\t' -v measured_start="$workload_started" -v final_bytes="$workload_emitted" '
    NR == 1 { next }
    NF != 10 { exit 1 }
    $1 !~ /^[0-9]+$/ || $2 !~ /^[0-9]+$/ || $5 !~ /^[0-9]+$/ \
        || $6 !~ /^[0-9]+$/ || $7 !~ /^[0-9]+$/ || $8 !~ /^[0-9]+$/ \
        || $9 !~ /^[0-9]+$/ { exit 1 }
    !($3 == "started" || $3 == "seed-complete" || $3 == "measurement-ready" \
        || $3 == "input-read" \
        || $3 == "input-ack-written" || $3 == "geometry" \
        || $3 == "progress" || $3 == "producer-end") { exit 1 }
    !(($3 == "producer-end" && $10 == "success") \
        || ($3 != "producer-end" && $10 == "ok")) { exit 1 }
    $1 + 0 != NR - 2 || (NR > 2 && $2 + 0 <= prior_time) { exit 1 }
    { prior_time = $2 + 0 }
    $3 == "started" { started += 1; start_row = NR }
    $3 == "seed-complete" { seeds += 1; seed_row = NR }
    $3 == "measurement-ready" {
        ready += 1
        ready_row = NR
        if ($4 != "none") exit 1
    }
    $3 == "progress" {
        if (ready != 1) exit 1
        if (progress == 0) first_progress_row = NR
        expected = sprintf("progress-%06d", progress)
        if ($4 != expected || $5 + 0 <= 0 || $6 + 0 <= 0 || $7 + 0 <= 0 \
            || $8 + 0 <= 0 || $9 + 0 <= 0 \
            || (progress > 0 && ($2 - progress_time > 2000000000 \
                || $5 + 0 <= progress_bytes))) exit 1
        if (progress == 0 && $2 + 0 != measured_start) exit 1
        progress += 1
        progress_time = $2 + 0
        progress_bytes = $5 + 0
    }
    $3 == "producer-end" { ended += 1; end_row = NR }
    END { exit !(started == 1 && start_row == 2 && seeds == 1 && ready == 1 \
        && seed_row > start_row && ready_row > seed_row \
        && first_progress_row > ready_row \
        && progress >= 2 && progress_bytes == final_bytes \
        && progress_time < $2 + 0 && ended == 1 && end_row == NR) }
' "$WORKLOAD_EVENTS" || not_run "invalid-workload-event-stream"
workload_stream_summary="$(awk -F '\t' '
    NR > 1 && $3 == "started" { started_time = $2 }
    NR > 1 && $3 == "input-read" { reads += 1 }
    NR > 1 && $3 == "seed-complete" { seeds += 1; seed_time = $2 }
    NR > 1 && $3 == "geometry" { geometry += 1 }
    NR > 1 && $3 == "producer-end" {
        print reads + 0 "\t" seeds + 0 "\t" geometry + 0 "\t" $5 \
            "\t" started_time "\t" seed_time
    }
' "$WORKLOAD_EVENTS")"
IFS=$'\t' read -r workload_reads workload_seeds workload_geometry workload_end_bytes \
    workload_start_event workload_seed_event \
    <<< "$workload_stream_summary"
require_uint "$(require_kv "$WORKLOAD_METADATA" input_events workload)" workload-input-events
[[ "$workload_end_bytes" == "$workload_emitted" \
    && "$workload_reads" == "$(require_kv "$WORKLOAD_METADATA" input_events workload)" ]] \
    || not_run "workload-event-accounting-mismatch"
require_uint "$workload_start_event" workload-start-event-time
require_uint "$workload_seed_event" workload-seed-event-time
if [[ "$SCENARIO" == scrolled || "$SCENARIO" == resize ]]; then
    requested_seed_rows="$(require_kv "$WORKLOAD_METADATA" requested_seed_rows workload)"
    require_uint "$requested_seed_rows" requested-seed-rows
    (( requested_seed_rows >= 10000 && workload_seeds == 1 && workload_geometry >= 1 )) \
        || not_run "seed-or-geometry-evidence-incomplete"
fi

subject_process_pid="$(require_kv "$SUBJECT_IDENTITY" process_pid subject)"
awk -F '\t' -v subject_pid="$subject_process_pid" '
    NR == 1 { next }
    NF != 11 { exit 1 }
    $1 !~ /^[0-9]+$/ || $2 !~ /^[0-9]+$/ || $5 !~ /^[1-9][0-9]*$/ \
        || $6 !~ /^[1-9][0-9]*$/ || $7 !~ /^-?[0-9]+$/ \
        || $8 !~ /^-?[0-9]+$/ || $9 !~ /^-?[0-9]+$/ \
        || $10 !~ /^-?[0-9]+$/ { exit 1 }
    !($4 == "input" || $4 == "scroll-rows" || $4 == "minimize" \
        || $4 == "restore" || $4 == "occluder-show" || $4 == "occluder-hide" \
        || $4 == "resize-grid" || $4 == "checkpoint" || $4 == "stop") { exit 1 }
    $1 + 0 != NR - 2 || $5 != subject_pid \
        || (NR > 2 && $2 + 0 <= prior_time) || seen[$3]++ { exit 1 }
    { prior_time = $2 + 0 }
' "$DRIVER_EVENTS" || not_run "invalid-driver-event-stream"
awk -F '\t' 'NR > 1 && $11 != "verified" { exit 1 }' "$DRIVER_EVENTS" \
    || fail "native-driver-action-failed"

# The signed readiness receipt authorizes one shared plan-start boundary.
driver_first_event="$(awk -F '\t' 'NR == 2 { print $2 }' "$DRIVER_EVENTS")"
plan_started="$(require_kv "$WORKLOAD_METADATA" plan_start_continuous_ns workload)"
measured_event_id=measured-start
[[ "$SCENARIO" != resize ]] || measured_event_id=seed-checkpoint
driver_measured_event="$(awk -F '\t' -v wanted="$measured_event_id" \
    '$3 == wanted { count += 1; value = $2 } \
    END { if (count == 1) print value }' "$DRIVER_EVENTS")"
require_uint "$driver_first_event" driver-first-event-time
require_uint "$plan_started" workload-plan-start-time
require_uint "$driver_measured_event" driver-measured-event-time
awk -v driver="$driver_first_event" -v plan="$plan_started" \
    -v measured="$driver_measured_event" -v workload="$workload_started" '
    BEGIN {
        plan_skew = driver - plan
        measured_skew = measured - workload
        if (plan_skew < 0 || plan_skew > 100000000 \
            || measured_skew < -100000000 || measured_skew > 100000000) exit 1
    }
' || not_run "driver-and-producer-clocks-are-not-correlated"

# Every plan event is executed exactly once, in plan order, with the same action.
awk -F '\t' '
    NR == FNR { if (FNR > 1) { ids[++count] = $1; actions[count] = $3 }; next }
    FNR == 1 { next }
    {
        event_index += 1
        if (event_index > count || $3 != ids[event_index] \
            || $4 != actions[event_index]) bad = 1
    }
    END { exit bad || event_index != count }
' "$PLAN" "$DRIVER_EVENTS" || not_run "driver-plan-parity-failed"

# Input IDs must be one-to-one and acknowledge within 250 ms of injection.
awk -F '\t' '
    NR == FNR { if (FNR > 1 && $4 == "input") sent[$3] = $2 + 0; next }
    FNR == 1 { next }
    $3 == "input-read" { if (!($4 in sent) || read[$4]++) bad = 1; read_time[$4] = $2 + 0 }
    $3 == "input-ack-written" {
        if (!($4 in sent) || ack[$4]++ || !($4 in read_time)) bad = 1
        latency_ms = ($2 - sent[$4]) / 1000000
        if (read_time[$4] < sent[$4] || $2 < read_time[$4] || latency_ms > 250) too_slow = 1
    }
    END {
        for (id in sent) if (read[id] != 1 || ack[id] != 1) bad = 1
        exit bad ? 2 : (too_slow ? 1 : 0)
    }
' "$DRIVER_EVENTS" "$WORKLOAD_EVENTS" || input_status=$?
input_status="${input_status:-0}"
[[ "$input_status" != 2 ]] || not_run "input-event-correlation-incomplete"
[[ "$input_status" != 1 ]] || fail "input-latency-exceeds-250ms"
(( workload_reads > 0 )) || not_run "target-pane-ingestion-receipt-missing"

[[ "$(require_kv "$TRACE_METADATA" format_version trace)" == 3 ]] \
    || not_run "unsupported-trace-format"
[[ "$(require_kv "$TRACE_METADATA" capture_status trace)" == CAPTURED \
    && "$(require_kv "$TRACE_METADATA" incomplete_reason trace)" == none \
    && "$(require_kv "$TRACE_METADATA" status trace)" == complete ]] \
    || not_run "trace-capture-incomplete"
[[ "$(require_kv "$TRACE_METADATA" subject_identity_sha256 trace)" == "$subject_hash" \
    && "$(require_kv "$TRACE_METADATA" run_metadata_sha256 trace)" == "$(sha256 "$RUN_METADATA")" \
    && "$(require_kv "$TRACE_METADATA" workload_metadata_sha256 trace)" == "$(sha256 "$WORKLOAD_METADATA")" \
    && "$(require_kv "$TRACE_METADATA" workload_ready_receipt_sha256 trace)" == "$(sha256 "$READY_RECEIPT")" \
    && "$(require_kv "$TRACE_METADATA" supplemental_evidence_sha256 trace)" == "$(sha256 "$PLAN_START_GATE")" \
    && "$(require_kv "$TRACE_METADATA" target_identity_verified trace)" == true \
    && "$(require_kv "$TRACE_METADATA" trace_target_pid_verified trace)" == true ]] \
    || not_run "trace-target-binding-unsupported-or-mismatched"
[[ "$TRACE_ARCHIVE_SHA256" \
    == "$(require_kv "$TRACE_PROVISIONAL_RECEIPT" trace_bundle_tree_sha256 trace-provisional)" ]] \
    || not_run "trace-archive-provisional-mismatch"
trace_requested="$(require_kv "$TRACE_METADATA" requested_duration_ms trace)"
trace_actual="$(require_kv "$TRACE_METADATA" actual_duration_ms trace)"
trace_started="$(require_kv "$TRACE_METADATA" capture_started_continuous_ns trace)"
trace_ended="$(require_kv "$TRACE_METADATA" capture_ended_continuous_ns trace)"
require_uint "$trace_requested" trace-requested-duration
require_uint "$trace_actual" trace-actual-duration
require_uint "$trace_started" trace-started-continuous-ns
require_uint "$trace_ended" trace-ended-continuous-ns
(( trace_requested == measured_duration_ms && trace_actual >= measured_duration_ms \
    && trace_actual <= measured_duration_ms + 3250 )) \
    || not_run "trace-duration-incomplete"
awk -v trace_start="$trace_started" -v trace_end="$trace_ended" \
    -v trace_actual_ms="$trace_actual" \
    -v workload_start="$workload_started" -v workload_end="$workload_ended" '
    BEGIN {
        start_lead = workload_start - trace_start
        timestamp_duration_ms = (trace_end - trace_start) / 1000000
        duration_error_ms = timestamp_duration_ms - trace_actual_ms
        if (duration_error_ms < 0) duration_error_ms = -duration_error_ms
        exit !(start_lead >= 0 && start_lead <= 2000000000 \
            && trace_start < trace_end && trace_end >= workload_end \
            && trace_end <= workload_end + 2000000000 \
            && duration_error_ms <= 100)
    }
' || not_run "trace-does-not-temporally-bind-workload"
for instrument in time_profiler allocations hangs; do
    [[ "$(require_kv "$TRACE_METADATA" "${instrument}_instrument" trace)" == true \
        && "$(require_kv "$TRACE_METADATA" "${instrument}_target_verified" trace)" == true ]] \
        || not_run "trace-$instrument-not-proven"
    rows="$(require_kv "$TRACE_METADATA" "${instrument}_rows" trace)"
    require_uint "$rows" "trace-$instrument-rows"
done
hang_ms="$(require_kv "$TRACE_METADATA" maximum_main_thread_hang_ms trace)"
    [[ "$hang_ms" =~ ^[0-9]+([.][0-9]+)?$ ]] || not_run "invalid-main-thread-hang-duration"
    if awk -v hang="$hang_ms" 'BEGIN { exit !(hang + 0 > 250) }'; then
        fail "main-thread-hang-exceeds-250ms"
    fi

# Manual evidence is intentionally a gate; automation cannot certify pixels,
# content integrity, anchoring, Primary/Alternate restoration, or Ghostty state.
reject_unknown_kv "$MANUAL_ARTIFACTS" \
    "format_version screenshot_sha256 video_sha256 final_content_review anchor_review restoration_review geometry_review reviewer result" \
    manual-artifacts
[[ "$(require_kv "$MANUAL_ARTIFACTS" format_version manual)" == 1 \
    && "$(require_kv "$MANUAL_ARTIFACTS" result manual)" == PASS ]] \
    || not_run "manual-evidence-not-approved"
for review in final_content_review anchor_review restoration_review geometry_review; do
    [[ "$(require_kv "$MANUAL_ARTIFACTS" "$review" manual)" == PASS ]] \
        || not_run "manual-$review-missing"
done
for artifact_hash in screenshot_sha256 video_sha256; do
    hash_value="$(require_kv "$MANUAL_ARTIFACTS" "$artifact_hash" manual)"
    require_hash "$hash_value" "manual-$artifact_hash"
done
[[ "$(require_kv "$MANUAL_ARTIFACTS" screenshot_sha256 manual)" \
        == "$MANUAL_SCREENSHOT_SHA256" \
    && "$(require_kv "$MANUAL_ARTIFACTS" video_sha256 manual)" \
        == "$MANUAL_VIDEO_SHA256" ]] \
    || not_run "manual-artifact-file-hash-mismatch"
manual_reviewer="$(require_kv "$MANUAL_ARTIFACTS" reviewer manual)"
reject_missing_marker "$manual_reviewer"
[[ -n "$manual_reviewer" ]] || not_run "manual-reviewer-missing"

if [[ "$SUBJECT" == spaceterm ]]; then
    require_ordered_kv_schema "$NATIVE_LAUNCH_OBSERVATION" \
        "schema observation.source launch.nonce run.id package.app.sha256 runtime.schema runtime.sample_interval_ms runtime.transition_capacity failure.action.schema failure.action.enabled process.pid process.pidversion process.executable.path process.executable.device process.executable.inode process.executable.fsid process.signature.cdhash process.signature.identifier process.signature.team_identifier terminal_font_selected initial_grid.rows initial_grid.columns initial_grid.logical_width initial_grid.logical_height initial_grid.backing_pixel_width initial_grid.backing_pixel_height provisional.observation.sha256 runtime.metadata.schema runtime.metadata.path runtime.metadata.sha256 failure.result.schema failure.actions.path failure.actions.sha256 failure.request_count failure.result_count observation.complete" \
        native-launch-observation
    [[ "$(awk 'END { print NR }' "$NATIVE_LAUNCH_OBSERVATION")" == 36 ]] \
        || not_run "native-launch-observation-record-count-mismatch"
    for launch_key in launch.nonce package.app.sha256 runtime.schema \
        runtime.sample_interval_ms runtime.transition_capacity \
        process.pid process.pidversion \
        process.executable.path process.executable.device process.executable.inode \
        process.executable.fsid process.signature.cdhash process.signature.identifier \
        terminal_font_selected initial_grid.rows \
        initial_grid.columns initial_grid.logical_width initial_grid.logical_height \
        initial_grid.backing_pixel_width initial_grid.backing_pixel_height; do
        [[ -n "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" "$launch_key" launch)" ]] \
            || not_run "native-launch-observation-missing-$launch_key"
    done
    [[ "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" launch.nonce launch)" \
            =~ ^[0-9a-f]{64}$ \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" run.id launch)" \
            =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,79}$ \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" process.pidversion launch)" \
            =~ ^[1-9][0-9]*$ \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" process.executable.fsid launch)" \
            =~ ^-?[0-9]+:-?[0-9]+$ ]] \
        || not_run "native-launch-observation-value-invalid"
    subject_team_identifier="$(require_kv "$SUBJECT_IDENTITY" team_identifier subject)"
    expected_native_team_identifier="$subject_team_identifier"
    [[ "$subject_team_identifier" != none ]] || expected_native_team_identifier=""
    [[ "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" schema launch)" \
            == spaceterm.acceptance.native-launch-proof/v5 \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" observation.source launch)" \
            == production-app \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" launch.nonce launch)" \
            == "$NONCE" \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" run.id launch)" \
            == "$CAMPAIGN_ID" \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" runtime.schema launch)" \
            == spaceterm.acceptance.runtime-stream/v1 \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" runtime.sample_interval_ms launch)" \
            == 1000 \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" runtime.transition_capacity launch)" \
            == 64 \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" failure.action.schema launch)" \
            == spaceterm.acceptance.failure-action/v1 \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" failure.action.enabled launch)" == false \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" provisional.observation.sha256 launch)" \
            == "$(sha256 "$NATIVE_PROVISIONAL_OBSERVATION")" \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" runtime.metadata.schema launch)" \
            == spaceterm.acceptance.runtime-observation-metadata/v3 \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" runtime.metadata.path launch)" \
            == runtime-metadata.tsv \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" runtime.metadata.sha256 launch)" \
            == "$(sha256 "$RUNTIME_METADATA")" \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" failure.result.schema launch)" \
            == spaceterm.acceptance.failure-action-result/v2 \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" failure.actions.path launch)" \
            == failure-actions.tsv \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" failure.actions.sha256 launch)" \
            == "$(sha256 "$FAILURE_ACTIONS")" \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" failure.request_count launch)" == 0 \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" failure.result_count launch)" == 0 \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" observation.complete launch)" == true \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" process.pid launch)" \
            == "$(require_kv "$SUBJECT_IDENTITY" process_pid subject)" \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" process.executable.device launch)" \
            == "$(require_kv "$SUBJECT_IDENTITY" executable_device subject)" \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" process.executable.inode launch)" \
            == "$(require_kv "$SUBJECT_IDENTITY" executable_inode subject)" \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" process.signature.cdhash launch \
                | tr '[:upper:]' '[:lower:]')" \
            == "$(require_kv "$SUBJECT_IDENTITY" cdhash subject \
                | tr '[:upper:]' '[:lower:]')" \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" process.signature.identifier launch)" \
            == "$(require_kv "$SUBJECT_IDENTITY" signing_identifier subject)" \
        && "$(kv "$NATIVE_LAUNCH_OBSERVATION" process.signature.team_identifier)" \
            == "$expected_native_team_identifier" ]] \
        || not_run "native-launch-observation-does-not-bind-subject"
    reject_unknown_kv "$RUNTIME_METADATA" \
        "schema observation.source run.id package.app.sha256 process.pid runtime.samples.path runtime.samples.sha256 runtime.events.path runtime.events.sha256 failure.action.schema failure.action.enabled failure.result.schema failure.actions.path failure.actions.sha256 failure.request_count failure.result_count observer.started_continuous_ns observer.ended_continuous_ns observer.sample_interval_ms observer.transition_capacity observer.sample_count observer.event_count observer.status observation.complete" \
        runtime-metadata
    [[ "$(require_kv "$RUNTIME_METADATA" schema runtime)" \
            == spaceterm.acceptance.runtime-observation-metadata/v3 \
        && "$(require_kv "$RUNTIME_METADATA" observation.source runtime)" == production-app \
        && "$(require_kv "$RUNTIME_METADATA" observer.status runtime)" == complete \
        && "$(require_kv "$RUNTIME_METADATA" observation.complete runtime)" == true \
        && "$(require_kv "$RUNTIME_METADATA" run.id runtime)" \
            == "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" run.id launch)" \
        && "$(require_kv "$RUNTIME_METADATA" package.app.sha256 runtime)" \
            == "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" package.app.sha256 launch)" \
        && "$(require_kv "$RUNTIME_METADATA" process.pid runtime)" \
            == "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" process.pid launch)" \
        && "$(require_kv "$RUNTIME_METADATA" runtime.samples.path runtime)" \
            == runtime-samples.tsv \
        && "$(require_kv "$RUNTIME_METADATA" runtime.events.path runtime)" \
            == runtime-events.tsv \
        && "$(require_kv "$RUNTIME_METADATA" failure.action.schema runtime)" \
            == spaceterm.acceptance.failure-action/v1 \
        && "$(require_kv "$RUNTIME_METADATA" failure.action.enabled runtime)" == false \
        && "$(require_kv "$RUNTIME_METADATA" failure.result.schema runtime)" \
            == spaceterm.acceptance.failure-action-result/v2 \
        && "$(require_kv "$RUNTIME_METADATA" failure.actions.path runtime)" \
            == failure-actions.tsv \
        && "$(require_kv "$RUNTIME_METADATA" failure.actions.sha256 runtime)" \
            == "$(sha256 "$FAILURE_ACTIONS")" \
        && "$(require_kv "$RUNTIME_METADATA" failure.request_count runtime)" == 0 \
        && "$(require_kv "$RUNTIME_METADATA" failure.result_count runtime)" == 0 \
        && "$(require_kv "$RUNTIME_METADATA" observer.sample_interval_ms runtime)" == 1000 \
        && "$(require_kv "$RUNTIME_METADATA" observer.transition_capacity runtime)" == 64 ]] \
        || not_run "runtime-observer-incomplete"
    readonly FAILURE_ACTION_HEADER=$'request_id\tsequence\tcase_id\taction\tresult\tpane_id\tpane_state\tfailure_class\tfailure_recoverability\tfailure_operation\tstate_revision\tlatest_generation\tlast_valid_generation\tvisible_generation\tpending_recovery\tterminal_input_usable\tsession_attached\tresource_staged_count\tresource_staged_bytes\tresource_rolled_back_count\tresource_rolled_back_bytes'
    [[ "$(head -n 1 "$FAILURE_ACTIONS")" == "$FAILURE_ACTION_HEADER" \
        && "$(awk 'END { print NR }' "$FAILURE_ACTIONS")" == 1 ]] \
        || not_run "performance-run-has-failure-action-results"
    observer_started="$(require_kv "$RUNTIME_METADATA" observer.started_continuous_ns runtime)"
    observer_ended="$(require_kv "$RUNTIME_METADATA" observer.ended_continuous_ns runtime)"
    require_uint "$observer_started" runtime-observer-started
    require_uint "$observer_ended" runtime-observer-ended
    (( observer_started <= workload_started && observer_ended >= workload_ended )) \
        || not_run "runtime-observer-does-not-cover-workload"
    runtime_sample_count="$(require_kv "$RUNTIME_METADATA" observer.sample_count runtime)"
    runtime_event_count="$(require_kv "$RUNTIME_METADATA" observer.event_count runtime)"
    require_uint "$runtime_sample_count" runtime-sample-count
    require_uint "$runtime_event_count" runtime-event-count
    [[ "$runtime_sample_count" == "$(awk 'END { print NR - 1 }' "$RUNTIME_SAMPLES")" \
        && "$runtime_event_count" == "$(awk 'END { print NR - 1 }' "$RUNTIME_EVENTS")" \
        && "$(require_kv "$RUNTIME_METADATA" runtime.samples.sha256 runtime)" \
            == "$(sha256 "$RUNTIME_SAMPLES")" \
        && "$(require_kv "$RUNTIME_METADATA" runtime.events.sha256 runtime)" \
            == "$(sha256 "$RUNTIME_EVENTS")" ]] \
        || not_run "runtime-observer-identity-or-hash-mismatch"

    required_runtime_samples=$((measured_duration_ms / 1000 + 1))
    awk -F '\t' -v required="$required_runtime_samples" \
        -v required_inputs="$workload_reads" -v scenario="$SCENARIO" \
        -v workload_start="$workload_started" -v workload_end="$workload_ended" '
        NR == 1 { next }
        NF != 36 { exit 1 }
        {
            for (field = 1; field <= 34; field += 1) {
                if ($field !~ /^[0-9]+$/) exit 1
            }
            if ($36 !~ /^[0-9]+$/) exit 1
        }
        $1 + 0 != NR - 2 || (NR > 2 && $2 + 0 <= prior_time) { exit 1 }
        NR > 2 && ($2 + 0 - prior_time < 900000000 \
            || $2 + 0 - prior_time > 1100000000) { exit 1 }
        NR > 2 {
            monotonic[3] = 1; monotonic[4] = 1; monotonic[5] = 1
            monotonic[6] = 1; monotonic[8] = 1; monotonic[9] = 1
            monotonic[10] = 1; monotonic[11] = 1; monotonic[12] = 1
            monotonic[13] = 1; monotonic[14] = 1; monotonic[15] = 1
            monotonic[22] = 1; monotonic[26] = 1; monotonic[27] = 1
            monotonic[28] = 1; monotonic[29] = 1; monotonic[34] = 1
            monotonic[36] = 1
            for (counter in monotonic) if ($counter + 0 < prior[counter]) exit 1
        }
        { for (counter in monotonic) prior[counter] = $counter + 0 }
        { if (NR == 2) first_time = $2 + 0; prior_time = $2 + 0 }
        $16 !~ /^[01]$/ || $17 !~ /^[01]$/ || $18 !~ /^[01]$/ \
            || $19 !~ /^[01]$/ || $20 !~ /^[01]$/ || $21 !~ /^[01]$/ \
            || $25 !~ /^[01]$/ { exit 1 }
        !($35 == "running" || $35 == "exited" || $35 == "failed" \
            || $35 == "observer-failed") { exit 1 }
        $36 + 0 != 0 { exit 1 }
        $7 + 0 > $8 + 0 { exit 1 }
        $8 + 0 > 2 || $11 + 0 > 2 { fail_gate = 1 }
        $6 + 0 > 0 { superseded = 1 }
        $35 == "exited" {
            exited += 1
            exit_row = NR
            if ($3 + 0 != $12 + 0 \
                || $3 + 0 != $13 + 0 || $3 + 0 != $14 + 0) final_mismatch = 1
        }
        $35 == "failed" || $35 == "observer-failed" { failed = 1 }
        END {
            if ($34 + 0 != required_inputs) coverage_bad = 1
            if (scenario == "resize" && ($26 + 0 < 300 || $28 + 0 == 0 \
                || $29 + 0 == 0 || $26 + 0 != $28 + 0 + $29 + 0)) {
                resize_bad = 1
            }
            coverage_bad = NR - 1 < required || first_time > workload_start \
                || prior_time < workload_end - 1000000000 \
                || exit_row != NR || exited != 1 \
                || coverage_bad
            exit resize_bad ? 7 : (failed ? 6 : (final_mismatch ? 5 : (fail_gate ? 4 \
                : (!superseded ? 3 : (coverage_bad ? 2 : (!exited ? 1 : 0))))))
        }
    ' "$RUNTIME_SAMPLES" || runtime_status=$?
    runtime_status="${runtime_status:-0}"
    [[ "$runtime_status" != 1 ]] || not_run "runtime-lifecycle-missing"
    [[ "$runtime_status" != 2 ]] || not_run "runtime-sample-coverage-is-incomplete"
    [[ "$runtime_status" != 3 ]] || fail "runtime-produced-no-superseded-screen-evidence"
    [[ "$runtime_status" != 4 ]] || fail "runtime-backlog-bound-exceeded"
    [[ "$runtime_status" != 5 ]] || fail "final-screen-was-not-presented-before-exit"
    [[ "$runtime_status" != 6 ]] || fail "runtime-reported-failure"
    [[ "$runtime_status" != 7 ]] \
        || fail "runtime-resize-request-apply-coalescing-incomplete"

    final_runtime_generation="$(awk -F '\t' 'END { print $3 }' "$RUNTIME_SAMPLES")"
    require_uint "$final_runtime_generation" runtime-final-generation
    awk -F '\t' -v workload_end="$workload_ended" \
        -v final_generation="$final_runtime_generation" '
        NR == 1 { next }
        NF != 6 || $1 !~ /^[0-9]+$/ || $2 !~ /^[0-9]+$/ \
            || $4 !~ /^[0-9]+$/ || $5 !~ /^[0-9]+$/ || $6 !~ /^[0-9]+$/ { exit 1 }
        !($3 == "visibility-lost" || $3 == "visibility-restored" \
            || $3 == "first-next-frame-after-restore" || $3 == "session-exited" \
            || $3 == "session-failed" || $3 == "observer-failed") { exit 1 }
        $1 + 0 != NR - 2 || (NR > 2 && $2 + 0 <= prior_time) { exit 1 }
        { prior_time = $2 + 0 }
        $3 == "visibility-lost" {
            if (hidden || awaiting) stale = 1
            hidden = 1
            lost += 1
        }
        $3 == "visibility-restored" {
            if (!hidden || awaiting) stale = 1
            hidden = 0
            restored += 1
            restored_generation = $4 + 0
            awaiting = 1
        }
        $3 == "first-next-frame-after-restore" {
            if (!awaiting || $4 + 0 < restored_generation) stale = 1
            awaiting = 0
        }
        $3 == "session-exited" {
            exited += 1
            exit_row = NR
            exit_time = $2 + 0
            exit_generation = $4 + 0
            if ($4 + 0 != final_generation || $5 + 0 != 0 || $6 + 0 != 0) failed = 1
        }
        $3 == "session-failed" || $3 == "observer-failed" { failed = 1 }
        END {
            incomplete = exit_row != NR || exited != 1 || exit_time < workload_end \
                || awaiting || hidden || lost != restored
            exit failed ? 3 : (stale ? 2 : (incomplete ? 1 : 0))
        }
    ' "$RUNTIME_EVENTS" || runtime_event_status=$?
    runtime_event_status="${runtime_event_status:-0}"
    [[ "$runtime_event_status" != 1 ]] || not_run "runtime-exit-event-missing"
    [[ "$runtime_event_status" != 2 ]] || fail "stale-generation-presented-after-restore"
    [[ "$runtime_event_status" != 3 ]] || fail "runtime-observer-or-session-failed"

    if [[ "$SCENARIO" == scrolled ]]; then
        awk -F '\t' -v start="$workload_started" -v end="$workload_ended" '
            NR > 1 && $2 + 0 >= start && $2 + 0 <= end {
                covered += 1
                if ($24 + 0 == 0 || $22 + 0 < 10000) bad = 1
            }
            END { exit !covered || bad }
        ' "$RUNTIME_SAMPLES" || fail "scrolled-viewport-was-not-continuously-anchored"
    fi
    if [[ "$SCENARIO" == scrolled || "$SCENARIO" == resize ]]; then
        awk -F '\t' 'NR > 1 && $22 + 0 >= 10000 { found = 1 } END { exit !found }' \
            "$RUNTIME_SAMPLES" || not_run "runtime-does-not-prove-10000-retained-rows"
    fi
    if [[ "$SCENARIO" == hidden-occluded ]]; then
        hidden_transition_count="$(awk -F '\t' \
            '$3 == "visibility-lost" { lost += 1 } \
             $3 == "visibility-restored" { restored += 1 } \
             $3 == "first-next-frame-after-restore" { first += 1 } \
             END { print lost "\t" restored "\t" first }' "$RUNTIME_EVENTS")"
        IFS=$'\t' read -r hidden_lost hidden_restored hidden_first \
            <<< "$hidden_transition_count"
        (( hidden_lost == 4 && hidden_restored == 4 && hidden_first == 4 )) \
            || not_run "hidden-runtime-transition-evidence-incomplete"
        # Native driver actions do not prove nonpresentability. Require runtime
        # samples in every native hidden/occluded interval and no next frames.
        awk -F '\t' '
            NR == FNR {
                if (FNR > 1 && ($4 == "minimize" || $4 == "occluder-show")) {
                    starts[++n] = $2 + 0
                } else if (FNR > 1 && ($4 == "restore" || $4 == "occluder-hide")) {
                    ends[n] = $2 + 0
                }
                next
            }
            FNR == 1 { next }
            {
                for (i = 1; i <= n; i++) if ($2 + 0 >= starts[i] && $2 + 0 <= ends[i]) {
                    covered[i] += 1
                    if ($16 + 0 != 0) bad = 1
                    if (covered[i] > 1 && $15 + 0 != prior_frames[i]) bad = 1
                    prior_frames[i] = $15 + 0
                }
            }
            END {
                for (i = 1; i <= n; i++) if (!covered[i]) bad = 1
                exit bad
            }
        ' "$DRIVER_EVENTS" "$RUNTIME_SAMPLES" \
            || fail "hidden-state-or-no-frame-proof-failed"
    fi
else
    # Ghostty has no SpaceTerm internal observation contract. Its hidden state
    # must be proven by native driver observations and retained manual video.
    if [[ "$SCENARIO" == hidden-occluded ]]; then
        awk -F '\t' '
            NR > 1 && ($4 == "minimize" || $4 == "restore" \
                || $4 == "occluder-show" || $4 == "occluder-hide") {
                seen[$4] += 1
                if ($11 != "verified") bad = 1
            }
            END {
                exit bad || !seen["minimize"] || !seen["restore"] \
                    || !seen["occluder-show"] || !seen["occluder-hide"]
            }
        ' "$DRIVER_EVENTS" || not_run "ghostty-native-hidden-state-not-proven"
    fi
fi

analyzer_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
if [[ "$SCENARIO" == resize ]]; then
    resize_driver_count="$(awk -F '\t' 'NR > 1 && $4 == "resize-grid" \
        && $11 == "verified" { count += 1 } END { print count + 0 }' "$DRIVER_EVENTS")"
    geometry_summary="$(awk -F '\t' '
        NR > 1 && $3 == "geometry" {
            count += 1
            key = $6 "x" $7 "@" $8 "x" $9
            distinct[key] = 1
            if (prior != "" && key != prior) changes += 1
            prior = key
        }
        END {
            for (key in distinct) distinct_count += 1
            print count "\t" distinct_count "\t" changes + 0
        }
    ' "$WORKLOAD_EVENTS")"
    IFS=$'\t' read -r geometry_count distinct_geometry_count geometry_change_count \
        <<< "$geometry_summary"
    (( resize_driver_count >= 300 && geometry_count >= 301 \
        && geometry_change_count >= 300 && distinct_geometry_count >= 3 )) \
        || not_run "native-resize-or-producer-geometry-evidence-incomplete"
    [[ "$(comment_kv "$RSS_SAMPLES" completed_resize_cycles)" == "$resize_driver_count" \
        && "$(comment_kv "$RSS_SAMPLES" geometry_change_count)" \
            == "$geometry_change_count" \
        && "$(comment_kv "$RSS_SAMPLES" distinct_geometry_count)" \
            == "$distinct_geometry_count" \
        && "$(comment_kv "$RSS_SAMPLES" geometry_correlated)" == true ]] \
        || not_run "resize-rss-geometry-accounting-mismatch"
    if [[ "$SUBJECT" == spaceterm ]]; then
        awk -F '\t' '
            NR == FNR {
                if (FNR > 1 && $3 == "geometry") {
                    times[++n] = $2 + 0
                    geometry[n] = $6 "\t" $7 "\t" $8 "\t" $9
                }
                next
            }
            FNR == 1 { next }
            {
                while (geometry_index < n \
                    && times[geometry_index + 1] <= $2 + 0) geometry_index += 1
                if (geometry_index > 0 \
                    && geometry[geometry_index] != $30 "\t" $31 "\t" $32 "\t" $33) bad = 1
            }
            END { exit !n || !geometry_index || bad }
        ' "$WORKLOAD_EVENTS" "$RUNTIME_SAMPLES" \
            || not_run "runtime-pty-geometry-does-not-match-producer-tiocgwinsz"
    fi
    analyzer="$analyzer_directory/analyze-release-performance-resize.awk"
    [[ -f "$analyzer" ]] || not_run "resize-analyzer-missing"
    resize_report="$(awk -f "$analyzer" "$RSS_SAMPLES" 2>/dev/null)" || resize_status=$?
    resize_status="${resize_status:-0}"
    [[ "$resize_status" != 2 ]] || not_run "resize-memory-evidence-not-runnable"
    [[ "$resize_status" != 1 ]] || fail "resize-memory-growth-correlated"
    grep -Fxq $'result\tPASS' <<< "$resize_report" \
        || not_run "resize-memory-result-missing"
else
    analyzer="$analyzer_directory/analyze-release-performance-sustained.awk"
    [[ -f "$analyzer" ]] || not_run "sustained-analyzer-missing"
    sustained_report="$(awk -f "$analyzer" "$RSS_SAMPLES" 2>/dev/null)" \
        || sustained_status=$?
    sustained_status="${sustained_status:-0}"
    [[ "$sustained_status" != 2 ]] || not_run "sustained-memory-evidence-not-runnable"
    [[ "$sustained_status" != 1 ]] || fail "sustained-memory-did-not-plateau"
    grep -Fxq $'result\tPASS' <<< "$sustained_report" \
        || not_run "sustained-memory-result-missing"
fi

verdict CASE-COMPLETE all-required-evidence-complete
