#!/bin/bash
# shellcheck disable=SC2016 # Awk and sed programs intentionally use literal dollar fields.

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
umask 077
readonly TRUSTED_SYSTEM_PATH="/usr/bin:/bin:/usr/sbin:/sbin"
export PATH="$TRUSTED_SYSTEM_PATH"
readonly AWK_COMMAND=/usr/bin/awk
readonly BASENAME_COMMAND=/usr/bin/basename
readonly CHMOD_COMMAND=/bin/chmod
readonly DIRNAME_COMMAND=/usr/bin/dirname
readonly HEAD_COMMAND=/usr/bin/head
readonly LN_COMMAND=/bin/ln
readonly OD_COMMAND=/usr/bin/od
readonly RM_COMMAND=/bin/rm
readonly SED_COMMAND=/usr/bin/sed
readonly SHASUM_COMMAND=/usr/bin/shasum
readonly STAT_COMMAND=/usr/bin/stat
readonly TAIL_COMMAND=/usr/bin/tail
readonly TR_COMMAND=/usr/bin/tr
readonly WC_COMMAND=/usr/bin/wc

SCRIPT_DIRECTORY="$(cd -- "$("$DIRNAME_COMMAND" -- "$0")" && pwd -P)"
readonly HMAC_HELPER="$SCRIPT_DIRECTORY/render-profile-hmac.py"

INTENT=""
PLAN=""
PLAN_METADATA=""
PAIR_METADATA=""
RUN_INTENT=""
COMMAND_MANIFEST=""
ENVIRONMENT_MANIFEST=""
FONT_MANIFEST=""
INITIAL_GRID_MANIFEST=""
SUBJECT_IDENTITY=""
DRIVER_EVENTS=""
ACTION_VIDEO=""
RENDER_WORKLOAD_METADATA=""
HMAC_SECRET=""
OUTPUT=""
TEMP=""
BODY_TEMP=""
SECRET_FINGERPRINT=""
HMAC_KEY_IDENTIFIER=""
HMAC_RESULT=""
RENDER_TOOL_BUNDLE_MANIFEST=""
EXPECTED_SOURCE_COMMIT=""
TRUSTED_SOURCE_REPOSITORY=""

readonly INTENT_AUTH_DOMAIN='SPACETERM_RENDER_PROFILE_INTENT_V1'
readonly EVIDENCE_AUTH_DOMAIN='SPACETERM_RENDER_PROFILE_EVIDENCE_V1'
readonly CANONICALIZATION='utf8-lf-tab-kv-fixed-order-domain-nul-v1'
readonly DRIVER_HEADER=$'sequence\tcontinuous_ns\tevent_id\taction\ttarget_pid\twindow_number\trequested_a\trequested_b\tobserved_a\tobserved_b\tresult'
readonly INTENT_KEYS='format_version canonicalization auth_domain scenario subject campaign_id session_id nonce plan_sha256 plan_metadata_sha256 pair_metadata_sha256 run_intent_sha256 command_sha256 environment_sha256 font_sha256 initial_grid_sha256 subject_identity_sha256 subject_process_pid subject_process_start_identity expected_driver_events_path expected_driver_parent_device expected_driver_parent_inode action_video_path action_video_parent_device action_video_parent_inode final_metadata_path final_metadata_parent_device final_metadata_parent_inode warmup_ms measured_duration_ms required_action_count action_interval_ms hmac_key_identifier_sha256 intent_hmac_sha256'

usage() {
    cat <<EOF
Usage: $("$BASENAME_COMMAND" -- "$0") --intent FILE --plan FILE --plan-metadata FILE \\
  --pair-metadata FILE --run-intent FILE --command-manifest FILE \\
  --environment-manifest FILE --font-manifest FILE \\
  --initial-grid-manifest FILE --subject-identity FILE \\
  --driver-events FILE --action-video FILE --render-workload-metadata FILE \\
  --hmac-secret FILE --output FILE \
  --render-tool-bundle-manifest FILE --expected-source-commit SHA1 \
  --trusted-source-repository DIRECTORY

Validate the canonical raw driver stream against the pre-capture intent and
freeze authenticated post-capture render evidence at the exact path reserved
by that intent.
EOF
}

die() { echo "error: $*" >&2; exit 1; }
verify_self_bundle() {
    /usr/bin/python3 - "$RENDER_TOOL_BUNDLE_MANIFEST" "$EXPECTED_SOURCE_COMMIT" \
        "$TRUSTED_SOURCE_REPOSITORY" "${BASH_SOURCE[0]}" "$HMAC_HELPER" <<'PY'
import hashlib, pathlib, stat, subprocess, sys
manifest_raw, commit, repository_raw, self_raw, helper_raw = sys.argv[1:]
names = "record_release_performance_trace freeze_render_profile_intent finalize_render_profile_evidence render_profile_hmac render_trace_receipt analyze_release_render_profile_case archive_render_trace verify_render_action_video verify_render_trace_archive verify_release_performance_trace inspect_release_performance_process run_release_performance_command freeze_render_profile_tool_bundle".split()
relatives = "scripts/record-release-performance-trace.sh scripts/acceptance/freeze-render-profile-intent.sh scripts/acceptance/finalize-render-profile-evidence.sh scripts/acceptance/render-profile-hmac.py scripts/acceptance/render-trace-receipt.py scripts/acceptance/analyze-release-render-profile-case.sh scripts/acceptance/archive-render-trace.py scripts/acceptance/verify-render-action-video.py scripts/acceptance/verify-render-trace-archive.py scripts/verify-release-performance-trace.py scripts/inspect-release-performance-process.py scripts/run-release-performance-command.py scripts/acceptance/freeze-render-profile-tool-bundle.sh".split()
keys = ["format_version", "schema", "source_commit", "tool_count"]
for name in names: keys += [f"{name}_source_path", f"{name}_source_sha256", f"{name}_bundle_path", f"{name}_bundle_sha256"]
def frozen(raw, executable=False):
    path = pathlib.Path(raw); before = path.lstat()
    if (not path.is_absolute() or path.is_symlink() or path.resolve(strict=True) != path or not stat.S_ISREG(before.st_mode) or before.st_nlink != 1 or before.st_mode & 0o222 or executable and not before.st_mode & 0o111): raise SystemExit(1)
    payload = path.read_bytes(); after = path.lstat()
    if (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns) != (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns): raise SystemExit(1)
    return path, payload
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
for name, rel, invoked in (("finalize_render_profile_evidence", "scripts/acceptance/finalize-render-profile-evidence.sh", self_raw), ("render_profile_hmac", "scripts/acceptance/render-profile-hmac.py", helper_raw)):
    bundle, body = frozen(values[f"{name}_bundle_path"], executable=True)
    blob = subprocess.run(["/usr/bin/git", "--no-replace-objects", "-C", str(repository), "show", f"{commit}:{rel}"], capture_output=True, env={"PATH":"/usr/bin:/bin", "HOME":"/var/empty", "GIT_NO_REPLACE_OBJECTS":"1", "LC_ALL":"C"}); digest = hashlib.sha256(blob.stdout).hexdigest()
    if (blob.returncode or pathlib.Path(invoked).resolve(strict=True) != bundle or pathlib.Path(values[f"{name}_source_path"]) != repository / rel or values[f"{name}_source_sha256"] != digest or values[f"{name}_bundle_sha256"] != digest or hashlib.sha256(body).hexdigest() != digest): raise SystemExit(1)
PY
}
cleanup() {
    [[ -z "$TEMP" ]] || "$RM_COMMAND" -f -- "$TEMP"
    [[ -z "$BODY_TEMP" ]] || "$RM_COMMAND" -f -- "$BODY_TEMP"
}
sha256() { "$SHASUM_COMMAND" -a 256 "$1" | "$AWK_COMMAND" '{ print $1 }'; }
kv() {
    "$AWK_COMMAND" -F '\t' -v wanted="$2" '
        NF != 2 { bad = 1 }
        $1 == wanted { count += 1; value = $2 }
        END { if (!bad && count == 1) print value }
    ' "$1"
}

safe_value() {
    [[ -n "$1" && "$1" != *$'\t'* && "$1" != *$'\r'* && "$1" != *$'\n'* ]]
}

exact_schema() {
    local file="$1"
    local keys="$2"
    local label="$3"
    local last_byte
    last_byte="$("$TAIL_COMMAND" -c 1 "$file" | "$OD_COMMAND" -An -tx1 | "$TR_COMMAND" -d ' \n')"
    [[ "$last_byte" == 0a ]] || die "$label must end with LF"
    "$AWK_COMMAND" -F '\t' -v expected="$keys" '
        BEGIN { count = split(expected, keys, " ") }
        {
            if (NF != 2 || $1 != keys[NR] || $2 == "" || index($2, "\r") != 0) bad = 1
        }
        END { exit bad || NR != count }
    ' "$file" || die "$label schema or key order is invalid"
}

canonical_existing_path() {
    local requested="$1"
    local label="$2"
    local parent base physical_parent
    safe_value "$requested" || die "$label path is empty or contains control characters"
    [[ "$requested" == /* && "$requested" != */ ]] || die "$label path must be absolute"
    base="$("$BASENAME_COMMAND" -- "$requested")"
    parent="$("$DIRNAME_COMMAND" -- "$requested")"
    [[ -d "$parent" && ! -L "$parent" ]] || die "$label parent is unavailable or symbolic"
    physical_parent="$(cd -P -- "$parent" && pwd -P)"
    [[ "$requested" == "$physical_parent/$base" ]] \
        || die "$label path must be canonical and physical"
    [[ -f "$physical_parent/$base" && ! -L "$physical_parent/$base" ]] \
        || die "$label is unavailable or symbolic"
    printf '%s/%s\n' "$physical_parent" "$base"
}

canonical_absent_path() {
    local requested="$1"
    local label="$2"
    local parent base physical_parent
    safe_value "$requested" || die "$label path is empty or contains control characters"
    [[ "$requested" == /* && "$requested" != */ ]] || die "$label path must be absolute"
    base="$("$BASENAME_COMMAND" -- "$requested")"
    parent="$("$DIRNAME_COMMAND" -- "$requested")"
    [[ -d "$parent" && ! -L "$parent" ]] || die "$label parent is unavailable or symbolic"
    physical_parent="$(cd -P -- "$parent" && pwd -P)"
    [[ "$requested" == "$physical_parent/$base" ]] \
        || die "$label path must be canonical and physical"
    [[ ! -e "$physical_parent/$base" ]] || die "$label path already exists"
    printf '%s/%s\n' "$physical_parent" "$base"
}

immutable_hash() {
    local path="$1"
    local label="$2"
    local before after digest
    [[ -f "$path" && ! -L "$path" && -r "$path" && -s "$path" && ! -w "$path" ]] \
        || die "$label must be an immutable, nonempty regular file"
    before="$("$STAT_COMMAND" -f '%d:%i:%z:%m:%c' "$path")"
    digest="$(sha256 "$path")"
    after="$("$STAT_COMMAND" -f '%d:%i:%z:%m:%c' "$path")"
    [[ "$before" == "$after" && "$digest" =~ ^[0-9a-f]{64}$ ]] \
        || die "$label changed while it was hashed"
    printf '%s\n' "$digest"
}

hmac_body() {
    local domain="$1"
    local body="$2"
    local output fingerprint identifier digest
    output="$(/usr/bin/python3 "$HMAC_HELPER" --secret "$HMAC_SECRET" \
        --domain "$domain" --body "$body")" \
        || die "HMAC secret or authentication helper is invalid"
    [[ "$(printf '%s\n' "$output" | "$WC_COMMAND" -l | "$TR_COMMAND" -d ' ')" == 3 ]] \
        || die "authentication helper output is invalid"
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
        && "$digest" =~ ^[0-9a-f]{64}$ ]] \
        || die "authentication helper output is invalid"
    if [[ -n "$SECRET_FINGERPRINT" ]]; then
        [[ "$fingerprint" == "$SECRET_FINGERPRINT" \
            && "$identifier" == "$HMAC_KEY_IDENTIFIER" ]] \
            || die "HMAC secret changed during evidence finalization"
    else
        SECRET_FINGERPRINT="$fingerprint"
        HMAC_KEY_IDENTIFIER="$identifier"
    fi
    HMAC_RESULT="$digest"
}

while (( $# > 0 )); do
    case "$1" in
        --intent) INTENT="${2:-}"; shift ;;
        --plan) PLAN="${2:-}"; shift ;;
        --plan-metadata) PLAN_METADATA="${2:-}"; shift ;;
        --pair-metadata) PAIR_METADATA="${2:-}"; shift ;;
        --run-intent) RUN_INTENT="${2:-}"; shift ;;
        --command-manifest) COMMAND_MANIFEST="${2:-}"; shift ;;
        --environment-manifest) ENVIRONMENT_MANIFEST="${2:-}"; shift ;;
        --font-manifest) FONT_MANIFEST="${2:-}"; shift ;;
        --initial-grid-manifest) INITIAL_GRID_MANIFEST="${2:-}"; shift ;;
        --subject-identity) SUBJECT_IDENTITY="${2:-}"; shift ;;
        --driver-events) DRIVER_EVENTS="${2:-}"; shift ;;
        --action-video) ACTION_VIDEO="${2:-}"; shift ;;
        --render-workload-metadata) RENDER_WORKLOAD_METADATA="${2:-}"; shift ;;
        --hmac-secret) HMAC_SECRET="${2:-}"; shift ;;
        --render-tool-bundle-manifest) RENDER_TOOL_BUNDLE_MANIFEST="${2:-}"; shift ;;
        --expected-source-commit) EXPECTED_SOURCE_COMMIT="${2:-}"; shift ;;
        --trusted-source-repository) TRUSTED_SOURCE_REPOSITORY="${2:-}"; shift ;;
        --output) OUTPUT="${2:-}"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument: $1" ;;
    esac
    shift
done

verify_self_bundle || die "evidence finalizer is not the selected frozen bundle tool"

for command in "$AWK_COMMAND" "$BASENAME_COMMAND" "$CHMOD_COMMAND" \
    "$DIRNAME_COMMAND" "$LN_COMMAND" "$OD_COMMAND" "$RM_COMMAND" \
    "$SED_COMMAND" "$SHASUM_COMMAND" "$STAT_COMMAND" "$TAIL_COMMAND" \
    "$TR_COMMAND" "$WC_COMMAND"; do
    command -v "$command" >/dev/null 2>&1 || die "required command not found: $command"
done
intent_hash="$(immutable_hash "$INTENT" intent)"
exact_schema "$INTENT" "$INTENT_KEYS" intent
hmac_body "$INTENT_AUTH_DOMAIN" "$INTENT"
[[ "$(kv "$INTENT" format_version)" == 1 \
    && "$(kv "$INTENT" canonicalization)" == "$CANONICALIZATION" \
    && "$(kv "$INTENT" auth_domain)" == "$INTENT_AUTH_DOMAIN" \
    && "$(kv "$INTENT" hmac_key_identifier_sha256)" == "$HMAC_KEY_IDENTIFIER" ]] \
    || die "intent authentication parameters are invalid"

BODY_TEMP="${INTENT}.body.$$"
"$SED_COMMAND" '$d' "$INTENT" > "$BODY_TEMP"
hmac_body "$INTENT_AUTH_DOMAIN" "$BODY_TEMP"
expected_intent_hmac="$HMAC_RESULT"
[[ "$expected_intent_hmac" =~ ^[0-9a-f]{64}$ \
    && "$expected_intent_hmac" == "$(kv "$INTENT" intent_hmac_sha256)" ]] \
    || die "intent HMAC verification failed"
"$RM_COMMAND" -f -- "$BODY_TEMP"
BODY_TEMP=""

declare -a immutable_inputs=(
    "$PLAN" "$PLAN_METADATA" "$PAIR_METADATA" "$RUN_INTENT"
    "$COMMAND_MANIFEST" "$ENVIRONMENT_MANIFEST" "$FONT_MANIFEST"
    "$INITIAL_GRID_MANIFEST" "$SUBJECT_IDENTITY"
)
declare -a intent_hash_keys=(
    plan_sha256 plan_metadata_sha256 pair_metadata_sha256 run_intent_sha256
    command_sha256 environment_sha256 font_sha256 initial_grid_sha256
    subject_identity_sha256
)
for ((index = 0; index < ${#immutable_inputs[@]}; index += 1)); do
    [[ "$(immutable_hash "${immutable_inputs[index]}" "${intent_hash_keys[index]}")" \
        == "$(kv "$INTENT" "${intent_hash_keys[index]}")" ]] \
        || die "${intent_hash_keys[index]} no longer matches intent"
done

scenario="$(kv "$INTENT" scenario)"
subject="$(kv "$INTENT" subject)"
campaign_id="$(kv "$INTENT" campaign_id)"
session_id="$(kv "$INTENT" session_id)"
nonce="$(kv "$INTENT" nonce)"
process_pid="$(kv "$INTENT" subject_process_pid)"
process_start_identity="$(kv "$INTENT" subject_process_start_identity)"
warmup_ms="$(kv "$INTENT" warmup_ms)"
measured_duration_ms="$(kv "$INTENT" measured_duration_ms)"
required_action_count="$(kv "$INTENT" required_action_count)"
action_interval_ms="$(kv "$INTENT" action_interval_ms)"
[[ "$subject" == spaceterm || "$subject" == ghostty ]] || die "intent subject is invalid"
[[ "$process_pid" =~ ^[1-9][0-9]*$ \
    && "$warmup_ms" =~ ^[0-9]+$ && "$measured_duration_ms" =~ ^[1-9][0-9]*$ \
    && "$required_action_count" =~ ^[1-9][0-9]*$ \
    && "$action_interval_ms" =~ ^[1-9][0-9]*$ ]] \
    || die "intent numeric fields are invalid"
[[ "$(kv "$SUBJECT_IDENTITY" subject)" == "$subject" \
    && "$(kv "$SUBJECT_IDENTITY" process_pid)" == "$process_pid" \
    && "$(kv "$SUBJECT_IDENTITY" process_start_identity)" == "$process_start_identity" \
    && "$(kv "$SUBJECT_IDENTITY" identity_status)" == frozen ]] \
    || die "subject identity no longer matches intent"
readonly RUN_INTENT_KEYS='format_version subject subject_identity_sha256 scenario scenario_plan_sha256 workload_sha256 command_sha256 environment_sha256 font_sha256 initial_grid_sha256 measured_duration_ms process_pid process_start_identity campaign_id session_id nonce native_provisional_observation_sha256 evidence_mode status'
exact_schema "$RUN_INTENT" "$RUN_INTENT_KEYS" "run intent"
[[ "$(kv "$RUN_INTENT" format_version)" == 1 \
    && "$(kv "$RUN_INTENT" subject)" == "$subject" \
    && "$(kv "$RUN_INTENT" scenario)" == "$scenario" \
    && "$(kv "$RUN_INTENT" campaign_id)" == "$campaign_id" \
    && "$(kv "$RUN_INTENT" session_id)" == "$session_id" \
    && "$(kv "$RUN_INTENT" nonce)" == "$nonce" \
    && "$(kv "$RUN_INTENT" subject_identity_sha256)" \
        == "$(kv "$INTENT" subject_identity_sha256)" \
    && "$(kv "$RUN_INTENT" process_pid)" == "$process_pid" \
    && "$(kv "$RUN_INTENT" process_start_identity)" == "$process_start_identity" \
    && "$(kv "$RUN_INTENT" evidence_mode)" == production \
    && "$(kv "$RUN_INTENT" status)" == prepared ]] \
    || die "run intent no longer binds the render campaign"

expected_driver_path="$(kv "$INTENT" expected_driver_events_path)"
driver_path="$(canonical_existing_path "$DRIVER_EVENTS" "driver events")"
[[ "$driver_path" == "$expected_driver_path" ]] \
    || die "driver event path does not match intent"
driver_parent="$("$DIRNAME_COMMAND" -- "$driver_path")"
[[ "$("$STAT_COMMAND" -f '%d' "$driver_parent")" == "$(kv "$INTENT" expected_driver_parent_device)" \
    && "$("$STAT_COMMAND" -f '%i' "$driver_parent")" == "$(kv "$INTENT" expected_driver_parent_inode)" ]] \
    || die "driver event parent identity changed after intent"
driver_hash="$(immutable_hash "$driver_path" "driver events")"
driver_identity="$("$STAT_COMMAND" -f '%d:%i' "$driver_path")"
[[ "$("$HEAD_COMMAND" -n 1 "$driver_path")" == "$DRIVER_HEADER" ]] \
    || die "driver event header is invalid"

# The native driver must execute every frozen plan event once, in order, for
# the frozen PID and one verified window. Relative event timing is allowed at
# most 250 ms of error so the recorded action cadence cannot be self-claimed.
"$AWK_COMMAND" -F '\t' -v pid="$process_pid" '
    NR == FNR {
        if (FNR > 1) {
            ids[++plan_count] = $1
            offsets[plan_count] = $2 + 0
            actions[plan_count] = $3
            arguments0[plan_count] = $4
            arguments1[plan_count] = $5
        }
        next
    }
    FNR == 1 { next }
    {
        event_count += 1
        if (NF != 11 || $1 !~ /^[0-9]+$/ || $2 !~ /^[0-9]+$/ \
            || $5 !~ /^[1-9][0-9]*$/ || $6 !~ /^[1-9][0-9]*$/ \
            || $7 !~ /^-?[0-9]+$/ || $8 !~ /^-?[0-9]+$/ \
            || $9 !~ /^-?[0-9]+$/ || $10 !~ /^-?[0-9]+$/ \
            || $1 + 0 != event_count - 1 || $5 != pid || $11 != "verified" \
            || event_count > plan_count || $3 != ids[event_count] \
            || $4 != actions[event_count] \
            || $7 != arguments0[event_count] || $8 != arguments1[event_count] \
            || (event_count > 1 && $2 + 0 <= prior_time)) bad = 1
        if (event_count == 1) {
            first_time = $2 + 0
            first_offset = offsets[event_count]
            window = $6
        } else {
            expected_delta = (offsets[event_count] - first_offset) * 1000000
            observed_delta = $2 + 0 - first_time
            cadence_error = observed_delta - expected_delta
            if (cadence_error < 0) cadence_error = -cadence_error
            if ($6 != window || cadence_error > 250000000) bad = 1
        }
        if ($4 == "resize-grid") {
            delta_a = $9 - $7
            delta_b = $10 - $8
            if (delta_a < 0) delta_a = -delta_a
            if (delta_b < 0) delta_b = -delta_b
            if (delta_a > 8 || delta_b > 8) bad = 1
        } else if ($4 == "checkpoint" || $4 == "stop") {
            if ($9 != 1 || ($10 != 0 && $10 != 1)) bad = 1
        }
        prior_time = $2 + 0
    }
    END { exit bad || event_count != plan_count }
' "$PLAN" "$driver_path" || die "driver events do not prove exact plan execution"

measured_started="$("$AWK_COMMAND" -F '\t' '$3 == "measured-start" { count += 1; value = $2 } \
    END { if (count == 1) print value }' "$driver_path")"
measured_ended="$("$AWK_COMMAND" -F '\t' '$3 == "measured-end" { count += 1; value = $2 } \
    END { if (count == 1) print value }' "$driver_path")"
[[ "$measured_started" =~ ^[0-9]+$ && "$measured_ended" =~ ^[0-9]+$ \
    && "$measured_ended" -ge "$((measured_started + measured_duration_ms * 1000000))" \
    && "$measured_ended" -le "$((measured_started + (measured_duration_ms + 2000) * 1000000))" ]] \
    || die "driver events do not cover the intended measured interval"

case "$scenario" in
    perf-render-idle-cursor-blink) action_prefix=cursor-blink ;;
    perf-render-text-blink) action_prefix=text-blink ;;
    perf-render-sustained-output) action_prefix=changed-row ;;
    perf-render-selection) action_prefix=selection-overlay ;;
    perf-render-marked-text) action_prefix=marked-text-overlay ;;
    perf-render-live-resize) action_prefix=resize ;;
    *) die "intent render scenario is invalid" ;;
esac
completed_action_count="$("$AWK_COMMAND" -F '\t' \
    -v prefix="^${action_prefix}-[0-9][0-9][0-9]$" \
    '$3 ~ prefix && $11 == "verified" { count += 1 } END { print count + 0 }' \
    "$driver_path")"
[[ "$completed_action_count" == "$required_action_count" ]] \
    || die "driver event action count does not match intent"
if [[ "$scenario" == perf-render-live-resize ]]; then
    [[ "$("$AWK_COMMAND" -F '\t' '$3 ~ /^resize-[0-9][0-9][0-9]$/ \
        && $4 == "resize-grid" && $11 == "verified" { count += 1 } \
        END { print count + 0 }' "$driver_path")" == "$required_action_count" ]] \
        || die "native live-resize evidence is incomplete"
fi

# Detect mutation that occurred during semantic validation, after the first
# stable hash was taken.
[[ "$(immutable_hash "$driver_path" "driver events")" == "$driver_hash" ]] \
    || die "driver events changed during validation"
[[ "$("$STAT_COMMAND" -f '%d:%i' "$driver_path")" == "$driver_identity" ]] \
    || die "driver event identity changed during validation"
driver_device="${driver_identity%%:*}"
driver_inode="${driver_identity#*:}"
action_video_path="$(canonical_existing_path "$ACTION_VIDEO" "action video")"
[[ "$action_video_path" == "$(kv "$INTENT" action_video_path)" ]] \
    || die "action video path does not match intent"
action_video_parent="$("$DIRNAME_COMMAND" -- "$action_video_path")"
[[ "$("$STAT_COMMAND" -f '%d' "$action_video_parent")" == "$(kv "$INTENT" action_video_parent_device)" \
    && "$("$STAT_COMMAND" -f '%i' "$action_video_parent")" == "$(kv "$INTENT" action_video_parent_inode)" ]] \
    || die "action video parent identity changed after intent"
action_video_hash="$(immutable_hash "$action_video_path" "action video")"
action_video_identity="$("$STAT_COMMAND" -f '%d:%i' "$action_video_path")"
[[ "$(immutable_hash "$action_video_path" "action video")" == "$action_video_hash" \
    && "$("$STAT_COMMAND" -f '%d:%i' "$action_video_path")" == "$action_video_identity" ]] \
    || die "action video changed during validation"
action_video_device="${action_video_identity%%:*}"
action_video_inode="${action_video_identity#*:}"

render_workload_metadata_hash="$(immutable_hash "$RENDER_WORKLOAD_METADATA" \
    "render workload metadata")"
[[ "$(kv "$RENDER_WORKLOAD_METADATA" format_version)" == 1 \
    && "$(kv "$RENDER_WORKLOAD_METADATA" scenario)" == "$scenario" \
    && "$(kv "$RENDER_WORKLOAD_METADATA" subject)" == "$subject" \
    && "$(kv "$RENDER_WORKLOAD_METADATA" subject_identity_sha256)" \
        == "$(kv "$INTENT" subject_identity_sha256)" \
    && "$(kv "$RENDER_WORKLOAD_METADATA" pair_metadata_sha256)" \
        == "$(kv "$INTENT" pair_metadata_sha256)" \
    && "$(kv "$RENDER_WORKLOAD_METADATA" driver_events_sha256)" == "$driver_hash" \
    && "$(kv "$RENDER_WORKLOAD_METADATA" action_video_sha256)" == "$action_video_hash" \
    && "$(kv "$RENDER_WORKLOAD_METADATA" required_action_count)" \
        == "$required_action_count" \
    && "$(kv "$RENDER_WORKLOAD_METADATA" completed_action_count)" \
        == "$completed_action_count" \
    && "$(kv "$RENDER_WORKLOAD_METADATA" started_continuous_ns)" \
        == "$measured_started" \
    && "$(kv "$RENDER_WORKLOAD_METADATA" ended_continuous_ns)" \
        == "$measured_ended" \
    && "$(kv "$RENDER_WORKLOAD_METADATA" status)" == complete ]] \
    || die "render workload metadata does not bind final evidence"

output_path="$(canonical_absent_path "$OUTPUT" "final metadata")"
[[ "$output_path" == "$(kv "$INTENT" final_metadata_path)" ]] \
    || die "final metadata path does not match intent"
output_parent="$("$DIRNAME_COMMAND" -- "$output_path")"
output_parent_identity="$("$STAT_COMMAND" -f '%d:%i' "$output_parent")"
[[ "${output_parent_identity%%:*}" == "$(kv "$INTENT" final_metadata_parent_device)" \
    && "${output_parent_identity#*:}" == "$(kv "$INTENT" final_metadata_parent_inode)" ]] \
    || die "final metadata parent identity changed after intent"

TEMP="${output_path}.tmp.$$"
trap cleanup EXIT INT TERM
{
    printf 'format_version\t1\n'
    printf 'canonicalization\t%s\n' "$CANONICALIZATION"
    printf 'auth_domain\t%s\n' "$EVIDENCE_AUTH_DOMAIN"
    printf 'intent_sha256\t%s\n' "$intent_hash"
    printf 'scenario\t%s\n' "$scenario"
    printf 'subject\t%s\n' "$subject"
    printf 'campaign_id\t%s\n' "$campaign_id"
    printf 'session_id\t%s\n' "$session_id"
    printf 'nonce\t%s\n' "$nonce"
    printf 'subject_identity_sha256\t%s\n' "$(kv "$INTENT" subject_identity_sha256)"
    printf 'subject_process_pid\t%s\n' "$process_pid"
    printf 'subject_process_start_identity\t%s\n' "$process_start_identity"
    printf 'driver_events_path\t%s\n' "$driver_path"
    printf 'driver_events_device\t%s\n' "$driver_device"
    printf 'driver_events_inode\t%s\n' "$driver_inode"
    printf 'driver_events_sha256\t%s\n' "$driver_hash"
    printf 'action_video_path\t%s\n' "$action_video_path"
    printf 'action_video_device\t%s\n' "$action_video_device"
    printf 'action_video_inode\t%s\n' "$action_video_inode"
    printf 'action_video_sha256\t%s\n' "$action_video_hash"
    printf 'render_workload_metadata_sha256\t%s\n' "$render_workload_metadata_hash"
    printf 'required_action_count\t%s\n' "$required_action_count"
    printf 'completed_action_count\t%s\n' "$completed_action_count"
    printf 'action_interval_ms\t%s\n' "$action_interval_ms"
    printf 'started_continuous_ns\t%s\n' "$measured_started"
    printf 'ended_continuous_ns\t%s\n' "$measured_ended"
    printf 'measured_span_ns\t%s\n' "$((measured_ended - measured_started))"
    printf 'result\tverified\n'
    printf 'hmac_key_identifier_sha256\t%s\n' "$HMAC_KEY_IDENTIFIER"
} > "$TEMP"
hmac_body "$EVIDENCE_AUTH_DOMAIN" "$TEMP"
evidence_hmac="$HMAC_RESULT"
[[ "$evidence_hmac" =~ ^[0-9a-f]{64}$ ]] || die "could not authenticate final evidence"
printf 'evidence_hmac_sha256\t%s\n' "$evidence_hmac" >> "$TEMP"
"$CHMOD_COMMAND" 0444 "$TEMP"
[[ ! -e "$output_path" && "$("$STAT_COMMAND" -f '%d:%i' "$output_parent")" \
    == "$output_parent_identity" ]] \
    || die "final metadata path or parent identity changed during finalization"
"$LN_COMMAND" "$TEMP" "$output_path" || die "final metadata path was created concurrently"
"$RM_COMMAND" -f -- "$TEMP"
TEMP=""
trap - EXIT INT TERM
printf 'render_profile_evidence_sha256\t%s\n' "$(sha256 "$output_path")"
