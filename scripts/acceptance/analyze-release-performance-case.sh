#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C

SUBJECT=""
SCENARIO=""
PLAN=""
PLAN_METADATA=""
PAIR_METADATA=""
SUBJECT_IDENTITY=""
RUN_METADATA=""
WORKLOAD_METADATA=""
WORKLOAD_EVENTS=""
DRIVER_EVENTS=""
RSS_SAMPLES=""
RUNTIME_SAMPLES=""
RUNTIME_EVENTS=""
RUNTIME_METADATA=""
NATIVE_LAUNCH_OBSERVATION=""
TRACE_METADATA=""
MANUAL_ARTIFACTS=""
MANUAL_SCREENSHOT=""
MANUAL_VIDEO=""

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
  --subject-identity FILE --run-metadata FILE \\
  --workload-metadata FILE --workload-events FILE \\
  --driver-events FILE --rss-samples FILE --trace-metadata FILE \\
  --manual-artifacts FILE --manual-screenshot FILE --manual-video FILE \
  [SPACETERM RUNTIME FILES]

SpaceTerm runtime files:
  --runtime-samples FILE --runtime-events FILE --runtime-metadata FILE
  --native-launch-observation FILE

Print a content-free PASS, FAIL, or NOT-RUN verdict for one native release-
performance case. PASS requires the paired immutable plan and workload,
authenticated evidence, exact state/event correlation, and manual artifacts.
EOF
}

verdict() {
    local result="$1"
    local reason="$2"
    printf 'format_version\t1\n'
    printf 'subject\t%s\n' "${SUBJECT:-unknown}"
    printf 'scenario\t%s\n' "${SCENARIO:-unknown}"
    printf 'result\t%s\n' "$result"
    printf 'reason\t%s\n' "$reason"
    case "$result" in
        PASS) exit 0 ;;
        FAIL) exit 1 ;;
        NOT-RUN) exit 2 ;;
        *) exit 3 ;;
    esac
}

not_run() { verdict NOT-RUN "$1"; }
fail() { verdict FAIL "$1"; }

require_file() {
    [[ -f "$2" && -r "$2" ]] || not_run "missing-$1"
}

sha256() {
    shasum -a 256 "$1" | awk '{ print $1 }'
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

while (( $# > 0 )); do
    case "$1" in
        --subject) SUBJECT="${2:-}"; shift ;;
        --scenario) SCENARIO="${2:-}"; shift ;;
        --plan) PLAN="${2:-}"; shift ;;
        --plan-metadata) PLAN_METADATA="${2:-}"; shift ;;
        --pair-metadata) PAIR_METADATA="${2:-}"; shift ;;
        --subject-identity) SUBJECT_IDENTITY="${2:-}"; shift ;;
        --run-metadata) RUN_METADATA="${2:-}"; shift ;;
        --workload-metadata) WORKLOAD_METADATA="${2:-}"; shift ;;
        --workload-events) WORKLOAD_EVENTS="${2:-}"; shift ;;
        --driver-events) DRIVER_EVENTS="${2:-}"; shift ;;
        --rss-samples) RSS_SAMPLES="${2:-}"; shift ;;
        --runtime-samples) RUNTIME_SAMPLES="${2:-}"; shift ;;
        --runtime-events) RUNTIME_EVENTS="${2:-}"; shift ;;
        --runtime-metadata) RUNTIME_METADATA="${2:-}"; shift ;;
        --native-launch-observation) NATIVE_LAUNCH_OBSERVATION="${2:-}"; shift ;;
        --trace-metadata) TRACE_METADATA="${2:-}"; shift ;;
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
command -v shasum >/dev/null 2>&1 || not_run "shasum-unavailable"

require_file scenario-plan "$PLAN"
require_file plan-metadata "$PLAN_METADATA"
require_file pair-metadata "$PAIR_METADATA"
require_file subject-identity "$SUBJECT_IDENTITY"
require_file run-metadata "$RUN_METADATA"
require_file workload-metadata "$WORKLOAD_METADATA"
require_file workload-events "$WORKLOAD_EVENTS"
require_file driver-events "$DRIVER_EVENTS"
require_file rss-samples "$RSS_SAMPLES"
require_file trace-metadata "$TRACE_METADATA"
require_file manual-artifacts "$MANUAL_ARTIFACTS"
require_file manual-screenshot "$MANUAL_SCREENSHOT"
require_file manual-video "$MANUAL_VIDEO"
if [[ "$SUBJECT" == spaceterm ]]; then
    require_file runtime-samples "$RUNTIME_SAMPLES"
    require_file runtime-events "$RUNTIME_EVENTS"
    require_file runtime-metadata "$RUNTIME_METADATA"
    require_file native-launch-observation "$NATIVE_LAUNCH_OBSERVATION"
elif [[ -n "$RUNTIME_SAMPLES$RUNTIME_EVENTS$RUNTIME_METADATA$NATIVE_LAUNCH_OBSERVATION" ]]; then
    not_run "ghostty-must-not-claim-spaceterm-runtime-observations"
fi

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

[[ "$(comment_kv "$RSS_SAMPLES" format_version)" == 3 \
    && "$(comment_kv "$RSS_SAMPLES" scenario)" == "$SCENARIO" \
    && "$(comment_kv "$RSS_SAMPLES" subject_identity_sha256)" == "$(sha256 "$SUBJECT_IDENTITY")" \
    && "$(comment_kv "$RSS_SAMPLES" workload_events_sha256)" == "$(sha256 "$WORKLOAD_EVENTS")" \
    && "$(comment_kv "$RSS_SAMPLES" driver_events_sha256)" == "$(sha256 "$DRIVER_EVENTS")" ]] \
    || not_run "rss-evidence-binding-mismatch"

reject_unknown_kv "$PLAN_METADATA" \
    "format_version scenario plan_sha256 input_schedule_sha256 warmup_ms measured_duration_ms input_interval_ms required_seed_rows required_resize_cycles geometry_authority native_resize_arguments" \
    plan-metadata
reject_unknown_kv "$PAIR_METADATA" \
    "format_version pair_id scenario plan_sha256 workload_sha256 command_sha256 environment_sha256 font_sha256 initial_grid_sha256 duration_ms spaceterm_subject_identity_sha256 ghostty_subject_identity_sha256" \
    pair-metadata
reject_unknown_kv "$WORKLOAD_METADATA" \
    "format_version scenario producer_sha256 seed_sha256 seed_bytes requested_duration_ms warmup_ms requested_iterations requested_seed_rows emitted_bytes input_events started_continuous_ns ended_continuous_ns status" \
    workload-metadata
reject_unknown_kv "$SUBJECT_IDENTITY" \
    "format_version subject app_bundle_path bundle_identifier bundle_version executable_path executable_sha256 executable_device executable_inode executable_fsid signature_valid signing_identifier team_identifier cdhash process_pid process_start_identity identity_status" \
    subject-identity
reject_unknown_kv "$RUN_METADATA" \
    "format_version subject subject_identity_sha256 scenario scenario_plan_sha256 workload_sha256 command_sha256 environment_sha256 font_sha256 initial_grid_sha256 measured_duration_ms process_pid process_start_identity status" \
    run-metadata
reject_unknown_kv "$TRACE_METADATA" \
    "format_version capture_status incomplete_reason subject_identity_sha256 requested_duration_ms actual_duration_ms target_identity_verified trace_target_pid_verified time_profiler_instrument allocations_instrument hangs_instrument time_profiler_target_verified allocations_target_verified hangs_target_verified time_profiler_rows allocations_rows hangs_rows maximum_main_thread_hang_ms status" \
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

[[ "$(require_kv "$RUN_METADATA" format_version run)" == 1 \
    && "$(require_kv "$RUN_METADATA" subject run)" == "$SUBJECT" \
    && "$(require_kv "$RUN_METADATA" scenario run)" == "$SCENARIO" \
    && "$(require_kv "$RUN_METADATA" subject_identity_sha256 run)" == "$subject_hash" \
    && "$(require_kv "$RUN_METADATA" scenario_plan_sha256 run)" == "$plan_hash" \
    && "$(require_kv "$RUN_METADATA" measured_duration_ms run)" == "$measured_duration_ms" \
    && "$(require_kv "$RUN_METADATA" status run)" == complete ]] \
    || not_run "run-metadata-binding-mismatch"
for parity_key in workload_sha256 command_sha256 environment_sha256 font_sha256 initial_grid_sha256; do
    run_hash="$(require_kv "$RUN_METADATA" "$parity_key" run)"
    require_hash "$run_hash" "run-$parity_key"
    [[ "$run_hash" == "$(require_kv "$PAIR_METADATA" "$parity_key" pair)" ]] \
        || not_run "paired-$parity_key-mismatch"
done
[[ "$(require_kv "$RUN_METADATA" process_pid run)" \
        == "$(require_kv "$SUBJECT_IDENTITY" process_pid subject)" \
    && "$(require_kv "$RUN_METADATA" process_start_identity run)" \
        == "$(require_kv "$SUBJECT_IDENTITY" process_start_identity subject)" ]] \
    || not_run "run-process-identity-mismatch"
for identity_key in executable_device executable_inode executable_fsid process_pid; do
    identity_value="$(require_kv "$SUBJECT_IDENTITY" "$identity_key" subject)"
    require_uint "$identity_value" "subject-$identity_key"
done

[[ "$(require_kv "$WORKLOAD_METADATA" format_version workload)" == 2 ]] \
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
require_uint "$workload_started" workload-started-continuous-ns
require_uint "$workload_ended" workload-ended-continuous-ns
require_uint "$workload_emitted" workload-emitted-bytes
(( workload_ended > workload_started )) || not_run "invalid-workload-duration"
actual_workload_ms=$(((workload_ended - workload_started) / 1000000))
(( actual_workload_ms >= measured_duration_ms \
    && actual_workload_ms <= measured_duration_ms + 2000 )) \
    || not_run "workload-does-not-cover-duration"

# All event artifacts are append-only, exact-schema, sequence- and time-ordered.
awk -F '\t' '
    NR == 1 { next }
    NF != 10 { exit 1 }
    $1 !~ /^[0-9]+$/ || $2 !~ /^[0-9]+$/ || $5 !~ /^[0-9]+$/ \
        || $6 !~ /^[0-9]+$/ || $7 !~ /^[0-9]+$/ || $8 !~ /^[0-9]+$/ \
        || $9 !~ /^[0-9]+$/ { exit 1 }
    !($3 == "started" || $3 == "seed-complete" || $3 == "input-read" \
        || $3 == "input-ack-written" || $3 == "geometry" || $3 == "producer-end") { exit 1 }
    !(($3 == "producer-end" && $10 == "success") \
        || ($3 != "producer-end" && $10 == "ok")) { exit 1 }
    $1 + 0 != NR - 2 || (NR > 2 && $2 + 0 <= prior_time) { exit 1 }
    { prior_time = $2 + 0 }
    $3 == "started" { started += 1; start_row = NR }
    $3 == "seed-complete" { seeds += 1; seed_row = NR }
    $3 == "producer-end" { ended += 1; end_row = NR }
    END { exit !(started == 1 && start_row == 2 && seeds == 1 \
        && seed_row > start_row && ended == 1 && end_row == NR) }
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

awk -F '\t' '
    NR == 1 { next }
    NF != 11 { exit 1 }
    $1 !~ /^[0-9]+$/ || $2 !~ /^[0-9]+$/ || $5 !~ /^[1-9][0-9]*$/ \
        || $6 !~ /^[1-9][0-9]*$/ || $7 !~ /^-?[0-9]+$/ \
        || $8 !~ /^-?[0-9]+$/ || $9 !~ /^-?[0-9]+$/ \
        || $10 !~ /^-?[0-9]+$/ { exit 1 }
    !($4 == "input" || $4 == "scroll-rows" || $4 == "minimize" \
        || $4 == "restore" || $4 == "occluder-show" || $4 == "occluder-hide" \
        || $4 == "resize-grid" || $4 == "checkpoint" || $4 == "stop") { exit 1 }
    $1 + 0 != NR - 2 || (NR > 2 && $2 + 0 <= prior_time) || seen[$3]++ { exit 1 }
    { prior_time = $2 + 0 }
' "$DRIVER_EVENTS" || not_run "invalid-driver-event-stream"
awk -F '\t' 'NR > 1 && $11 != "verified" { exit 1 }' "$DRIVER_EVENTS" \
    || fail "native-driver-action-failed"

# The driver starts only after observing seed-complete. This binds its plan
# clock to the producer without inventing a cross-process launch timestamp.
driver_first_event="$(awk -F '\t' 'NR == 2 { print $2 }' "$DRIVER_EVENTS")"
measured_event_id=measured-start
[[ "$SCENARIO" != resize ]] || measured_event_id=seed-checkpoint
driver_measured_event="$(awk -F '\t' -v wanted="$measured_event_id" \
    '$3 == wanted { count += 1; value = $2 } \
    END { if (count == 1) print value }' "$DRIVER_EVENTS")"
require_uint "$driver_first_event" driver-first-event-time
require_uint "$driver_measured_event" driver-measured-event-time
awk -v driver="$driver_first_event" -v seed="$workload_seed_event" \
    -v measured="$driver_measured_event" -v workload="$workload_started" '
    BEGIN {
        seed_skew = driver - seed
        measured_skew = measured - workload
        if (seed_skew < 0 || seed_skew > 2000000000 \
            || measured_skew < -2000000000 || measured_skew > 2000000000) exit 1
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

[[ "$(require_kv "$TRACE_METADATA" format_version trace)" == 3 ]] \
    || not_run "unsupported-trace-format"
[[ "$(require_kv "$TRACE_METADATA" capture_status trace)" == CAPTURED \
    && "$(require_kv "$TRACE_METADATA" status trace)" == complete ]] \
    || not_run "trace-capture-incomplete"
[[ "$(require_kv "$TRACE_METADATA" subject_identity_sha256 trace)" == "$subject_hash" \
    && "$(require_kv "$TRACE_METADATA" target_identity_verified trace)" == true \
    && "$(require_kv "$TRACE_METADATA" trace_target_pid_verified trace)" == true ]] \
    || not_run "trace-target-binding-unsupported-or-mismatched"
trace_requested="$(require_kv "$TRACE_METADATA" requested_duration_ms trace)"
trace_actual="$(require_kv "$TRACE_METADATA" actual_duration_ms trace)"
require_uint "$trace_requested" trace-requested-duration
require_uint "$trace_actual" trace-actual-duration
(( trace_requested == measured_duration_ms && trace_actual >= measured_duration_ms \
    && trace_actual <= measured_duration_ms + 2000 )) \
    || not_run "trace-duration-incomplete"
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
        == "$(sha256 "$MANUAL_SCREENSHOT")" \
    && "$(require_kv "$MANUAL_ARTIFACTS" video_sha256 manual)" \
        == "$(sha256 "$MANUAL_VIDEO")" ]] \
    || not_run "manual-artifact-file-hash-mismatch"
manual_reviewer="$(require_kv "$MANUAL_ARTIFACTS" reviewer manual)"
reject_missing_marker "$manual_reviewer"
[[ -n "$manual_reviewer" ]] || not_run "manual-reviewer-missing"

if [[ "$SUBJECT" == spaceterm ]]; then
    reject_unknown_kv "$NATIVE_LAUNCH_OBSERVATION" \
        "schema observation.source launch.nonce run.id package.app.sha256 process.pid process.pidversion process.executable.path process.executable.device process.executable.inode process.executable.fsid process.signature.cdhash process.signature.identifier process.signature.team_identifier terminal_font_selected initial_grid.rows initial_grid.columns initial_grid.logical_width initial_grid.logical_height initial_grid.backing_pixel_width initial_grid.backing_pixel_height observation.complete" \
        native-launch-observation
    [[ "$(awk 'END { print NR }' "$NATIVE_LAUNCH_OBSERVATION")" == 22 ]] \
        || not_run "native-launch-observation-record-count-mismatch"
    for launch_key in launch.nonce package.app.sha256 process.pid process.pidversion \
        process.executable.path process.executable.device process.executable.inode \
        process.executable.fsid process.signature.cdhash process.signature.identifier \
        process.signature.team_identifier terminal_font_selected initial_grid.rows \
        initial_grid.columns initial_grid.logical_width initial_grid.logical_height \
        initial_grid.backing_pixel_width initial_grid.backing_pixel_height; do
        [[ -n "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" "$launch_key" launch)" ]] \
            || not_run "native-launch-observation-missing-$launch_key"
    done
    [[ "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" schema launch)" \
            == spaceterm.acceptance.native-launch-proof/v2 \
        && "$(require_kv "$NATIVE_LAUNCH_OBSERVATION" observation.source launch)" \
            == production-app \
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
            == "$(require_kv "$SUBJECT_IDENTITY" signing_identifier subject)" ]] \
        || not_run "native-launch-observation-does-not-bind-subject"
    reject_unknown_kv "$RUNTIME_METADATA" \
        "schema observation.source run.id package.app.sha256 process.pid runtime.samples.path runtime.samples.sha256 runtime.events.path runtime.events.sha256 observer.started_continuous_ns observer.ended_continuous_ns observer.sample_interval_ms observer.transition_capacity observer.sample_count observer.event_count observer.status observation.complete" \
        runtime-metadata
    [[ "$(require_kv "$RUNTIME_METADATA" schema runtime)" \
            == spaceterm.acceptance.runtime-observation-metadata/v1 \
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
        && "$(require_kv "$RUNTIME_METADATA" observer.sample_interval_ms runtime)" == 1000 \
        && "$(require_kv "$RUNTIME_METADATA" observer.transition_capacity runtime)" == 64 ]] \
        || not_run "runtime-observer-incomplete"
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

verdict PASS all-required-evidence-passed
