#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
umask 077

readonly XCRUN_COMMAND="${SPACETERM_XCRUN:-xcrun}"
readonly PS_COMMAND="${SPACETERM_PS:-ps}"
readonly SHASUM_COMMAND="${SPACETERM_SHASUM:-shasum}"
PROCESS_INSPECTOR="${SPACETERM_PROCESS_INSPECTOR:-}"
TRACE_VERIFIER="${SPACETERM_TRACE_VERIFIER:-}"
TEST_OVERRIDES_ACTIVE=false
[[ -z "${SPACETERM_XCRUN:-}${SPACETERM_PS:-}${SPACETERM_SHASUM:-}${SPACETERM_PROCESS_INSPECTOR:-}${SPACETERM_TRACE_VERIFIER:-}" ]] \
    || TEST_OVERRIDES_ACTIVE=true
readonly TEST_OVERRIDES_ACTIVE

PID=""
APPLICATION_LABEL=""
SCENARIO=""
DURATION_SECONDS=""
OUTPUT_DIRECTORY=""
APP_BUNDLE=""
EXPECTED_BUNDLE_IDENTIFIER=""
EXPECTED_EXECUTABLE_SHA256=""
EXPECTED_COMMIT=""
EXPECTED_MARKETING_VERSION=""
EXPECTED_BUILD_VERSION=""
EXPECTED_CARGO_LOCK_SHA256=""

usage() {
    cat <<EOF
Usage: $(basename -- "$0") --pid PID --application LABEL --scenario LABEL \\
  --duration-seconds N --output-directory PATH --app-bundle PATH \\
  --bundle-identifier ID --expected-executable-sha256 SHA256 \\
  --expected-commit COMMIT --expected-marketing-version VERSION \\
  --expected-build-version VERSION --expected-cargo-lock-sha256 SHA256

Attach a combined Time Profiler, Allocations, and Hangs recording to one
already-running packaged application. The bundle identity, executable hash,
repository commit, target process, elapsed coverage, and exported trace tables
are verified before metadata is labeled CAPTURED. CAPTURED is evidence state,
not a performance pass verdict.

Metadata intentionally excludes command arguments, environment values,
executable paths, and terminal contents.

Options:
  --doctor  Verify Xcode Instruments and metadata prerequisites.
  -h, --help
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

doctor() {
    local instruments
    require_command "$XCRUN_COMMAND"
    require_command awk
    require_command basename
    require_command date
    require_command find
    require_command git
    require_command grep
    require_command plutil
    require_command "$PS_COMMAND"
    require_command realpath
    require_command sed
    require_command "$SHASUM_COMMAND"
    require_command sw_vers
    require_command sysctl
    require_command tr
    require_command uname
    require_command xmllint
    require_command python3
    instruments="$("$XCRUN_COMMAND" xctrace list instruments)"
    for instrument in "Time Profiler" "Allocations" "Hangs"; do
        grep -Fxq "$instrument" <<<"$instruments" \
            || die "required xctrace instrument is unavailable: $instrument"
    done
    "$XCRUN_COMMAND" xcodebuild -version >/dev/null
    echo "release performance trace prerequisites are available"
}

one_line() {
    tr '\t\r\n' '   ' | awk '{$1=$1; print}'
}

sysctl_value() {
    local key="$1"
    sysctl -n "$key" 2>/dev/null | one_line || printf 'unavailable\n'
}

plist_value() {
    local plist="$1"
    local key="$2"
    plutil -extract "$key" raw -o - "$plist"
}

canonical_path() {
    realpath "$1"
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

live_target_code_identity_verified() {
    # The current inspector binds a kernel process generation to the mapped
    # executable vnode, but it cannot attest the PID's live kernel CDHash.
    # Fail closed until that app-owned/native identity proof is available.
    return 1
}

package_is_frozen() {
    local current_build current_hash current_commit current_identifier
    local current_executable_name current_lock_hash current_marketing
    current_hash="$("$SHASUM_COMMAND" -a 256 "$PACKAGE_EXECUTABLE" | awk '{ print $1 }')"
    current_commit="$(git -C "$REPOSITORY_ROOT" rev-parse HEAD)"
    current_identifier="$(plist_value "$APP_INFO_PLIST" CFBundleIdentifier)"
    current_executable_name="$(plist_value "$APP_INFO_PLIST" CFBundleExecutable)"
    current_marketing="$(plist_value "$APP_INFO_PLIST" CFBundleShortVersionString)"
    current_build="$(plist_value "$APP_INFO_PLIST" CFBundleVersion)"
    current_lock_hash="$("$SHASUM_COMMAND" -a 256 "$REPOSITORY_ROOT/Cargo.lock" | awk '{print $1}')"
    [[ "$current_hash" == "$EXPECTED_EXECUTABLE_SHA256" \
        && "$current_commit" == "$EXPECTED_COMMIT" \
        && "$current_identifier" == "$EXPECTED_BUNDLE_IDENTIFIER" \
        && "$current_executable_name" == "$BUNDLE_EXECUTABLE_NAME" \
        && "$current_marketing" == "$EXPECTED_MARKETING_VERSION" \
        && "$current_build" == "$EXPECTED_BUILD_VERSION" \
        && "$current_lock_hash" == "$EXPECTED_CARGO_LOCK_SHA256" \
        && "$("$SHASUM_COMMAND" -a 256 "$PROCESS_INSPECTOR" | awk '{print $1}')" \
            == "$PROCESS_INSPECTOR_SHA256" \
        && "$("$SHASUM_COMMAND" -a 256 "$TRACE_VERIFIER" | awk '{print $1}')" \
            == "$TRACE_VERIFIER_SHA256" \
        && "$("$SHASUM_COMMAND" -a 256 "$RECORDER_PATH" | awk '{print $1}')" \
            == "$TRACE_RECORDER_SHA256" \
        && "$("$SHASUM_COMMAND" -a 256 "$COMMAND_RUNNER" | awk '{print $1}')" \
            == "$COMMAND_RUNNER_SHA256" ]]
}

trace_bundle_has_data() {
    [[ -d "$TRACE_PATH" ]] \
        && [[ -n "$(find "$TRACE_PATH" -type f -size +0c -print -quit 2>/dev/null)" ]]
}

export_trace_table() {
    local xpath="$1"
    local output="$2"
    "$XCRUN_COMMAND" xctrace export \
        --input "$TRACE_PATH" \
        --xpath "$xpath" \
        --output "$output"
}

verification_metric() {
    local key="$1"
    [[ -f "$TRACE_VERIFICATION_PATH" ]] || return 0
    awk -F '\t' -v key="$key" '$1 == key {print $2}' "$TRACE_VERIFICATION_PATH"
}

write_metadata() {
    local capture_status="$1"
    local incomplete_reason="$2"
    local started="$3"
    local finished="$4"
    local actual_duration="$5"
    local target_survived="$6"
    local package_frozen="$7"
    local trace_tables_verified="$8"
    local metadata_temp="$9"
    local xcode_version

    xcode_version="$("$XCRUN_COMMAND" xcodebuild -version | one_line)"
    {
        printf 'format_version\t2\n'
        printf 'capture_status\t%s\n' "$capture_status"
        printf 'incomplete_reason\t%s\n' "$incomplete_reason"
        printf 'application_label\t%s\n' "$APPLICATION_LABEL"
        printf 'scenario\t%s\n' "$SCENARIO"
        printf 'pid\t%s\n' "$PID"
        printf 'requested_duration_seconds\t%s\n' "$DURATION_SECONDS"
        printf 'actual_record_duration_seconds\t%s\n' "$actual_duration"
        printf 'recorder_elapsed_seconds\t%s\n' "$record_command_elapsed_seconds"
        printf 'trace_started_at\t%s\n' "$TRACE_STARTED_AT"
        printf 'trace_ended_at\t%s\n' "$TRACE_ENDED_AT"
        printf 'started_epoch_seconds\t%s\n' "$started"
        printf 'finished_epoch_seconds\t%s\n' "$finished"
        printf 'record_exit_status\t%s\n' "$record_status"
        printf 'export_exit_status\t%s\n' "$export_status"
        printf 'target_identity_verified\t%s\n' "$target_survived"
        printf 'target_survived_duration\t%s\n' "$target_survived"
        printf 'package_identity_verified_before_capture\ttrue\n'
        printf 'package_frozen_during_capture\t%s\n' "$package_frozen"
        printf 'bundle_name\t%s\n' "$BUNDLE_NAME"
        printf 'bundle_identifier\t%s\n' "$EXPECTED_BUNDLE_IDENTIFIER"
        printf 'bundle_marketing_version\t%s\n' "$BUNDLE_MARKETING_VERSION"
        printf 'bundle_build_version\t%s\n' "$BUNDLE_BUILD_VERSION"
        printf 'executable_sha256\t%s\n' "$EXPECTED_EXECUTABLE_SHA256"
        printf 'commit\t%s\n' "$EXPECTED_COMMIT"
        printf 'cargo_lock_sha256\t%s\n' "$CARGO_LOCK_SHA256"
        printf 'process_identity_sha256\t%s\n' \
            "$(printf '%s' "$TARGET_IDENTITY_TOKEN" | "$SHASUM_COMMAND" -a 256 | awk '{print $1}')"
        printf 'trace_recorder_sha256\t%s\n' "$TRACE_RECORDER_SHA256"
        printf 'process_inspector_sha256\t%s\n' "$PROCESS_INSPECTOR_SHA256"
        printf 'trace_verifier_sha256\t%s\n' "$TRACE_VERIFIER_SHA256"
        printf 'command_runner_sha256\t%s\n' "$COMMAND_RUNNER_SHA256"
        printf 'macos_version\t%s\n' "$(sw_vers -productVersion | one_line)"
        printf 'macos_build\t%s\n' "$(sw_vers -buildVersion | one_line)"
        printf 'architecture\t%s\n' "$(uname -m | one_line)"
        printf 'hardware_model\t%s\n' "$(sysctl_value hw.model)"
        printf 'processor\t%s\n' "$(sysctl_value machdep.cpu.brand_string)"
        printf 'logical_cpu_count\t%s\n' "$(sysctl_value hw.ncpu)"
        printf 'memory_bytes\t%s\n' "$(sysctl_value hw.memsize)"
        printf 'xcode_version\t%s\n' "$xcode_version"
        printf 'trace_template\tTime Profiler\n'
        printf 'required_trace_instruments\tAllocations,Hangs\n'
        printf 'trace_tables_verified\t%s\n' "$trace_tables_verified"
        printf 'trace_target_pid_verified\t%s\n' "$trace_target_pid_verified"
        printf 'time_profiler_sample_count\t%s\n' "$time_profile_row_count"
        printf 'allocations_event_count\t%s\n' "$allocations_row_count"
        printf 'hangs_event_count\t%s\n' "$hangs_row_count"
        printf 'trace_file\t%s\n' "$(basename -- "$TRACE_PATH")"
        printf 'toc_file\t%s\n' "$(basename -- "$TOC_PATH")"
        printf 'time_profiler_export_file\t%s\n' "$(basename -- "$TIME_PROFILE_EXPORT_PATH")"
        printf 'allocations_export_file\t%s\n' "$(basename -- "$ALLOCATIONS_EXPORT_PATH")"
        printf 'hangs_export_file\t%s\n' "$(basename -- "$HANGS_EXPORT_PATH")"
        printf 'trace_verification_file\t%s\n' "$(basename -- "$TRACE_VERIFICATION_PATH")"
        printf 'trace_exports_privacy\tprivate-sensitive-0700\n'
    } > "$metadata_temp"
}

if [[ "${1:-}" == "--doctor" ]]; then
    doctor
    exit 0
fi

while (( $# > 0 )); do
    case "$1" in
        --pid)
            (( $# >= 2 )) || die "--pid requires a value"
            PID="$2"
            shift
            ;;
        --application)
            (( $# >= 2 )) || die "--application requires a value"
            APPLICATION_LABEL="$2"
            shift
            ;;
        --scenario)
            (( $# >= 2 )) || die "--scenario requires a value"
            SCENARIO="$2"
            shift
            ;;
        --duration-seconds)
            (( $# >= 2 )) || die "--duration-seconds requires a value"
            DURATION_SECONDS="$2"
            shift
            ;;
        --output-directory)
            (( $# >= 2 )) || die "--output-directory requires a path"
            OUTPUT_DIRECTORY="$2"
            shift
            ;;
        --app-bundle)
            (( $# >= 2 )) || die "--app-bundle requires a path"
            APP_BUNDLE="$2"
            shift
            ;;
        --bundle-identifier)
            (( $# >= 2 )) || die "--bundle-identifier requires a value"
            EXPECTED_BUNDLE_IDENTIFIER="$2"
            shift
            ;;
        --expected-executable-sha256)
            (( $# >= 2 )) || die "--expected-executable-sha256 requires a value"
            EXPECTED_EXECUTABLE_SHA256="$2"
            shift
            ;;
        --expected-commit)
            (( $# >= 2 )) || die "--expected-commit requires a value"
            EXPECTED_COMMIT="$2"
            shift
            ;;
        --expected-marketing-version)
            (( $# >= 2 )) || die "--expected-marketing-version requires a value"
            EXPECTED_MARKETING_VERSION="$2"
            shift
            ;;
        --expected-build-version)
            (( $# >= 2 )) || die "--expected-build-version requires a value"
            EXPECTED_BUILD_VERSION="$2"
            shift
            ;;
        --expected-cargo-lock-sha256)
            (( $# >= 2 )) || die "--expected-cargo-lock-sha256 requires a value"
            EXPECTED_CARGO_LOCK_SHA256="$2"
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            die "unknown argument: $1"
            ;;
    esac
    shift
done

doctor >/dev/null
is_positive_integer "$PID" || die "PID must be a positive integer"
is_positive_integer "$DURATION_SECONDS" || die "duration must be a positive integer"
is_safe_label "$APPLICATION_LABEL" || die "application label must be filesystem-safe"
is_safe_label "$SCENARIO" || die "scenario label must be filesystem-safe"
is_safe_label "$EXPECTED_BUNDLE_IDENTIFIER" || die "bundle identifier must be filesystem-safe"
is_sha256 "$EXPECTED_EXECUTABLE_SHA256" || die "expected executable SHA-256 is invalid"
is_sha256 "$EXPECTED_CARGO_LOCK_SHA256" || die "expected Cargo.lock SHA-256 is invalid"
is_commit "$EXPECTED_COMMIT" || die "expected commit is invalid"
is_safe_label "$EXPECTED_MARKETING_VERSION" || die "expected marketing version is invalid"
is_safe_label "$EXPECTED_BUILD_VERSION" || die "expected build version is invalid"
[[ -n "$OUTPUT_DIRECTORY" ]] || die "--output-directory is required"
[[ -d "$APP_BUNDLE" && "$APP_BUNDLE" == *.app ]] || die "app bundle is not a .app directory"

SCRIPT_DIRECTORY="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIRECTORY
if [[ -z "$PROCESS_INSPECTOR" ]]; then
    PROCESS_INSPECTOR="$SCRIPT_DIRECTORY/inspect-release-performance-process.py"
fi
if [[ -z "$TRACE_VERIFIER" ]]; then
    TRACE_VERIFIER="$SCRIPT_DIRECTORY/verify-release-performance-trace.py"
fi
readonly PROCESS_INSPECTOR TRACE_VERIFIER
COMMAND_RUNNER="$SCRIPT_DIRECTORY/run-release-performance-command.py"
readonly COMMAND_RUNNER
[[ -x "$COMMAND_RUNNER" ]] || die "release performance command runner is not executable"
RECORDER_PATH="$SCRIPT_DIRECTORY/$(basename -- "$0")"
readonly RECORDER_PATH
REPOSITORY_ROOT="$(cd -- "$SCRIPT_DIRECTORY/.." && pwd -P)"
readonly REPOSITORY_ROOT
[[ -z "$(git -C "$REPOSITORY_ROOT" status --porcelain --untracked-files=no)" ]] \
    || die "repository has tracked changes; freeze and commit tooling first"
APP_BUNDLE="$(canonical_path "$APP_BUNDLE")"
readonly APP_BUNDLE
BUNDLE_NAME="$(basename -- "$APP_BUNDLE")"
readonly BUNDLE_NAME
is_safe_label "$BUNDLE_NAME" || die "app bundle name must be filesystem-safe"
APP_INFO_PLIST="$APP_BUNDLE/Contents/Info.plist"
readonly APP_INFO_PLIST
[[ -f "$APP_INFO_PLIST" ]] || die "app bundle is missing Contents/Info.plist"
BUNDLE_EXECUTABLE_NAME="$(plist_value "$APP_INFO_PLIST" CFBundleExecutable)"
readonly BUNDLE_EXECUTABLE_NAME
is_safe_label "$BUNDLE_EXECUTABLE_NAME" || die "bundle executable name is invalid"
PACKAGE_EXECUTABLE="$(canonical_path "$APP_BUNDLE/Contents/MacOS/$BUNDLE_EXECUTABLE_NAME")"
readonly PACKAGE_EXECUTABLE
[[ -f "$PACKAGE_EXECUTABLE" && -x "$PACKAGE_EXECUTABLE" ]] \
    || die "packaged executable is missing or not executable"
[[ "$(plist_value "$APP_INFO_PLIST" CFBundleIdentifier)" == "$EXPECTED_BUNDLE_IDENTIFIER" ]] \
    || die "bundle identifier does not match the supplied identity"
[[ "$("$SHASUM_COMMAND" -a 256 "$PACKAGE_EXECUTABLE" | awk '{ print $1 }')" \
    == "$EXPECTED_EXECUTABLE_SHA256" ]] || die "packaged executable SHA-256 mismatch"
[[ "$(git -C "$REPOSITORY_ROOT" rev-parse HEAD)" == "$EXPECTED_COMMIT" ]] \
    || die "repository commit does not match the supplied commit"
[[ "$(plist_value "$APP_INFO_PLIST" CFBundleShortVersionString)" \
    == "$EXPECTED_MARKETING_VERSION" ]] \
    || die "bundle marketing version does not match the supplied identity"
[[ "$(plist_value "$APP_INFO_PLIST" CFBundleVersion)" \
    == "$EXPECTED_BUILD_VERSION" ]] \
    || die "bundle build version does not match the supplied identity"
[[ "$("$SHASUM_COMMAND" -a 256 "$REPOSITORY_ROOT/Cargo.lock" | awk '{print $1}')" \
    == "$EXPECTED_CARGO_LOCK_SHA256" ]] \
    || die "Cargo.lock SHA-256 does not match the supplied identity"
PROCESS_INSPECTOR_SHA256="$("$SHASUM_COMMAND" -a 256 "$PROCESS_INSPECTOR" | awk '{print $1}')"
TRACE_VERIFIER_SHA256="$("$SHASUM_COMMAND" -a 256 "$TRACE_VERIFIER" | awk '{print $1}')"
TRACE_RECORDER_SHA256="$("$SHASUM_COMMAND" -a 256 "$RECORDER_PATH" | awk '{print $1}')"
COMMAND_RUNNER_SHA256="$("$SHASUM_COMMAND" -a 256 "$COMMAND_RUNNER" | awk '{print $1}')"
readonly PROCESS_INSPECTOR_SHA256 TRACE_VERIFIER_SHA256 TRACE_RECORDER_SHA256 \
    COMMAND_RUNNER_SHA256
TARGET_IDENTITY_TOKEN="$(process_identity)"
readonly TARGET_IDENTITY_TOKEN
[[ -n "$TARGET_IDENTITY_TOKEN" ]] || die "could not capture kernel process identity"

BUNDLE_MARKETING_VERSION="$EXPECTED_MARKETING_VERSION"
readonly BUNDLE_MARKETING_VERSION
BUNDLE_BUILD_VERSION="$EXPECTED_BUILD_VERSION"
readonly BUNDLE_BUILD_VERSION
is_safe_label "$BUNDLE_MARKETING_VERSION" || die "bundle marketing version is invalid"
is_safe_label "$BUNDLE_BUILD_VERSION" || die "bundle build version is invalid"
CARGO_LOCK_SHA256="$EXPECTED_CARGO_LOCK_SHA256"
readonly CARGO_LOCK_SHA256
readonly ARTIFACT_PREFIX="${APPLICATION_LABEL}-${SCENARIO}"
readonly TRACE_PATH="$OUTPUT_DIRECTORY/${ARTIFACT_PREFIX}.trace"
readonly TOC_PATH="$OUTPUT_DIRECTORY/${ARTIFACT_PREFIX}-trace-toc.xml"
readonly PRIVATE_EXPORT_DIRECTORY="$OUTPUT_DIRECTORY/.private-${ARTIFACT_PREFIX}-exports"
readonly TIME_PROFILE_EXPORT_PATH="$PRIVATE_EXPORT_DIRECTORY/time-profile.xml"
readonly ALLOCATIONS_EXPORT_PATH="$PRIVATE_EXPORT_DIRECTORY/allocations.xml"
readonly HANGS_EXPORT_PATH="$PRIVATE_EXPORT_DIRECTORY/hangs.xml"
readonly TRACE_VERIFICATION_PATH="$PRIVATE_EXPORT_DIRECTORY/verification.tsv"
readonly METADATA_PATH="$OUTPUT_DIRECTORY/${ARTIFACT_PREFIX}-trace-metadata.tsv"
readonly METADATA_TEMP="${METADATA_PATH}.tmp.$$"
readonly RECORD_ELAPSED_PATH="$PRIVATE_EXPORT_DIRECTORY/record-elapsed-seconds.txt"

mkdir -p -- "$OUTPUT_DIRECTORY"
mkdir -m 700 -- "$PRIVATE_EXPORT_DIRECTORY"
for path in "$TRACE_PATH" "$TOC_PATH" "$TIME_PROFILE_EXPORT_PATH" \
    "$ALLOCATIONS_EXPORT_PATH" "$HANGS_EXPORT_PATH" "$TRACE_VERIFICATION_PATH" \
    "$RECORD_ELAPSED_PATH" "$METADATA_PATH"; do
    [[ ! -e "$path" ]] || die "refusing to overwrite trace artifact: $path"
done
cleanup_trace_metadata_temp() {
    rm -f -- "$METADATA_TEMP"
}

handle_trace_signal() {
    cleanup_trace_metadata_temp
    trap - EXIT INT TERM
    exit 130
}

trap cleanup_trace_metadata_temp EXIT
trap handle_trace_signal INT TERM

record_status=0
export_status="not-run"
started_epoch_seconds="$(date +%s)"
set +e
"$COMMAND_RUNNER" "$RECORD_ELAPSED_PATH" "$XCRUN_COMMAND" xctrace record \
    --template "Time Profiler" \
    --instrument "Allocations" \
    --instrument "Hangs" \
    --attach "$PID" \
    --time-limit "${DURATION_SECONDS}s" \
    --output "$TRACE_PATH" \
    --no-prompt
record_status=$?
set -e
record_command_elapsed_seconds="$(tr -d '[:space:]' < "$RECORD_ELAPSED_PATH" 2>/dev/null || true)"
[[ "$record_command_elapsed_seconds" =~ ^[0-9]+([.][0-9]+)?$ ]] \
    || record_command_elapsed_seconds=0
finished_epoch_seconds="$(date +%s)"

target_survived=false
if target_identity_matches; then
    target_survived=true
fi
package_frozen=false
if package_is_frozen; then
    package_frozen=true
fi

if (( record_status == 0 )) && trace_bundle_has_data; then
    set +e
    "$XCRUN_COMMAND" xctrace export \
        --input "$TRACE_PATH" \
        --toc \
        --output "$TOC_PATH"
    export_status=$?
    set -e
fi

table_export_status=0
if [[ "$export_status" == 0 ]]; then
    set +e
    export_trace_table \
        '/trace-toc/run[@number="1"]/data/table[@schema="time-profile"]' \
        "$TIME_PROFILE_EXPORT_PATH"
    (( table_export_status |= $? ))
    export_trace_table \
        '/trace-toc/run[@number="1"]/tracks/track[@name="Allocations"]/details/detail[@name="Allocations List"]' \
        "$ALLOCATIONS_EXPORT_PATH"
    (( table_export_status |= $? ))
    export_trace_table \
        '/trace-toc/run[@number="1"]/data/table[@schema="potential-hangs"]' \
        "$HANGS_EXPORT_PATH"
    (( table_export_status |= $? ))
    set -e
else
    table_export_status=1
fi

verification_status="not-run"
if (( table_export_status == 0 )); then
    set +e
    "$TRACE_VERIFIER" \
        --toc "$TOC_PATH" \
        --time-profile "$TIME_PROFILE_EXPORT_PATH" \
        --allocations "$ALLOCATIONS_EXPORT_PATH" \
        --hangs "$HANGS_EXPORT_PATH" \
        --pid "$PID" \
        --process-name "$BUNDLE_EXECUTABLE_NAME" \
        --requested-seconds "$DURATION_SECONDS" \
        --command-elapsed-seconds "$record_command_elapsed_seconds" \
        > "$TRACE_VERIFICATION_PATH"
    verification_status=$?
    set -e
fi
actual_record_duration_seconds="$(verification_metric actual_record_duration_seconds)"
actual_record_duration_seconds="${actual_record_duration_seconds:-0}"
TRACE_STARTED_AT="$(verification_metric trace_started_at)"
TRACE_ENDED_AT="$(verification_metric trace_ended_at)"
time_profile_row_count="$(verification_metric time_profiler_sample_count)"
time_profile_row_count="${time_profile_row_count:-0}"
allocations_row_count="$(verification_metric allocations_event_count)"
allocations_row_count="${allocations_row_count:-0}"
hangs_row_count="$(verification_metric hangs_event_count)"
hangs_row_count="${hangs_row_count:-0}"
trace_target_pid_verified=false
[[ "$verification_status" == 0 ]] && trace_target_pid_verified=true

capture_status="INCOMPLETE"
incomplete_reason="none"
trace_tables_verified=false
[[ "$verification_status" == 0 ]] && trace_tables_verified=true
if (( record_status != 0 )); then
    incomplete_reason="record-command-failed"
elif [[ "$target_survived" != true ]]; then
    incomplete_reason="target-did-not-survive-duration"
elif [[ "$package_frozen" != true ]]; then
    incomplete_reason="package-identity-changed"
elif ! trace_bundle_has_data; then
    incomplete_reason="trace-bundle-is-empty"
elif [[ "$export_status" != 0 ]]; then
    incomplete_reason="trace-toc-export-failed"
elif (( table_export_status != 0 )); then
    incomplete_reason="trace-table-export-failed"
elif [[ "$verification_status" != 0 ]]; then
    incomplete_reason="$(verification_metric reason)"
    incomplete_reason="${incomplete_reason:-trace-evidence-not-verifiable}"
elif [[ "$TEST_OVERRIDES_ACTIVE" == true ]]; then
    incomplete_reason="test-overrides-active"
elif ! live_target_code_identity_verified; then
    incomplete_reason="live-target-code-identity-unavailable"
else
    capture_status="CAPTURED"
fi

write_metadata \
    "$capture_status" \
    "$incomplete_reason" \
    "$started_epoch_seconds" \
    "$finished_epoch_seconds" \
    "$actual_record_duration_seconds" \
    "$target_survived" \
    "$package_frozen" \
    "$trace_tables_verified" \
    "$METADATA_TEMP"
mv -- "$METADATA_TEMP" "$METADATA_PATH"
trap - EXIT INT TERM

if [[ "$capture_status" != "CAPTURED" ]]; then
    echo "error: trace capture is incomplete: $incomplete_reason" >&2
    exit 1
fi
printf 'Trace: %s\nMetadata: %s\nTable of contents: %s\n' \
    "$TRACE_PATH" "$METADATA_PATH" "$TOC_PATH"
