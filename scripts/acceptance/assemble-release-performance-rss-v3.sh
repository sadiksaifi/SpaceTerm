#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
umask 077

SUBJECT_IDENTITY=""
SCENARIO=""
REQUESTED_DURATION_MS=""
REQUESTED_WARMUP_MS=""
RAW_SAMPLES=""
WORKLOAD_EVENTS=""
DRIVER_EVENTS=""
OUTPUT=""
TEMP=""

usage() {
    cat <<EOF
Usage: $(basename -- "$0") --subject-identity FILE --scenario NAME \\
  --requested-warmup-ms N --requested-duration-ms N \\
  --raw-samples FILE --workload-events FILE \\
  --driver-events FILE --output FILE

Join authenticated raw 10-second RSS observations with exact workload and
native-driver event times. The input sampler owns process pinning; this step
does not manufacture intermediate byte counts or resize completions.

Raw sample header:
  elapsed_ms  continuous_ns  rss_kib

The authenticated raw file also carries exact format, subject-identity,
warm-up, duration, interval, and completion metadata.
EOF
}

die() { echo "error: $*" >&2; exit 1; }
cleanup() { [[ -z "$TEMP" ]] || rm -f -- "$TEMP"; }
sha256() { shasum -a 256 "$1" | awk '{ print $1 }'; }

while (( $# > 0 )); do
    case "$1" in
        --subject-identity) SUBJECT_IDENTITY="${2:-}"; shift ;;
        --scenario) SCENARIO="${2:-}"; shift ;;
        --requested-warmup-ms) REQUESTED_WARMUP_MS="${2:-}"; shift ;;
        --requested-duration-ms) REQUESTED_DURATION_MS="${2:-}"; shift ;;
        --raw-samples) RAW_SAMPLES="${2:-}"; shift ;;
        --workload-events) WORKLOAD_EVENTS="${2:-}"; shift ;;
        --driver-events) DRIVER_EVENTS="${2:-}"; shift ;;
        --output) OUTPUT="${2:-}"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument: $1" ;;
    esac
    shift
done

case "$SCENARIO" in
    ascii|unicode-styles|scrolled|hidden-occluded|resize) ;;
    *) die "invalid scenario" ;;
esac
[[ "$REQUESTED_DURATION_MS" =~ ^[1-9][0-9]*$ \
    && $((REQUESTED_DURATION_MS % 10000)) == 0 ]] \
    || die "duration must be a positive multiple of 10000 ms"
[[ "$REQUESTED_WARMUP_MS" =~ ^[0-9]+$ ]] || die "warm-up must be milliseconds"
case "$SCENARIO:$REQUESTED_WARMUP_MS" in
    resize:0|ascii:60000|unicode-styles:60000|scrolled:60000|hidden-occluded:60000) ;;
    *) die "warm-up does not match scenario protocol" ;;
esac
for input in "$SUBJECT_IDENTITY" "$RAW_SAMPLES" "$WORKLOAD_EVENTS" "$DRIVER_EVENTS"; do
    [[ -f "$input" && -r "$input" ]] || die "required input is unavailable"
done
[[ -n "$OUTPUT" && ! -e "$OUTPUT" ]] || die "output path is missing or exists"
for command in awk chmod ln mkdir rm shasum; do
    command -v "$command" >/dev/null 2>&1 || die "required command not found: $command"
done
[[ "$(head -n 1 "$RAW_SAMPLES")" == $'elapsed_ms\tcontinuous_ns\trss_kib' ]] \
    || die "raw RSS header is invalid"
[[ "$(head -n 1 "$WORKLOAD_EVENTS")" \
    == $'sequence\tcontinuous_ns\tkind\tevent_id\tbyte_count\trows\tcolumns\tpixel_width\tpixel_height\tstatus' ]] \
    || die "workload event header is invalid"
[[ "$(head -n 1 "$DRIVER_EVENTS")" \
    == $'sequence\tcontinuous_ns\tevent_id\taction\ttarget_pid\twindow_number\trequested_a\trequested_b\tobserved_a\tobserved_b\tresult' ]] \
    || die "driver event header is invalid"

mkdir -p -- "$(dirname -- "$OUTPUT")"
TEMP="${OUTPUT}.tmp.$$"
trap cleanup EXIT INT TERM

awk -F '\t' -v OFS='\t' \
    -v scenario="$SCENARIO" \
    -v duration="$REQUESTED_DURATION_MS" \
    -v warmup="$REQUESTED_WARMUP_MS" \
    -v identity_hash="$(sha256 "$SUBJECT_IDENTITY")" \
    -v workload_hash="$(sha256 "$WORKLOAD_EVENTS")" \
    -v driver_hash="$(sha256 "$DRIVER_EVENTS")" \
    -v workload_file="$WORKLOAD_EVENTS" \
    -v driver_file="$DRIVER_EVENTS" \
    -v raw_file="$RAW_SAMPLES" '
    FILENAME == workload_file {
        if (FNR == 1) next
        if (NF != 10 || $1 + 0 != FNR - 2 || $2 !~ /^[0-9]+$/ \
            || $5 !~ /^[0-9]+$/ || $6 !~ /^[0-9]+$/ || $7 !~ /^[0-9]+$/ \
            || $8 !~ /^[0-9]+$/ || $9 !~ /^[0-9]+$/) exit 10
        if (FNR > 2 && $2 + 0 <= workload_prior_time) exit 10
        workload_prior_time = $2 + 0
        if ($3 == "producer-end") {
            if (++producer_end_count != 1 || $10 != "success") exit 10
            producer_end_time = $2 + 0
            emitted_bytes = $5 + 0
        }
        if ($3 == "geometry") {
            geometry_events += 1
            geometry_key = $6 "x" $7 "@" $8 "x" $9
            distinct_geometry[geometry_key] = 1
            if (prior_geometry != "" && geometry_key != prior_geometry) {
                geometry_changes += 1
            }
            prior_geometry = geometry_key
        }
        next
    }
    FILENAME == driver_file {
        if (FNR == 1) next
        if (NF != 11 || $1 + 0 != FNR - 2 || $2 !~ /^[0-9]+$/) exit 11
        if (FNR > 2 && $2 + 0 <= driver_prior_time) exit 11
        driver_prior_time = $2 + 0
        if ($11 != "verified") exit 11
        if ($4 == "resize-grid") resize_time[++completed_resizes] = $2 + 0
        next
    }
    FILENAME == raw_file {
        if (FNR == 1) next
        if (substr($1, 1, 2) == "# ") {
            if (NF != 2) exit 12
            key = substr($1, 3)
            if (key == "status") {
                if (raw_status_seen++) exit 12
                raw_status = $2
                raw_status_row = FNR
            } else {
                if (sample_count || raw_metadata_seen[key]++) exit 12
                if (!(key == "format_version" || key == "sample_interval_ms" \
                    || key == "requested_warmup_ms" \
                    || key == "requested_duration_ms" \
                    || key == "subject_identity_sha256")) exit 12
                raw_metadata[key] = $2
            }
            next
        }
        if (raw_status_seen) exit 12
        if (NF != 3 || $1 !~ /^[0-9]+$/ || $2 !~ /^[0-9]+$/ \
            || $3 !~ /^[1-9][0-9]*$/) exit 12
        sample_count += 1
        sample_elapsed[sample_count] = $1 + 0
        sample_time[sample_count] = $2 + 0
        sample_rss[sample_count] = $3 + 0
        if (sample_count > 1 && (sample_elapsed[sample_count] <= sample_elapsed[sample_count - 1] \
            || sample_time[sample_count] <= sample_time[sample_count - 1])) exit 12
        next
    }
    END {
        if (producer_end_count != 1 || !sample_count \
            || raw_metadata["format_version"] != "1" \
            || raw_metadata["sample_interval_ms"] != "10000" \
            || raw_metadata["requested_warmup_ms"] != warmup \
            || raw_metadata["requested_duration_ms"] != duration \
            || raw_metadata["subject_identity_sha256"] != identity_hash \
            || raw_status != "complete" || raw_status_row != FNR \
            || sample_elapsed[1] > 1000 \
            || sample_elapsed[sample_count] < duration \
            || sample_elapsed[sample_count] > duration + 1000 \
            || sample_time[sample_count] < producer_end_time) exit 13
        for (key in distinct_geometry) distinct_geometry_count += 1
        print "elapsed_ms", "continuous_ns", "rss_kib", "workload_bytes", "resize_count"
        print "# format_version", "3"
        print "# scenario", scenario
        print "# sample_interval_ms", "10000"
        print "# requested_duration_ms", duration
        print "# subject_identity_sha256", identity_hash
        print "# workload_events_sha256", workload_hash
        print "# driver_events_sha256", driver_hash
        if (scenario == "resize") {
            print "# distinct_geometry_count", distinct_geometry_count
            print "# geometry_change_count", geometry_changes
            print "# completed_resize_cycles", completed_resizes
            print "# geometry_correlated", \
                (completed_resizes >= 300 && geometry_changes >= 300 \
                    && distinct_geometry_count >= 3 ? "true" : "false")
        }
        resize_index = 1
        for (sample = 1; sample <= sample_count; sample += 1) {
            while (resize_index <= completed_resizes \
                && resize_time[resize_index] <= sample_time[sample]) resize_index += 1
            # Workload protocol exposes exact total bytes only at producer-end.
            # Earlier values remain zero; they are never interpolated.
            observed_bytes = sample_time[sample] >= producer_end_time ? emitted_bytes : 0
            print sample_elapsed[sample], sample_time[sample], sample_rss[sample], \
                observed_bytes, resize_index - 1
        }
        print "# status", "complete"
    }
' "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$RAW_SAMPLES" > "$TEMP" \
    || die "input event or raw-sample evidence is invalid"

chmod 0444 "$TEMP"
ln "$TEMP" "$OUTPUT" || die "output path was created concurrently"
rm -f -- "$TEMP"
TEMP=""
trap - EXIT INT TERM
