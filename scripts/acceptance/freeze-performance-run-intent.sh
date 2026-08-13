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
CAMPAIGN_ID=""
SESSION_ID=""
NONCE=""
NATIVE_PROVISIONAL_OBSERVATION=""
OUTPUT=""
TEMP=""
EVIDENCE_MODE=production
[[ "${SPACETERM_PERFORMANCE_TEST_MODE:-0}" != 1 ]] || EVIDENCE_MODE=test-only
readonly EVIDENCE_MODE

SCRIPT_DIRECTORY="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIRECTORY

usage() {
    cat <<EOF
Usage: $(basename -- "$0") --subject spaceterm|ghostty --pair-metadata FILE \\
  --subject-identity FILE --plan FILE --workload-binary FILE \\
  --command-manifest FILE --environment-manifest FILE --font-manifest FILE \\
  --initial-grid-manifest FILE --campaign-id LABEL --session-id LABEL \\
  --nonce 64_LOWER_HEX --output ABSENT_FILE \\
  [--native-provisional-observation FILE]

Freeze the exact pre-action run intent. SpaceTerm requires one authenticated
27-record provisional native v5 observation; Ghostty rejects that artifact.
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
        --subject) SUBJECT="${2:-}"; shift ;;
        --pair-metadata) PAIR_METADATA="${2:-}"; shift ;;
        --subject-identity) SUBJECT_IDENTITY="${2:-}"; shift ;;
        --plan) PLAN="${2:-}"; shift ;;
        --workload-binary) WORKLOAD="${2:-}"; shift ;;
        --command-manifest) COMMAND_MANIFEST="${2:-}"; shift ;;
        --environment-manifest) ENVIRONMENT_MANIFEST="${2:-}"; shift ;;
        --font-manifest) FONT_MANIFEST="${2:-}"; shift ;;
        --initial-grid-manifest) INITIAL_GRID_MANIFEST="${2:-}"; shift ;;
        --campaign-id) CAMPAIGN_ID="${2:-}"; shift ;;
        --session-id) SESSION_ID="${2:-}"; shift ;;
        --nonce) NONCE="${2:-}"; shift ;;
        --native-provisional-observation) NATIVE_PROVISIONAL_OBSERVATION="${2:-}"; shift ;;
        --output) OUTPUT="${2:-}"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument: $1" ;;
    esac
    shift
done

[[ "$SUBJECT" == spaceterm || "$SUBJECT" == ghostty ]] || die "invalid subject"
[[ "$CAMPAIGN_ID" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,79}$ \
    && "$SESSION_ID" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,79}$ \
    && "$NONCE" =~ ^[0-9a-f]{64}$ ]] || die "invalid campaign binding"
for command in awk chmod ln mkdir rm shasum; do
    command -v "$command" >/dev/null 2>&1 || die "required command not found: $command"
done
for input in "$PAIR_METADATA" "$SUBJECT_IDENTITY" "$PLAN" "$WORKLOAD" \
    "$COMMAND_MANIFEST" "$ENVIRONMENT_MANIFEST" "$FONT_MANIFEST" \
    "$INITIAL_GRID_MANIFEST"; do
    [[ -f "$input" && ! -L "$input" && -r "$input" ]] \
        || die "required immutable input is unavailable"
done
[[ -n "$OUTPUT" && ! -e "$OUTPUT" && ! -L "$OUTPUT" ]] \
    || die "output path is missing or exists"
readonly PAIR_KEYS="format_version pair_id scenario plan_sha256 workload_sha256 command_sha256 environment_sha256 font_sha256 initial_grid_sha256 duration_ms spaceterm_subject_identity_sha256 ghostty_subject_identity_sha256"
exact_schema "$PAIR_METADATA" "$PAIR_KEYS" 12 || die "pair metadata schema is invalid"
[[ "$(kv "$PAIR_METADATA" format_version)" == 1 \
    && "$(kv "$SUBJECT_IDENTITY" format_version)" == 1 \
    && "$(kv "$SUBJECT_IDENTITY" subject)" == "$SUBJECT" \
    && "$(kv "$SUBJECT_IDENTITY" identity_status)" == frozen ]] \
    || die "pair or subject identity is invalid"

scenario="$(kv "$PAIR_METADATA" scenario)"
duration_ms="$(kv "$PAIR_METADATA" duration_ms)"
[[ "$scenario" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ \
    && "$duration_ms" =~ ^[1-9][0-9]*$ ]] || die "pair timing is invalid"
identity_hash="$(sha256 "$SUBJECT_IDENTITY")"
[[ "$(kv "$PAIR_METADATA" "${SUBJECT}_subject_identity_sha256")" == "$identity_hash" ]] \
    || die "subject identity is not part of the pair"
declare -a actual_hashes=(
    "$(sha256 "$PLAN")" "$(sha256 "$WORKLOAD")" "$(sha256 "$COMMAND_MANIFEST")"
    "$(sha256 "$ENVIRONMENT_MANIFEST")" "$(sha256 "$FONT_MANIFEST")"
    "$(sha256 "$INITIAL_GRID_MANIFEST")"
)
declare -a pair_keys=(
    plan_sha256 workload_sha256 command_sha256 environment_sha256 font_sha256 initial_grid_sha256
)
for ((index = 0; index < ${#pair_keys[@]}; index += 1)); do
    [[ "$(kv "$PAIR_METADATA" "${pair_keys[index]}")" == "${actual_hashes[index]}" ]] \
        || die "run input does not match paired ${pair_keys[index]}"
done
process_pid="$(kv "$SUBJECT_IDENTITY" process_pid)"
process_start_identity="$(kv "$SUBJECT_IDENTITY" process_start_identity)"
[[ "$process_pid" =~ ^[1-9][0-9]*$ \
    && "$process_start_identity" =~ ^[1-9][0-9]*:[0-9]+$ ]] \
    || die "subject process identity is invalid"
if [[ "$SUBJECT" == spaceterm ]]; then
    [[ -f "$NATIVE_PROVISIONAL_OBSERVATION" && ! -L "$NATIVE_PROVISIONAL_OBSERVATION" ]] \
        || die "SpaceTerm provisional native observation is missing"
    "$SCRIPT_DIRECTORY/verify-performance-native-closure.py" \
        --subject-identity "$SUBJECT_IDENTITY" \
        --provisional-observation "$NATIVE_PROVISIONAL_OBSERVATION" >/dev/null \
        || die "SpaceTerm provisional native observation is invalid"
    provisional_hash="$(sha256 "$NATIVE_PROVISIONAL_OBSERVATION")"
else
    [[ -z "$NATIVE_PROVISIONAL_OBSERVATION" ]] \
        || die "Ghostty must not receive a SpaceTerm provisional observation"
    provisional_hash=not-applicable
fi

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
    printf 'campaign_id\t%s\n' "$CAMPAIGN_ID"
    printf 'session_id\t%s\n' "$SESSION_ID"
    printf 'nonce\t%s\n' "$NONCE"
    printf 'native_provisional_observation_sha256\t%s\n' "$provisional_hash"
    printf 'evidence_mode\t%s\n' "$EVIDENCE_MODE"
    printf 'status\tprepared\n'
} > "$TEMP"
chmod 0444 "$TEMP"
ln "$TEMP" "$OUTPUT" || die "output path was created concurrently"
rm -f -- "$TEMP"
TEMP=""
trap - EXIT INT TERM
printf 'run_intent_sha256\t%s\n' "$(sha256 "$OUTPUT")"
