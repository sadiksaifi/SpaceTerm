#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
umask 077

SCENARIO=""
PLAN_PATH=""
METADATA_PATH=""
PLAN_TEMP=""
METADATA_TEMP=""

usage() {
    cat <<EOF
Usage: $(basename -- "$0") --scenario NAME --plan PATH --metadata PATH

Create one immutable, subject-independent release-performance scenario plan.
The same plan and metadata files must be used for the paired SpaceTerm and
Ghostty runs.

Scenarios:
  ascii             60-second warm-up and 10-minute ASCII output.
  unicode-styles    60-second warm-up and 10-minute Unicode/style output.
  scrolled          10,000-row seed and 10 minutes away from the bottom.
  hidden-occluded   10 minutes with fixed minimize and occlusion phases.
  resize            10,000-row seed and 300 native live-resize cycles.
  perf-render-idle-cursor-blink
                    15-second warm-up and 60 observed Cursor-blink cycles.
  perf-render-text-blink
                    15-second warm-up and 60 observed text-blink cycles.
  perf-render-sustained-output
                    30-second warm-up and 18 changed-row observation windows.
  perf-render-selection
                    15-second warm-up and 30 Selection overlay cycles.
  perf-render-marked-text
                    15-second warm-up and 24 Marked Text overlay cycles.
  perf-render-live-resize
                    Three minutes and 180 native live-resize cycles.
EOF
}

die() {
    echo "error: $*" >&2
    exit 1
}

cleanup() {
    [[ -z "$PLAN_TEMP" ]] || rm -f -- "$PLAN_TEMP"
    [[ -z "$PLAN_TEMP" ]] || rm -f -- "${PLAN_TEMP}.sorted"
    [[ -z "$METADATA_TEMP" ]] || rm -f -- "$METADATA_TEMP"
}

is_safe_output() {
    [[ -n "$1" && "$1" != */ && "$(basename -- "$1")" != "." \
        && "$(basename -- "$1")" != ".." ]]
}

emit() {
    local event_id="$1"
    local offset_ms="$2"
    local action="$3"
    local arg0="$4"
    local arg1="$5"
    printf '%s\t%s\t%s\t%s\t%s\n' \
        "$event_id" "$offset_ms" "$action" "$arg0" "$arg1" >> "$PLAN_TEMP"
}

emit_inputs() {
    local start_ms="$1"
    local end_ms="$2"
    local interval_ms="$3"
    local offset_ms
    local index=0
    for ((offset_ms = start_ms; offset_ms < end_ms; offset_ms += interval_ms)); do
        printf -v event_id 'input-%03d' "$index"
        emit "$event_id" "$offset_ms" input 0 0
        ((index += 1))
    done
}

emit_profile_checkpoints() {
    local prefix="$1"
    local start_ms="$2"
    local end_ms="$3"
    local interval_ms="$4"
    local offset_ms
    local index=0
    for ((offset_ms = start_ms; offset_ms < end_ms; offset_ms += interval_ms)); do
        printf -v event_id '%s-%03d' "$prefix" "$index"
        emit "$event_id" "$offset_ms" checkpoint "$index" 0
        ((index += 1))
    done
}

while (( $# > 0 )); do
    case "$1" in
        --scenario) SCENARIO="${2:-}"; shift ;;
        --plan) PLAN_PATH="${2:-}"; shift ;;
        --metadata) METADATA_PATH="${2:-}"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument: $1" ;;
    esac
    shift
done

case "$SCENARIO" in
    ascii|unicode-styles|scrolled|hidden-occluded|resize \
        |perf-render-idle-cursor-blink|perf-render-text-blink \
        |perf-render-sustained-output|perf-render-selection \
        |perf-render-marked-text|perf-render-live-resize) ;;
    *) die "unknown or missing scenario: $SCENARIO" ;;
esac
is_safe_output "$PLAN_PATH" || die "--plan must name a file"
is_safe_output "$METADATA_PATH" || die "--metadata must name a file"
[[ "$PLAN_PATH" != "$METADATA_PATH" ]] || die "plan and metadata paths must differ"
[[ ! -e "$PLAN_PATH" ]] || die "plan path already exists: $PLAN_PATH"
[[ ! -e "$METADATA_PATH" ]] || die "metadata path already exists: $METADATA_PATH"
command -v shasum >/dev/null 2>&1 || die "required command not found: shasum"

mkdir -p -- "$(dirname -- "$PLAN_PATH")" "$(dirname -- "$METADATA_PATH")"
PLAN_TEMP="${PLAN_PATH}.tmp.$$"
METADATA_TEMP="${METADATA_PATH}.tmp.$$"
trap cleanup EXIT INT TERM

printf 'event_id\toffset_ms\taction\targ0\targ1\n' > "$PLAN_TEMP"

warmup_ms=60000
measured_duration_ms=600000
required_resize_cycles=0
required_seed_rows=0
input_interval_ms=30000
plan_format_version=1
profile_kind="none"
required_action="none"
required_action_count=0
required_unchanged_row_windows=0
required_changed_row_windows=0
required_overlay_change_windows=0

case "$SCENARIO" in
    ascii|unicode-styles)
        emit warmup-start 0 checkpoint 0 0
        emit measured-start "$warmup_ms" checkpoint 0 0
        emit_inputs "$warmup_ms" "$((warmup_ms + measured_duration_ms))" \
            "$input_interval_ms"
        emit measured-end "$((warmup_ms + measured_duration_ms))" checkpoint 0 0
        emit stop "$((warmup_ms + measured_duration_ms))" stop 0 0
        ;;
    scrolled)
        required_seed_rows=10000
        emit seed-checkpoint 0 checkpoint "$required_seed_rows" 0
        # Native line deltas are positive when scrolling into retained rows.
        emit scroll-away 55000 scroll-rows 500 0
        emit measured-start "$warmup_ms" checkpoint 0 0
        emit_inputs "$warmup_ms" "$((warmup_ms + measured_duration_ms))" \
            "$input_interval_ms"
        emit measured-end "$((warmup_ms + measured_duration_ms))" checkpoint 0 0
        emit stop "$((warmup_ms + measured_duration_ms))" stop 0 0
        ;;
    hidden-occluded)
        emit warmup-start 0 checkpoint 0 0
        emit measured-start "$warmup_ms" checkpoint 0 0
        # Phase offsets are shifted by the 60-second warm-up. The measured
        # windows are 30-150, 160-280, 290-410, and 420-540 seconds.
        emit minimize-1 90000 minimize 0 0
        emit restore-1 210000 restore 0 0
        emit occlude-1 220000 occluder-show 0 0
        emit unocclude-1 340000 occluder-hide 0 0
        emit minimize-2 350000 minimize 0 0
        emit restore-2 470000 restore 0 0
        emit occlude-2 480000 occluder-show 0 0
        emit unocclude-2 600000 occluder-hide 0 0
        emit_inputs "$warmup_ms" "$((warmup_ms + measured_duration_ms))" \
            "$input_interval_ms"
        emit measured-end "$((warmup_ms + measured_duration_ms))" checkpoint 0 0
        emit stop "$((warmup_ms + measured_duration_ms))" stop 0 0
        ;;
    resize)
        warmup_ms=0
        measured_duration_ms=300000
        required_resize_cycles=300
        required_seed_rows=10000
        input_interval_ms=15000
        emit seed-checkpoint 0 checkpoint "$required_seed_rows" 0
        for ((cycle = 0; cycle < required_resize_cycles; cycle += 1)); do
            sign=1
            (( cycle % 2 == 0 )) || sign=-1
            case $((cycle % 3)) in
                0) width_delta=$((sign * 160)); height_delta=0 ;;
                1) width_delta=0; height_delta=$((sign * 120)) ;;
                2) width_delta=$((sign * 120)); height_delta=$((sign * 80)) ;;
            esac
            printf -v event_id 'resize-%03d' "$cycle"
            emit "$event_id" "$((cycle * 1000))" resize-grid \
                "$width_delta" "$height_delta"
        done
        emit_inputs 0 "$measured_duration_ms" "$input_interval_ms"
        emit measured-end "$measured_duration_ms" checkpoint 0 0
        emit stop "$measured_duration_ms" stop 0 0
        ;;
    perf-render-idle-cursor-blink)
        plan_format_version=2
        profile_kind=idle-cursor-blink
        warmup_ms=15000
        measured_duration_ms=120000
        required_action=cursor-blink-observation
        required_action_count=60
        required_unchanged_row_windows=60
        emit profile-warmup-start 0 checkpoint 0 0
        emit measured-start "$warmup_ms" checkpoint 0 0
        emit_profile_checkpoints cursor-blink "$warmup_ms" \
            "$((warmup_ms + measured_duration_ms))" 2000
        emit measured-end "$((warmup_ms + measured_duration_ms))" checkpoint 0 0
        emit stop "$((warmup_ms + measured_duration_ms))" stop 0 0
        ;;
    perf-render-text-blink)
        plan_format_version=2
        profile_kind=text-blink
        warmup_ms=15000
        measured_duration_ms=120000
        required_action=text-blink-observation
        required_action_count=60
        required_unchanged_row_windows=60
        emit profile-warmup-start 0 checkpoint 0 0
        emit measured-start "$warmup_ms" checkpoint 0 0
        emit_profile_checkpoints text-blink "$warmup_ms" \
            "$((warmup_ms + measured_duration_ms))" 2000
        emit measured-end "$((warmup_ms + measured_duration_ms))" checkpoint 0 0
        emit stop "$((warmup_ms + measured_duration_ms))" stop 0 0
        ;;
    perf-render-sustained-output)
        plan_format_version=2
        profile_kind=sustained-output
        warmup_ms=30000
        measured_duration_ms=180000
        required_action=changed-row-observation
        required_action_count=18
        required_changed_row_windows=18
        emit profile-warmup-start 0 checkpoint 0 0
        emit measured-start "$warmup_ms" checkpoint 0 0
        emit_profile_checkpoints changed-row "$warmup_ms" \
            "$((warmup_ms + measured_duration_ms))" 10000
        emit measured-end "$((warmup_ms + measured_duration_ms))" checkpoint 0 0
        emit stop "$((warmup_ms + measured_duration_ms))" stop 0 0
        ;;
    perf-render-selection)
        plan_format_version=2
        profile_kind=selection
        warmup_ms=15000
        measured_duration_ms=120000
        required_action=selection-overlay-cycle
        required_action_count=30
        required_unchanged_row_windows=30
        required_overlay_change_windows=30
        emit profile-warmup-start 0 checkpoint 0 0
        emit measured-start "$warmup_ms" checkpoint 0 0
        emit_profile_checkpoints selection-overlay "$warmup_ms" \
            "$((warmup_ms + measured_duration_ms))" 4000
        emit measured-end "$((warmup_ms + measured_duration_ms))" checkpoint 0 0
        emit stop "$((warmup_ms + measured_duration_ms))" stop 0 0
        ;;
    perf-render-marked-text)
        plan_format_version=2
        profile_kind=marked-text
        warmup_ms=15000
        measured_duration_ms=120000
        required_action=marked-text-overlay-cycle
        required_action_count=24
        required_unchanged_row_windows=24
        required_overlay_change_windows=24
        emit profile-warmup-start 0 checkpoint 0 0
        emit measured-start "$warmup_ms" checkpoint 0 0
        emit_profile_checkpoints marked-text-overlay "$warmup_ms" \
            "$((warmup_ms + measured_duration_ms))" 5000
        emit measured-end "$((warmup_ms + measured_duration_ms))" checkpoint 0 0
        emit stop "$((warmup_ms + measured_duration_ms))" stop 0 0
        ;;
    perf-render-live-resize)
        plan_format_version=2
        profile_kind=live-resize
        warmup_ms=0
        measured_duration_ms=180000
        required_resize_cycles=180
        required_action=live-resize-cycle
        required_action_count=180
        required_changed_row_windows=180
        emit measured-start 0 checkpoint 0 0
        for ((cycle = 0; cycle < required_resize_cycles; cycle += 1)); do
            sign=1
            (( cycle % 2 == 0 )) || sign=-1
            case $((cycle % 3)) in
                0) width_delta=$((sign * 120)); height_delta=0 ;;
                1) width_delta=0; height_delta=$((sign * 80)) ;;
                2) width_delta=$((sign * 96)); height_delta=$((sign * 64)) ;;
            esac
            printf -v event_id 'resize-%03d' "$cycle"
            emit "$event_id" "$((cycle * 1000))" resize-grid \
                "$width_delta" "$height_delta"
        done
        emit measured-end "$measured_duration_ms" checkpoint 0 0
        emit stop "$measured_duration_ms" stop 0 0
        ;;
esac

# Sort by numeric offset while preserving file order for simultaneous actions.
{
    head -n 1 "$PLAN_TEMP"
    tail -n +2 "$PLAN_TEMP" | awk -F '\t' '{ print $2 "\t" NR "\t" $0 }' \
        | sort -t $'\t' -k1,1n -k2,2n | cut -f3-
} > "${PLAN_TEMP}.sorted"
mv -- "${PLAN_TEMP}.sorted" "$PLAN_TEMP"

plan_sha256="$(shasum -a 256 "$PLAN_TEMP" | awk '{ print $1 }')"
input_schedule_sha256="$(awk -F '\t' 'NR == 1 || $3 == "input"' "$PLAN_TEMP" \
    | shasum -a 256 | awk '{ print $1 }')"
{
    printf 'format_version\t%d\n' "$plan_format_version"
    printf 'scenario\t%s\n' "$SCENARIO"
    printf 'plan_sha256\t%s\n' "$plan_sha256"
    printf 'warmup_ms\t%d\n' "$warmup_ms"
    printf 'measured_duration_ms\t%d\n' "$measured_duration_ms"
    if (( plan_format_version == 1 )); then
        printf 'input_schedule_sha256\t%s\n' "$input_schedule_sha256"
        printf 'input_interval_ms\t%d\n' "$input_interval_ms"
        printf 'required_seed_rows\t%d\n' "$required_seed_rows"
        printf 'required_resize_cycles\t%d\n' "$required_resize_cycles"
        printf 'geometry_authority\tproducer-tiocgwinsz\n'
        printf 'native_resize_arguments\twindow-pixel-deltas-not-grid-claims\n'
    else
        printf 'profile_kind\t%s\n' "$profile_kind"
        printf 'required_action\t%s\n' "$required_action"
        printf 'required_action_count\t%d\n' "$required_action_count"
        printf 'required_unchanged_row_windows\t%d\n' \
            "$required_unchanged_row_windows"
        printf 'required_changed_row_windows\t%d\n' \
            "$required_changed_row_windows"
        printf 'required_overlay_change_windows\t%d\n' \
            "$required_overlay_change_windows"
        printf 'required_resize_cycles\t%d\n' "$required_resize_cycles"
        printf 'trace_instruments\tTime-Profiler,Allocations,Hangs\n'
        printf 'manual_review_schema\trender-call-tree-review-v1\n'
    fi
} > "$METADATA_TEMP"

mv -- "$PLAN_TEMP" "$PLAN_PATH"
PLAN_TEMP=""
mv -- "$METADATA_TEMP" "$METADATA_PATH"
METADATA_TEMP=""
chmod 0444 "$PLAN_PATH" "$METADATA_PATH"
trap - EXIT INT TERM
printf 'plan_sha256\t%s\n' "$plan_sha256"
