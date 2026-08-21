#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
umask 077

PAIR_ID=""
SCENARIO=""
PLAN=""
PLAN_METADATA=""
WORKLOAD=""
COMMAND_MANIFEST=""
ENVIRONMENT_MANIFEST=""
FONT_MANIFEST=""
INITIAL_GRID_MANIFEST=""
SPACETERM_IDENTITY=""
GHOSTTY_IDENTITY=""
OUTPUT=""
TEMP=""

usage() {
    cat <<EOF
Usage: $(basename -- "$0") --pair-id ID --scenario NAME \\
  --plan FILE --plan-metadata FILE --workload-binary FILE \\
  --command-manifest FILE --environment-manifest FILE --font-manifest FILE \\
  --initial-grid-manifest FILE --spaceterm-identity FILE \\
  --ghostty-identity FILE --output FILE

Freeze the subject-independent inputs that must be byte-identical for a paired
SpaceTerm/Ghostty performance case. Only hashes and safe identifiers are
written; the caller retains the exact immutable input manifests as evidence.
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
        --pair-id) PAIR_ID="${2:-}"; shift ;;
        --scenario) SCENARIO="${2:-}"; shift ;;
        --plan) PLAN="${2:-}"; shift ;;
        --plan-metadata) PLAN_METADATA="${2:-}"; shift ;;
        --workload-binary) WORKLOAD="${2:-}"; shift ;;
        --command-manifest) COMMAND_MANIFEST="${2:-}"; shift ;;
        --environment-manifest) ENVIRONMENT_MANIFEST="${2:-}"; shift ;;
        --font-manifest) FONT_MANIFEST="${2:-}"; shift ;;
        --initial-grid-manifest) INITIAL_GRID_MANIFEST="${2:-}"; shift ;;
        --spaceterm-identity) SPACETERM_IDENTITY="${2:-}"; shift ;;
        --ghostty-identity) GHOSTTY_IDENTITY="${2:-}"; shift ;;
        --output) OUTPUT="${2:-}"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument: $1" ;;
    esac
    shift
done

[[ "$PAIR_ID" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] || die "invalid pair ID"
case "$SCENARIO" in
    ascii|unicode-styles|scrolled|hidden-occluded|resize \
        |perf-render-idle-cursor-blink|perf-render-text-blink \
        |perf-render-sustained-output|perf-render-selection \
        |perf-render-marked-text|perf-render-live-resize) ;;
    *) die "invalid scenario" ;;
esac
for command in awk chmod ln mkdir rm shasum; do
    command -v "$command" >/dev/null 2>&1 || die "required command not found: $command"
done
for input in "$PLAN" "$PLAN_METADATA" "$WORKLOAD" "$COMMAND_MANIFEST" \
    "$ENVIRONMENT_MANIFEST" "$FONT_MANIFEST" "$INITIAL_GRID_MANIFEST" \
    "$SPACETERM_IDENTITY" "$GHOSTTY_IDENTITY"; do
    [[ -f "$input" && -r "$input" ]] || die "required input is unavailable"
done
[[ -n "$OUTPUT" && ! -e "$OUTPUT" ]] || die "output path is missing or exists"

plan_hash="$(sha256 "$PLAN")"
plan_format="$(kv "$PLAN_METADATA" format_version)"
[[ ( "$plan_format" == 1 || "$plan_format" == 2 ) \
    && "$(kv "$PLAN_METADATA" scenario)" == "$SCENARIO" \
    && "$(kv "$PLAN_METADATA" plan_sha256)" == "$plan_hash" ]] \
    || die "plan metadata does not bind the supplied plan and scenario"
duration_ms="$(kv "$PLAN_METADATA" measured_duration_ms)"
[[ "$duration_ms" =~ ^[1-9][0-9]*$ ]] || die "plan duration is invalid"
[[ "$(kv "$SPACETERM_IDENTITY" subject)" == spaceterm \
    && "$(kv "$SPACETERM_IDENTITY" identity_status)" == frozen ]] \
    || die "SpaceTerm identity is not frozen"
[[ "$(kv "$GHOSTTY_IDENTITY" subject)" == ghostty \
    && "$(kv "$GHOSTTY_IDENTITY" identity_status)" == frozen ]] \
    || die "Ghostty identity is not frozen"

mkdir -p -- "$(dirname -- "$OUTPUT")"
TEMP="${OUTPUT}.tmp.$$"
trap cleanup EXIT INT TERM
{
    printf 'format_version\t1\n'
    printf 'pair_id\t%s\n' "$PAIR_ID"
    printf 'scenario\t%s\n' "$SCENARIO"
    printf 'plan_sha256\t%s\n' "$plan_hash"
    printf 'workload_sha256\t%s\n' "$(sha256 "$WORKLOAD")"
    printf 'command_sha256\t%s\n' "$(sha256 "$COMMAND_MANIFEST")"
    printf 'environment_sha256\t%s\n' "$(sha256 "$ENVIRONMENT_MANIFEST")"
    printf 'font_sha256\t%s\n' "$(sha256 "$FONT_MANIFEST")"
    printf 'initial_grid_sha256\t%s\n' "$(sha256 "$INITIAL_GRID_MANIFEST")"
    printf 'duration_ms\t%s\n' "$duration_ms"
    printf 'spaceterm_subject_identity_sha256\t%s\n' "$(sha256 "$SPACETERM_IDENTITY")"
    printf 'ghostty_subject_identity_sha256\t%s\n' "$(sha256 "$GHOSTTY_IDENTITY")"
} > "$TEMP"
chmod 0444 "$TEMP"
ln "$TEMP" "$OUTPUT" || die "output path was created concurrently"
rm -f -- "$TEMP"
TEMP=""
trap - EXIT INT TERM
printf 'pair_metadata_sha256\t%s\n' "$(sha256 "$OUTPUT")"
