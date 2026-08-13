#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
umask 077

readonly XCRUN_COMMAND="${SPACETERM_XCRUN:-xcrun}"
readonly SHASUM_COMMAND="${SPACETERM_SHASUM:-shasum}"
readonly CONTINUOUS_CLOCK_COMMAND="${SPACETERM_CONTINUOUS_CLOCK:-}"
PROCESS_INSPECTOR="${SPACETERM_PROCESS_INSPECTOR:-}"
TRACE_VERIFIER="${SPACETERM_TRACE_VERIFIER:-}"
TEST_OVERRIDES_ACTIVE=false
[[ -z "${SPACETERM_XCRUN:-}${SPACETERM_SHASUM:-}${SPACETERM_CONTINUOUS_CLOCK:-}${SPACETERM_PROCESS_INSPECTOR:-}${SPACETERM_TRACE_VERIFIER:-}" ]] \
    || TEST_OVERRIDES_ACTIVE=true
readonly TEST_OVERRIDES_ACTIVE

SUBJECT_IDENTITY=""
RUN_METADATA=""
WORKLOAD_METADATA=""
WORKLOAD_EVENTS=""
READY_RECEIPT=""
SUPPLEMENTAL_EVIDENCE=""
CAMPAIGN_SECRET_FILE=""
CAMPAIGN_ID=""
SESSION_ID=""
NONCE=""
SCENARIO=""
DURATION_MILLISECONDS=""
WARMUP_MILLISECONDS=""
OUTPUT_DIRECTORY=""
CAPTURE_START_NOTIFICATION=""

usage() {
    cat <<EOF
Usage: $(basename -- "$0") --subject-identity FILE --run-metadata FILE \\
  --workload-metadata FILE --workload-events FILE --workload-ready-receipt FILE \\
  [--supplemental-evidence FILE] \\
  --campaign-secret-file FILE --campaign-id LABEL --session-id LABEL \\
  --nonce SHA256 --scenario LABEL --warmup-ms N --duration-ms N \\
  --output-directory NEW_PATH [--capture-start-notification NEW_FILE]

Attach Time Profiler, Allocations, and Hangs to the exact process frozen in a
subject identity. The finalized privacy-safe v3 metadata binds the live guest
code, immutable subject/run/workload evidence, continuous capture interval,
target-scoped trace tables, and measured duration. CAPTURED is evidence state,
not a performance verdict.

The workload events and metadata may be created during capture. Their canonical
parent directories are bound before capture, and finalization waits at most five
seconds after xctrace returns for both immutable regular files. An optional
capture-start notification is published atomically immediately before xctrace is
launched; the finalized trace interval remains the authoritative coverage proof.

Options:
  --doctor  Verify Xcode Instruments and metadata prerequisites.
  -h, --help
EOF
}

die() { echo "error: $*" >&2; exit 1; }
require_command() { command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"; }
is_positive_integer() { [[ "$1" =~ ^[1-9][0-9]*$ ]]; }
is_uint() { [[ "$1" =~ ^[0-9]+$ ]]; }
is_safe_label() { [[ "$1" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]]; }
is_sha256() { [[ "$1" =~ ^[0-9a-f]{64}$ ]]; }

doctor() {
    local instruments
    for command in "$XCRUN_COMMAND" awk basename chmod date find id ln mkdir mv \
        codesign plutil python3 realpath rm sleep stat "$SHASUM_COMMAND" xmllint; do
        require_command "$command"
    done
    instruments="$("$XCRUN_COMMAND" xctrace list instruments)"
    for instrument in "Time Profiler" "Allocations" "Hangs"; do
        grep -Fxq "$instrument" <<< "$instruments" \
            || die "required xctrace instrument is unavailable: $instrument"
    done
    "$XCRUN_COMMAND" xcodebuild -version >/dev/null
    echo "release performance trace prerequisites are available"
}

kv() {
    local file="$1" key="$2"
    awk -F '\t' -v wanted="$key" '
        NF != 2 { bad = 1 }
        $1 == wanted { count += 1; value = $2 }
        END { if (!bad && count == 1) print value }
    ' "$file"
}

sha256() { "$SHASUM_COMMAND" -a 256 "$1" | awk '{ print $1 }'; }

clock_anchor() {
    if [[ -n "$CONTINUOUS_CLOCK_COMMAND" ]]; then
        "$CONTINUOUS_CLOCK_COMMAND"
    else
        python3 - <<'PY'
import ctypes
import time

libsystem = ctypes.CDLL("/usr/lib/libSystem.B.dylib")
libsystem.mach_continuous_time.restype = ctypes.c_uint64
libsystem.mach_timebase_info.argtypes = [ctypes.POINTER(ctypes.c_uint32 * 2)]
info = (ctypes.c_uint32 * 2)()
if libsystem.mach_timebase_info(ctypes.byref(info)) != 0 or not info[1]:
    raise SystemExit(1)
first = libsystem.mach_continuous_time()
epoch = time.time_ns()
second = libsystem.mach_continuous_time()
continuous = ((first + second) // 2) * info[0] // info[1]
width = (second - first) * info[0] // info[1]
print(f"{continuous}\t{epoch}\t{width}")
PY
    fi
}

file_is_immutable_regular() {
    [[ -f "$1" && ! -L "$1" && ! -w "$1" && "$(stat -f '%l' "$1")" == 1 ]]
}

canonical_pending_path() {
    local path="$1" leaf parent mode
    leaf="$(basename -- "$path")"
    [[ -n "$leaf" && "$leaf" != . && "$leaf" != .. \
        && "$leaf" != *$'\n'* && "$leaf" != *$'\t'* ]] \
        || die "pending evidence path is invalid"
    parent="$(realpath "$(dirname -- "$path")")" \
        || die "pending evidence parent is unavailable"
    [[ -d "$parent" ]] || die "pending evidence parent is unavailable"
    mode="$(stat -f '%Lp' "$parent")"
    [[ "$mode" =~ ^[0-7]{3,4}$ ]] || die "pending evidence parent mode is invalid"
    (( (8#$mode & 077) == 0 )) || die "pending evidence parent must be private"
    printf '%s/%s\n' "$parent" "$leaf"
}

validate_campaign_secret() {
    local mode owner
    [[ -f "$CAMPAIGN_SECRET_FILE" && ! -L "$CAMPAIGN_SECRET_FILE" \
        && "$(stat -f '%l' "$CAMPAIGN_SECRET_FILE")" == 1 ]] \
        || die "campaign secret must be a non-symlink singleton regular file"
    mode="$(stat -f '%Lp' "$CAMPAIGN_SECRET_FILE")"
    owner="$(stat -f '%u' "$CAMPAIGN_SECRET_FILE")"
    [[ "$mode" =~ ^[0-7]{3,4}$ && "$owner" == "$(id -u)" ]] \
        || die "campaign secret ownership or mode is invalid"
    (( (8#$mode & 077) == 0 && (8#$mode & 0200) == 0 )) \
        || die "campaign secret must be private and immutable"
    CAMPAIGN_SECRET_IDENTITY="$(stat -f '%d:%i' "$CAMPAIGN_SECRET_FILE")"
    CAMPAIGN_SECRET_SHA256="$(sha256 "$CAMPAIGN_SECRET_FILE")"
    readonly CAMPAIGN_SECRET_IDENTITY CAMPAIGN_SECRET_SHA256
}

validate_supplemental_evidence() {
    SUPPLEMENTAL_EVIDENCE_SHA256=0000000000000000000000000000000000000000000000000000000000000000
    [[ -n "$SUPPLEMENTAL_EVIDENCE" ]] || {
        readonly SUPPLEMENTAL_EVIDENCE_SHA256
        return 0
    }
    local attempt
    for attempt in {1..51}; do
        [[ -e "$SUPPLEMENTAL_EVIDENCE" ]] && break
        (( attempt == 51 )) || sleep 0.1
    done
    [[ "$(stat -f '%d:%i' "$SUPPLEMENTAL_EVIDENCE_PARENT")" \
            == "$SUPPLEMENTAL_EVIDENCE_PARENT_IDENTITY" ]] || return 1
    file_is_immutable_regular "$SUPPLEMENTAL_EVIDENCE" || return 1
    if [[ "$SUPPLEMENTAL_WAS_PREEXISTING" == true ]]; then
        [[ "$(stat -f '%d:%i' "$SUPPLEMENTAL_EVIDENCE")" \
                == "$PRECAPTURE_SUPPLEMENTAL_IDENTITY" \
            && "$(sha256 "$SUPPLEMENTAL_EVIDENCE")" \
                == "$PRECAPTURE_SUPPLEMENTAL_SHA256" ]] || return 1
    fi
    SUPPLEMENTAL_EVIDENCE_SHA256="$(sha256 "$SUPPLEMENTAL_EVIDENCE")"
    readonly SUPPLEMENTAL_EVIDENCE_SHA256
}

validate_ready_receipt() {
    local secret_mode
    readonly READY_KEYS="format_version campaign_id session_id nonce subject_identity_sha256 producer_pid producer_started_continuous_ns producer_session_id producer_process_group tty_device tty_inode tty_rdev events_device events_inode events_prefix_bytes events_prefix_sha256 measurement_ready_continuous_ns measurement_ready_byte_count auth_algorithm ready_hmac_sha256"
    file_is_immutable_regular "$READY_RECEIPT" \
        || die "workload ready receipt must be immutable singleton evidence"
    validate_exact_schema "$READY_RECEIPT" "$READY_KEYS" 20 "workload ready receipt"
    [[ "$(kv "$READY_RECEIPT" format_version)" == 1 \
        && "$(kv "$READY_RECEIPT" campaign_id)" == "$CAMPAIGN_ID" \
        && "$(kv "$READY_RECEIPT" session_id)" == "$SESSION_ID" \
        && "$(kv "$READY_RECEIPT" nonce)" == "$NONCE" \
        && "$(kv "$READY_RECEIPT" subject_identity_sha256)" == "$SUBJECT_IDENTITY_SHA256" \
        && "$(kv "$READY_RECEIPT" auth_algorithm)" == hmac-sha256 ]] \
        || die "workload ready receipt binding is invalid"
    python3 - "$READY_RECEIPT" "$WORKLOAD_EVENTS" "$CAMPAIGN_SECRET_FILE" <<'PY' \
        || die "workload ready receipt authentication is invalid"
import hashlib, hmac, os, pathlib, struct, sys

receipt_path, events_path, secret_path = map(pathlib.Path, sys.argv[1:])
receipt = receipt_path.read_bytes()
lines = receipt.splitlines(keepends=True)
if not receipt.endswith(b"\n") or not lines[-1].startswith(b"ready_hmac_sha256\t"):
    raise SystemExit(1)
fields = dict(line.rstrip(b"\n").split(b"\t", 1) for line in lines)
prefix_bytes = int(fields[b"events_prefix_bytes"])
events = events_path.read_bytes()
stat = os.stat(events_path, follow_symlinks=False)
if prefix_bytes <= 0 or prefix_bytes > len(events):
    raise SystemExit(1)
if fields[b"events_device"] != str(stat.st_dev).encode() or fields[b"events_inode"] != str(stat.st_ino).encode():
    raise SystemExit(1)
if fields[b"events_prefix_sha256"] != hashlib.sha256(events[:prefix_bytes]).hexdigest().encode():
    raise SystemExit(1)
unsigned = b"".join(lines[:-1])
authenticated = b"spaceterm.performance.workload-ready/v1\0" + struct.pack(">Q", len(unsigned)) + unsigned
actual = hmac.new(secret_path.read_bytes(), authenticated, hashlib.sha256).hexdigest().encode()
if not hmac.compare_digest(fields[b"ready_hmac_sha256"], actual):
    raise SystemExit(1)
PY
    READY_RECEIPT_SHA256="$(sha256 "$READY_RECEIPT")"
    READY_RECEIPT_IDENTITY="$(stat -f '%d:%i' "$READY_RECEIPT")"
    READY_PRODUCER_PID="$(kv "$READY_RECEIPT" producer_pid)"
    READY_PRODUCER_STARTED_NS="$(kv "$READY_RECEIPT" producer_started_continuous_ns)"
    READY_PRODUCER_SESSION_ID="$(kv "$READY_RECEIPT" producer_session_id)"
    READY_PRODUCER_PROCESS_GROUP="$(kv "$READY_RECEIPT" producer_process_group)"
    READY_TTY_DEVICE="$(kv "$READY_RECEIPT" tty_device)"
    READY_TTY_INODE="$(kv "$READY_RECEIPT" tty_inode)"
    READY_TTY_RDEV="$(kv "$READY_RECEIPT" tty_rdev)"
    READY_EVENTS_DEVICE="$(kv "$READY_RECEIPT" events_device)"
    READY_EVENTS_INODE="$(kv "$READY_RECEIPT" events_inode)"
    READY_MEASUREMENT_NS="$(kv "$READY_RECEIPT" measurement_ready_continuous_ns)"
    readonly READY_RECEIPT_SHA256 READY_RECEIPT_IDENTITY READY_PRODUCER_PID \
        READY_PRODUCER_STARTED_NS READY_PRODUCER_SESSION_ID \
        READY_PRODUCER_PROCESS_GROUP READY_TTY_DEVICE READY_TTY_INODE \
        READY_TTY_RDEV READY_EVENTS_DEVICE READY_EVENTS_INODE READY_MEASUREMENT_NS
}

validate_exact_schema() {
    local file="$1" allowed="$2" expected_count="$3" label="$4"
    schema_is_exact "$file" "$allowed" "$expected_count" \
        || die "$label schema is malformed, duplicate, or contains unknown fields"
}

schema_is_exact() {
    local file="$1" allowed="$2" expected_count="$3"
    awk -F '\t' -v allowed="$allowed" -v expected="$expected_count" '
        BEGIN {
            count = split(allowed, keys, " ")
            for (i = 1; i <= count; i += 1) valid[keys[i]] = 1
        }
        NF != 2 || $1 == "" || $2 == "" || !($1 in valid) \
            || $1 != keys[NR] || seen[$1]++ { bad = 1 }
        END { exit bad || NR != expected }
    ' "$file"
}

validate_subject_identity() {
    local app_canonical executable_canonical
    readonly SUBJECT_KEYS="format_version subject app_bundle_path bundle_identifier bundle_version executable_path executable_sha256 executable_device executable_inode executable_fsid signature_valid signing_identifier team_identifier cdhash process_pid process_start_identity identity_status"
    file_is_immutable_regular "$SUBJECT_IDENTITY" \
        || die "subject identity must be an immutable non-symlink regular file"
    validate_exact_schema "$SUBJECT_IDENTITY" "$SUBJECT_KEYS" 17 "subject identity"
    [[ "$(kv "$SUBJECT_IDENTITY" format_version)" == 1 \
        && "$(kv "$SUBJECT_IDENTITY" identity_status)" == frozen \
        && "$(kv "$SUBJECT_IDENTITY" signature_valid)" == true ]] \
        || die "subject identity is not frozen v1 evidence"
    SUBJECT="$(kv "$SUBJECT_IDENTITY" subject)"
    [[ "$SUBJECT" == spaceterm || "$SUBJECT" == ghostty ]] || die "subject is invalid"
    APP_BUNDLE="$(kv "$SUBJECT_IDENTITY" app_bundle_path)"
    BUNDLE_IDENTIFIER="$(kv "$SUBJECT_IDENTITY" bundle_identifier)"
    BUNDLE_VERSION="$(kv "$SUBJECT_IDENTITY" bundle_version)"
    PACKAGE_EXECUTABLE="$(kv "$SUBJECT_IDENTITY" executable_path)"
    BUNDLE_EXECUTABLE_NAME="$(basename -- "$PACKAGE_EXECUTABLE")"
    EXPECTED_EXECUTABLE_SHA256="$(kv "$SUBJECT_IDENTITY" executable_sha256)"
    EXECUTABLE_DEVICE="$(kv "$SUBJECT_IDENTITY" executable_device)"
    EXECUTABLE_INODE="$(kv "$SUBJECT_IDENTITY" executable_inode)"
    EXECUTABLE_FSID="$(kv "$SUBJECT_IDENTITY" executable_fsid)"
    SIGNING_IDENTIFIER="$(kv "$SUBJECT_IDENTITY" signing_identifier)"
    TEAM_IDENTIFIER="$(kv "$SUBJECT_IDENTITY" team_identifier)"
    EXPECTED_CDHASH="$(kv "$SUBJECT_IDENTITY" cdhash)"
    PID="$(kv "$SUBJECT_IDENTITY" process_pid)"
    PROCESS_START_IDENTITY="$(kv "$SUBJECT_IDENTITY" process_start_identity)"
    is_positive_integer "$PID" || die "subject process PID is invalid"
    [[ "$PROCESS_START_IDENTITY" =~ ^[1-9][0-9]*:([0-9]{1,6})$ ]] \
        || die "subject process start identity is not kernel-precise"
    (( 10#${BASH_REMATCH[1]} < 1000000 )) \
        || die "subject process start microseconds are invalid"
    if ! is_uint "$EXECUTABLE_DEVICE" || ! is_uint "$EXECUTABLE_INODE" \
        || ! is_uint "$EXECUTABLE_FSID"; then
        die "subject executable vnode is invalid"
    fi
    [[ "$EXECUTABLE_DEVICE" == "$EXECUTABLE_FSID" ]] \
        || die "subject executable filesystem identity is inconsistent"
    is_sha256 "$EXPECTED_EXECUTABLE_SHA256" || die "subject executable hash is invalid"
    [[ "$EXPECTED_CDHASH" =~ ^[0-9a-f]+$ ]] || die "subject CDHash is invalid"
    [[ "$APP_BUNDLE" == /* && "$APP_BUNDLE" == *.app && -d "$APP_BUNDLE" \
        && "$PACKAGE_EXECUTABLE" == /* && -f "$PACKAGE_EXECUTABLE" \
        && -x "$PACKAGE_EXECUTABLE" ]] || die "subject bundle or executable is unavailable"
    app_canonical="$(realpath "$APP_BUNDLE")"
    executable_canonical="$(realpath "$PACKAGE_EXECUTABLE")"
    [[ "$PACKAGE_EXECUTABLE" == "$APP_BUNDLE"/Contents/MacOS/* \
        && "$executable_canonical" == "$app_canonical"/Contents/MacOS/* ]] \
        || die "subject executable is outside its application bundle"
    APP_BUNDLE="$app_canonical"
    PACKAGE_EXECUTABLE="$executable_canonical"
    APP_INFO_PLIST="$APP_BUNDLE/Contents/Info.plist"
    [[ -f "$APP_INFO_PLIST" ]] || die "subject bundle Info.plist is unavailable"
    SUBJECT_IDENTITY_SHA256="$(sha256 "$SUBJECT_IDENTITY")"
    readonly SUBJECT APP_BUNDLE APP_INFO_PLIST BUNDLE_IDENTIFIER BUNDLE_VERSION \
        PACKAGE_EXECUTABLE BUNDLE_EXECUTABLE_NAME \
        EXPECTED_EXECUTABLE_SHA256 \
        EXECUTABLE_DEVICE EXECUTABLE_INODE EXECUTABLE_FSID SIGNING_IDENTIFIER \
        TEAM_IDENTIFIER EXPECTED_CDHASH PID PROCESS_START_IDENTITY \
        SUBJECT_IDENTITY_SHA256
}

package_bundle_matches() {
    local current_version
    current_version="$(plutil -extract CFBundleShortVersionString raw -o - \
        "$APP_INFO_PLIST" 2>/dev/null || true)+$(plutil -extract CFBundleVersion \
        raw -o - "$APP_INFO_PLIST" 2>/dev/null || true)"
    [[ "$(plutil -extract CFBundleIdentifier raw -o - "$APP_INFO_PLIST" \
            2>/dev/null || true)" == "$BUNDLE_IDENTIFIER" \
        && "$current_version" == "$BUNDLE_VERSION" \
        && "$(plutil -extract CFBundleExecutable raw -o - "$APP_INFO_PLIST" \
            2>/dev/null || true)" == "$BUNDLE_EXECUTABLE_NAME" \
        && "$(realpath "$APP_BUNDLE/Contents/MacOS/$BUNDLE_EXECUTABLE_NAME" \
            2>/dev/null || true)" == "$PACKAGE_EXECUTABLE" ]] \
        && codesign --verify --strict "$APP_BUNDLE" >/dev/null 2>&1
}

validate_run_metadata() {
    readonly RUN_KEYS="format_version subject subject_identity_sha256 scenario scenario_plan_sha256 workload_sha256 command_sha256 environment_sha256 font_sha256 initial_grid_sha256 measured_duration_ms process_pid process_start_identity status"
    file_is_immutable_regular "$RUN_METADATA" \
        || die "run metadata must be an immutable non-symlink regular file"
    validate_exact_schema "$RUN_METADATA" "$RUN_KEYS" 14 "run metadata"
    [[ "$(kv "$RUN_METADATA" format_version)" == 1 \
        && "$(kv "$RUN_METADATA" status)" == complete \
        && "$(kv "$RUN_METADATA" subject)" == "$SUBJECT" \
        && "$(kv "$RUN_METADATA" subject_identity_sha256)" == "$SUBJECT_IDENTITY_SHA256" \
        && "$(kv "$RUN_METADATA" scenario)" == "$SCENARIO" \
        && "$(kv "$RUN_METADATA" measured_duration_ms)" == "$DURATION_MILLISECONDS" \
        && "$(kv "$RUN_METADATA" process_pid)" == "$PID" \
        && "$(kv "$RUN_METADATA" process_start_identity)" == "$PROCESS_START_IDENTITY" ]] \
        || die "run metadata does not bind the requested frozen subject and scenario"
    RUN_WORKLOAD_SHA256="$(kv "$RUN_METADATA" workload_sha256)"
    is_sha256 "$RUN_WORKLOAD_SHA256" || die "run workload hash is invalid"
    RUN_METADATA_SHA256="$(sha256 "$RUN_METADATA")"
    readonly RUN_WORKLOAD_SHA256 RUN_METADATA_SHA256
}

wait_for_workload_evidence() {
    local attempt
    for attempt in {1..51}; do
        [[ -e "$WORKLOAD_METADATA" && -e "$WORKLOAD_EVENTS" ]] && return 0
        (( attempt == 51 )) || sleep 0.1
    done
    return 1
}

validate_workload_metadata() {
    local secret_mode
    readonly WORKLOAD_KEYS="format_version scenario campaign_id session_id nonce subject_identity_sha256 subject_process_pid subject_process_start_identity producer_sha256 producer_pid producer_started_continuous_ns producer_session_id producer_process_group tty_device tty_inode tty_rdev ready_receipt_sha256 events_sha256 auth_algorithm seed_sha256 seed_bytes requested_duration_ms warmup_ms requested_iterations requested_seed_rows emitted_bytes input_events plan_start_continuous_ns started_continuous_ns ended_continuous_ns status events_hmac_sha256"
    wait_for_workload_evidence || return 1
    [[ "$(stat -f '%d:%i' "$WORKLOAD_METADATA_PARENT")" \
            == "$WORKLOAD_METADATA_PARENT_IDENTITY" \
        && "$(stat -f '%d:%i' "$WORKLOAD_EVENTS_PARENT")" \
            == "$WORKLOAD_EVENTS_PARENT_IDENTITY" ]] || return 1
    file_is_immutable_regular "$WORKLOAD_METADATA" || return 1
    file_is_immutable_regular "$WORKLOAD_EVENTS" || return 1
    [[ -f "$CAMPAIGN_SECRET_FILE" && ! -L "$CAMPAIGN_SECRET_FILE" ]] || return 1
    secret_mode="$(stat -f '%Lp' "$CAMPAIGN_SECRET_FILE")"
    [[ "$secret_mode" =~ ^[0-7]{3,4}$ ]] || return 1
    (( (8#$secret_mode & 077) == 0 )) || return 1
    schema_is_exact "$WORKLOAD_METADATA" "$WORKLOAD_KEYS" 32 || return 1
    [[ "$(kv "$WORKLOAD_METADATA" format_version)" == 3 \
        && "$(kv "$WORKLOAD_METADATA" scenario)" == "$SCENARIO" \
        && "$(kv "$WORKLOAD_METADATA" campaign_id)" == "$CAMPAIGN_ID" \
        && "$(kv "$WORKLOAD_METADATA" session_id)" == "$SESSION_ID" \
        && "$(kv "$WORKLOAD_METADATA" nonce)" == "$NONCE" \
        && "$(kv "$WORKLOAD_METADATA" subject_identity_sha256)" == "$SUBJECT_IDENTITY_SHA256" \
        && "$(kv "$WORKLOAD_METADATA" subject_process_pid)" == "$PID" \
        && "$(kv "$WORKLOAD_METADATA" subject_process_start_identity)" == "$PROCESS_START_IDENTITY" \
        && "$(kv "$WORKLOAD_METADATA" producer_sha256)" == "$RUN_WORKLOAD_SHA256" \
        && "$(kv "$WORKLOAD_METADATA" ready_receipt_sha256)" == "$READY_RECEIPT_SHA256" \
        && "$(kv "$WORKLOAD_METADATA" producer_pid)" == "$READY_PRODUCER_PID" \
        && "$(kv "$WORKLOAD_METADATA" producer_started_continuous_ns)" == "$READY_PRODUCER_STARTED_NS" \
        && "$(kv "$WORKLOAD_METADATA" producer_session_id)" == "$READY_PRODUCER_SESSION_ID" \
        && "$(kv "$WORKLOAD_METADATA" producer_process_group)" == "$READY_PRODUCER_PROCESS_GROUP" \
        && "$(kv "$WORKLOAD_METADATA" tty_device)" == "$READY_TTY_DEVICE" \
        && "$(kv "$WORKLOAD_METADATA" tty_inode)" == "$READY_TTY_INODE" \
        && "$(kv "$WORKLOAD_METADATA" tty_rdev)" == "$READY_TTY_RDEV" \
        && "$(stat -f '%d' "$WORKLOAD_EVENTS")" == "$READY_EVENTS_DEVICE" \
        && "$(stat -f '%i' "$WORKLOAD_EVENTS")" == "$READY_EVENTS_INODE" \
        && "$(kv "$WORKLOAD_METADATA" events_sha256)" == "$(sha256 "$WORKLOAD_EVENTS")" \
        && "$(kv "$WORKLOAD_METADATA" auth_algorithm)" == hmac-sha256 \
        && "$(kv "$WORKLOAD_METADATA" warmup_ms)" == "$WARMUP_MILLISECONDS" \
        && "$(kv "$WORKLOAD_METADATA" requested_duration_ms)" == "$DURATION_MILLISECONDS" \
        && "$(kv "$WORKLOAD_METADATA" status)" == complete ]] || return 1
    PLAN_START_NS="$(kv "$WORKLOAD_METADATA" plan_start_continuous_ns)"
    MEASUREMENT_START_NS="$(python3 - "$PLAN_START_NS" "$WARMUP_MILLISECONDS" <<'PY'
import sys
plan, warmup = map(int, sys.argv[1:])
print(plan + warmup * 1_000_000)
PY
)"
    WORKLOAD_STARTED_NS="$(kv "$WORKLOAD_METADATA" started_continuous_ns)"
    WORKLOAD_ENDED_NS="$(kv "$WORKLOAD_METADATA" ended_continuous_ns)"
    is_uint "$MEASUREMENT_START_NS" && is_uint "$WORKLOAD_STARTED_NS" \
        && is_uint "$WORKLOAD_ENDED_NS" \
        && (( WORKLOAD_STARTED_NS >= MEASUREMENT_START_NS \
            && WORKLOAD_STARTED_NS - MEASUREMENT_START_NS <= 100000000 \
            && WORKLOAD_ENDED_NS > WORKLOAD_STARTED_NS )) || return 1
    python3 - "$DURATION_MILLISECONDS" \
        "$(kv "$WORKLOAD_METADATA" producer_started_continuous_ns)" \
        "$PLAN_START_NS" "$MEASUREMENT_START_NS" "$WORKLOAD_STARTED_NS" \
        "$WORKLOAD_ENDED_NS" "$READY_MEASUREMENT_NS" <<'PY' \
        || return 1
import sys

values = sys.argv[1:]
if any(not value.isascii() or not value.isdecimal() for value in values):
    raise SystemExit(1)
requested_ms, producer_started, plan_start, measurement_start, started, ended, ready_measurement = map(int, values)
duration = ended - started
if not (producer_started <= ready_measurement <= plan_start <= measurement_start <= started):
    raise SystemExit(1)
if not requested_ms * 1_000_000 <= duration <= (requested_ms + 2_000) * 1_000_000:
    raise SystemExit(1)
PY
    python3 - "$WORKLOAD_METADATA" "$WORKLOAD_EVENTS" "$CAMPAIGN_SECRET_FILE" <<'PY' \
        || return 1
import hashlib
import hmac
import pathlib
import struct
import sys

metadata = pathlib.Path(sys.argv[1]).read_bytes()
events = pathlib.Path(sys.argv[2]).read_bytes()
secret = pathlib.Path(sys.argv[3]).read_bytes()
if len(secret) < 32 or not metadata.endswith(b"\n"):
    raise SystemExit(1)
lines = metadata.splitlines(keepends=True)
if not lines or not lines[-1].startswith(b"events_hmac_sha256\t"):
    raise SystemExit(1)
expected = lines[-1].split(b"\t", 1)[1].strip().decode("ascii")
unsigned = b"".join(lines[:-1])
authenticated = (
    b"spaceterm.performance.workload-auth/v1\0"
    + struct.pack(">Q", len(unsigned))
    + unsigned
    + struct.pack(">Q", len(events))
    + events
)
actual = hmac.new(secret, authenticated, hashlib.sha256).hexdigest()
if not hmac.compare_digest(expected, actual):
    raise SystemExit(1)
PY
    WORKLOAD_METADATA_SHA256="$(sha256 "$WORKLOAD_METADATA")"
    readonly PLAN_START_NS MEASUREMENT_START_NS WORKLOAD_STARTED_NS WORKLOAD_ENDED_NS \
        WORKLOAD_METADATA_SHA256
}

process_identity() {
    "$PROCESS_INSPECTOR" \
        --pid "$PID" \
        --expected-executable "$PACKAGE_EXECUTABLE" \
        --expected-sha256 "$EXPECTED_EXECUTABLE_SHA256" \
        --expected-device "$EXECUTABLE_DEVICE" \
        --expected-inode "$EXECUTABLE_INODE" \
        --expected-start-identity "$PROCESS_START_IDENTITY" \
        --expected-signing-identifier "$SIGNING_IDENTIFIER" \
        --expected-team-identifier "$TEAM_IDENTIFIER" \
        --expected-cdhash "$EXPECTED_CDHASH"
}

target_identity_matches() {
    local result
    result="$(process_identity 2>/dev/null || true)"
    [[ "$(awk -F '\t' '$1 == "identity_token" { print $2 }' <<< "$result")" \
            == "$TARGET_IDENTITY_TOKEN" \
        && "$(awk -F '\t' '$1 == "live_code_identity_verified" { print $2 }' \
            <<< "$result")" == true ]]
}

frozen_inputs_match() {
    local supplemental_parent_matches=true
    if [[ -n "$SUPPLEMENTAL_EVIDENCE" \
        && "$(stat -f '%d:%i' "$SUPPLEMENTAL_EVIDENCE_PARENT")" \
            != "$SUPPLEMENTAL_EVIDENCE_PARENT_IDENTITY" ]]; then
        supplemental_parent_matches=false
    fi
    if [[ "$SUPPLEMENTAL_WAS_PREEXISTING" == true ]]; then
        if [[ "$(stat -f '%d:%i' "$SUPPLEMENTAL_EVIDENCE")" \
                != "$PRECAPTURE_SUPPLEMENTAL_IDENTITY" \
            || "$(sha256 "$SUPPLEMENTAL_EVIDENCE")" \
                != "$PRECAPTURE_SUPPLEMENTAL_SHA256" ]]; then
            supplemental_parent_matches=false
        fi
    fi
    [[ "$(sha256 "$SUBJECT_IDENTITY")" == "$SUBJECT_IDENTITY_SHA256" \
        && "$(sha256 "$RUN_METADATA")" == "$RUN_METADATA_SHA256" \
        && "$(sha256 "$PACKAGE_EXECUTABLE")" == "$EXPECTED_EXECUTABLE_SHA256" \
        && "$(stat -f '%d' "$PACKAGE_EXECUTABLE")" == "$EXECUTABLE_DEVICE" \
        && "$(stat -f '%i' "$PACKAGE_EXECUTABLE")" == "$EXECUTABLE_INODE" \
        && "$(stat -f '%d:%i' "$CAMPAIGN_SECRET_FILE")" \
            == "$CAMPAIGN_SECRET_IDENTITY" \
        && "$(sha256 "$CAMPAIGN_SECRET_FILE")" == "$CAMPAIGN_SECRET_SHA256" \
        && "$(stat -f '%d:%i' "$READY_RECEIPT")" == "$READY_RECEIPT_IDENTITY" \
        && "$(sha256 "$READY_RECEIPT")" == "$READY_RECEIPT_SHA256" \
        && "$supplemental_parent_matches" == true \
        && "$(stat -f '%d:%i' "$WORKLOAD_METADATA_PARENT")" \
            == "$WORKLOAD_METADATA_PARENT_IDENTITY" \
        && "$(stat -f '%d:%i' "$WORKLOAD_EVENTS_PARENT")" \
            == "$WORKLOAD_EVENTS_PARENT_IDENTITY" ]] \
        && package_bundle_matches
}

publish_capture_start_notification() {
    [[ -n "$CAPTURE_START_NOTIFICATION" ]] || return 0
    local temporary="${CAPTURE_START_NOTIFICATION}.tmp.$$"
    {
        printf 'format_version\t1\n'
        printf 'subject_identity_sha256\t%s\n' "$SUBJECT_IDENTITY_SHA256"
        printf 'scenario\t%s\nprocess_pid\t%s\n' "$SCENARIO" "$PID"
        printf 'launched_continuous_ns\t%s\nstatus\tlaunched\n' "$wrapper_started_ns"
    } > "$temporary"
    chmod 0400 "$temporary"
    ln "$temporary" "$CAPTURE_START_NOTIFICATION" \
        || die "capture-start notification path was created concurrently"
    rm -f -- "$temporary"
}

trace_bundle_has_data() {
    [[ -d "$TRACE_PATH" ]] \
        && [[ -n "$(find "$TRACE_PATH" -type f -size +0c -print -quit 2>/dev/null)" ]]
}

export_trace_table() {
    "$XCRUN_COMMAND" xctrace export --input "$TRACE_PATH" --xpath "$1" --output "$2"
}

verification_metric() {
    [[ -f "$TRACE_VERIFICATION_PATH" ]] || return 0
    awk -F '\t' -v key="$1" '$1 == key { print $2 }' "$TRACE_VERIFICATION_PATH"
}

write_metadata() {
    local output="$1"
    {
        printf 'format_version\t3\n'
        printf 'capture_status\t%s\n' "$capture_status"
        printf 'incomplete_reason\t%s\n' "$incomplete_reason"
        printf 'subject_identity_sha256\t%s\n' "$SUBJECT_IDENTITY_SHA256"
        printf 'run_metadata_sha256\t%s\n' "$RUN_METADATA_SHA256"
        printf 'workload_metadata_sha256\t%s\n' "$workload_metadata_hash"
        printf 'workload_ready_receipt_sha256\t%s\n' "$READY_RECEIPT_SHA256"
        printf 'supplemental_evidence_sha256\t%s\n' "$SUPPLEMENTAL_EVIDENCE_SHA256"
        printf 'requested_duration_ms\t%s\n' "$DURATION_MILLISECONDS"
        printf 'actual_duration_ms\t%s\n' "$actual_duration_ms"
        printf 'capture_started_continuous_ns\t%s\n' "$capture_started_ns"
        printf 'capture_ended_continuous_ns\t%s\n' "$capture_ended_ns"
        printf 'target_identity_verified\t%s\n' "$target_identity_verified"
        printf 'trace_target_pid_verified\t%s\n' "$trace_target_pid_verified"
        printf 'time_profiler_instrument\t%s\n' "$time_profiler_instrument"
        printf 'allocations_instrument\t%s\n' "$allocations_instrument"
        printf 'hangs_instrument\t%s\n' "$hangs_instrument"
        printf 'time_profiler_target_verified\t%s\n' "$time_profiler_target_verified"
        printf 'allocations_target_verified\t%s\n' "$allocations_target_verified"
        printf 'hangs_target_verified\t%s\n' "$hangs_target_verified"
        printf 'time_profiler_rows\t%s\n' "$time_profiler_rows"
        printf 'allocations_rows\t%s\n' "$allocations_rows"
        printf 'hangs_rows\t%s\n' "$hangs_rows"
        printf 'maximum_main_thread_hang_ms\t%s\n' "$maximum_main_thread_hang_ms"
        printf 'status\t%s\n' "$metadata_status"
    } > "$output"
}

if [[ "${1:-}" == --doctor ]]; then doctor; exit 0; fi
while (( $# > 0 )); do
    case "$1" in
        --subject-identity) SUBJECT_IDENTITY="${2:-}"; shift ;;
        --run-metadata) RUN_METADATA="${2:-}"; shift ;;
        --workload-metadata) WORKLOAD_METADATA="${2:-}"; shift ;;
        --workload-events) WORKLOAD_EVENTS="${2:-}"; shift ;;
        --workload-ready-receipt) READY_RECEIPT="${2:-}"; shift ;;
        --supplemental-evidence) SUPPLEMENTAL_EVIDENCE="${2:-}"; shift ;;
        --campaign-secret-file) CAMPAIGN_SECRET_FILE="${2:-}"; shift ;;
        --campaign-id) CAMPAIGN_ID="${2:-}"; shift ;;
        --session-id) SESSION_ID="${2:-}"; shift ;;
        --nonce) NONCE="${2:-}"; shift ;;
        --scenario) SCENARIO="${2:-}"; shift ;;
        --warmup-ms) WARMUP_MILLISECONDS="${2:-}"; shift ;;
        --duration-ms) DURATION_MILLISECONDS="${2:-}"; shift ;;
        --output-directory) OUTPUT_DIRECTORY="${2:-}"; shift ;;
        --capture-start-notification) CAPTURE_START_NOTIFICATION="${2:-}"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument: $1" ;;
    esac
    shift
done

doctor >/dev/null
is_safe_label "$SCENARIO" || die "scenario label is invalid"
is_safe_label "$CAMPAIGN_ID" || die "campaign ID is invalid"
is_safe_label "$SESSION_ID" || die "session ID is invalid"
is_sha256 "$NONCE" || die "nonce is invalid"
is_uint "$WARMUP_MILLISECONDS" || die "warmup must be unsigned milliseconds"
is_positive_integer "$DURATION_MILLISECONDS" || die "duration must be positive milliseconds"
(( DURATION_MILLISECONDS % 1000 == 0 )) || die "duration must be whole seconds"
[[ -n "$SUBJECT_IDENTITY" && -n "$RUN_METADATA" && -n "$WORKLOAD_METADATA" \
    && -n "$WORKLOAD_EVENTS" && -n "$READY_RECEIPT" \
    && -n "$CAMPAIGN_SECRET_FILE" \
    && -n "$OUTPUT_DIRECTORY" ]] || die "required evidence or output argument is missing"
[[ ! -e "$OUTPUT_DIRECTORY" ]] || die "output directory already exists"

SCRIPT_DIRECTORY="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIRECTORY
PROCESS_INSPECTOR="${PROCESS_INSPECTOR:-$SCRIPT_DIRECTORY/inspect-release-performance-process.py}"
TRACE_VERIFIER="${TRACE_VERIFIER:-$SCRIPT_DIRECTORY/verify-release-performance-trace.py}"
COMMAND_RUNNER="$SCRIPT_DIRECTORY/run-release-performance-command.py"
readonly PROCESS_INSPECTOR TRACE_VERIFIER COMMAND_RUNNER
[[ -x "$PROCESS_INSPECTOR" && -x "$TRACE_VERIFIER" && -x "$COMMAND_RUNNER" ]] \
    || die "trace verifier tooling is not executable"

[[ ! -L "$SUBJECT_IDENTITY" && ! -L "$RUN_METADATA" \
    && ( ! -e "$WORKLOAD_EVENTS" || ! -L "$WORKLOAD_EVENTS" ) \
    && ! -L "$READY_RECEIPT" \
    && ( -z "$SUPPLEMENTAL_EVIDENCE" || ! -L "$SUPPLEMENTAL_EVIDENCE" ) \
    && ! -L "$CAMPAIGN_SECRET_FILE" \
    && ( ! -e "$WORKLOAD_METADATA" || ! -L "$WORKLOAD_METADATA" ) ]] \
    || die "evidence inputs must not be symlinks"
SUBJECT_IDENTITY="$(realpath "$SUBJECT_IDENTITY")"
RUN_METADATA="$(realpath "$RUN_METADATA")"
CAMPAIGN_SECRET_FILE="$(realpath "$CAMPAIGN_SECRET_FILE")"
READY_RECEIPT="$(realpath "$READY_RECEIPT")"
if [[ -n "$SUPPLEMENTAL_EVIDENCE" ]]; then
    SUPPLEMENTAL_EVIDENCE="$(canonical_pending_path "$SUPPLEMENTAL_EVIDENCE")"
    SUPPLEMENTAL_EVIDENCE_PARENT="$(dirname -- "$SUPPLEMENTAL_EVIDENCE")"
    SUPPLEMENTAL_EVIDENCE_PARENT_IDENTITY="$(stat -f '%d:%i' "$SUPPLEMENTAL_EVIDENCE_PARENT")"
    SUPPLEMENTAL_WAS_PREEXISTING=false
    PRECAPTURE_SUPPLEMENTAL_IDENTITY=none
    PRECAPTURE_SUPPLEMENTAL_SHA256=none
    if [[ -e "$SUPPLEMENTAL_EVIDENCE" ]]; then
        file_is_immutable_regular "$SUPPLEMENTAL_EVIDENCE" \
            || die "preexisting supplemental evidence must be immutable singleton evidence"
        SUPPLEMENTAL_WAS_PREEXISTING=true
        PRECAPTURE_SUPPLEMENTAL_IDENTITY="$(stat -f '%d:%i' "$SUPPLEMENTAL_EVIDENCE")"
        PRECAPTURE_SUPPLEMENTAL_SHA256="$(sha256 "$SUPPLEMENTAL_EVIDENCE")"
    fi
else
    SUPPLEMENTAL_EVIDENCE_PARENT=none
    SUPPLEMENTAL_EVIDENCE_PARENT_IDENTITY=none
    SUPPLEMENTAL_WAS_PREEXISTING=false
    PRECAPTURE_SUPPLEMENTAL_IDENTITY=none
    PRECAPTURE_SUPPLEMENTAL_SHA256=none
fi
WORKLOAD_METADATA="$(canonical_pending_path "$WORKLOAD_METADATA")"
WORKLOAD_EVENTS="$(canonical_pending_path "$WORKLOAD_EVENTS")"
WORKLOAD_METADATA_PARENT="$(dirname -- "$WORKLOAD_METADATA")"
WORKLOAD_EVENTS_PARENT="$(dirname -- "$WORKLOAD_EVENTS")"
WORKLOAD_METADATA_PARENT_IDENTITY="$(stat -f '%d:%i' "$WORKLOAD_METADATA_PARENT")"
WORKLOAD_EVENTS_PARENT_IDENTITY="$(stat -f '%d:%i' "$WORKLOAD_EVENTS_PARENT")"
if [[ -n "$CAPTURE_START_NOTIFICATION" ]]; then
    [[ ! -e "$CAPTURE_START_NOTIFICATION" && ! -L "$CAPTURE_START_NOTIFICATION" ]] \
        || die "capture-start notification must not exist"
    CAPTURE_START_NOTIFICATION="$(canonical_pending_path "$CAPTURE_START_NOTIFICATION")"
fi
readonly SUBJECT_IDENTITY RUN_METADATA WORKLOAD_METADATA WORKLOAD_EVENTS READY_RECEIPT \
    CAMPAIGN_SECRET_FILE WORKLOAD_METADATA_PARENT WORKLOAD_EVENTS_PARENT \
    WORKLOAD_METADATA_PARENT_IDENTITY WORKLOAD_EVENTS_PARENT_IDENTITY \
    CAPTURE_START_NOTIFICATION SUPPLEMENTAL_EVIDENCE SUPPLEMENTAL_EVIDENCE_PARENT \
    SUPPLEMENTAL_EVIDENCE_PARENT_IDENTITY SUPPLEMENTAL_WAS_PREEXISTING \
    PRECAPTURE_SUPPLEMENTAL_IDENTITY PRECAPTURE_SUPPLEMENTAL_SHA256
validate_subject_identity
validate_run_metadata
validate_campaign_secret
validate_ready_receipt
package_bundle_matches || die "frozen application bundle identity does not match"

identity_output="$(process_identity)"
TARGET_IDENTITY_TOKEN="$(awk -F '\t' '$1 == "identity_token" { print $2 }' <<< "$identity_output")"
[[ -n "$TARGET_IDENTITY_TOKEN" \
    && "$(awk -F '\t' '$1 == "live_code_identity_verified" { print $2 }' \
        <<< "$identity_output")" == true ]] || die "live target code identity is unavailable"
readonly TARGET_IDENTITY_TOKEN

readonly ARTIFACT_PREFIX="${SUBJECT}-${SCENARIO}"
readonly TRACE_PATH="$OUTPUT_DIRECTORY/${ARTIFACT_PREFIX}.trace"
readonly TOC_PATH="$OUTPUT_DIRECTORY/${ARTIFACT_PREFIX}-trace-toc.xml"
readonly PRIVATE_EXPORT_DIRECTORY="$OUTPUT_DIRECTORY/.private-${ARTIFACT_PREFIX}-exports"
readonly TIME_PROFILE_EXPORT_PATH="$PRIVATE_EXPORT_DIRECTORY/time-profile.xml"
readonly ALLOCATIONS_EXPORT_PATH="$PRIVATE_EXPORT_DIRECTORY/allocations.xml"
readonly HANGS_EXPORT_PATH="$PRIVATE_EXPORT_DIRECTORY/hangs.xml"
readonly TRACE_VERIFICATION_PATH="$PRIVATE_EXPORT_DIRECTORY/verification.tsv"
readonly RECORD_ELAPSED_PATH="$PRIVATE_EXPORT_DIRECTORY/record-elapsed-seconds.txt"
readonly METADATA_PATH="$OUTPUT_DIRECTORY/${ARTIFACT_PREFIX}-trace-metadata.tsv"
readonly METADATA_TEMP="${METADATA_PATH}.tmp.$$"

mkdir -m 700 -- "$OUTPUT_DIRECTORY"
mkdir -m 700 -- "$PRIVATE_EXPORT_DIRECTORY"
cleanup() { rm -f -- "$METADATA_TEMP"; }
trap cleanup EXIT
trap 'cleanup; trap - EXIT INT TERM; exit 130' INT TERM

record_status=0
export_status=not-run
table_export_status=1
verification_status=not-run
record_duration_seconds=$((DURATION_MILLISECONDS / 1000 + 3))
start_anchor="$(clock_anchor)" || die "continuous clock is unavailable"
IFS=$'\t' read -r wrapper_started_ns wrapper_started_epoch_ns start_anchor_width_ns \
    <<< "$start_anchor"
if ! is_uint "$wrapper_started_ns" || ! is_uint "$wrapper_started_epoch_ns" \
    || ! is_uint "$start_anchor_width_ns"; then
    die "continuous clock anchor is invalid"
fi
start_anchor_precise=false
(( start_anchor_width_ns <= 10000000 )) && start_anchor_precise=true
publish_capture_start_notification
set +e
"$COMMAND_RUNNER" "$RECORD_ELAPSED_PATH" "$XCRUN_COMMAND" xctrace record \
    --template "Time Profiler" --instrument "Allocations" --instrument "Hangs" \
    --attach "$PID" --time-limit "${record_duration_seconds}s" \
    --output "$TRACE_PATH" --no-prompt
record_status=$?
set -e
end_anchor="$(clock_anchor)" || die "continuous clock is unavailable"
IFS=$'\t' read -r wrapper_ended_ns wrapper_ended_epoch_ns end_anchor_width_ns \
    <<< "$end_anchor"
if ! is_uint "$wrapper_ended_ns" || ! is_uint "$wrapper_ended_epoch_ns" \
    || ! is_uint "$end_anchor_width_ns"; then
    die "continuous clock anchor is invalid"
fi
end_anchor_precise=false
(( end_anchor_width_ns <= 10000000 )) && end_anchor_precise=true
record_elapsed="$(tr -d '[:space:]' < "$RECORD_ELAPSED_PATH" 2>/dev/null || true)"
[[ "$record_elapsed" =~ ^[0-9]+([.][0-9]+)?$ ]] || record_elapsed=0

target_identity_verified=false
target_identity_matches && target_identity_verified=true
inputs_frozen=false
frozen_inputs_match && inputs_frozen=true

if (( record_status == 0 )) && trace_bundle_has_data; then
    set +e
    "$XCRUN_COMMAND" xctrace export --input "$TRACE_PATH" --toc --output "$TOC_PATH"
    export_status=$?
    set -e
fi
if [[ "$export_status" == 0 ]]; then
    set +e
    table_export_status=0
    export_trace_table '/trace-toc/run[@number="1"]/data/table[@schema="time-profile"]' "$TIME_PROFILE_EXPORT_PATH" || table_export_status=1
    export_trace_table '/trace-toc/run[@number="1"]/tracks/track[@name="Allocations"]/details/detail[@name="Allocations List"]' "$ALLOCATIONS_EXPORT_PATH" || table_export_status=1
    export_trace_table '/trace-toc/run[@number="1"]/data/table[@schema="potential-hangs"]' "$HANGS_EXPORT_PATH" || table_export_status=1
    set -e
fi
if (( table_export_status == 0 )); then
    set +e
    "$TRACE_VERIFIER" --toc "$TOC_PATH" --time-profile "$TIME_PROFILE_EXPORT_PATH" \
        --allocations "$ALLOCATIONS_EXPORT_PATH" --hangs "$HANGS_EXPORT_PATH" \
        --pid "$PID" --process-name "$BUNDLE_EXECUTABLE_NAME" \
        --requested-seconds "$((DURATION_MILLISECONDS / 1000))" \
        --command-elapsed-seconds "$record_elapsed" > "$TRACE_VERIFICATION_PATH"
    verification_status=$?
    set -e
fi

workload_valid=false
if validate_workload_metadata; then workload_valid=true; fi
supplemental_valid=false
if validate_supplemental_evidence; then supplemental_valid=true; fi
workload_metadata_hash="${WORKLOAD_METADATA_SHA256:-0000000000000000000000000000000000000000000000000000000000000000}"
actual_seconds="$(verification_metric actual_record_duration_seconds)"
actual_duration_ms="$(awk -v seconds="${actual_seconds:-0}" 'BEGIN { printf "%.0f", seconds * 1000 }')"
trace_started_epoch_ns="$(verification_metric trace_started_epoch_ns)"
trace_ended_epoch_ns="$(verification_metric trace_ended_epoch_ns)"
clock_mapping_verified=false
capture_started_ns="$wrapper_started_ns"
capture_ended_ns="$wrapper_ended_ns"
if [[ "$start_anchor_precise" == true && "$end_anchor_precise" == true ]] \
    && is_uint "$trace_started_epoch_ns" && is_uint "$trace_ended_epoch_ns"; then
    start_offset=$((wrapper_started_ns - wrapper_started_epoch_ns))
    end_offset=$((wrapper_ended_ns - wrapper_ended_epoch_ns))
    offset_delta=$((start_offset - end_offset))
    (( offset_delta >= 0 )) || offset_delta=$((-offset_delta))
    if (( offset_delta <= 50000000 )); then
        mapping_offset=$(((start_offset + end_offset) / 2))
        capture_started_ns=$((trace_started_epoch_ns + mapping_offset))
        capture_ended_ns=$((trace_ended_epoch_ns + mapping_offset))
        if (( capture_started_ns >= wrapper_started_ns - 50000000 \
            && capture_started_ns <= wrapper_ended_ns \
            && capture_ended_ns >= capture_started_ns \
            && capture_ended_ns <= wrapper_ended_ns + 50000000 )); then
            clock_mapping_verified=true
        fi
    fi
fi
trace_target_pid_verified=false
time_profiler_instrument=false
allocations_instrument=false
hangs_instrument=false
time_profiler_target_verified=false
allocations_target_verified=false
hangs_target_verified=false
time_profiler_rows="$(verification_metric time_profiler_rows)"; time_profiler_rows="${time_profiler_rows:-0}"
allocations_rows="$(verification_metric allocations_rows)"; allocations_rows="${allocations_rows:-0}"
hangs_rows="$(verification_metric hangs_rows)"; hangs_rows="${hangs_rows:-0}"
maximum_main_thread_hang_ms="$(verification_metric maximum_main_thread_hang_ms)"; maximum_main_thread_hang_ms="${maximum_main_thread_hang_ms:-0}"
if [[ "$verification_status" == 0 ]]; then
    trace_target_pid_verified=true
    time_profiler_instrument=true; allocations_instrument=true; hangs_instrument=true
    time_profiler_target_verified=true; allocations_target_verified=true; hangs_target_verified=true
fi

capture_status=INCOMPLETE
incomplete_reason=none
metadata_status=incomplete
if (( record_status != 0 )); then incomplete_reason=record-command-failed
elif [[ "$target_identity_verified" != true ]]; then incomplete_reason=target-identity-changed
elif [[ "$inputs_frozen" != true ]]; then incomplete_reason=frozen-input-changed
elif ! trace_bundle_has_data; then incomplete_reason=trace-bundle-is-empty
elif [[ "$export_status" != 0 ]]; then incomplete_reason=trace-toc-export-failed
elif (( table_export_status != 0 )); then incomplete_reason=trace-table-export-failed
elif [[ "$verification_status" != 0 ]]; then
    incomplete_reason="$(verification_metric reason)"
    incomplete_reason="${incomplete_reason:-trace-evidence-not-verifiable}"
elif [[ "$workload_valid" != true ]]; then incomplete_reason=workload-metadata-invalid
elif [[ "$supplemental_valid" != true ]]; then incomplete_reason=supplemental-evidence-invalid
elif [[ "$clock_mapping_verified" != true ]]; then incomplete_reason=trace-clock-correlation-invalid
elif (( capture_started_ns > MEASUREMENT_START_NS \
    || MEASUREMENT_START_NS - capture_started_ns > 2000000000 \
    || capture_ended_ns < WORKLOAD_ENDED_NS \
    || capture_ended_ns - WORKLOAD_ENDED_NS > 2000000000 )); then
    incomplete_reason=trace-workload-interval-mismatch
elif [[ "$TEST_OVERRIDES_ACTIVE" == true ]]; then incomplete_reason=test-overrides-active
else
    capture_status=CAPTURED
    metadata_status=complete
fi

write_metadata "$METADATA_TEMP"
chmod 0444 "$METADATA_TEMP"
ln "$METADATA_TEMP" "$METADATA_PATH" || die "metadata path was created concurrently"
rm -f -- "$METADATA_TEMP"
trap - EXIT INT TERM
if [[ "$capture_status" != CAPTURED ]]; then
    echo "error: trace capture is incomplete: $incomplete_reason" >&2
    exit 1
fi
printf 'Trace: %s\nMetadata: %s\nTable of contents: %s\n' \
    "$TRACE_PATH" "$METADATA_PATH" "$TOC_PATH"
