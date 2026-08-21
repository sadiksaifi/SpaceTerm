#!/bin/bash
# shellcheck disable=SC2016 # Awk programs intentionally use literal dollar fields.

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
umask 077

readonly TRUSTED_SYSTEM_PATH="/usr/bin:/bin:/usr/sbin:/sbin"
export PATH="$TRUSTED_SYSTEM_PATH"
readonly AWK_COMMAND=/usr/bin/awk
readonly BASENAME_COMMAND=/usr/bin/basename
readonly CHMOD_COMMAND=/bin/chmod
readonly CODESIGN_COMMAND=/usr/bin/codesign
readonly DIRNAME_COMMAND=/usr/bin/dirname
readonly FIND_COMMAND=/usr/bin/find
readonly GREP_COMMAND=/usr/bin/grep
readonly ID_COMMAND=/usr/bin/id
readonly LN_COMMAND=/bin/ln
readonly MKDIR_COMMAND=/bin/mkdir
readonly PLUTIL_COMMAND=/usr/bin/plutil
readonly PYTHON_COMMAND=/usr/bin/python3
readonly REALPATH_COMMAND=/bin/realpath
readonly RM_COMMAND=/bin/rm
readonly SLEEP_COMMAND=/bin/sleep
readonly STAT_COMMAND=/usr/bin/stat
readonly TR_COMMAND=/usr/bin/tr
readonly WC_COMMAND=/usr/bin/wc
readonly XCRUN_COMMAND="${SPACETERM_XCRUN:-/usr/bin/xcrun}"
readonly SHASUM_COMMAND="${SPACETERM_SHASUM:-/usr/bin/shasum}"
readonly CONTINUOUS_CLOCK_COMMAND="${SPACETERM_CONTINUOUS_CLOCK:-}"
readonly RUN_METADATA_WAIT_TENTHS="${SPACETERM_TEST_RUN_METADATA_WAIT_TENTHS:-1200}"
PROCESS_INSPECTOR="${SPACETERM_PROCESS_INSPECTOR:-}"
TRACE_VERIFIER="${SPACETERM_TRACE_VERIFIER:-}"
TEST_OVERRIDES_ACTIVE=false
[[ -z "${SPACETERM_XCRUN:-}${SPACETERM_SHASUM:-}${SPACETERM_CONTINUOUS_CLOCK:-}${SPACETERM_PROCESS_INSPECTOR:-}${SPACETERM_TRACE_VERIFIER:-}${SPACETERM_TEST_RUN_METADATA_WAIT_TENTHS:-}" ]] \
    || TEST_OVERRIDES_ACTIVE=true
readonly TEST_OVERRIDES_ACTIVE
EVIDENCE_MODE=production
[[ "${SPACETERM_PERFORMANCE_TEST_MODE:-0}" != 1 ]] || EVIDENCE_MODE=test-only
readonly EVIDENCE_MODE

SUBJECT_IDENTITY=""
RUN_INTENT=""
RUN_METADATA=""
WORKLOAD_METADATA=""
WORKLOAD_EVENTS=""
READY_RECEIPT=""
SUPPLEMENTAL_EVIDENCE=""
CAPTURE_MODE=workload-v3
SCENARIO_PLAN=""
RENDER_INTENT=""
RENDER_EVIDENCE=""
CAMPAIGN_MANIFEST=""
RENDER_TOOL_BUNDLE_MANIFEST=""
EXPECTED_SOURCE_COMMIT=""
TRUSTED_SOURCE_REPOSITORY=""
TRACE_ANCHOR_RECEIPT=""
CAMPAIGN_SECRET_FILE=""
CAMPAIGN_ID=""
SESSION_ID=""
NONCE=""
SCENARIO=""
DURATION_MILLISECONDS=""
WARMUP_MILLISECONDS=""
OUTPUT_DIRECTORY=""
CAPTURE_START_NOTIFICATION=""
PROVISIONAL_RECEIPT=""

RENDER_EVIDENCE_TIMEOUT_SECONDS=600
RENDER_SECRET_FINGERPRINT=""
RENDER_HMAC_KEY_IDENTIFIER=""
RENDER_HMAC_DIGEST=""

usage() {
    /bin/cat <<EOF
Usage: $("$BASENAME_COMMAND" -- "$0") --subject-identity FILE --run-intent FILE \\
  --run-metadata PENDING_FILE --provisional-receipt PENDING_FILE \\
  [--evidence-mode workload-v3 --workload-metadata FILE \\
   --workload-events FILE --workload-ready-receipt FILE [--supplemental-evidence FILE]] \\
  [--evidence-mode render-profile-v1 --scenario-plan FILE \\
   --render-intent FILE --render-evidence PENDING_FILE \\
   --campaign-manifest FILE --trace-anchor-receipt PENDING_FILE \\
   --render-tool-bundle-manifest FILE --expected-source-commit SHA1 \\
   --trusted-source-repository DIRECTORY \\
   [--workload-metadata FILE --workload-events FILE \\
    --workload-ready-receipt FILE]] \\
  --campaign-secret-file FILE --campaign-id LABEL --session-id LABEL \\
  --nonce SHA256 --scenario LABEL --warmup-ms N --duration-ms N \\
  --output-directory NEW_PATH [--capture-start-notification NEW_FILE] \\
  [--render-evidence-timeout-seconds N]

Attach Time Profiler, Allocations, and Hangs to the exact process frozen in a
subject identity. The finalized privacy-safe v3 metadata binds the live guest
code, immutable subject/run/workload evidence, continuous capture interval,
target-scoped trace tables, and measured duration. CAPTURED is evidence state,
not a performance verdict.

The finalized run metadata must be absent at launch. Its canonical private parent
and exact path are bound to an immutable run intent before capture. Finalization
waits up to 120 seconds after xctrace for the atomic immutable metadata. Workload
events and metadata may also be created during capture. An optional
capture-start notification is published atomically immediately before xctrace is
launched; the finalized trace interval remains the authoritative coverage proof.
Render-profile mode instead authenticates the immutable pre-capture intent and
the pending post-capture final evidence, waiting up to 600 seconds by default for
large video hashing/finalization (configurable from 1 to 3600 seconds). Only
perf-render-sustained-output also requires the authenticated workload-v3 producer
evidence. The other render scenarios reject normal workload evidence.

Options:
  --doctor  Verify Xcode Instruments and metadata prerequisites.
  -h, --help
EOF
}

die() { echo "error: $*" >&2; exit 1; }
[[ "$TEST_OVERRIDES_ACTIVE" == false || "$EVIDENCE_MODE" == test-only ]] \
    || die "trace test overrides require SPACETERM_PERFORMANCE_TEST_MODE=1"
require_command() { command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"; }
is_positive_integer() { [[ "$1" =~ ^[1-9][0-9]*$ ]]; }
is_uint() { [[ "$1" =~ ^[0-9]+$ ]]; }
is_safe_label() { [[ "$1" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]]; }
is_sha256() { [[ "$1" =~ ^[0-9a-f]{64}$ ]]; }

doctor() {
    local instruments
    for command in "$XCRUN_COMMAND" "$AWK_COMMAND" "$BASENAME_COMMAND" \
        "$CHMOD_COMMAND" "$CODESIGN_COMMAND" "$FIND_COMMAND" "$GREP_COMMAND" \
        "$ID_COMMAND" "$LN_COMMAND" "$MKDIR_COMMAND" "$PLUTIL_COMMAND" \
        "$PYTHON_COMMAND" "$REALPATH_COMMAND" "$RM_COMMAND" "$SLEEP_COMMAND" \
        "$STAT_COMMAND" "$SHASUM_COMMAND" /bin/cp /bin/date /bin/mv \
        /usr/bin/xmllint; do
        require_command "$command"
    done
    instruments="$("$XCRUN_COMMAND" xctrace list instruments)"
    for instrument in "Time Profiler" "Allocations" "Hangs"; do
        "$GREP_COMMAND" -Fxq "$instrument" <<< "$instruments" \
            || die "required xctrace instrument is unavailable: $instrument"
    done
    "$XCRUN_COMMAND" xcodebuild -version >/dev/null
    echo "release performance trace prerequisites are available"
}

kv() {
    local file="$1" key="$2"
    "$AWK_COMMAND" -F '\t' -v wanted="$key" '
        NF != 2 { bad = 1 }
        $1 == wanted { count += 1; value = $2 }
        END { if (!bad && count == 1) print value }
    ' "$file"
}

sha256() { "$SHASUM_COMMAND" -a 256 "$1" | "$AWK_COMMAND" '{ print $1 }'; }

clock_anchor() {
    if [[ -n "$CONTINUOUS_CLOCK_COMMAND" ]]; then
        "$CONTINUOUS_CLOCK_COMMAND"
    else
        "$PYTHON_COMMAND" - <<'PY'
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
    [[ -f "$1" && ! -L "$1" && ! -w "$1" \
        && "$("$STAT_COMMAND" -f '%l' "$1")" == 1 ]]
}

trusted_tool_file() {
    local path="$1" mode owner links
    [[ "$path" == /* && -f "$path" && ! -L "$path" && -x "$path" ]] || return 1
    mode="$("$STAT_COMMAND" -f '%Lp' "$path")"
    owner="$("$STAT_COMMAND" -f '%u' "$path")"
    links="$("$STAT_COMMAND" -f '%l' "$path")"
    if [[ "$path" == /usr/bin/* || "$path" == /bin/* \
        || "$path" == /usr/sbin/* || "$path" == /sbin/* ]]; then
        [[ "$owner" == 0 && "$mode" =~ ^[0-7]{3,4}$ ]] || return 1
        (( (8#$mode & 022) == 0 ))
    else
        [[ "$links" == 1 && "$mode" =~ ^[0-7]{3,4}$ ]] || return 1
        (( (8#$mode & 0222) == 0 ))
    fi
}

render_recorder_tool_identity_snapshot() {
    local tool
    for tool in "$HMAC_HELPER" "$TRACE_RECEIPT_HELPER" "$PROCESS_INSPECTOR" \
        "${BASH_SOURCE[0]}" \
        "$TRACE_VERIFIER" "$COMMAND_RUNNER" "$XCRUN_COMMAND" "$SHASUM_COMMAND" \
        "$AWK_COMMAND" "$BASENAME_COMMAND" "$CHMOD_COMMAND" "$CODESIGN_COMMAND" \
        "$DIRNAME_COMMAND" "$FIND_COMMAND" "$GREP_COMMAND" "$ID_COMMAND" \
        "$LN_COMMAND" "$MKDIR_COMMAND" "$PLUTIL_COMMAND" "$PYTHON_COMMAND" \
        "$REALPATH_COMMAND" "$RM_COMMAND" "$SLEEP_COMMAND" "$STAT_COMMAND" \
        "$TR_COMMAND" "$WC_COMMAND"; do
        trusted_tool_file "$tool" || return 1
        printf '%s\t%s\t%s\n' "$tool" \
            "$("$STAT_COMMAND" -f '%d:%i:%z:%m:%c' "$tool")" "$(sha256 "$tool")"
    done
}

verify_recorder_tool_bundle() {
    "$PYTHON_COMMAND" - "$RENDER_TOOL_BUNDLE_MANIFEST" "$EXPECTED_SOURCE_COMMIT" \
        "$TRUSTED_SOURCE_REPOSITORY" "${BASH_SOURCE[0]}" "$HMAC_HELPER" \
        "$TRACE_RECEIPT_HELPER" "$PROCESS_INSPECTOR" "$TRACE_VERIFIER" \
        "$COMMAND_RUNNER" <<'PY'
import hashlib, pathlib, stat, subprocess, sys
manifest_raw, commit, repository_raw, recorder_raw, hmac_raw, receipt_raw, inspector_raw, verifier_raw, runner_raw = sys.argv[1:]
names = "record_release_performance_trace freeze_render_profile_intent finalize_render_profile_evidence render_profile_hmac render_trace_receipt analyze_release_render_profile_case archive_render_trace verify_render_action_video verify_render_trace_archive verify_release_performance_trace inspect_release_performance_process run_release_performance_command freeze_render_profile_tool_bundle".split()
relatives = "scripts/record-release-performance-trace.sh scripts/acceptance/freeze-render-profile-intent.sh scripts/acceptance/finalize-render-profile-evidence.sh scripts/acceptance/render-profile-hmac.py scripts/acceptance/render-trace-receipt.py scripts/acceptance/analyze-release-render-profile-case.sh scripts/acceptance/archive-render-trace.py scripts/acceptance/verify-render-action-video.py scripts/acceptance/verify-render-trace-archive.py scripts/verify-release-performance-trace.py scripts/inspect-release-performance-process.py scripts/run-release-performance-command.py scripts/acceptance/freeze-render-profile-tool-bundle.sh".split()
keys = ["format_version", "schema", "source_commit", "tool_count"]
for name in names: keys += [f"{name}_source_path", f"{name}_source_sha256", f"{name}_bundle_path", f"{name}_bundle_sha256"]
def frozen(raw, executable=False):
    path = pathlib.Path(raw); before = path.lstat()
    if (not path.is_absolute() or path.is_symlink() or path.resolve(strict=True) != path or not stat.S_ISREG(before.st_mode) or before.st_nlink != 1 or before.st_mode & 0o222 or executable and not before.st_mode & 0o111): raise SystemExit(1)
    body = path.read_bytes(); after = path.lstat()
    if (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns) != (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns): raise SystemExit(1)
    return path, body
_, payload = frozen(manifest_raw); lines = payload.splitlines()
if not payload.endswith(b"\n") or len(lines) != len(keys): raise SystemExit(1)
values = {}
for key, line in zip(keys, lines):
    try: actual, value = line.split(b"\t", 1); actual = actual.decode("ascii"); value = value.decode()
    except (ValueError, UnicodeDecodeError): raise SystemExit(1)
    if actual != key or not value or "\t" in value or "\r" in value: raise SystemExit(1)
    values[key] = value
repository = pathlib.Path(repository_raw)
if (not repository.is_absolute() or repository.is_symlink() or repository.resolve(strict=True) != repository or values["format_version"] != "1" or values["schema"] != "spaceterm.render-profile-tool-bundle/v1" or values["source_commit"] != commit or values["tool_count"] != "13"): raise SystemExit(1)
invoked = {"record_release_performance_trace": recorder_raw, "render_profile_hmac": hmac_raw, "render_trace_receipt": receipt_raw, "inspect_release_performance_process": inspector_raw, "verify_release_performance_trace": verifier_raw, "run_release_performance_command": runner_raw}
for name, relative in zip(names, relatives):
    bundle, body = frozen(values[f"{name}_bundle_path"], executable=True)
    blob = subprocess.run(["/usr/bin/git", "--no-replace-objects", "-C", str(repository), "show", f"{commit}:{relative}"], capture_output=True, env={"PATH":"/usr/bin:/bin", "HOME":"/var/empty", "GIT_NO_REPLACE_OBJECTS":"1", "LC_ALL":"C"}); digest = hashlib.sha256(blob.stdout).hexdigest()
    if (blob.returncode or pathlib.Path(values[f"{name}_source_path"]) != repository / relative or values[f"{name}_source_sha256"] != digest or values[f"{name}_bundle_sha256"] != digest or hashlib.sha256(body).hexdigest() != digest or name in invoked and pathlib.Path(invoked[name]).resolve(strict=True) != bundle): raise SystemExit(1)
PY
}

canonical_pending_path() {
    local path="$1" leaf parent mode
    leaf="$("$BASENAME_COMMAND" -- "$path")"
    [[ -n "$leaf" && "$leaf" != . && "$leaf" != .. \
        && "$leaf" != *$'\n'* && "$leaf" != *$'\t'* ]] \
        || die "pending evidence path is invalid"
    parent="$("$REALPATH_COMMAND" "$("$DIRNAME_COMMAND" -- "$path")")" \
        || die "pending evidence parent is unavailable"
    [[ -d "$parent" ]] || die "pending evidence parent is unavailable"
    mode="$("$STAT_COMMAND" -f '%Lp' "$parent")"
    [[ "$mode" =~ ^[0-7]{3,4}$ ]] || die "pending evidence parent mode is invalid"
    (( (8#$mode & 077) == 0 )) || die "pending evidence parent must be private"
    printf '%s/%s\n' "$parent" "$leaf"
}

validate_campaign_secret() {
    local mode owner
    [[ -f "$CAMPAIGN_SECRET_FILE" && ! -L "$CAMPAIGN_SECRET_FILE" \
        && "$("$STAT_COMMAND" -f '%l' "$CAMPAIGN_SECRET_FILE")" == 1 ]] \
        || die "campaign secret must be a non-symlink singleton regular file"
    mode="$("$STAT_COMMAND" -f '%Lp' "$CAMPAIGN_SECRET_FILE")"
    owner="$("$STAT_COMMAND" -f '%u' "$CAMPAIGN_SECRET_FILE")"
    [[ "$mode" =~ ^[0-7]{3,4}$ && "$owner" == "$("$ID_COMMAND" -u)" ]] \
        || die "campaign secret ownership or mode is invalid"
    (( (8#$mode & 077) == 0 && (8#$mode & 0200) == 0 )) \
        || die "campaign secret must be private and immutable"
    CAMPAIGN_SECRET_IDENTITY="$("$STAT_COMMAND" -f '%d:%i' "$CAMPAIGN_SECRET_FILE")"
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
        (( attempt == 51 )) || "$SLEEP_COMMAND" 0.1
    done
    [[ "$("$STAT_COMMAND" -f '%d:%i' "$SUPPLEMENTAL_EVIDENCE_PARENT")" \
            == "$SUPPLEMENTAL_EVIDENCE_PARENT_IDENTITY" ]] || return 1
    file_is_immutable_regular "$SUPPLEMENTAL_EVIDENCE" || return 1
    if [[ "$SUPPLEMENTAL_WAS_PREEXISTING" == true ]]; then
        [[ "$("$STAT_COMMAND" -f '%d:%i' "$SUPPLEMENTAL_EVIDENCE")" \
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
    "$PYTHON_COMMAND" - "$READY_RECEIPT" "$WORKLOAD_EVENTS" "$CAMPAIGN_SECRET_FILE" <<'PY' \
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
    READY_RECEIPT_IDENTITY="$("$STAT_COMMAND" -f '%d:%i' "$READY_RECEIPT")"
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
    "$AWK_COMMAND" -F '\t' -v allowed="$allowed" -v expected="$expected_count" '
        BEGIN {
            count = split(allowed, keys, " ")
            for (i = 1; i <= count; i += 1) valid[keys[i]] = 1
        }
        NF != 2 || $1 == "" || $2 == "" || !($1 in valid) \
            || $1 != keys[NR] || seen[$1]++ { bad = 1 }
        END { exit bad || NR != expected }
    ' "$file"
}

render_hmac() {
    local domain="$1" file="$2" last_key="$3"
    local output fingerprint identifier digest
    output="$(/usr/bin/python3 "$HMAC_HELPER" --secret "$CAMPAIGN_SECRET_FILE" \
        --domain "$domain" --artifact "$file" --last-key "$last_key")" \
        || return 1
    [[ "$(printf '%s\n' "$output" | "$WC_COMMAND" -l | "$TR_COMMAND" -d ' ')" == 3 ]] || return 1
    fingerprint="$(printf '%s\n' "$output" | "$AWK_COMMAND" -F '\t' \
        '$1 == "secret_fingerprint" { count += 1; value = $2 } \
        END { if (count == 1) print value }')"
    identifier="$(printf '%s\n' "$output" | "$AWK_COMMAND" -F '\t' \
        '$1 == "key_identifier_sha256" { count += 1; value = $2 } \
        END { if (count == 1) print value }')"
    digest="$(printf '%s\n' "$output" | "$AWK_COMMAND" -F '\t' \
        '$1 == "hmac_sha256" { count += 1; value = $2 } \
        END { if (count == 1) print value }')"
    [[ -n "$fingerprint" && "$identifier" =~ ^[0-9a-f]{64}$ \
        && "$digest" =~ ^[0-9a-f]{64}$ ]] || return 1
    if [[ -n "$RENDER_SECRET_FINGERPRINT" ]]; then
        [[ "$fingerprint" == "$RENDER_SECRET_FINGERPRINT" \
            && "$identifier" == "$RENDER_HMAC_KEY_IDENTIFIER" ]] || return 1
    else
        RENDER_SECRET_FINGERPRINT="$fingerprint"
        RENDER_HMAC_KEY_IDENTIFIER="$identifier"
    fi
    RENDER_HMAC_DIGEST="$digest"
}

validate_render_intent() {
    readonly RENDER_INTENT_KEYS="format_version canonicalization auth_domain scenario subject campaign_id session_id nonce plan_sha256 plan_metadata_sha256 pair_metadata_sha256 run_intent_sha256 command_sha256 environment_sha256 font_sha256 initial_grid_sha256 subject_identity_sha256 subject_process_pid subject_process_start_identity expected_driver_events_path expected_driver_parent_device expected_driver_parent_inode action_video_path action_video_parent_device action_video_parent_inode final_metadata_path final_metadata_parent_device final_metadata_parent_inode warmup_ms measured_duration_ms required_action_count action_interval_ms hmac_key_identifier_sha256 intent_hmac_sha256"
    file_is_immutable_regular "$SCENARIO_PLAN" \
        || die "render scenario plan must be immutable singleton evidence"
    file_is_immutable_regular "$RENDER_INTENT" \
        || die "render intent must be immutable singleton evidence"
    validate_exact_schema "$RENDER_INTENT" "$RENDER_INTENT_KEYS" 35 "render intent"
    render_hmac SPACETERM_RENDER_PROFILE_INTENT_V1 \
        "$RENDER_INTENT" intent_hmac_sha256 \
        || die "render intent authentication is invalid"
    expected_intent_hmac="$RENDER_HMAC_DIGEST"
    [[ "$(kv "$RENDER_INTENT" format_version)" == 1 \
        && "$(kv "$RENDER_INTENT" canonicalization)" \
            == utf8-lf-tab-kv-fixed-order-domain-nul-v1 \
        && "$(kv "$RENDER_INTENT" auth_domain)" == SPACETERM_RENDER_PROFILE_INTENT_V1 \
        && "$(kv "$RENDER_INTENT" scenario)" == "$SCENARIO" \
        && "$(kv "$RENDER_INTENT" subject)" == "$SUBJECT" \
        && "$(kv "$RENDER_INTENT" campaign_id)" == "$CAMPAIGN_ID" \
        && "$(kv "$RENDER_INTENT" session_id)" == "$SESSION_ID" \
        && "$(kv "$RENDER_INTENT" nonce)" == "$NONCE" \
        && "$(kv "$RENDER_INTENT" plan_sha256)" == "$(sha256 "$SCENARIO_PLAN")" \
        && "$(kv "$RENDER_INTENT" run_intent_sha256)" == "$RUN_INTENT_SHA256" \
        && "$(kv "$RENDER_INTENT" command_sha256)" == "$(kv "$RUN_INTENT" command_sha256)" \
        && "$(kv "$RENDER_INTENT" environment_sha256)" \
            == "$(kv "$RUN_INTENT" environment_sha256)" \
        && "$(kv "$RENDER_INTENT" font_sha256)" == "$(kv "$RUN_INTENT" font_sha256)" \
        && "$(kv "$RENDER_INTENT" initial_grid_sha256)" \
            == "$(kv "$RUN_INTENT" initial_grid_sha256)" \
        && "$(kv "$RENDER_INTENT" subject_identity_sha256)" == "$SUBJECT_IDENTITY_SHA256" \
        && "$(kv "$RENDER_INTENT" subject_process_pid)" == "$PID" \
        && "$(kv "$RENDER_INTENT" subject_process_start_identity)" \
            == "$PROCESS_START_IDENTITY" \
        && "$(kv "$RENDER_INTENT" warmup_ms)" == "$WARMUP_MILLISECONDS" \
        && "$(kv "$RENDER_INTENT" measured_duration_ms)" == "$DURATION_MILLISECONDS" \
        && "$(kv "$RENDER_INTENT" final_metadata_path)" == "$RENDER_EVIDENCE" \
        && "$(kv "$RENDER_INTENT" final_metadata_parent_device):$(kv "$RENDER_INTENT" final_metadata_parent_inode)" \
            == "$RENDER_EVIDENCE_PARENT_IDENTITY" \
        && "$(kv "$RENDER_INTENT" hmac_key_identifier_sha256)" \
            == "$RENDER_HMAC_KEY_IDENTIFIER" \
        && "$(kv "$RENDER_INTENT" intent_hmac_sha256)" == "$expected_intent_hmac" ]] \
        || die "render intent binding is invalid"
    RENDER_INTENT_SHA256="$(sha256 "$RENDER_INTENT")"
    RENDER_INTENT_IDENTITY="$("$STAT_COMMAND" -f '%d:%i' "$RENDER_INTENT")"
    SCENARIO_PLAN_SHA256="$(sha256 "$SCENARIO_PLAN")"
    SCENARIO_PLAN_IDENTITY="$("$STAT_COMMAND" -f '%d:%i' "$SCENARIO_PLAN")"
    readonly RENDER_INTENT_SHA256 RENDER_INTENT_IDENTITY SCENARIO_PLAN_SHA256 \
        SCENARIO_PLAN_IDENTITY
}

validate_render_evidence() {
    readonly RENDER_EVIDENCE_KEYS="format_version canonicalization auth_domain intent_sha256 scenario subject campaign_id session_id nonce subject_identity_sha256 subject_process_pid subject_process_start_identity driver_events_path driver_events_device driver_events_inode driver_events_sha256 action_video_path action_video_device action_video_inode action_video_sha256 render_workload_metadata_sha256 required_action_count completed_action_count action_interval_ms started_continuous_ns ended_continuous_ns measured_span_ns result hmac_key_identifier_sha256 evidence_hmac_sha256"
    local attempt expected_hmac
    local maximum_attempts=$((RENDER_EVIDENCE_TIMEOUT_SECONDS * 10 + 1))
    for ((attempt = 1; attempt <= maximum_attempts; attempt += 1)); do
        [[ -e "$RENDER_EVIDENCE" ]] && break
        (( attempt == maximum_attempts )) || "$SLEEP_COMMAND" 0.1
    done
    [[ "$("$STAT_COMMAND" -f '%d:%i' "$RENDER_EVIDENCE_PARENT")" \
        == "$RENDER_EVIDENCE_PARENT_IDENTITY" ]] || return 1
    file_is_immutable_regular "$RENDER_EVIDENCE" || return 1
    schema_is_exact "$RENDER_EVIDENCE" "$RENDER_EVIDENCE_KEYS" 31 || return 1
    render_hmac SPACETERM_RENDER_PROFILE_EVIDENCE_V1 \
        "$RENDER_EVIDENCE" evidence_hmac_sha256 || return 1
    expected_hmac="$RENDER_HMAC_DIGEST"
    [[ "$(kv "$RENDER_EVIDENCE" format_version)" == 1 \
        && "$(kv "$RENDER_EVIDENCE" canonicalization)" \
            == utf8-lf-tab-kv-fixed-order-domain-nul-v1 \
        && "$(kv "$RENDER_EVIDENCE" auth_domain)" \
            == SPACETERM_RENDER_PROFILE_EVIDENCE_V1 \
        && "$(kv "$RENDER_EVIDENCE" intent_sha256)" == "$RENDER_INTENT_SHA256" \
        && "$(kv "$RENDER_EVIDENCE" scenario)" == "$SCENARIO" \
        && "$(kv "$RENDER_EVIDENCE" subject)" == "$SUBJECT" \
        && "$(kv "$RENDER_EVIDENCE" campaign_id)" == "$CAMPAIGN_ID" \
        && "$(kv "$RENDER_EVIDENCE" session_id)" == "$SESSION_ID" \
        && "$(kv "$RENDER_EVIDENCE" nonce)" == "$NONCE" \
        && "$(kv "$RENDER_EVIDENCE" subject_identity_sha256)" == "$SUBJECT_IDENTITY_SHA256" \
        && "$(kv "$RENDER_EVIDENCE" subject_process_pid)" == "$PID" \
        && "$(kv "$RENDER_EVIDENCE" subject_process_start_identity)" \
            == "$PROCESS_START_IDENTITY" \
        && "$(kv "$RENDER_EVIDENCE" driver_events_path)" \
            == "$(kv "$RENDER_INTENT" expected_driver_events_path)" \
        && "$(kv "$RENDER_EVIDENCE" action_video_path)" \
            == "$(kv "$RENDER_INTENT" action_video_path)" \
        && "$(kv "$RENDER_EVIDENCE" required_action_count)" \
            == "$(kv "$RENDER_INTENT" required_action_count)" \
        && "$(kv "$RENDER_EVIDENCE" completed_action_count)" \
            == "$(kv "$RENDER_INTENT" required_action_count)" \
        && "$(kv "$RENDER_EVIDENCE" action_interval_ms)" \
            == "$(kv "$RENDER_INTENT" action_interval_ms)" \
        && "$(kv "$RENDER_EVIDENCE" result)" == verified \
        && "$(kv "$RENDER_EVIDENCE" hmac_key_identifier_sha256)" \
            == "$(kv "$RENDER_INTENT" hmac_key_identifier_sha256)" \
        && "$(kv "$RENDER_EVIDENCE" evidence_hmac_sha256)" == "$expected_hmac" ]] \
        || return 1
    local referenced path device inode digest
    for referenced in driver_events action_video; do
        path="$(kv "$RENDER_EVIDENCE" "${referenced}_path")"
        device="$(kv "$RENDER_EVIDENCE" "${referenced}_device")"
        inode="$(kv "$RENDER_EVIDENCE" "${referenced}_inode")"
        digest="$(kv "$RENDER_EVIDENCE" "${referenced}_sha256")"
        [[ "$path" == /* && -f "$path" && ! -L "$path" && ! -w "$path" \
            && "$("$STAT_COMMAND" -f '%l' "$path")" == 1 \
            && "$("$STAT_COMMAND" -f '%d' "$path")" == "$device" \
            && "$("$STAT_COMMAND" -f '%i' "$path")" == "$inode" \
            && "$(sha256 "$path")" == "$digest" ]] || return 1
    done
    local started ended span
    started="$(kv "$RENDER_EVIDENCE" started_continuous_ns)"
    ended="$(kv "$RENDER_EVIDENCE" ended_continuous_ns)"
    span="$(kv "$RENDER_EVIDENCE" measured_span_ns)"
    is_uint "$started" && is_uint "$ended" && is_uint "$span" \
        && (( ended > started && span == ended - started \
            && span >= DURATION_MILLISECONDS * 1000000 \
            && span <= (DURATION_MILLISECONDS + 2000) * 1000000 )) || return 1
    RENDER_MEASUREMENT_START_NS="$started"
    RENDER_MEASUREMENT_END_NS="$ended"
    SUPPLEMENTAL_EVIDENCE_SHA256="$(sha256 "$RENDER_EVIDENCE")"
    RENDER_EVIDENCE_IDENTITY="$("$STAT_COMMAND" -f '%d:%i' "$RENDER_EVIDENCE")"
    readonly RENDER_MEASUREMENT_START_NS RENDER_MEASUREMENT_END_NS \
        SUPPLEMENTAL_EVIDENCE_SHA256 RENDER_EVIDENCE_IDENTITY
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
    BUNDLE_EXECUTABLE_NAME="$("$BASENAME_COMMAND" -- "$PACKAGE_EXECUTABLE")"
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
    app_canonical="$("$REALPATH_COMMAND" "$APP_BUNDLE")"
    executable_canonical="$("$REALPATH_COMMAND" "$PACKAGE_EXECUTABLE")"
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
    current_version="$("$PLUTIL_COMMAND" -extract CFBundleShortVersionString raw -o - \
        "$APP_INFO_PLIST" 2>/dev/null || true)+$("$PLUTIL_COMMAND" -extract CFBundleVersion \
        raw -o - "$APP_INFO_PLIST" 2>/dev/null || true)"
    [[ "$("$PLUTIL_COMMAND" -extract CFBundleIdentifier raw -o - "$APP_INFO_PLIST" \
            2>/dev/null || true)" == "$BUNDLE_IDENTIFIER" \
        && "$current_version" == "$BUNDLE_VERSION" \
        && "$("$PLUTIL_COMMAND" -extract CFBundleExecutable raw -o - "$APP_INFO_PLIST" \
            2>/dev/null || true)" == "$BUNDLE_EXECUTABLE_NAME" \
        && "$("$REALPATH_COMMAND" "$APP_BUNDLE/Contents/MacOS/$BUNDLE_EXECUTABLE_NAME" \
            2>/dev/null || true)" == "$PACKAGE_EXECUTABLE" ]] \
        && "$CODESIGN_COMMAND" --verify --strict "$APP_BUNDLE" >/dev/null 2>&1
}

validate_run_intent() {
    readonly RUN_INTENT_KEYS="format_version subject subject_identity_sha256 scenario scenario_plan_sha256 workload_sha256 command_sha256 environment_sha256 font_sha256 initial_grid_sha256 measured_duration_ms process_pid process_start_identity campaign_id session_id nonce native_provisional_observation_sha256 evidence_mode status"
    file_is_immutable_regular "$RUN_INTENT" \
        || die "run intent must be immutable singleton evidence"
    validate_exact_schema "$RUN_INTENT" "$RUN_INTENT_KEYS" 19 "run intent"
    [[ "$(kv "$RUN_INTENT" format_version)" == 1 \
        && "$(kv "$RUN_INTENT" status)" == prepared \
        && "$(kv "$RUN_INTENT" subject)" == "$SUBJECT" \
        && "$(kv "$RUN_INTENT" subject_identity_sha256)" == "$SUBJECT_IDENTITY_SHA256" \
        && "$(kv "$RUN_INTENT" scenario)" == "$SCENARIO" \
        && "$(kv "$RUN_INTENT" measured_duration_ms)" == "$DURATION_MILLISECONDS" \
        && "$(kv "$RUN_INTENT" process_pid)" == "$PID" \
        && "$(kv "$RUN_INTENT" process_start_identity)" == "$PROCESS_START_IDENTITY" \
        && "$(kv "$RUN_INTENT" campaign_id)" == "$CAMPAIGN_ID" \
        && "$(kv "$RUN_INTENT" session_id)" == "$SESSION_ID" \
        && "$(kv "$RUN_INTENT" nonce)" == "$NONCE" \
        && "$(kv "$RUN_INTENT" evidence_mode)" == "$EVIDENCE_MODE" ]] \
        || die "run intent does not bind the requested frozen campaign run"
    RUN_WORKLOAD_SHA256="$(kv "$RUN_INTENT" workload_sha256)"
    is_sha256 "$RUN_WORKLOAD_SHA256" || die "run workload hash is invalid"
    if [[ "$SUBJECT" == spaceterm ]]; then
        is_sha256 "$(kv "$RUN_INTENT" native_provisional_observation_sha256)" \
            || die "SpaceTerm run intent lacks provisional native evidence"
    else
        [[ "$(kv "$RUN_INTENT" native_provisional_observation_sha256)" \
                == not-applicable ]] \
            || die "Ghostty run intent contains SpaceTerm native evidence"
    fi
    RUN_INTENT_SHA256="$(sha256 "$RUN_INTENT")"
    readonly RUN_WORKLOAD_SHA256 RUN_INTENT_SHA256
}

wait_for_run_metadata() {
    local attempt
    for ((attempt = 0; attempt <= RUN_METADATA_WAIT_TENTHS; attempt += 1)); do
        [[ -e "$RUN_METADATA" ]] && return 0
        (( attempt == RUN_METADATA_WAIT_TENTHS )) || sleep 0.1
    done
    return 1
}

validate_run_metadata() {
    local key
    readonly RUN_KEYS="format_version subject subject_identity_sha256 scenario scenario_plan_sha256 workload_sha256 command_sha256 environment_sha256 font_sha256 initial_grid_sha256 measured_duration_ms process_pid process_start_identity run_intent_sha256 native_observation_sha256 native_runtime_metadata_sha256 native_failure_actions_sha256 native_failure_action_enabled native_failure_request_count native_failure_result_count native_failure_resource_staged_count native_failure_resource_staged_bytes native_failure_resource_rolled_back_count native_failure_resource_rolled_back_bytes trace_provisional_receipt_sha256 performance_tail_receipt_sha256 performance_quit_receipt_sha256 subject_exit_receipt_sha256 lifecycle_ready_receipt_sha256 lifecycle_registration_receipt_sha256 lifecycle_helper_sha256 terminator_source_sha256 terminator_binary_sha256 evidence_mode status"
    wait_for_run_metadata || return 1
    [[ "$(stat -f '%d:%i' "$RUN_METADATA_PARENT")" == "$RUN_METADATA_PARENT_IDENTITY" ]] \
        || return 1
    file_is_immutable_regular "$RUN_METADATA" || return 1
    schema_is_exact "$RUN_METADATA" "$RUN_KEYS" 35 || return 1
    [[ "$(kv "$RUN_METADATA" format_version)" == 4 \
        && "$(kv "$RUN_METADATA" evidence_mode)" == "$EVIDENCE_MODE" \
        && "$(kv "$RUN_METADATA" status)" == "$([[ "$EVIDENCE_MODE" == production ]] && printf complete || printf test-only)" \
        && "$(kv "$RUN_METADATA" run_intent_sha256)" == "$RUN_INTENT_SHA256" \
        && "$(kv "$RUN_METADATA" trace_provisional_receipt_sha256)" \
            == "$PROVISIONAL_RECEIPT_SHA256" ]] \
        || return 1
    for key in performance_tail_receipt_sha256 performance_quit_receipt_sha256 \
        subject_exit_receipt_sha256 lifecycle_ready_receipt_sha256 \
        lifecycle_registration_receipt_sha256 lifecycle_helper_sha256 \
        terminator_source_sha256 terminator_binary_sha256; do
        is_sha256 "$(kv "$RUN_METADATA" "$key")" || return 1
    done
    for key in subject subject_identity_sha256 scenario scenario_plan_sha256 \
        workload_sha256 command_sha256 environment_sha256 font_sha256 \
        initial_grid_sha256 measured_duration_ms process_pid process_start_identity; do
        [[ "$(kv "$RUN_METADATA" "$key")" == "$(kv "$RUN_INTENT" "$key")" ]] \
            || return 1
    done
    if [[ "$SUBJECT" == spaceterm ]]; then
        for key in native_observation_sha256 native_runtime_metadata_sha256 \
            native_failure_actions_sha256; do
            is_sha256 "$(kv "$RUN_METADATA" "$key")" || return 1
        done
        [[ "$(kv "$RUN_METADATA" native_failure_action_enabled)" == false ]] \
            || return 1
        for key in native_failure_request_count native_failure_result_count \
            native_failure_resource_staged_count native_failure_resource_staged_bytes \
            native_failure_resource_rolled_back_count \
            native_failure_resource_rolled_back_bytes; do
            [[ "$(kv "$RUN_METADATA" "$key")" == 0 ]] || return 1
        done
    else
        for key in native_observation_sha256 native_runtime_metadata_sha256 \
            native_failure_actions_sha256 native_failure_action_enabled \
            native_failure_request_count native_failure_result_count \
            native_failure_resource_staged_count native_failure_resource_staged_bytes \
            native_failure_resource_rolled_back_count \
            native_failure_resource_rolled_back_bytes; do
            [[ "$(kv "$RUN_METADATA" "$key")" == not-applicable ]] || return 1
        done
    fi
    RUN_METADATA_SHA256="$(sha256 "$RUN_METADATA")"
    RUN_METADATA_IDENTITY="$(stat -f '%d:%i' "$RUN_METADATA")"
    readonly RUN_METADATA_SHA256 RUN_METADATA_IDENTITY
}

wait_for_workload_evidence() {
    local attempt
    for attempt in {1..51}; do
        [[ -e "$WORKLOAD_METADATA" && -e "$WORKLOAD_EVENTS" ]] && return 0
        (( attempt == 51 )) || "$SLEEP_COMMAND" 0.1
    done
    return 1
}

validate_workload_metadata() {
    local secret_mode
    readonly WORKLOAD_KEYS="format_version scenario campaign_id session_id nonce subject_identity_sha256 subject_process_pid subject_process_start_identity producer_sha256 producer_pid producer_started_continuous_ns producer_session_id producer_process_group tty_device tty_inode tty_rdev ready_receipt_sha256 events_sha256 auth_algorithm seed_sha256 seed_bytes requested_duration_ms warmup_ms requested_iterations requested_seed_rows emitted_bytes input_events plan_start_continuous_ns started_continuous_ns ended_continuous_ns status events_hmac_sha256"
    wait_for_workload_evidence || return 1
    [[ "$("$STAT_COMMAND" -f '%d:%i' "$WORKLOAD_METADATA_PARENT")" \
            == "$WORKLOAD_METADATA_PARENT_IDENTITY" \
        && "$("$STAT_COMMAND" -f '%d:%i' "$WORKLOAD_EVENTS_PARENT")" \
            == "$WORKLOAD_EVENTS_PARENT_IDENTITY" ]] || return 1
    file_is_immutable_regular "$WORKLOAD_METADATA" || return 1
    file_is_immutable_regular "$WORKLOAD_EVENTS" || return 1
    [[ -f "$CAMPAIGN_SECRET_FILE" && ! -L "$CAMPAIGN_SECRET_FILE" ]] || return 1
    secret_mode="$("$STAT_COMMAND" -f '%Lp' "$CAMPAIGN_SECRET_FILE")"
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
        && "$("$STAT_COMMAND" -f '%d' "$WORKLOAD_EVENTS")" == "$READY_EVENTS_DEVICE" \
        && "$("$STAT_COMMAND" -f '%i' "$WORKLOAD_EVENTS")" == "$READY_EVENTS_INODE" \
        && "$(kv "$WORKLOAD_METADATA" events_sha256)" == "$(sha256 "$WORKLOAD_EVENTS")" \
        && "$(kv "$WORKLOAD_METADATA" auth_algorithm)" == hmac-sha256 \
        && "$(kv "$WORKLOAD_METADATA" warmup_ms)" == "$WARMUP_MILLISECONDS" \
        && "$(kv "$WORKLOAD_METADATA" requested_duration_ms)" == "$DURATION_MILLISECONDS" \
        && "$(kv "$WORKLOAD_METADATA" status)" == complete ]] || return 1
    PLAN_START_NS="$(kv "$WORKLOAD_METADATA" plan_start_continuous_ns)"
    MEASUREMENT_START_NS="$("$PYTHON_COMMAND" - "$PLAN_START_NS" "$WARMUP_MILLISECONDS" <<'PY'
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
    "$PYTHON_COMMAND" - "$DURATION_MILLISECONDS" \
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
    "$PYTHON_COMMAND" - "$WORKLOAD_METADATA" "$WORKLOAD_EVENTS" "$CAMPAIGN_SECRET_FILE" \
        "$RENDER_REQUIRES_WORKLOAD" <<'PY' \
        || return 1
import hashlib
import hmac
import pathlib
import struct
import sys

metadata = pathlib.Path(sys.argv[1]).read_bytes()
events = pathlib.Path(sys.argv[2]).read_bytes()
secret = pathlib.Path(sys.argv[3]).read_bytes()
render_sustained = sys.argv[4] == "true"
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

if render_sustained:
    fields = dict(line.rstrip(b"\n").split(b"\t", 1) for line in lines)
    decimal_keys = (
        b"seed_bytes", b"requested_duration_ms", b"warmup_ms",
        b"requested_iterations", b"requested_seed_rows", b"emitted_bytes",
        b"input_events", b"producer_started_continuous_ns",
        b"started_continuous_ns", b"ended_continuous_ns",
    )
    if any(not fields[key].isascii() or not fields[key].isdigit()
           for key in decimal_keys):
        raise SystemExit(1)
    numbers = {key: int(fields[key]) for key in decimal_keys}
    if not (
        numbers[b"seed_bytes"] > 0
        and numbers[b"emitted_bytes"] > numbers[b"seed_bytes"]
        and numbers[b"requested_iterations"] == 0
        and numbers[b"requested_seed_rows"] == 0
        and numbers[b"input_events"] == 0
    ):
        raise SystemExit(1)

    event_lines = events.splitlines()
    expected_header = (
        b"sequence\tcontinuous_ns\tkind\tevent_id\tbyte_count\trows\tcolumns\t"
        b"pixel_width\tpixel_height\tstatus"
    )
    if not events.endswith(b"\n") or not event_lines or event_lines[0] != expected_header:
        raise SystemExit(1)
    rows = []
    for sequence, line in enumerate(event_lines[1:]):
        columns = line.split(b"\t")
        if len(columns) != 10 or any(not columns[index].isdigit()
                                     for index in (0, 1, 4, 5, 6, 7, 8)):
            raise SystemExit(1)
        if int(columns[0]) != sequence or int(columns[5]) <= 0 or int(columns[6]) <= 0:
            raise SystemExit(1)
        rows.append(columns)
    if len(rows) < 4 or any(
        int(rows[index][1]) <= int(rows[index - 1][1])
        for index in range(1, len(rows))
    ):
        raise SystemExit(1)
    kinds = [row[2] for row in rows]
    if (
        rows[0][2:] != [b"started", b"none", b"0", rows[0][5], rows[0][6],
                        rows[0][7], rows[0][8], b"ok"]
        or kinds[1] != b"geometry"
        or kinds.count(b"seed-complete") != 1
        or kinds[-1] != b"producer-end"
        or any(kind not in {b"started", b"geometry", b"seed-complete", b"producer-end"}
               for kind in kinds)
        or rows[-1][3] != b"none"
        or rows[-1][9] != b"success"
        or int(rows[-1][4]) != numbers[b"emitted_bytes"]
        or int(rows[kinds.index(b"seed-complete")][4]) != numbers[b"seed_bytes"]
        or int(rows[0][1]) != numbers[b"producer_started_continuous_ns"]
        or int(rows[-1][1]) != numbers[b"ended_continuous_ns"]
        or not (int(rows[kinds.index(b"seed-complete")][1])
                < numbers[b"started_continuous_ns"] < numbers[b"ended_continuous_ns"])
    ):
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
    [[ "$("$AWK_COMMAND" -F '\t' '$1 == "identity_token" { print $2 }' <<< "$result")" \
            == "$TARGET_IDENTITY_TOKEN" \
        && "$("$AWK_COMMAND" -F '\t' '$1 == "live_code_identity_verified" { print $2 }' \
            <<< "$result")" == true ]]
}

frozen_inputs_match() {
    local supplemental_parent_matches=true
    if [[ -n "$SUPPLEMENTAL_EVIDENCE" \
        && "$("$STAT_COMMAND" -f '%d:%i' "$SUPPLEMENTAL_EVIDENCE_PARENT")" \
            != "$SUPPLEMENTAL_EVIDENCE_PARENT_IDENTITY" ]]; then
        supplemental_parent_matches=false
    fi
    if [[ "$SUPPLEMENTAL_WAS_PREEXISTING" == true ]]; then
        if [[ "$("$STAT_COMMAND" -f '%d:%i' "$SUPPLEMENTAL_EVIDENCE")" \
                != "$PRECAPTURE_SUPPLEMENTAL_IDENTITY" \
            || "$(sha256 "$SUPPLEMENTAL_EVIDENCE")" \
                != "$PRECAPTURE_SUPPLEMENTAL_SHA256" ]]; then
            supplemental_parent_matches=false
        fi
    fi
    local mode_inputs_match=true
    if [[ "$WORKLOAD_EVIDENCE_REQUIRED" == true ]]; then
        [[ "$("$STAT_COMMAND" -f '%d:%i' "$READY_RECEIPT")" == "$READY_RECEIPT_IDENTITY" \
            && "$(sha256 "$READY_RECEIPT")" == "$READY_RECEIPT_SHA256" \
            && "$("$STAT_COMMAND" -f '%d:%i' "$WORKLOAD_METADATA_PARENT")" \
                == "$WORKLOAD_METADATA_PARENT_IDENTITY" \
            && "$("$STAT_COMMAND" -f '%d:%i' "$WORKLOAD_EVENTS_PARENT")" \
                == "$WORKLOAD_EVENTS_PARENT_IDENTITY" ]] || mode_inputs_match=false
    fi
    if [[ "$CAPTURE_MODE" == render-profile-v1 ]]; then
        [[ "$("$STAT_COMMAND" -f '%d:%i' "$RENDER_INTENT")" == "$RENDER_INTENT_IDENTITY" \
            && "$(sha256 "$RENDER_INTENT")" == "$RENDER_INTENT_SHA256" \
            && "$("$STAT_COMMAND" -f '%d:%i' "$SCENARIO_PLAN")" == "$SCENARIO_PLAN_IDENTITY" \
            && "$(sha256 "$SCENARIO_PLAN")" == "$SCENARIO_PLAN_SHA256" \
            && "$("$STAT_COMMAND" -f '%d:%i' "$RENDER_EVIDENCE_PARENT")" \
                == "$RENDER_EVIDENCE_PARENT_IDENTITY" \
            && "$("$STAT_COMMAND" -f '%d:%i' "$RENDER_EVIDENCE")" \
                == "$RENDER_EVIDENCE_IDENTITY" \
            && "$(sha256 "$RENDER_EVIDENCE")" \
                == "$SUPPLEMENTAL_EVIDENCE_SHA256" ]] || mode_inputs_match=false
    fi
    [[ "$(sha256 "$SUBJECT_IDENTITY")" == "$SUBJECT_IDENTITY_SHA256" \
        && "$(sha256 "$RUN_INTENT")" == "$RUN_INTENT_SHA256" \
        && "$(stat -f '%d:%i' "$RUN_METADATA_PARENT")" \
            == "$RUN_METADATA_PARENT_IDENTITY" \
        && "$(sha256 "$PACKAGE_EXECUTABLE")" == "$EXPECTED_EXECUTABLE_SHA256" \
        && "$("$STAT_COMMAND" -f '%d' "$PACKAGE_EXECUTABLE")" == "$EXECUTABLE_DEVICE" \
        && "$("$STAT_COMMAND" -f '%i' "$PACKAGE_EXECUTABLE")" == "$EXECUTABLE_INODE" \
        && "$("$STAT_COMMAND" -f '%d:%i' "$CAMPAIGN_SECRET_FILE")" \
            == "$CAMPAIGN_SECRET_IDENTITY" \
        && "$(sha256 "$CAMPAIGN_SECRET_FILE")" == "$CAMPAIGN_SECRET_SHA256" \
        && "$supplemental_parent_matches" == true \
        && "$mode_inputs_match" == true ]] \
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
    "$CHMOD_COMMAND" 0400 "$temporary"
    "$LN_COMMAND" "$temporary" "$CAPTURE_START_NOTIFICATION" \
        || die "capture-start notification path was created concurrently"
    "$RM_COMMAND" -f -- "$temporary"
}

trace_bundle_has_data() {
    [[ -d "$TRACE_PATH" ]] \
        && [[ -n "$("$FIND_COMMAND" "$TRACE_PATH" -type f -size +0c -print -quit 2>/dev/null)" ]]
}

export_trace_table() {
    "$XCRUN_COMMAND" xctrace export --input "$TRACE_PATH" --xpath "$1" --output "$2"
}

verification_metric() {
    [[ -f "$TRACE_VERIFICATION_PATH" ]] || return 0
    "$AWK_COMMAND" -F '\t' -v key="$1" '$1 == key { print $2 }' "$TRACE_VERIFICATION_PATH"
}

write_metadata() {
    local output="$1"
    {
        printf 'format_version\t3\n'
        printf 'capture_status\t%s\n' "$capture_status"
        printf 'incomplete_reason\t%s\n' "$incomplete_reason"
        printf 'subject_identity_sha256\t%s\n' "$SUBJECT_IDENTITY_SHA256"
        printf 'run_metadata_sha256\t%s\n' "$run_metadata_hash"
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

trace_tree_sha256() {
    python3 - "$TRACE_PATH" <<'PY'
import hashlib, os, pathlib, struct, sys, unicodedata

root = pathlib.Path(sys.argv[1])
digest = hashlib.sha256(b"spaceterm.performance.trace-tree/v1\0")
entries = []
for path in root.rglob("*"):
    if path.is_symlink() or (path.exists() and not path.is_file() and not path.is_dir()):
        raise SystemExit(1)
    if path.is_file():
        relative = unicodedata.normalize("NFC", path.relative_to(root).as_posix())
        if relative != path.relative_to(root).as_posix():
            raise SystemExit(1)
        entries.append((relative.encode("utf-8"), path))
for encoded, path in sorted(entries):
    data = path.read_bytes()
    digest.update(struct.pack(">Q", len(encoded)))
    digest.update(encoded)
    digest.update(struct.pack(">Q", len(data)))
    digest.update(data)
print(digest.hexdigest())
PY
}

publish_provisional_receipt() {
    local temporary="${PROVISIONAL_RECEIPT}.tmp.$$" unsigned hmac_value
    [[ "$(stat -f '%d:%i' "$PROVISIONAL_RECEIPT_PARENT")" \
            == "$PROVISIONAL_RECEIPT_PARENT_IDENTITY" \
        && ! -e "$PROVISIONAL_RECEIPT" && ! -L "$PROVISIONAL_RECEIPT" ]] \
        || die "provisional receipt destination changed"
    unsigned="${temporary}.unsigned"
    {
        printf 'format_version\t1\n'
        printf 'subject_identity_sha256\t%s\n' "$SUBJECT_IDENTITY_SHA256"
        printf 'run_intent_sha256\t%s\n' "$RUN_INTENT_SHA256"
        printf 'workload_metadata_sha256\t%s\n' "$WORKLOAD_METADATA_SHA256"
        printf 'workload_ready_receipt_sha256\t%s\n' "$READY_RECEIPT_SHA256"
        printf 'supplemental_evidence_sha256\t%s\n' "$SUPPLEMENTAL_EVIDENCE_SHA256"
        printf 'capture_status\tCAPTURED\nrequested_duration_ms\t%s\n' "$DURATION_MILLISECONDS"
        printf 'actual_duration_ms\t%s\n' "$actual_duration_ms"
        printf 'capture_started_continuous_ns\t%s\n' "$capture_started_ns"
        printf 'capture_ended_continuous_ns\t%s\n' "$capture_ended_ns"
        printf 'trace_bundle_tree_sha256\t%s\n' "$(trace_tree_sha256)"
        printf 'toc_sha256\t%s\n' "$(sha256 "$TOC_PATH")"
        printf 'time_profile_export_sha256\t%s\n' "$(sha256 "$TIME_PROFILE_EXPORT_PATH")"
        printf 'allocations_export_sha256\t%s\n' "$(sha256 "$ALLOCATIONS_EXPORT_PATH")"
        printf 'hangs_export_sha256\t%s\n' "$(sha256 "$HANGS_EXPORT_PATH")"
        printf 'trace_verification_sha256\t%s\n' "$(sha256 "$TRACE_VERIFICATION_PATH")"
        printf 'verifier_sha256\t%s\n' "$(sha256 "$TRACE_VERIFIER")"
        printf 'evidence_mode\t%s\n' "$EVIDENCE_MODE"
        printf 'status\tcomplete\nauth_algorithm\thmac-sha256\n'
    } > "$unsigned"
    hmac_value="$(python3 - "$unsigned" "$CAMPAIGN_SECRET_FILE" <<'PY'
import hashlib, hmac, pathlib, struct, sys
data = pathlib.Path(sys.argv[1]).read_bytes()
payload = b"spaceterm.performance.trace-provisional/v1\0" + struct.pack(">Q", len(data)) + data
print(hmac.new(pathlib.Path(sys.argv[2]).read_bytes(), payload, hashlib.sha256).hexdigest())
PY
)"
    cp "$unsigned" "$temporary"
    printf 'provisional_hmac_sha256\t%s\n' "$hmac_value" >> "$temporary"
    chmod 0400 "$temporary"
    ln "$temporary" "$PROVISIONAL_RECEIPT" \
        || die "provisional receipt path was created concurrently"
    rm -f -- "$temporary" "$unsigned"
    PROVISIONAL_RECEIPT_SHA256="$(sha256 "$PROVISIONAL_RECEIPT")"
    readonly PROVISIONAL_RECEIPT_SHA256
}

if [[ "${1:-}" == --doctor ]]; then doctor; exit 0; fi
while (( $# > 0 )); do
    case "$1" in
        --subject-identity) SUBJECT_IDENTITY="${2:-}"; shift ;;
        --run-intent) RUN_INTENT="${2:-}"; shift ;;
        --run-metadata) RUN_METADATA="${2:-}"; shift ;;
        --workload-metadata) WORKLOAD_METADATA="${2:-}"; shift ;;
        --workload-events) WORKLOAD_EVENTS="${2:-}"; shift ;;
        --workload-ready-receipt) READY_RECEIPT="${2:-}"; shift ;;
        --supplemental-evidence) SUPPLEMENTAL_EVIDENCE="${2:-}"; shift ;;
        --evidence-mode) CAPTURE_MODE="${2:-}"; shift ;;
        --scenario-plan) SCENARIO_PLAN="${2:-}"; shift ;;
        --render-intent) RENDER_INTENT="${2:-}"; shift ;;
        --render-evidence) RENDER_EVIDENCE="${2:-}"; shift ;;
        --campaign-manifest) CAMPAIGN_MANIFEST="${2:-}"; shift ;;
        --render-tool-bundle-manifest) RENDER_TOOL_BUNDLE_MANIFEST="${2:-}"; shift ;;
        --expected-source-commit) EXPECTED_SOURCE_COMMIT="${2:-}"; shift ;;
        --trusted-source-repository) TRUSTED_SOURCE_REPOSITORY="${2:-}"; shift ;;
        --trace-anchor-receipt) TRACE_ANCHOR_RECEIPT="${2:-}"; shift ;;
        --campaign-secret-file) CAMPAIGN_SECRET_FILE="${2:-}"; shift ;;
        --campaign-id) CAMPAIGN_ID="${2:-}"; shift ;;
        --session-id) SESSION_ID="${2:-}"; shift ;;
        --nonce) NONCE="${2:-}"; shift ;;
        --scenario) SCENARIO="${2:-}"; shift ;;
        --warmup-ms) WARMUP_MILLISECONDS="${2:-}"; shift ;;
        --duration-ms) DURATION_MILLISECONDS="${2:-}"; shift ;;
        --output-directory) OUTPUT_DIRECTORY="${2:-}"; shift ;;
        --capture-start-notification) CAPTURE_START_NOTIFICATION="${2:-}"; shift ;;
        --provisional-receipt) PROVISIONAL_RECEIPT="${2:-}"; shift ;;
        --render-evidence-timeout-seconds) RENDER_EVIDENCE_TIMEOUT_SECONDS="${2:-}"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument: $1" ;;
    esac
    shift
done

doctor >/dev/null
[[ "$CAPTURE_MODE" == workload-v3 || "$CAPTURE_MODE" == render-profile-v1 ]] \
    || die "evidence mode is invalid"
is_safe_label "$SCENARIO" || die "scenario label is invalid"
is_safe_label "$CAMPAIGN_ID" || die "campaign ID is invalid"
is_safe_label "$SESSION_ID" || die "session ID is invalid"
is_sha256 "$NONCE" || die "nonce is invalid"
is_uint "$WARMUP_MILLISECONDS" || die "warmup must be unsigned milliseconds"
is_positive_integer "$DURATION_MILLISECONDS" || die "duration must be positive milliseconds"
if ! is_uint "$RUN_METADATA_WAIT_TENTHS" \
    || (( RUN_METADATA_WAIT_TENTHS > 1200 )); then
    die "run metadata wait override is invalid"
fi
(( DURATION_MILLISECONDS % 1000 == 0 )) || die "duration must be whole seconds"
is_positive_integer "$RENDER_EVIDENCE_TIMEOUT_SECONDS" \
    || die "render evidence timeout must be positive seconds"
(( RENDER_EVIDENCE_TIMEOUT_SECONDS <= 3600 )) \
    || die "render evidence timeout exceeds the one-hour safety bound"
RENDER_REQUIRES_WORKLOAD=false
if [[ "$CAPTURE_MODE" == render-profile-v1 \
    && "$SCENARIO" == perf-render-sustained-output ]]; then
    RENDER_REQUIRES_WORKLOAD=true
fi
WORKLOAD_EVIDENCE_REQUIRED=false
if [[ "$CAPTURE_MODE" == workload-v3 || "$RENDER_REQUIRES_WORKLOAD" == true ]]; then
    WORKLOAD_EVIDENCE_REQUIRED=true
fi
readonly RENDER_REQUIRES_WORKLOAD WORKLOAD_EVIDENCE_REQUIRED
[[ -n "$SUBJECT_IDENTITY" && -n "$RUN_INTENT" && -n "$RUN_METADATA" \
    && -n "$CAMPAIGN_SECRET_FILE" && -n "$OUTPUT_DIRECTORY" \
    && -n "$PROVISIONAL_RECEIPT" ]] \
    || die "required evidence or output argument is missing"
if [[ "$CAPTURE_MODE" == workload-v3 ]]; then
    [[ -n "$WORKLOAD_METADATA" && -n "$WORKLOAD_EVENTS" && -n "$READY_RECEIPT" \
        && -z "$SCENARIO_PLAN$RENDER_INTENT$RENDER_EVIDENCE" ]] \
        || die "workload-v3 evidence arguments are incomplete or mixed"
else
    [[ -n "$SCENARIO_PLAN" && -n "$RENDER_INTENT" && -n "$RENDER_EVIDENCE" \
        && -n "$CAMPAIGN_MANIFEST" && -n "$TRACE_ANCHOR_RECEIPT" \
        && ( "$TEST_OVERRIDES_ACTIVE" == true \
            || ( -n "$RENDER_TOOL_BUNDLE_MANIFEST" \
                && "$EXPECTED_SOURCE_COMMIT" =~ ^[0-9a-f]{40}$ \
                && -d "$TRUSTED_SOURCE_REPOSITORY" && ! -L "$TRUSTED_SOURCE_REPOSITORY" ) ) \
        && -z "$SUPPLEMENTAL_EVIDENCE" ]] \
        || die "render-profile-v1 evidence arguments are incomplete or mixed"
    if [[ "$RENDER_REQUIRES_WORKLOAD" == true ]]; then
        [[ -n "$WORKLOAD_METADATA" && -n "$WORKLOAD_EVENTS" \
            && -n "$READY_RECEIPT" ]] \
            || die "sustained render profile requires workload-v3 evidence"
    else
        [[ -z "$WORKLOAD_METADATA$WORKLOAD_EVENTS$READY_RECEIPT" ]] \
            || die "non-output render profile rejects workload-v3 evidence"
    fi
fi
[[ ! -e "$OUTPUT_DIRECTORY" ]] || die "output directory already exists"

SCRIPT_DIRECTORY="$(cd -- "$("$DIRNAME_COMMAND" -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIRECTORY
HMAC_HELPER="$SCRIPT_DIRECTORY/acceptance/render-profile-hmac.py"
TRACE_RECEIPT_HELPER="$SCRIPT_DIRECTORY/acceptance/render-trace-receipt.py"
readonly HMAC_HELPER
PROCESS_INSPECTOR="${PROCESS_INSPECTOR:-$SCRIPT_DIRECTORY/inspect-release-performance-process.py}"
TRACE_VERIFIER="${TRACE_VERIFIER:-$SCRIPT_DIRECTORY/verify-release-performance-trace.py}"
COMMAND_RUNNER="$SCRIPT_DIRECTORY/run-release-performance-command.py"
readonly PROCESS_INSPECTOR TRACE_VERIFIER COMMAND_RUNNER
[[ -x "$PROCESS_INSPECTOR" && -x "$TRACE_VERIFIER" && -x "$COMMAND_RUNNER" ]] \
    || die "trace verifier tooling is not executable"
[[ -f "$HMAC_HELPER" && ! -L "$HMAC_HELPER" && -x "$HMAC_HELPER" ]] \
    || die "render authentication helper is unavailable"
[[ -f "$TRACE_RECEIPT_HELPER" && ! -L "$TRACE_RECEIPT_HELPER" \
    && -x "$TRACE_RECEIPT_HELPER" ]] \
    || die "render trace receipt helper is unavailable"

[[ ! -L "$SUBJECT_IDENTITY" && ! -L "$RUN_INTENT" \
    && ( ! -e "$RUN_METADATA" || ! -L "$RUN_METADATA" ) \
    && ( -z "$SUPPLEMENTAL_EVIDENCE" || ! -L "$SUPPLEMENTAL_EVIDENCE" ) \
    && ( ! -e "$PROVISIONAL_RECEIPT" || ! -L "$PROVISIONAL_RECEIPT" ) \
    && ! -L "$CAMPAIGN_SECRET_FILE" \
    && ( -z "$WORKLOAD_EVENTS" || ! -e "$WORKLOAD_EVENTS" || ! -L "$WORKLOAD_EVENTS" ) \
    && ( -z "$WORKLOAD_METADATA" || ! -e "$WORKLOAD_METADATA" || ! -L "$WORKLOAD_METADATA" ) \
    && ( -z "$READY_RECEIPT" || ! -L "$READY_RECEIPT" ) \
    && ( -z "$SCENARIO_PLAN" || ! -L "$SCENARIO_PLAN" ) \
    && ( -z "$RENDER_INTENT" || ! -L "$RENDER_INTENT" ) \
    && ( -z "$CAMPAIGN_MANIFEST" || ! -L "$CAMPAIGN_MANIFEST" ) \
    && ( -z "$TRACE_ANCHOR_RECEIPT" || ! -e "$TRACE_ANCHOR_RECEIPT" \
        || ! -L "$TRACE_ANCHOR_RECEIPT" ) \
    && ( -z "$RENDER_EVIDENCE" || ! -e "$RENDER_EVIDENCE" \
        || ! -L "$RENDER_EVIDENCE" ) ]] \
    || die "evidence inputs must not be symlinks"
SUBJECT_IDENTITY="$("$REALPATH_COMMAND" "$SUBJECT_IDENTITY")"
RUN_INTENT="$("$REALPATH_COMMAND" "$RUN_INTENT")"
[[ ! -e "$RUN_METADATA" ]] || die "final run metadata must be absent at capture launch"
RUN_METADATA="$(canonical_pending_path "$RUN_METADATA")"
RUN_METADATA_PARENT="$("$DIRNAME_COMMAND" -- "$RUN_METADATA")"
RUN_METADATA_PARENT_IDENTITY="$("$STAT_COMMAND" -f '%d:%i' "$RUN_METADATA_PARENT")"
CAMPAIGN_SECRET_FILE="$("$REALPATH_COMMAND" "$CAMPAIGN_SECRET_FILE")"
if [[ "$CAPTURE_MODE" == render-profile-v1 ]]; then
    CAMPAIGN_MANIFEST="$("$REALPATH_COMMAND" "$CAMPAIGN_MANIFEST")"
    file_is_immutable_regular "$CAMPAIGN_MANIFEST" \
        || die "campaign manifest must be immutable singleton evidence"
    TRACE_ANCHOR_RECEIPT="$(canonical_pending_path "$TRACE_ANCHOR_RECEIPT")"
    if [[ "$TEST_OVERRIDES_ACTIVE" == false ]]; then
        RENDER_TOOL_BUNDLE_MANIFEST="$("$REALPATH_COMMAND" "$RENDER_TOOL_BUNDLE_MANIFEST")"
        TRUSTED_SOURCE_REPOSITORY="$("$REALPATH_COMMAND" "$TRUSTED_SOURCE_REPOSITORY")"
        file_is_immutable_regular "$RENDER_TOOL_BUNDLE_MANIFEST" \
            || die "render tool bundle manifest must be immutable singleton evidence"
    fi
fi
if [[ "$WORKLOAD_EVIDENCE_REQUIRED" == true ]]; then
    READY_RECEIPT="$("$REALPATH_COMMAND" "$READY_RECEIPT")"
fi
if [[ "$CAPTURE_MODE" == workload-v3 && -n "$SUPPLEMENTAL_EVIDENCE" ]]; then
    SUPPLEMENTAL_EVIDENCE="$(canonical_pending_path "$SUPPLEMENTAL_EVIDENCE")"
    SUPPLEMENTAL_EVIDENCE_PARENT="$("$DIRNAME_COMMAND" -- "$SUPPLEMENTAL_EVIDENCE")"
    SUPPLEMENTAL_EVIDENCE_PARENT_IDENTITY="$("$STAT_COMMAND" -f '%d:%i' "$SUPPLEMENTAL_EVIDENCE_PARENT")"
    SUPPLEMENTAL_WAS_PREEXISTING=false
    PRECAPTURE_SUPPLEMENTAL_IDENTITY=none
    PRECAPTURE_SUPPLEMENTAL_SHA256=none
    if [[ -e "$SUPPLEMENTAL_EVIDENCE" ]]; then
        file_is_immutable_regular "$SUPPLEMENTAL_EVIDENCE" \
            || die "preexisting supplemental evidence must be immutable singleton evidence"
        SUPPLEMENTAL_WAS_PREEXISTING=true
        PRECAPTURE_SUPPLEMENTAL_IDENTITY="$("$STAT_COMMAND" -f '%d:%i' "$SUPPLEMENTAL_EVIDENCE")"
        PRECAPTURE_SUPPLEMENTAL_SHA256="$(sha256 "$SUPPLEMENTAL_EVIDENCE")"
    fi
else
    SUPPLEMENTAL_EVIDENCE_PARENT=none
    SUPPLEMENTAL_EVIDENCE_PARENT_IDENTITY=none
    SUPPLEMENTAL_WAS_PREEXISTING=false
    PRECAPTURE_SUPPLEMENTAL_IDENTITY=none
    PRECAPTURE_SUPPLEMENTAL_SHA256=none
fi
if [[ "$WORKLOAD_EVIDENCE_REQUIRED" == true ]]; then
    WORKLOAD_METADATA="$(canonical_pending_path "$WORKLOAD_METADATA")"
    WORKLOAD_EVENTS="$(canonical_pending_path "$WORKLOAD_EVENTS")"
    WORKLOAD_METADATA_PARENT="$("$DIRNAME_COMMAND" -- "$WORKLOAD_METADATA")"
    WORKLOAD_EVENTS_PARENT="$("$DIRNAME_COMMAND" -- "$WORKLOAD_EVENTS")"
    WORKLOAD_METADATA_PARENT_IDENTITY="$("$STAT_COMMAND" -f '%d:%i' "$WORKLOAD_METADATA_PARENT")"
    WORKLOAD_EVENTS_PARENT_IDENTITY="$("$STAT_COMMAND" -f '%d:%i' "$WORKLOAD_EVENTS_PARENT")"
else
    WORKLOAD_METADATA_PARENT=none
    WORKLOAD_EVENTS_PARENT=none
    WORKLOAD_METADATA_PARENT_IDENTITY=none
    WORKLOAD_EVENTS_PARENT_IDENTITY=none
fi
if [[ "$CAPTURE_MODE" == render-profile-v1 ]]; then
    SCENARIO_PLAN="$("$REALPATH_COMMAND" "$SCENARIO_PLAN")"
    RENDER_INTENT="$("$REALPATH_COMMAND" "$RENDER_INTENT")"
    RENDER_EVIDENCE="$(canonical_pending_path "$RENDER_EVIDENCE")"
    RENDER_EVIDENCE_PARENT="$("$DIRNAME_COMMAND" -- "$RENDER_EVIDENCE")"
    RENDER_EVIDENCE_PARENT_IDENTITY="$("$STAT_COMMAND" -f '%d:%i' "$RENDER_EVIDENCE_PARENT")"
    [[ ! -e "$RENDER_EVIDENCE" ]] || die "render evidence already exists"
else
    RENDER_EVIDENCE_PARENT=none
    RENDER_EVIDENCE_PARENT_IDENTITY=none
fi
if [[ -n "$CAPTURE_START_NOTIFICATION" ]]; then
    [[ ! -e "$CAPTURE_START_NOTIFICATION" && ! -L "$CAPTURE_START_NOTIFICATION" ]] \
        || die "capture-start notification must not exist"
    CAPTURE_START_NOTIFICATION="$(canonical_pending_path "$CAPTURE_START_NOTIFICATION")"
fi
[[ ! -e "$PROVISIONAL_RECEIPT" ]] || die "provisional receipt must not exist"
PROVISIONAL_RECEIPT="$(canonical_pending_path "$PROVISIONAL_RECEIPT")"
PROVISIONAL_RECEIPT_PARENT="$(dirname -- "$PROVISIONAL_RECEIPT")"
PROVISIONAL_RECEIPT_PARENT_IDENTITY="$(stat -f '%d:%i' "$PROVISIONAL_RECEIPT_PARENT")"
readonly SUBJECT_IDENTITY RUN_INTENT RUN_METADATA RUN_METADATA_PARENT \
    RUN_METADATA_PARENT_IDENTITY WORKLOAD_METADATA WORKLOAD_EVENTS READY_RECEIPT \
    CAMPAIGN_SECRET_FILE WORKLOAD_METADATA_PARENT WORKLOAD_EVENTS_PARENT \
    WORKLOAD_METADATA_PARENT_IDENTITY WORKLOAD_EVENTS_PARENT_IDENTITY \
    CAPTURE_START_NOTIFICATION SUPPLEMENTAL_EVIDENCE SUPPLEMENTAL_EVIDENCE_PARENT \
    SUPPLEMENTAL_EVIDENCE_PARENT_IDENTITY SUPPLEMENTAL_WAS_PREEXISTING \
    PRECAPTURE_SUPPLEMENTAL_IDENTITY PRECAPTURE_SUPPLEMENTAL_SHA256 \
    CAPTURE_MODE SCENARIO_PLAN RENDER_INTENT RENDER_EVIDENCE \
    CAMPAIGN_MANIFEST TRACE_ANCHOR_RECEIPT TRACE_RECEIPT_HELPER \
    RENDER_TOOL_BUNDLE_MANIFEST EXPECTED_SOURCE_COMMIT TRUSTED_SOURCE_REPOSITORY \
    RENDER_EVIDENCE_PARENT RENDER_EVIDENCE_PARENT_IDENTITY \
    RENDER_EVIDENCE_TIMEOUT_SECONDS
readonly PROVISIONAL_RECEIPT PROVISIONAL_RECEIPT_PARENT \
    PROVISIONAL_RECEIPT_PARENT_IDENTITY
validate_subject_identity
validate_run_intent
if [[ "$CAPTURE_MODE" == render-profile-v1 && "$TEST_OVERRIDES_ACTIVE" == false ]]; then
    verify_recorder_tool_bundle \
        || die "trace recorder is not the selected frozen bundle tool"
    RENDER_RECORDER_TOOL_IDENTITY_SNAPSHOT="$(render_recorder_tool_identity_snapshot)" \
        || die "render recorder tools must be immutable trusted executables"
    readonly RENDER_RECORDER_TOOL_IDENTITY_SNAPSHOT
fi
validate_campaign_secret
if [[ "$WORKLOAD_EVIDENCE_REQUIRED" == true ]]; then
    validate_ready_receipt
else
    READY_RECEIPT_SHA256=0000000000000000000000000000000000000000000000000000000000000000
    WORKLOAD_METADATA_SHA256=0000000000000000000000000000000000000000000000000000000000000000
    readonly READY_RECEIPT_SHA256 WORKLOAD_METADATA_SHA256
fi
if [[ "$CAPTURE_MODE" == render-profile-v1 ]]; then
    validate_render_intent
    "$TRACE_RECEIPT_HELPER" verify-case \
        --manifest "$CAMPAIGN_MANIFEST" \
        --render-tool-bundle-manifest "$RENDER_TOOL_BUNDLE_MANIFEST" \
        --expected-source-commit "$EXPECTED_SOURCE_COMMIT" \
        --trusted-source-repository "$TRUSTED_SOURCE_REPOSITORY" \
        --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
        --subject-identity "$SUBJECT_IDENTITY" \
        --render-intent "$RENDER_INTENT" \
        --campaign-id "$CAMPAIGN_ID" \
        --session-id "$SESSION_ID" \
        --nonce "$NONCE" \
        --scenario "$SCENARIO" \
        --subject "$SUBJECT" \
        --render-profile-hmac "$HMAC_HELPER" \
        --render-trace-receipt-helper "$TRACE_RECEIPT_HELPER" \
        --process-inspector "$PROCESS_INSPECTOR" \
        --trace-verifier "$TRACE_VERIFIER" \
        --command-runner "$COMMAND_RUNNER" \
        >/dev/null \
        || die "campaign manifest does not bind the requested render case tuple"
fi
package_bundle_matches || die "frozen application bundle identity does not match"

identity_output="$(process_identity)"
TARGET_IDENTITY_TOKEN="$("$AWK_COMMAND" -F '\t' '$1 == "identity_token" { print $2 }' <<< "$identity_output")"
[[ -n "$TARGET_IDENTITY_TOKEN" \
    && "$("$AWK_COMMAND" -F '\t' '$1 == "live_code_identity_verified" { print $2 }' \
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

"$MKDIR_COMMAND" -m 700 -- "$OUTPUT_DIRECTORY"
"$MKDIR_COMMAND" -m 700 -- "$PRIVATE_EXPORT_DIRECTORY"
cleanup() { "$RM_COMMAND" -f -- "$METADATA_TEMP"; }
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
record_elapsed="$("$TR_COMMAND" -d '[:space:]' < "$RECORD_ELAPSED_PATH" 2>/dev/null || true)"
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
render_evidence_valid=false
if [[ "$WORKLOAD_EVIDENCE_REQUIRED" == true ]]; then
    if validate_workload_metadata; then workload_valid=true; fi
else
    workload_valid=true
fi
if [[ "$CAPTURE_MODE" == render-profile-v1 ]]; then
    if validate_render_evidence; then render_evidence_valid=true; fi
fi
render_workload_intervals_match=true
if [[ "$RENDER_REQUIRES_WORKLOAD" == true \
    && "$workload_valid" == true && "$render_evidence_valid" == true ]]; then
    start_delta=$((WORKLOAD_STARTED_NS - RENDER_MEASUREMENT_START_NS))
    end_delta=$((WORKLOAD_ENDED_NS - RENDER_MEASUREMENT_END_NS))
    (( start_delta >= 0 )) || start_delta=$((-start_delta))
    (( end_delta >= 0 )) || end_delta=$((-end_delta))
    if (( start_delta > 250000000 || end_delta > 250000000 )); then
        render_workload_intervals_match=false
    fi
fi
supplemental_valid=false
run_metadata_valid=false
if [[ "$CAPTURE_MODE" == workload-v3 ]]; then
    if validate_supplemental_evidence; then supplemental_valid=true; fi
else
    supplemental_valid="$render_evidence_valid"
fi
workload_metadata_hash="${WORKLOAD_METADATA_SHA256:-0000000000000000000000000000000000000000000000000000000000000000}"
actual_seconds="$(verification_metric actual_record_duration_seconds)"
actual_duration_ms="$("$AWK_COMMAND" -v seconds="${actual_seconds:-0}" 'BEGIN { printf "%.0f", seconds * 1000 }')"
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

provisional_interval_valid=false
if [[ "$CAPTURE_MODE" == workload-v3 && "$workload_valid" == true ]] \
    && (( capture_started_ns <= MEASUREMENT_START_NS \
        && MEASUREMENT_START_NS - capture_started_ns <= 2000000000 \
        && capture_ended_ns >= WORKLOAD_ENDED_NS \
        && capture_ended_ns - WORKLOAD_ENDED_NS <= 2000000000 )); then
    provisional_interval_valid=true
elif [[ "$CAPTURE_MODE" == render-profile-v1 && "$render_evidence_valid" == true ]] \
    && (( capture_started_ns <= RENDER_MEASUREMENT_START_NS \
        && RENDER_MEASUREMENT_START_NS - capture_started_ns <= 2000000000 \
        && capture_ended_ns >= RENDER_MEASUREMENT_END_NS \
        && capture_ended_ns - RENDER_MEASUREMENT_END_NS <= 2000000000 )); then
    provisional_interval_valid=true
fi
provisional_ready=false
if [[ "$record_status" == 0 && "$export_status" == 0 \
    && "$table_export_status" == 0 && "$verification_status" == 0 ]] \
    && [[ "$target_identity_verified" == true && "$inputs_frozen" == true \
        && "$workload_valid" == true && "$supplemental_valid" == true \
        && "$clock_mapping_verified" == true ]] \
    && [[ "$provisional_interval_valid" == true \
        && "$render_workload_intervals_match" == true ]] \
    && [[ "$TEST_OVERRIDES_ACTIVE" == false ]]; then
    provisional_ready=true
    [[ ! -e "$RUN_METADATA" ]] \
        || die "final run metadata appeared before provisional publication"
    publish_provisional_receipt
fi
if [[ "$provisional_ready" == true ]] && validate_run_metadata; then
    run_metadata_valid=true
fi
run_metadata_hash="${RUN_METADATA_SHA256:-0000000000000000000000000000000000000000000000000000000000000000}"
final_inputs_frozen=false
if [[ "$run_metadata_valid" == true ]] \
    && frozen_inputs_match \
    && [[ "$(stat -f '%d:%i' "$RUN_METADATA")" == "$RUN_METADATA_IDENTITY" \
        && "$(sha256 "$RUN_METADATA")" == "$RUN_METADATA_SHA256" ]]; then
    final_inputs_frozen=true
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
elif [[ "$render_workload_intervals_match" != true ]]; then
    incomplete_reason=render-workload-interval-mismatch
elif [[ "$clock_mapping_verified" != true ]]; then incomplete_reason=trace-clock-correlation-invalid
elif [[ "$CAPTURE_MODE" == workload-v3 ]] && (( capture_started_ns > MEASUREMENT_START_NS \
    || MEASUREMENT_START_NS - capture_started_ns > 2000000000 \
    || capture_ended_ns < WORKLOAD_ENDED_NS \
    || capture_ended_ns - WORKLOAD_ENDED_NS > 2000000000 )); then
    incomplete_reason=trace-workload-interval-mismatch
elif [[ "$CAPTURE_MODE" == render-profile-v1 ]] \
    && (( capture_started_ns > RENDER_MEASUREMENT_START_NS \
        || RENDER_MEASUREMENT_START_NS - capture_started_ns > 2000000000 \
        || capture_ended_ns < RENDER_MEASUREMENT_END_NS \
        || capture_ended_ns - RENDER_MEASUREMENT_END_NS > 2000000000 )); then
    incomplete_reason=trace-render-interval-mismatch
elif [[ "$RENDER_REQUIRES_WORKLOAD" == true ]] \
    && (( capture_started_ns > MEASUREMENT_START_NS \
        || MEASUREMENT_START_NS - capture_started_ns > 2000000000 \
        || capture_ended_ns < WORKLOAD_ENDED_NS \
        || capture_ended_ns - WORKLOAD_ENDED_NS > 2000000000 )); then
    incomplete_reason=trace-workload-interval-mismatch
elif [[ "$TEST_OVERRIDES_ACTIVE" == true ]]; then incomplete_reason=test-overrides-active
elif [[ "$run_metadata_valid" != true ]]; then incomplete_reason=run-metadata-invalid
elif [[ "$final_inputs_frozen" != true ]]; then incomplete_reason=frozen-input-changed
elif [[ "$provisional_ready" != true ]]; then incomplete_reason=provisional-receipt-not-published
else
    capture_status=CAPTURED
    metadata_status=complete
fi

if [[ "$provisional_ready" == true && "$run_metadata_valid" != true ]]; then
    echo "error: final run metadata did not causally bind the provisional trace" >&2
    exit 1
fi

write_metadata "$METADATA_TEMP"
"$CHMOD_COMMAND" 0444 "$METADATA_TEMP"
"$LN_COMMAND" "$METADATA_TEMP" "$METADATA_PATH" || die "metadata path was created concurrently"
"$RM_COMMAND" -f -- "$METADATA_TEMP"
trap - EXIT INT TERM
if [[ "$capture_status" != CAPTURED ]]; then
    echo "error: trace capture is incomplete: $incomplete_reason" >&2
    exit 1
fi
if [[ "$CAPTURE_MODE" == render-profile-v1 ]]; then
    if [[ "$TEST_OVERRIDES_ACTIVE" == false ]]; then
        [[ "$(render_recorder_tool_identity_snapshot)" \
            == "$RENDER_RECORDER_TOOL_IDENTITY_SNAPSHOT" ]] \
            || die "render recorder tool identity changed during capture"
    fi
    "$TRACE_RECEIPT_HELPER" anchor \
        --manifest "$CAMPAIGN_MANIFEST" \
        --render-tool-bundle-manifest "$RENDER_TOOL_BUNDLE_MANIFEST" \
        --expected-source-commit "$EXPECTED_SOURCE_COMMIT" \
        --trusted-source-repository "$TRUSTED_SOURCE_REPOSITORY" \
        --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
        --subject-identity "$SUBJECT_IDENTITY" \
        --run-metadata "$RUN_METADATA" \
        --render-intent "$RENDER_INTENT" \
        --render-evidence "$RENDER_EVIDENCE" \
        --trace-metadata "$METADATA_PATH" \
        --trace-started-epoch-ns "$trace_started_epoch_ns" \
        --trace-ended-epoch-ns "$trace_ended_epoch_ns" \
        --start-anchor-continuous-ns "$wrapper_started_ns" \
        --start-anchor-epoch-ns "$wrapper_started_epoch_ns" \
        --start-anchor-width-ns "$start_anchor_width_ns" \
        --end-anchor-continuous-ns "$wrapper_ended_ns" \
        --end-anchor-epoch-ns "$wrapper_ended_epoch_ns" \
        --end-anchor-width-ns "$end_anchor_width_ns" \
        --render-profile-hmac "$HMAC_HELPER" \
        --render-trace-receipt-helper "$TRACE_RECEIPT_HELPER" \
        --process-inspector "$PROCESS_INSPECTOR" \
        --trace-verifier "$TRACE_VERIFIER" \
        --command-runner "$COMMAND_RUNNER" \
        --output "$TRACE_ANCHOR_RECEIPT" \
        || die "authenticated render trace anchor receipt could not be finalized"
    if [[ "$TEST_OVERRIDES_ACTIVE" == false ]]; then
        [[ "$(render_recorder_tool_identity_snapshot)" \
            == "$RENDER_RECORDER_TOOL_IDENTITY_SNAPSHOT" ]] \
            || die "render recorder tool identity changed during anchor finalization"
    fi
fi
printf 'Trace: %s\nMetadata: %s\nTable of contents: %s\n' \
    "$TRACE_PATH" "$METADATA_PATH" "$TOC_PATH"
