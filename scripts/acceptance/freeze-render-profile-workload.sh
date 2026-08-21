#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
umask 077

SUBJECT=""
SCENARIO=""
PLAN=""
PLAN_METADATA=""
PAIR_METADATA=""
SUBJECT_IDENTITY=""
DRIVER_EVENTS=""
ACTION_VIDEO=""
OUTPUT=""
TEMP=""

readonly DRIVER_HEADER=$'sequence\tcontinuous_ns\tevent_id\taction\ttarget_pid\twindow_number\trequested_a\trequested_b\tobserved_a\tobserved_b\tresult'

usage() {
    cat <<EOF
Usage: $(basename -- "$0") --subject spaceterm|ghostty --scenario NAME \\
  --plan FILE --plan-metadata FILE --pair-metadata FILE \\
  --subject-identity FILE --driver-events FILE --action-video FILE \\
  --output FILE

Freeze one render profile's measured interval from the authenticated native
driver stream. Checkpoint rows provide cadence for operator-executed actions;
live-resize rows are the native resize actions. The action video remains the
required proof that manual Cursor/blink/Selection/Marked Text actions occurred.
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

while (( $# > 0 )); do
    case "$1" in
        --subject) SUBJECT="${2:-}"; shift ;;
        --scenario) SCENARIO="${2:-}"; shift ;;
        --plan) PLAN="${2:-}"; shift ;;
        --plan-metadata) PLAN_METADATA="${2:-}"; shift ;;
        --pair-metadata) PAIR_METADATA="${2:-}"; shift ;;
        --subject-identity) SUBJECT_IDENTITY="${2:-}"; shift ;;
        --driver-events) DRIVER_EVENTS="${2:-}"; shift ;;
        --action-video) ACTION_VIDEO="${2:-}"; shift ;;
        --output) OUTPUT="${2:-}"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument: $1" ;;
    esac
    shift
done

[[ "$SUBJECT" == spaceterm || "$SUBJECT" == ghostty ]] || die "invalid subject"
case "$SCENARIO" in
    perf-render-idle-cursor-blink|perf-render-text-blink \
        |perf-render-sustained-output|perf-render-selection \
        |perf-render-marked-text|perf-render-live-resize) ;;
    *) die "invalid render scenario" ;;
esac
for command in awk chmod head ln mkdir rm shasum; do
    command -v "$command" >/dev/null 2>&1 || die "required command not found: $command"
done
for input in "$PLAN" "$PLAN_METADATA" "$PAIR_METADATA" "$SUBJECT_IDENTITY" \
    "$DRIVER_EVENTS" "$ACTION_VIDEO"; do
    [[ -f "$input" && ! -L "$input" && -r "$input" && -s "$input" ]] \
        || die "required input is unavailable"
done
[[ -n "$OUTPUT" && ! -e "$OUTPUT" ]] || die "output path is missing or exists"
[[ "$(head -n 1 "$DRIVER_EVENTS")" == "$DRIVER_HEADER" ]] \
    || die "driver event header is invalid"
[[ ! -w "$DRIVER_EVENTS" ]] || die "driver event stream is not immutable"

plan_hash="$(sha256 "$PLAN")"
duration_ms="$(kv "$PLAN_METADATA" measured_duration_ms)"
required_count="$(kv "$PLAN_METADATA" required_action_count)"
[[ "$(kv "$PLAN_METADATA" format_version)" == 2 \
    && "$(kv "$PLAN_METADATA" scenario)" == "$SCENARIO" \
    && "$(kv "$PLAN_METADATA" plan_sha256)" == "$plan_hash" \
    && "$duration_ms" =~ ^[1-9][0-9]*$ \
    && "$required_count" =~ ^[1-9][0-9]*$ ]] \
    || die "render plan metadata is invalid"
pair_hash="$(sha256 "$PAIR_METADATA")"
subject_hash="$(sha256 "$SUBJECT_IDENTITY")"
[[ "$(kv "$PAIR_METADATA" format_version)" == 1 \
    && "$(kv "$PAIR_METADATA" scenario)" == "$SCENARIO" \
    && "$(kv "$PAIR_METADATA" plan_sha256)" == "$plan_hash" \
    && "$(kv "$PAIR_METADATA" duration_ms)" == "$duration_ms" \
    && "$(kv "$PAIR_METADATA" "${SUBJECT}_subject_identity_sha256")" \
        == "$subject_hash" \
    && "$(kv "$SUBJECT_IDENTITY" format_version)" == 1 \
    && "$(kv "$SUBJECT_IDENTITY" subject)" == "$SUBJECT" \
    && "$(kv "$SUBJECT_IDENTITY" identity_status)" == frozen ]] \
    || die "pair or subject identity is invalid"
process_pid="$(kv "$SUBJECT_IDENTITY" process_pid)"
[[ "$process_pid" =~ ^[1-9][0-9]*$ ]] || die "subject process PID is invalid"

# The native driver must execute every immutable plan event exactly once, in
# order, for the frozen process. This is direct native-action evidence for
# live resize and the measured cadence clock for the manually reviewed cases.
awk -F '\t' -v pid="$process_pid" '
    NR == FNR {
        if (FNR > 1) {
            ids[++plan_count] = $1
            offsets[plan_count] = $2 + 0
            actions[plan_count] = $3
            arguments0[plan_count] = $4
            arguments1[plan_count] = $5
        }
        next
    }
    FNR == 1 { next }
    {
        event_count += 1
        if (NF != 11 || $1 !~ /^[0-9]+$/ || $2 !~ /^[0-9]+$/ \
            || $5 !~ /^[1-9][0-9]*$/ || $6 !~ /^[1-9][0-9]*$/ \
            || $7 !~ /^-?[0-9]+$/ || $8 !~ /^-?[0-9]+$/ \
            || $9 !~ /^-?[0-9]+$/ || $10 !~ /^-?[0-9]+$/ \
            || $1 + 0 != event_count - 1 || $5 != pid || $11 != "verified" \
            || event_count > plan_count || $3 != ids[event_count] \
            || $4 != actions[event_count] \
            || $7 != arguments0[event_count] || $8 != arguments1[event_count] \
            || (event_count > 1 && $2 + 0 <= prior_time)) bad = 1
        if (event_count == 1) {
            first_time = $2 + 0
            first_offset = offsets[event_count]
            window = $6
        } else {
            expected_delta = (offsets[event_count] - first_offset) * 1000000
            observed_delta = $2 + 0 - first_time
            cadence_error = observed_delta - expected_delta
            if (cadence_error < 0) cadence_error = -cadence_error
            if ($6 != window || cadence_error > 250000000) bad = 1
        }
        if ($4 == "resize-grid") {
            delta_a = $9 - $7
            delta_b = $10 - $8
            if (delta_a < 0) delta_a = -delta_a
            if (delta_b < 0) delta_b = -delta_b
            if (delta_a > 8 || delta_b > 8) bad = 1
        } else if ($4 == "checkpoint" || $4 == "stop") {
            if ($9 != 1 || ($10 != 0 && $10 != 1)) bad = 1
        }
        prior_time = $2 + 0
    }
    END { exit bad || event_count != plan_count }
' "$PLAN" "$DRIVER_EVENTS" || die "driver events do not prove exact plan execution"

measured_started="$(awk -F '\t' '$3 == "measured-start" { count += 1; value = $2 } \
    END { if (count == 1) print value }' "$DRIVER_EVENTS")"
measured_ended="$(awk -F '\t' '$3 == "measured-end" { count += 1; value = $2 } \
    END { if (count == 1) print value }' "$DRIVER_EVENTS")"
[[ "$measured_started" =~ ^[0-9]+$ && "$measured_ended" =~ ^[0-9]+$ \
    && "$measured_ended" -ge "$((measured_started + duration_ms * 1000000))" \
    && "$measured_ended" -le "$((measured_started + (duration_ms + 2000) * 1000000))" ]] \
    || die "driver events do not cover the measured duration"

case "$SCENARIO" in
    perf-render-idle-cursor-blink) action_prefix=cursor-blink ;;
    perf-render-text-blink) action_prefix=text-blink ;;
    perf-render-sustained-output) action_prefix=changed-row ;;
    perf-render-selection) action_prefix=selection-overlay ;;
    perf-render-marked-text) action_prefix=marked-text-overlay ;;
    perf-render-live-resize) action_prefix=resize ;;
esac
completed_count="$(awk -F '\t' -v prefix="^${action_prefix}-[0-9][0-9][0-9]$" \
    '$3 ~ prefix && $11 == "verified" { count += 1 } END { print count + 0 }' \
    "$DRIVER_EVENTS")"
[[ "$completed_count" == "$required_count" ]] \
    || die "required render action cadence is incomplete"
if [[ "$SCENARIO" == perf-render-live-resize ]]; then
    [[ "$(awk -F '\t' '$3 ~ /^resize-[0-9][0-9][0-9]$/ \
        && $4 == "resize-grid" && $11 == "verified" { count += 1 } \
        END { print count + 0 }' "$DRIVER_EVENTS")" == "$required_count" ]] \
        || die "native live-resize evidence is incomplete"
fi

mkdir -p -- "$(dirname -- "$OUTPUT")"
TEMP="${OUTPUT}.tmp.$$"
trap cleanup EXIT INT TERM
{
    printf 'format_version\t1\n'
    printf 'scenario\t%s\n' "$SCENARIO"
    printf 'subject\t%s\n' "$SUBJECT"
    printf 'subject_identity_sha256\t%s\n' "$subject_hash"
    printf 'pair_metadata_sha256\t%s\n' "$pair_hash"
    printf 'driver_events_sha256\t%s\n' "$(sha256 "$DRIVER_EVENTS")"
    printf 'action_video_sha256\t%s\n' "$(sha256 "$ACTION_VIDEO")"
    printf 'required_action_count\t%s\n' "$required_count"
    printf 'completed_action_count\t%s\n' "$completed_count"
    printf 'started_continuous_ns\t%s\n' "$measured_started"
    printf 'ended_continuous_ns\t%s\n' "$measured_ended"
    printf 'status\tcomplete\n'
} > "$TEMP"
chmod 0444 "$TEMP"
ln "$TEMP" "$OUTPUT" || die "output path was created concurrently"
rm -f -- "$TEMP"
TEMP=""
trap - EXIT INT TERM
printf 'render_workload_metadata_sha256\t%s\n' "$(sha256 "$OUTPUT")"
