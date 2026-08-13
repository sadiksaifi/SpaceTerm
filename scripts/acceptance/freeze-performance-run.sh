#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
umask 077

SUBJECT=""
PAIR_METADATA=""
SUBJECT_IDENTITY=""
PLAN=""
WORKLOAD=""
COMMAND_MANIFEST=""
ENVIRONMENT_MANIFEST=""
FONT_MANIFEST=""
INITIAL_GRID_MANIFEST=""
OUTPUT=""
TEMP=""

usage() {
    cat <<EOF
Usage: $(basename -- "$0") --subject spaceterm|ghostty --pair-metadata FILE \\
  --subject-identity FILE --plan FILE --workload-binary FILE \\
  --command-manifest FILE --environment-manifest FILE --font-manifest FILE \\
  --initial-grid-manifest FILE --output FILE

Bind one subject run to the already-frozen paired inputs and process identity.
Any subject-specific command, environment, font, grid, plan, workload, duration,
or process mismatch is rejected rather than labeled comparable.
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
        --pair-metadata) PAIR_METADATA="${2:-}"; shift ;;
        --subject-identity) SUBJECT_IDENTITY="${2:-}"; shift ;;
        --plan) PLAN="${2:-}"; shift ;;
        --workload-binary) WORKLOAD="${2:-}"; shift ;;
        --command-manifest) COMMAND_MANIFEST="${2:-}"; shift ;;
        --environment-manifest) ENVIRONMENT_MANIFEST="${2:-}"; shift ;;
        --font-manifest) FONT_MANIFEST="${2:-}"; shift ;;
        --initial-grid-manifest) INITIAL_GRID_MANIFEST="${2:-}"; shift ;;
        --output) OUTPUT="${2:-}"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument: $1" ;;
    esac
    shift
done

[[ "$SUBJECT" == spaceterm || "$SUBJECT" == ghostty ]] || die "invalid subject"
for command in awk chmod ln mkdir rm shasum; do
    command -v "$command" >/dev/null 2>&1 || die "required command not found: $command"
done
for input in "$PAIR_METADATA" "$SUBJECT_IDENTITY" "$PLAN" "$WORKLOAD" \
    "$COMMAND_MANIFEST" "$ENVIRONMENT_MANIFEST" "$FONT_MANIFEST" \
    "$INITIAL_GRID_MANIFEST"; do
    [[ -f "$input" && -r "$input" ]] || die "required input is unavailable"
done
[[ -n "$OUTPUT" && ! -e "$OUTPUT" ]] || die "output path is missing or exists"
[[ "$(kv "$PAIR_METADATA" format_version)" == 1 \
    && "$(kv "$SUBJECT_IDENTITY" format_version)" == 1 \
    && "$(kv "$SUBJECT_IDENTITY" subject)" == "$SUBJECT" \
    && "$(kv "$SUBJECT_IDENTITY" identity_status)" == frozen ]] \
    || die "pair or subject identity is invalid"

scenario="$(kv "$PAIR_METADATA" scenario)"
duration_ms="$(kv "$PAIR_METADATA" duration_ms)"
[[ -n "$scenario" && "$duration_ms" =~ ^[1-9][0-9]*$ ]] \
    || die "pair scenario or duration is invalid"
identity_hash="$(sha256 "$SUBJECT_IDENTITY")"
subject_key="${SUBJECT}_subject_identity_sha256"
[[ "$(kv "$PAIR_METADATA" "$subject_key")" == "$identity_hash" ]] \
    || die "subject identity is not part of the pair"

declare -a actual_hashes=(
    "$(sha256 "$PLAN")"
    "$(sha256 "$WORKLOAD")"
    "$(sha256 "$COMMAND_MANIFEST")"
    "$(sha256 "$ENVIRONMENT_MANIFEST")"
    "$(sha256 "$FONT_MANIFEST")"
    "$(sha256 "$INITIAL_GRID_MANIFEST")"
)
declare -a pair_keys=(
    plan_sha256 workload_sha256 command_sha256 environment_sha256
    font_sha256 initial_grid_sha256
)
for ((index = 0; index < ${#pair_keys[@]}; index += 1)); do
    [[ "$(kv "$PAIR_METADATA" "${pair_keys[index]}")" == "${actual_hashes[index]}" ]] \
        || die "run input does not match paired ${pair_keys[index]}"
done

process_pid="$(kv "$SUBJECT_IDENTITY" process_pid)"
process_start_identity="$(kv "$SUBJECT_IDENTITY" process_start_identity)"
[[ "$process_pid" =~ ^[1-9][0-9]*$ && -n "$process_start_identity" ]] \
    || die "subject process identity is invalid"

mkdir -p -- "$(dirname -- "$OUTPUT")"
TEMP="${OUTPUT}.tmp.$$"
trap cleanup EXIT INT TERM
{
    printf 'format_version\t1\n'
    printf 'subject\t%s\n' "$SUBJECT"
    printf 'subject_identity_sha256\t%s\n' "$identity_hash"
    printf 'scenario\t%s\n' "$scenario"
    printf 'scenario_plan_sha256\t%s\n' "${actual_hashes[0]}"
    printf 'workload_sha256\t%s\n' "${actual_hashes[1]}"
    printf 'command_sha256\t%s\n' "${actual_hashes[2]}"
    printf 'environment_sha256\t%s\n' "${actual_hashes[3]}"
    printf 'font_sha256\t%s\n' "${actual_hashes[4]}"
    printf 'initial_grid_sha256\t%s\n' "${actual_hashes[5]}"
    printf 'measured_duration_ms\t%s\n' "$duration_ms"
    printf 'process_pid\t%s\n' "$process_pid"
    printf 'process_start_identity\t%s\n' "$process_start_identity"
    printf 'status\tcomplete\n'
} > "$TEMP"
chmod 0444 "$TEMP"
ln "$TEMP" "$OUTPUT" || die "output path was created concurrently"
rm -f -- "$TEMP"
TEMP=""
trap - EXIT INT TERM
printf 'run_metadata_sha256\t%s\n' "$(sha256 "$OUTPUT")"
