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
WORKLOAD_METADATA=""
READY_RECEIPT=""
PLAN_START_GATE=""
DRIVER_EVENTS=""
CAMPAIGN_ID=""
SESSION_ID=""
NONCE=""
CAMPAIGN_SECRET_FILE=""
OUTPUT=""
TEMP=""
AUTH_SNAPSHOT_DIR=""

usage() {
    cat <<EOF
Usage: $(basename -- "$0") --subject-identity FILE --scenario NAME \\
  --requested-warmup-ms N --requested-duration-ms N \\
  --raw-samples FILE --workload-events FILE --workload-metadata FILE \\
  --ready-receipt FILE \\
  --plan-start-gate FILE \\
  --campaign-id ID --session-id ID --nonce 64_LOWER_HEX \\
  --campaign-secret-file FILE --driver-events FILE --output FILE

Join authenticated raw 10-second RSS observations with the latest authenticated
one-second producer progress at or before each observation and exact native-
driver event times. This step never interpolates, backfills, or uses a future
producer event.

Raw sample header:
  elapsed_ms  continuous_ns  rss_kib

The authenticated raw file also carries exact format, subject-identity,
warm-up, duration, interval, and completion metadata.
EOF
}

die() { echo "error: $*" >&2; exit 1; }
cleanup() {
    [[ -z "$TEMP" ]] || rm -f -- "$TEMP"
    [[ -z "$AUTH_SNAPSHOT_DIR" ]] || rm -rf -- "$AUTH_SNAPSHOT_DIR"
}
sha256() { shasum -a 256 "$1" | awk '{ print $1 }'; }

while (( $# > 0 )); do
    case "$1" in
        --subject-identity) SUBJECT_IDENTITY="${2:-}"; shift ;;
        --scenario) SCENARIO="${2:-}"; shift ;;
        --requested-warmup-ms) REQUESTED_WARMUP_MS="${2:-}"; shift ;;
        --requested-duration-ms) REQUESTED_DURATION_MS="${2:-}"; shift ;;
        --raw-samples) RAW_SAMPLES="${2:-}"; shift ;;
        --workload-events) WORKLOAD_EVENTS="${2:-}"; shift ;;
        --workload-metadata) WORKLOAD_METADATA="${2:-}"; shift ;;
        --ready-receipt) READY_RECEIPT="${2:-}"; shift ;;
        --plan-start-gate) PLAN_START_GATE="${2:-}"; shift ;;
        --driver-events) DRIVER_EVENTS="${2:-}"; shift ;;
        --campaign-id) CAMPAIGN_ID="${2:-}"; shift ;;
        --session-id) SESSION_ID="${2:-}"; shift ;;
        --nonce) NONCE="${2:-}"; shift ;;
        --campaign-secret-file) CAMPAIGN_SECRET_FILE="${2:-}"; shift ;;
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
for input in "$SUBJECT_IDENTITY" "$RAW_SAMPLES" "$WORKLOAD_EVENTS" \
    "$WORKLOAD_METADATA" "$READY_RECEIPT" "$PLAN_START_GATE" "$DRIVER_EVENTS" \
    "$CAMPAIGN_SECRET_FILE"; do
    [[ -f "$input" && -r "$input" ]] || die "required input is unavailable"
done
[[ -n "$OUTPUT" && ! -e "$OUTPUT" ]] || die "output path is missing or exists"
for command in awk chmod ln mkdir mktemp python3 rm shasum; do
    command -v "$command" >/dev/null 2>&1 || die "required command not found: $command"
done
[[ "$CAMPAIGN_ID" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,79}$ ]] \
    || die "campaign ID is invalid"
[[ "$SESSION_ID" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,79}$ ]] \
    || die "session ID is invalid"
[[ "$NONCE" =~ ^[0-9a-f]{64}$ ]] || die "nonce is invalid"
AUTH_VERIFIER="$(cd -- "$(dirname -- "$0")" && pwd -P)/verify-performance-workload-auth.py"
readonly AUTH_VERIFIER
[[ -f "$AUTH_VERIFIER" ]] || die "workload authentication verifier is unavailable"
AUTH_SNAPSHOT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-rss-auth.XXXXXX")"
trap cleanup EXIT INT TERM
verified_metadata="$AUTH_SNAPSHOT_DIR/workload-metadata.tsv"
verified_events="$AUTH_SNAPSHOT_DIR/workload-events.tsv"
verified_subject="$AUTH_SNAPSHOT_DIR/subject-identity.tsv"
python3 "$(dirname -- "$AUTH_VERIFIER")/verify-performance-workload-ready.py" \
    --ready-receipt "$READY_RECEIPT" --events "$WORKLOAD_EVENTS" \
    --subject-identity "$SUBJECT_IDENTITY" \
    --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
    --campaign-id "$CAMPAIGN_ID" --session-id "$SESSION_ID" --nonce "$NONCE" \
    --plan-start-gate "$PLAN_START_GATE" \
    --expected-plan-start-continuous-ns \
        "$(awk -F '\t' '$1 == "plan_start_continuous_ns" { print $2 }' "$WORKLOAD_METADATA")" \
    || die "original workload readiness authentication is invalid"
python3 "$AUTH_VERIFIER" \
    --metadata "$WORKLOAD_METADATA" \
    --events "$WORKLOAD_EVENTS" \
    --subject-identity "$SUBJECT_IDENTITY" \
    --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
    --ready-receipt "$READY_RECEIPT" \
    --campaign-id "$CAMPAIGN_ID" \
    --session-id "$SESSION_ID" \
    --nonce "$NONCE" \
    --scenario "$SCENARIO" \
    --requested-warmup-ms "$REQUESTED_WARMUP_MS" \
    --requested-duration-ms "$REQUESTED_DURATION_MS" \
    --verified-metadata-output "$verified_metadata" \
    --verified-events-output "$verified_events" \
    --verified-subject-identity-output "$verified_subject" \
    || die "workload authentication is invalid"
python3 "$(dirname -- "$AUTH_VERIFIER")/verify-performance-workload-ready.py" \
    --ready-receipt "$READY_RECEIPT" \
    --events "$verified_events" \
    --subject-identity "$verified_subject" \
    --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
    --campaign-id "$CAMPAIGN_ID" --session-id "$SESSION_ID" --nonce "$NONCE" \
    --plan-start-gate "$PLAN_START_GATE" \
    --expected-plan-start-continuous-ns \
        "$(awk -F '\t' '$1 == "plan_start_continuous_ns" { print $2 }' "$verified_metadata")" \
    --ignore-events-file-identity \
    || die "workload readiness authentication is invalid"
WORKLOAD_METADATA="$verified_metadata"
WORKLOAD_EVENTS="$verified_events"
SUBJECT_IDENTITY="$verified_subject"
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

awk -F '\t' -v OFS='\t' \
    -v scenario="$SCENARIO" \
    -v duration="$REQUESTED_DURATION_MS" \
    -v warmup="$REQUESTED_WARMUP_MS" \
    -v identity_hash="$(sha256 "$SUBJECT_IDENTITY")" \
    -v workload_hash="$(sha256 "$WORKLOAD_EVENTS")" \
    -v workload_metadata_hash="$(sha256 "$WORKLOAD_METADATA")" \
    -v ready_receipt_hash="$(sha256 "$READY_RECEIPT")" \
    -v plan_start_gate_hash="$(sha256 "$PLAN_START_GATE")" \
    -v driver_hash="$(sha256 "$DRIVER_EVENTS")" \
    -v measured_start="$(awk -F '\t' '$1 == "started_continuous_ns" { print $2 }' "$WORKLOAD_METADATA")" \
    -v plan_start="$(awk -F '\t' '$1 == "plan_start_continuous_ns" { print $2 }' "$WORKLOAD_METADATA")" \
    -v measured_end="$(awk -F '\t' '$1 == "ended_continuous_ns" { print $2 }' "$WORKLOAD_METADATA")" \
    -v final_emitted="$(awk -F '\t' '$1 == "emitted_bytes" { print $2 }' "$WORKLOAD_METADATA")" \
    -v workload_file="$WORKLOAD_EVENTS" \
    -v driver_file="$DRIVER_EVENTS" \
    -v raw_file="$RAW_SAMPLES" '
    FILENAME == workload_file {
        if (FNR == 1) next
        workload_row_count = FNR - 1
        if (NF != 10 || $1 + 0 != FNR - 2 || $2 !~ /^[0-9]+$/ \
            || $5 !~ /^[0-9]+$/ || $6 !~ /^[0-9]+$/ || $7 !~ /^[0-9]+$/ \
            || $8 !~ /^[0-9]+$/ || $9 !~ /^[0-9]+$/) exit 10
        if (FNR > 2 && $2 + 0 <= workload_prior_time) exit 10
        workload_prior_time = $2 + 0
        if (!(($3 == "producer-end" && $10 == "success") \
            || ($3 != "producer-end" && $10 == "ok"))) exit 10
        if (!($3 == "started" || $3 == "geometry" \
            || $3 == "seed-complete" || $3 == "measurement-ready" \
            || $3 == "input-read" \
            || $3 == "input-ack-written" || $3 == "progress" \
            || $3 == "producer-end")) exit 10
        if ($3 == "started") {
            if (++producer_start_count != 1 || FNR != 2) exit 10
        }
        if ($3 == "seed-complete") {
            if (++seed_complete_count != 1) exit 10
            seed_complete_row = FNR
        }
        if ($3 == "measurement-ready") {
            if (++measurement_ready_count != 1 || $4 != "none") exit 10
            measurement_ready_row = FNR
        }
        if ($3 == "progress") {
            if (measurement_ready_count != 1) exit 10
            if (progress_count == 0) first_progress_row = FNR
            expected_progress_id = sprintf("progress-%06d", progress_count)
            if ($4 != expected_progress_id || $5 + 0 <= 0 \
                || $6 + 0 <= 0 || $7 + 0 <= 0 \
                || $8 + 0 <= 0 || $9 + 0 <= 0 \
                || (progress_count > 0 \
                    && ($2 + 0 - progress_time[progress_count] > 2000000000 \
                        || $5 + 0 <= progress_bytes[progress_count]))) exit 10
            progress_count += 1
            progress_time[progress_count] = $2 + 0
            progress_bytes[progress_count] = $5 + 0
        }
        if ($3 == "producer-end") {
            if (++producer_end_count != 1) exit 10
            producer_end_time = $2 + 0
            emitted_bytes = $5 + 0
            producer_end_row = FNR
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
                    || key == "plan_start_continuous_ns" \
                    || key == "measurement_start_continuous_ns" \
                    || key == "plan_start_gate_sha256" \
                    || key == "ready_receipt_sha256" \
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
        if (producer_start_count != 1 || seed_complete_count != 1 \
            || measurement_ready_count != 1 \
            || !(seed_complete_row < measurement_ready_row \
                && measurement_ready_row < first_progress_row) \
            || producer_end_count != 1 || producer_end_row != workload_row_count + 1 \
            || progress_count < 2 || progress_count > duration / 1000 + 3 \
            || progress_time[1] != measured_start \
            || progress_time[progress_count] >= producer_end_time \
            || progress_bytes[progress_count] != emitted_bytes \
            || emitted_bytes != final_emitted || producer_end_time != measured_end \
            || !sample_count \
            || raw_metadata["format_version"] != "1" \
            || raw_metadata["sample_interval_ms"] != "10000" \
            || raw_metadata["requested_warmup_ms"] != warmup \
            || raw_metadata["requested_duration_ms"] != duration \
            || raw_metadata["plan_start_continuous_ns"] != plan_start \
            || raw_metadata["measurement_start_continuous_ns"] \
                != plan_start + warmup * 1000000 \
            || raw_metadata["plan_start_gate_sha256"] \
                != plan_start_gate_hash \
            || raw_metadata["ready_receipt_sha256"] != ready_receipt_hash \
            || measured_start < raw_metadata["measurement_start_continuous_ns"] \
            || measured_start - raw_metadata["measurement_start_continuous_ns"] \
                > 100000000 \
            || raw_metadata["subject_identity_sha256"] != identity_hash \
            || raw_status != "complete" || raw_status_row != FNR \
            || sample_elapsed[1] > 1000 \
            || sample_elapsed[sample_count] < duration \
            || sample_elapsed[sample_count] > duration + 1000) exit 13
        for (key in distinct_geometry) distinct_geometry_count += 1
        print "elapsed_ms", "continuous_ns", "rss_kib", "workload_bytes", "resize_count"
        print "# format_version", "4"
        print "# scenario", scenario
        print "# sample_interval_ms", "10000"
        print "# requested_duration_ms", duration
        print "# plan_start_continuous_ns", plan_start
        print "# measurement_start_continuous_ns", \
            raw_metadata["measurement_start_continuous_ns"]
        print "# plan_start_gate_sha256", plan_start_gate_hash
        print "# subject_identity_sha256", identity_hash
        print "# workload_events_sha256", workload_hash
        print "# workload_metadata_sha256", workload_metadata_hash
        print "# ready_receipt_sha256", ready_receipt_hash
        print "# workload_authentication", "hmac-sha256"
        print "# progress_interval_ms", "1000"
        print "# maximum_progress_age_ms", "2000"
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
        progress_index = 1
        for (sample = 1; sample <= sample_count; sample += 1) {
            while (resize_index <= completed_resizes \
                && resize_time[resize_index] <= sample_time[sample]) resize_index += 1
            while (progress_index < progress_count \
                && progress_time[progress_index + 1] <= sample_time[sample]) {
                progress_index += 1
            }
            if (progress_time[progress_index] > sample_time[sample] \
                || sample_time[sample] - progress_time[progress_index] > 2000000000) exit 13
            observed_bytes = progress_bytes[progress_index]
            if (sample == 1) {
                sample_zero_skew = sample_time[sample] - measured_start
                if (sample_zero_skew < 0 || sample_zero_skew > 1000000000) exit 13
            } else if (observed_bytes < prior_observed_bytes) exit 13
            print sample_elapsed[sample], sample_time[sample], sample_rss[sample], \
                observed_bytes, resize_index - 1
            prior_observed_bytes = observed_bytes
        }
        if (prior_observed_bytes <= progress_bytes[1]) exit 13
        print "# status", "complete"
    }
' "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$RAW_SAMPLES" > "$TEMP" \
    || die "input event or raw-sample evidence is invalid"

chmod 0444 "$TEMP"
ln "$TEMP" "$OUTPUT" || die "output path was created concurrently"
rm -f -- "$TEMP"
TEMP=""
