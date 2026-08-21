#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
umask 077

readonly PS_COMMAND="${SPACETERM_PS:-ps}"
PROCESS_INSPECTOR="${SPACETERM_PROCESS_INSPECTOR:-}"
TEST_OVERRIDES_ACTIVE=false
[[ -z "${SPACETERM_PS:-}${SPACETERM_PROCESS_INSPECTOR:-}" ]] \
    || TEST_OVERRIDES_ACTIVE=true
readonly TEST_OVERRIDES_ACTIVE

PID=""
DURATION_SECONDS=""
INTERVAL_SECONDS=10
OUTPUT_PATH=""
APP_BUNDLE=""
EXPECTED_BUNDLE_IDENTIFIER=""
EXPECTED_EXECUTABLE_SHA256=""
EXPECTED_COMMIT=""
EXPECTED_MARKETING_VERSION=""
EXPECTED_BUILD_VERSION=""
EXPECTED_CARGO_LOCK_SHA256=""
WORKLOAD_METRICS_PATH=""
EXPECTED_CAMPAIGN_ID=""
EXPECTED_SCENARIO=""
EXPECTED_SESSION_ID=""
CAMPAIGN_SECRET_FILE=""
OUTPUT_RECEIPT_PATH=""

usage() {
    cat <<EOF
Usage: $(basename -- "$0") --pid PID --duration-seconds N --output PATH \\
  --app-bundle PATH --bundle-identifier ID \\
  --expected-executable-sha256 SHA256 --expected-commit COMMIT \\
  --expected-marketing-version VERSION --expected-build-version VERSION \\
  --expected-cargo-lock-sha256 SHA256 --workload-metrics PATH \\
  --campaign-id ID --scenario NAME --session-id ID \\
  --campaign-secret-file PATH --output-receipt PATH [OPTIONS]

Sample one packaged process's resident set size into a privacy-safe TSV
artifact. The target process and frozen package identity are reverified before
every sample. Duration must be an exact multiple of the sampling interval.

Options:
  --interval-seconds N  Sampling interval (default: 10; issue #43 requires 10).
  --doctor              Verify local sampling prerequisites.
  -h, --help            Show this help.
EOF
}

die() {
    echo "error: $*" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

is_positive_integer() {
    [[ "$1" =~ ^[1-9][0-9]*$ ]]
}

is_safe_label() {
    [[ "$1" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]]
}

is_sha256() {
    [[ "$1" =~ ^[0-9a-f]{64}$ ]]
}

is_commit() {
    [[ "$1" =~ ^[0-9a-f]{40,64}$ ]]
}

one_line() {
    tr '\t\r\n' '   ' | awk '{$1=$1; print}'
}

plist_value() {
    local plist="$1"
    local key="$2"
    plutil -extract "$key" raw -o - "$plist"
}

process_value() {
    local field="$1"
    "$PS_COMMAND" -p "$PID" -o "${field}=" 2>/dev/null \
        | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//'
}

process_identity() {
    "$PROCESS_INSPECTOR" \
        --pid "$PID" \
        --expected-executable "$PACKAGE_EXECUTABLE" \
        --expected-sha256 "$EXPECTED_EXECUTABLE_SHA256" \
        | awk -F '\t' '$1 == "identity_token" {print $2}'
}

target_identity_matches() {
    [[ "$(process_identity 2>/dev/null || true)" == "$TARGET_IDENTITY_TOKEN" ]]
}

package_is_frozen() {
    [[ "$(plist_value "$APP_INFO_PLIST" CFBundleIdentifier)" \
            == "$EXPECTED_BUNDLE_IDENTIFIER" \
        && "$(plist_value "$APP_INFO_PLIST" CFBundleExecutable)" \
            == "$BUNDLE_EXECUTABLE_NAME" \
        && "$(plist_value "$APP_INFO_PLIST" CFBundleShortVersionString)" \
            == "$EXPECTED_MARKETING_VERSION" \
        && "$(plist_value "$APP_INFO_PLIST" CFBundleVersion)" \
            == "$EXPECTED_BUILD_VERSION" \
        && "$(shasum -a 256 "$PACKAGE_EXECUTABLE" | awk '{print $1}')" \
            == "$EXPECTED_EXECUTABLE_SHA256" \
        && "$(git -C "$REPOSITORY_ROOT" rev-parse HEAD)" == "$EXPECTED_COMMIT" \
        && "$(shasum -a 256 "$REPOSITORY_ROOT/Cargo.lock" | awk '{print $1}')" \
            == "$EXPECTED_CARGO_LOCK_SHA256" \
        && "$(shasum -a 256 "$SCRIPT_DIRECTORY/release-performance-workload.sh" \
            | awk '{print $1}')" == "$WORKLOAD_TOOL_SHA256" \
        && "$(shasum -a 256 "$SCRIPT_DIRECTORY/analyze-release-performance-rss.awk" \
            | awk '{print $1}')" == "$ANALYZER_TOOL_SHA256" \
        && "$(shasum -a 256 "$SCRIPT_DIRECTORY/sample-release-performance-rss.sh" \
            | awk '{print $1}')" == "$SAMPLER_TOOL_SHA256" \
        && "$(shasum -a 256 "$PROCESS_INSPECTOR" | awk '{print $1}')" \
            == "$PROCESS_INSPECTOR_SHA256" ]]
}

record_incomplete() {
    local reason="$1"
    printf '# status\t%s\n' "$reason" >> "$OUTPUT_PATH"
    trap - INT TERM
    die "$reason"
}

workload_metric() {
    local key="$1"
    awk -F '\t' -v key="$key" '$1 == key {count += 1; value = $2}
        END {if (count == 1) print value}' "$WORKLOAD_METRICS_PATH" 2>/dev/null
}

evidence_hmac() {
    local path="$1"
    local hmac_key="$2"
    python3 - "$path" "$CAMPAIGN_SECRET_FILE" "$hmac_key" <<'PY'
import hashlib
import hmac
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
lines = path.read_bytes().splitlines(keepends=True)
key = sys.argv[3].encode() + b"\t"
payload = b"".join(line for line in lines if not line.startswith(key))
secret = pathlib.Path(sys.argv[2]).read_bytes()
print(hmac.new(secret, payload, hashlib.sha256).hexdigest())
PY
}

receipt_metric() {
    local key="$1"
    awk -F '\t' -v key="$key" '$1 == key {count += 1; value = $2}
        END {if (count == 1) print value}' "$OUTPUT_RECEIPT_PATH" 2>/dev/null
}

workload_evidence_is_bound() {
    local capture_finished="$1"
    local metrics_hmac receipt_hmac
    [[ -f "$WORKLOAD_METRICS_PATH" && -f "$OUTPUT_RECEIPT_PATH" ]] || return 1
    metrics_hmac="$(evidence_hmac "$WORKLOAD_METRICS_PATH" metrics_hmac_sha256 \
        2>/dev/null)" || return 1
    receipt_hmac="$(evidence_hmac "$OUTPUT_RECEIPT_PATH" receipt_hmac_sha256 \
        2>/dev/null)" || return 1
    [[ "$(workload_metric format_version)" == "2" \
        && "$(workload_metric status)" == "complete" \
        && "$(workload_metric campaign_id)" == "$EXPECTED_CAMPAIGN_ID" \
        && "$(workload_metric scenario)" == "$EXPECTED_SCENARIO" \
        && "$(workload_metric session_id)" == "$EXPECTED_SESSION_ID" \
        && "$(workload_metric output_mode)" == "pty-no-opost" \
        && "$(workload_metric opost_disabled)" == "true" \
        && "$(workload_metric requested_duration_seconds)" \
            =~ ^[1-9][0-9]*$ \
        && "$(workload_metric emitted_bytes)" =~ ^[1-9][0-9]*$ \
        && "$(workload_metric started_epoch_seconds)" =~ ^[0-9]+$ \
        && "$(workload_metric finished_epoch_seconds)" =~ ^[0-9]+$ ]] \
        || return 1
    [[ "$(workload_metric metrics_hmac_sha256)" == "$metrics_hmac" \
        && "$(receipt_metric format_version)" == "1" \
        && "$(receipt_metric source)" == "spaceterm-terminal-ingestion" \
        && "$(receipt_metric campaign_id)" == "$EXPECTED_CAMPAIGN_ID" \
        && "$(receipt_metric scenario)" == "$EXPECTED_SCENARIO" \
        && "$(receipt_metric session_id)" == "$EXPECTED_SESSION_ID" \
        && "$(receipt_metric subject_identity_sha256)" \
            == "$TARGET_IDENTITY_SHA256" \
        && "$(receipt_metric emitted_bytes)" == "$(workload_metric emitted_bytes)" \
        && "$(receipt_metric seed_sha256)" == "$(workload_metric seed_sha256)" \
        && "$(receipt_metric started_epoch_seconds)" \
            == "$(workload_metric started_epoch_seconds)" \
        && "$(receipt_metric finished_epoch_seconds)" \
            == "$(workload_metric finished_epoch_seconds)" \
        && "$(receipt_metric status)" == "complete" \
        && "$(receipt_metric receipt_hmac_sha256)" == "$receipt_hmac" ]] \
        || return 1
    (( $(workload_metric started_epoch_seconds) <= started_epoch_seconds \
        && $(workload_metric finished_epoch_seconds) >= capture_finished \
        && $(workload_metric requested_duration_seconds) >= DURATION_SECONDS ))
}

doctor() {
    require_command "$PS_COMMAND"
    require_command awk
    require_command date
    require_command git
    require_command plutil
    require_command realpath
    require_command sed
    require_command shasum
    require_command sleep
    require_command stat
    require_command tr
    if [[ -n "$PROCESS_INSPECTOR" ]]; then
        require_command "$PROCESS_INSPECTOR"
    else
        require_command python3
    fi
    echo "RSS sampling prerequisites are available"
}

if [[ "${1:-}" == "--doctor" ]]; then
    doctor
    exit 0
fi

while (( $# > 0 )); do
    case "$1" in
        --pid) PID="${2:-}"; shift ;;
        --duration-seconds) DURATION_SECONDS="${2:-}"; shift ;;
        --interval-seconds) INTERVAL_SECONDS="${2:-}"; shift ;;
        --output) OUTPUT_PATH="${2:-}"; shift ;;
        --app-bundle) APP_BUNDLE="${2:-}"; shift ;;
        --bundle-identifier) EXPECTED_BUNDLE_IDENTIFIER="${2:-}"; shift ;;
        --expected-executable-sha256) EXPECTED_EXECUTABLE_SHA256="${2:-}"; shift ;;
        --expected-commit) EXPECTED_COMMIT="${2:-}"; shift ;;
        --expected-marketing-version) EXPECTED_MARKETING_VERSION="${2:-}"; shift ;;
        --expected-build-version) EXPECTED_BUILD_VERSION="${2:-}"; shift ;;
        --expected-cargo-lock-sha256) EXPECTED_CARGO_LOCK_SHA256="${2:-}"; shift ;;
        --workload-metrics) WORKLOAD_METRICS_PATH="${2:-}"; shift ;;
        --campaign-id) EXPECTED_CAMPAIGN_ID="${2:-}"; shift ;;
        --scenario) EXPECTED_SCENARIO="${2:-}"; shift ;;
        --session-id) EXPECTED_SESSION_ID="${2:-}"; shift ;;
        --campaign-secret-file) CAMPAIGN_SECRET_FILE="${2:-}"; shift ;;
        --output-receipt) OUTPUT_RECEIPT_PATH="${2:-}"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument: $1" ;;
    esac
    shift
done

doctor >/dev/null
is_positive_integer "$PID" || die "PID must be a positive integer"
is_positive_integer "$DURATION_SECONDS" || die "duration must be a positive integer"
is_positive_integer "$INTERVAL_SECONDS" || die "interval must be a positive integer"
(( DURATION_SECONDS % INTERVAL_SECONDS == 0 )) \
    || die "duration must be an exact multiple of the sampling interval"
[[ -n "$OUTPUT_PATH" ]] || die "--output is required"
[[ ! -e "$OUTPUT_PATH" ]] || die "output path already exists: $OUTPUT_PATH"
[[ -d "$APP_BUNDLE" && "$APP_BUNDLE" == *.app ]] \
    || die "app bundle is not a .app directory"
is_safe_label "$EXPECTED_BUNDLE_IDENTIFIER" || die "bundle identifier is invalid"
is_safe_label "$EXPECTED_MARKETING_VERSION" || die "marketing version is invalid"
is_safe_label "$EXPECTED_BUILD_VERSION" || die "build version is invalid"
is_sha256 "$EXPECTED_EXECUTABLE_SHA256" || die "executable SHA-256 is invalid"
is_sha256 "$EXPECTED_CARGO_LOCK_SHA256" || die "Cargo.lock SHA-256 is invalid"
is_commit "$EXPECTED_COMMIT" || die "commit is invalid"
[[ -n "$WORKLOAD_METRICS_PATH" ]] || die "--workload-metrics is required"
is_safe_label "$EXPECTED_CAMPAIGN_ID" || die "campaign ID is invalid"
is_safe_label "$EXPECTED_SCENARIO" || die "scenario is invalid"
is_safe_label "$EXPECTED_SESSION_ID" || die "session ID is invalid"
[[ -f "$CAMPAIGN_SECRET_FILE" ]] || die "campaign secret file is required"
[[ -n "$OUTPUT_RECEIPT_PATH" ]] || die "terminal-ingestion output receipt path is required"
campaign_secret_mode="$(stat -f '%Lp' "$CAMPAIGN_SECRET_FILE")"
(( (8#$campaign_secret_mode & 8#077) == 0 )) \
    || die "campaign secret file must not be group/world accessible"

SCRIPT_DIRECTORY="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIRECTORY
REPOSITORY_ROOT="$(cd -- "$SCRIPT_DIRECTORY/.." && pwd -P)"
readonly REPOSITORY_ROOT
[[ -z "$(git -C "$REPOSITORY_ROOT" status --porcelain --untracked-files=no)" ]] \
    || die "repository has tracked changes; freeze and commit tooling first"
if [[ -z "$PROCESS_INSPECTOR" ]]; then
    PROCESS_INSPECTOR="$SCRIPT_DIRECTORY/inspect-release-performance-process.py"
fi
readonly PROCESS_INSPECTOR
APP_BUNDLE="$(realpath "$APP_BUNDLE")"
readonly APP_BUNDLE
APP_INFO_PLIST="$APP_BUNDLE/Contents/Info.plist"
readonly APP_INFO_PLIST
[[ -f "$APP_INFO_PLIST" ]] || die "app bundle is missing Contents/Info.plist"
BUNDLE_EXECUTABLE_NAME="$(plist_value "$APP_INFO_PLIST" CFBundleExecutable)"
readonly BUNDLE_EXECUTABLE_NAME
is_safe_label "$BUNDLE_EXECUTABLE_NAME" || die "bundle executable name is invalid"
PACKAGE_EXECUTABLE="$(realpath "$APP_BUNDLE/Contents/MacOS/$BUNDLE_EXECUTABLE_NAME")"
readonly PACKAGE_EXECUTABLE
[[ -f "$PACKAGE_EXECUTABLE" && -x "$PACKAGE_EXECUTABLE" ]] \
    || die "packaged executable is missing or not executable"

SAMPLER_TOOL_SHA256="$(shasum -a 256 "$SCRIPT_DIRECTORY/sample-release-performance-rss.sh" | awk '{print $1}')"
WORKLOAD_TOOL_SHA256="$(shasum -a 256 "$SCRIPT_DIRECTORY/release-performance-workload.sh" | awk '{print $1}')"
ANALYZER_TOOL_SHA256="$(shasum -a 256 "$SCRIPT_DIRECTORY/analyze-release-performance-rss.awk" | awk '{print $1}')"
PROCESS_INSPECTOR_SHA256="$(shasum -a 256 "$PROCESS_INSPECTOR" | awk '{print $1}')"
readonly SAMPLER_TOOL_SHA256 WORKLOAD_TOOL_SHA256 ANALYZER_TOOL_SHA256
readonly PROCESS_INSPECTOR_SHA256
TARGET_IDENTITY_TOKEN="$(process_identity)"
readonly TARGET_IDENTITY_TOKEN
[[ -n "$TARGET_IDENTITY_TOKEN" ]] || die "could not capture kernel process identity"
TARGET_IDENTITY_SHA256="$(printf '%s' "$TARGET_IDENTITY_TOKEN" | shasum -a 256 | awk '{print $1}')"
readonly TARGET_IDENTITY_SHA256
package_is_frozen || die "package identity does not match the supplied frozen identity"

mkdir -p -- "$(dirname -- "$OUTPUT_PATH")"
started_epoch_seconds="$(date +%s)"
next_sample_seconds=0

{
    printf 'elapsed_seconds\tepoch_seconds\trss_kib\n'
    printf '# format_version\t2\n'
    printf '# sample_interval_seconds\t%d\n' "$INTERVAL_SECONDS"
    printf '# requested_duration_seconds\t%d\n' "$DURATION_SECONDS"
    printf '# started_epoch_seconds\t%s\n' "$started_epoch_seconds"
    printf '# bundle_identifier\t%s\n' "$EXPECTED_BUNDLE_IDENTIFIER"
    printf '# bundle_marketing_version\t%s\n' "$EXPECTED_MARKETING_VERSION"
    printf '# bundle_build_version\t%s\n' "$EXPECTED_BUILD_VERSION"
    printf '# executable_sha256\t%s\n' "$EXPECTED_EXECUTABLE_SHA256"
    printf '# commit\t%s\n' "$EXPECTED_COMMIT"
    printf '# cargo_lock_sha256\t%s\n' "$EXPECTED_CARGO_LOCK_SHA256"
    printf '# campaign_id\t%s\n' "$EXPECTED_CAMPAIGN_ID"
    printf '# scenario\t%s\n' "$EXPECTED_SCENARIO"
    printf '# session_id\t%s\n' "$EXPECTED_SESSION_ID"
    printf '# process_identity_sha256\t%s\n' \
        "$TARGET_IDENTITY_SHA256"
    printf '# sampler_tool_sha256\t%s\n' "$SAMPLER_TOOL_SHA256"
    printf '# workload_tool_sha256\t%s\n' "$WORKLOAD_TOOL_SHA256"
    printf '# analyzer_tool_sha256\t%s\n' "$ANALYZER_TOOL_SHA256"
    printf '# process_inspector_tool_sha256\t%s\n' "$PROCESS_INSPECTOR_SHA256"
} > "$OUTPUT_PATH"

trap 'record_incomplete interrupted' INT TERM

SECONDS=0
while :; do
    elapsed_seconds=$SECONDS
    if (( elapsed_seconds < next_sample_seconds )); then
        sleep "$((next_sample_seconds - elapsed_seconds))"
        continue
    fi

    target_identity_matches || record_incomplete "target-identity-changed"
    package_is_frozen || record_incomplete "package-identity-changed"
    now_epoch_seconds="$(date +%s)"
    rss_kib="$(process_value rss)"
    rss_kib="${rss_kib//[[:space:]]/}"
    [[ "$rss_kib" =~ ^[0-9]+$ ]] || record_incomplete "process-unavailable"
    target_identity_matches || record_incomplete "target-identity-changed-after-rss-read"
    printf '%d\t%s\t%s\n' "$elapsed_seconds" "$now_epoch_seconds" "$rss_kib" \
        >> "$OUTPUT_PATH"

    (( elapsed_seconds >= DURATION_SECONDS )) && break
    ((next_sample_seconds += INTERVAL_SECONDS))
done

capture_finished_epoch_seconds="$(date +%s)"
evidence_bound=false
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 \
    21 22 23 24 25 26 27 28 29 30 31 32 33 34 35 36 37 38 39 40 \
    41 42 43 44 45 46 47 48 49 50; do
    if workload_evidence_is_bound "$capture_finished_epoch_seconds"; then
        evidence_bound=true
        break
    fi
    target_identity_matches || record_incomplete "target-identity-changed"
    package_is_frozen || record_incomplete "package-identity-changed"
    sleep 0.1
done
[[ "$evidence_bound" == true ]] \
    || record_incomplete "workload-evidence-is-not-bound"
emitted_bytes="$(workload_metric emitted_bytes)"
workload_metrics_sha256="$(shasum -a 256 "$WORKLOAD_METRICS_PATH" | awk '{print $1}')"
output_receipt_sha256="$(shasum -a 256 "$OUTPUT_RECEIPT_PATH" | awk '{print $1}')"
{
    printf '# workload_emitted_bytes\t%s\n' "$emitted_bytes"
    printf '# workload_metrics_sha256\t%s\n' "$workload_metrics_sha256"
    printf '# output_receipt_sha256\t%s\n' "$output_receipt_sha256"
printf '# status\tcomplete\n'
} >> "$OUTPUT_PATH"
if [[ "$TEST_OVERRIDES_ACTIVE" == true ]]; then
    sed -i '' -e '$d' "$OUTPUT_PATH"
    printf '# status\ttest-overrides-active\n' >> "$OUTPUT_PATH"
    die "test overrides cannot produce acceptance evidence"
fi
trap - INT TERM
