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
readonly DIRNAME_COMMAND=/usr/bin/dirname
readonly LN_COMMAND=/bin/ln
readonly MKDIR_COMMAND=/bin/mkdir
readonly RM_COMMAND=/bin/rm
readonly SHASUM_COMMAND=/usr/bin/shasum
readonly STAT_COMMAND=/usr/bin/stat
readonly TR_COMMAND=/usr/bin/tr
readonly WC_COMMAND=/usr/bin/wc

SCRIPT_DIRECTORY="$(cd -- "$("$DIRNAME_COMMAND" -- "$0")" && pwd -P)"
readonly HMAC_HELPER="$SCRIPT_DIRECTORY/render-profile-hmac.py"

SUBJECT=""
SCENARIO=""
CAMPAIGN_ID=""
SESSION_ID=""
NONCE=""
PLAN=""
PLAN_METADATA=""
PAIR_METADATA=""
RUN_INTENT=""
COMMAND_MANIFEST=""
ENVIRONMENT_MANIFEST=""
FONT_MANIFEST=""
INITIAL_GRID_MANIFEST=""
SUBJECT_IDENTITY=""
EXPECTED_DRIVER_EVENTS=""
ACTION_VIDEO=""
FINAL_METADATA=""
HMAC_SECRET=""
OUTPUT=""
TEMP=""
SECRET_FINGERPRINT=""
HMAC_KEY_IDENTIFIER=""
HMAC_RESULT=""
RENDER_TOOL_BUNDLE_MANIFEST=""
EXPECTED_SOURCE_COMMIT=""
TRUSTED_SOURCE_REPOSITORY=""

readonly AUTH_DOMAIN='SPACETERM_RENDER_PROFILE_INTENT_V1'
readonly CANONICALIZATION='utf8-lf-tab-kv-fixed-order-domain-nul-v1'

usage() {
    cat <<EOF
Usage: $("$BASENAME_COMMAND" -- "$0") --subject spaceterm|ghostty --scenario NAME \\
  --campaign-id ID --session-id ID --nonce 64_HEX \\
  --plan FILE --plan-metadata FILE --pair-metadata FILE --run-intent FILE \\
  --command-manifest FILE --environment-manifest FILE --font-manifest FILE \\
  --initial-grid-manifest FILE --subject-identity FILE \\
  --expected-driver-events ABSENT_PATH --action-video ABSENT_PATH \\
  --final-metadata ABSENT_PATH \\
  --hmac-secret FILE --output ABSENT_PATH \
  --render-tool-bundle-manifest FILE --expected-source-commit SHA1 \
  --trusted-source-repository DIRECTORY

Freeze the authenticated, pre-capture render-profile intent. The expected raw
driver stream and final evidence paths must not exist; their physical parent
directory identities are bound before capture starts.
EOF
}

die() { echo "error: $*" >&2; exit 1; }
verify_self_bundle() {
    /usr/bin/python3 - "$RENDER_TOOL_BUNDLE_MANIFEST" "$EXPECTED_SOURCE_COMMIT" \
        "$TRUSTED_SOURCE_REPOSITORY" "$1" "$2" "${BASH_SOURCE[0]}" "$HMAC_HELPER" <<'PY'
import hashlib, pathlib, stat, subprocess, sys
manifest_raw, commit, repository_raw, logical, relative, self_raw, helper_raw = sys.argv[1:]
names = "record_release_performance_trace freeze_render_profile_intent finalize_render_profile_evidence render_profile_hmac render_trace_receipt analyze_release_render_profile_case archive_render_trace verify_render_action_video verify_render_trace_archive verify_release_performance_trace inspect_release_performance_process run_release_performance_command freeze_render_profile_tool_bundle".split()
relatives = "scripts/record-release-performance-trace.sh scripts/acceptance/freeze-render-profile-intent.sh scripts/acceptance/finalize-render-profile-evidence.sh scripts/acceptance/render-profile-hmac.py scripts/acceptance/render-trace-receipt.py scripts/acceptance/analyze-release-render-profile-case.sh scripts/acceptance/archive-render-trace.py scripts/acceptance/verify-render-action-video.py scripts/acceptance/verify-render-trace-archive.py scripts/verify-release-performance-trace.py scripts/inspect-release-performance-process.py scripts/run-release-performance-command.py scripts/acceptance/freeze-render-profile-tool-bundle.sh".split()
keys = ["format_version", "schema", "source_commit", "tool_count"]
for name in names:
    keys += [f"{name}_source_path", f"{name}_source_sha256", f"{name}_bundle_path", f"{name}_bundle_sha256"]
def frozen(raw, executable=False):
    path = pathlib.Path(raw); before = path.lstat()
    if (not path.is_absolute() or path.is_symlink() or path.resolve(strict=True) != path
            or not stat.S_ISREG(before.st_mode) or before.st_nlink != 1
            or before.st_mode & 0o222 or executable and not before.st_mode & 0o111):
        raise SystemExit(1)
    payload = path.read_bytes(); after = path.lstat()
    if (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns) != (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns):
        raise SystemExit(1)
    return path, payload
manifest, payload = frozen(manifest_raw)
lines = payload.splitlines()
if not payload.endswith(b"\n") or len(lines) != len(keys): raise SystemExit(1)
values = {}
for key, line in zip(keys, lines):
    try: actual, value = line.split(b"\t", 1); actual = actual.decode("ascii"); value = value.decode()
    except (ValueError, UnicodeDecodeError): raise SystemExit(1)
    if actual != key or not value or "\t" in value or "\r" in value: raise SystemExit(1)
    values[key] = value
repository = pathlib.Path(repository_raw)
if (not repository.is_absolute() or repository.is_symlink() or repository.resolve(strict=True) != repository
        or values["format_version"] != "1" or values["schema"] != "spaceterm.render-profile-tool-bundle/v1"
        or values["source_commit"] != commit or values["tool_count"] != "13"): raise SystemExit(1)
for name, rel, invoked in ((logical, relative, self_raw), ("render_profile_hmac", "scripts/acceptance/render-profile-hmac.py", helper_raw)):
    bundle, body = frozen(values[f"{name}_bundle_path"], executable=True)
    blob = subprocess.run(["/usr/bin/git", "--no-replace-objects", "-C", str(repository), "show", f"{commit}:{rel}"], capture_output=True, env={"PATH":"/usr/bin:/bin", "HOME":"/var/empty", "GIT_NO_REPLACE_OBJECTS":"1", "LC_ALL":"C"})
    digest = hashlib.sha256(blob.stdout).hexdigest()
    if (blob.returncode or pathlib.Path(invoked).resolve(strict=True) != bundle
            or pathlib.Path(values[f"{name}_source_path"]) != repository / rel
            or values[f"{name}_source_sha256"] != digest
            or values[f"{name}_bundle_sha256"] != digest or hashlib.sha256(body).hexdigest() != digest): raise SystemExit(1)
PY
}
cleanup() {
    [[ -z "$TEMP" ]] || "$RM_COMMAND" -f -- "$TEMP"
}
sha256() { "$SHASUM_COMMAND" -a 256 "$1" | "$AWK_COMMAND" '{ print $1 }'; }
kv() {
    "$AWK_COMMAND" -F '\t' -v wanted="$2" '
        NF != 2 { bad = 1 }
        $1 == wanted { count += 1; value = $2 }
        END { if (!bad && count == 1) print value }
    ' "$1"
}
exact_schema() {
    local file="$1" keys="$2" count="$3"
    "$AWK_COMMAND" -F '\t' -v keys="$keys" -v count="$count" '
        BEGIN { split(keys, wanted, " ") }
        NF != 2 || NR > count || $1 != wanted[NR] { exit 1 }
        END { if (NR != count) exit 1 }
    ' "$file"
}

safe_value() {
    [[ -n "$1" && "$1" != *$'\t'* && "$1" != *$'\r'* && "$1" != *$'\n'* ]]
}

canonical_absent_path() {
    local requested="$1"
    local label="$2"
    local parent base physical_parent
    safe_value "$requested" || die "$label path is empty or contains control characters"
    [[ "$requested" == /* && "$requested" != */ ]] || die "$label path must be absolute"
    base="$("$BASENAME_COMMAND" -- "$requested")"
    [[ "$base" != . && "$base" != .. ]] || die "$label path must name a file"
    parent="$("$DIRNAME_COMMAND" -- "$requested")"
    [[ -d "$parent" && ! -L "$parent" ]] || die "$label parent is unavailable or symbolic"
    physical_parent="$(cd -P -- "$parent" && pwd -P)"
    [[ "$requested" == "$physical_parent/$base" ]] \
        || die "$label path must be canonical and physical"
    [[ -n "$physical_parent" && ! -e "$physical_parent/$base" ]] \
        || die "$label path already exists"
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
    local body="$1"
    local output fingerprint identifier digest
    output="$(/usr/bin/python3 "$HMAC_HELPER" --secret "$HMAC_SECRET" \
        --domain "$AUTH_DOMAIN" --body "$body")" \
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
            || die "HMAC secret changed during intent freezing"
    else
        SECRET_FINGERPRINT="$fingerprint"
        HMAC_KEY_IDENTIFIER="$identifier"
    fi
    HMAC_RESULT="$digest"
}

validate_plan() {
    local prefix="$1"
    local expected_action="$2"
    "$AWK_COMMAND" -F '\t' -v scenario="$SCENARIO" -v prefix="$prefix" \
        -v action="$expected_action" -v warmup="$warmup_ms" \
        -v duration="$measured_duration_ms" -v interval="$action_interval_ms" \
        -v count="$required_action_count" '
        NR == 1 {
            if ($0 != "event_id\toffset_ms\taction\targ0\targ1") bad = 1
            next
        }
        {
            row += 1
            if (NF != 5) bad = 1
            if (row == 1 && scenario != "perf-render-live-resize") {
                if ($1 != "profile-warmup-start" || $2 + 0 != 0 \
                    || $3 != "checkpoint" || $4 != 0 || $5 != 0) bad = 1
                next
            }
            base = scenario == "perf-render-live-resize" ? 0 : 1
            if (row == base + 1) {
                if ($1 != "measured-start" || $2 + 0 != warmup \
                    || $3 != "checkpoint" || $4 != 0 || $5 != 0) bad = 1
                next
            }
            action_index = row - (base + 2)
            if (action_index >= 0 && action_index < count) {
                expected_id = sprintf("%s-%03d", prefix, action_index)
                if ($1 != expected_id || $2 + 0 != warmup + action_index * interval \
                    || $3 != action) bad = 1
                if (action == "checkpoint") {
                    if ($4 + 0 != action_index || $5 != 0) bad = 1
                } else {
                    sign = action_index % 2 == 0 ? 1 : -1
                    modulo = action_index % 3
                    expected_a = modulo == 0 ? sign * 120 : (modulo == 2 ? sign * 96 : 0)
                    expected_b = modulo == 1 ? sign * 80 : (modulo == 2 ? sign * 64 : 0)
                    if ($4 + 0 != expected_a || $5 + 0 != expected_b) bad = 1
                }
                next
            }
            if (row == base + count + 2) {
                if ($1 != "measured-end" || $2 + 0 != warmup + duration \
                    || $3 != "checkpoint" || $4 != 0 || $5 != 0) bad = 1
                next
            }
            if (row == base + count + 3) {
                if ($1 != "stop" || $2 + 0 != warmup + duration \
                    || $3 != "stop" || $4 != 0 || $5 != 0) bad = 1
                next
            }
            bad = 1
        }
        END { exit bad || row != (scenario == "perf-render-live-resize" ? count + 3 : count + 4) }
    ' "$PLAN" || die "render plan actions or cadence are not canonical"
}

while (( $# > 0 )); do
    case "$1" in
        --subject) SUBJECT="${2:-}"; shift ;;
        --scenario) SCENARIO="${2:-}"; shift ;;
        --campaign-id) CAMPAIGN_ID="${2:-}"; shift ;;
        --session-id) SESSION_ID="${2:-}"; shift ;;
        --nonce) NONCE="${2:-}"; shift ;;
        --plan) PLAN="${2:-}"; shift ;;
        --plan-metadata) PLAN_METADATA="${2:-}"; shift ;;
        --pair-metadata) PAIR_METADATA="${2:-}"; shift ;;
        --run-intent) RUN_INTENT="${2:-}"; shift ;;
        --command-manifest) COMMAND_MANIFEST="${2:-}"; shift ;;
        --environment-manifest) ENVIRONMENT_MANIFEST="${2:-}"; shift ;;
        --font-manifest) FONT_MANIFEST="${2:-}"; shift ;;
        --initial-grid-manifest) INITIAL_GRID_MANIFEST="${2:-}"; shift ;;
        --subject-identity) SUBJECT_IDENTITY="${2:-}"; shift ;;
        --expected-driver-events) EXPECTED_DRIVER_EVENTS="${2:-}"; shift ;;
        --action-video) ACTION_VIDEO="${2:-}"; shift ;;
        --final-metadata) FINAL_METADATA="${2:-}"; shift ;;
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

verify_self_bundle freeze_render_profile_intent \
    scripts/acceptance/freeze-render-profile-intent.sh \
    || die "intent freezer is not the selected frozen bundle tool"

[[ "$SUBJECT" == spaceterm || "$SUBJECT" == ghostty ]] || die "invalid subject"
[[ "$CAMPAIGN_ID" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$ \
    && "$SESSION_ID" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$ ]] \
    || die "campaign and session IDs must be safe identifiers"
[[ "$NONCE" =~ ^[0-9a-f]{64}$ ]] || die "nonce must be 64 lowercase hex characters"
for command in "$AWK_COMMAND" "$BASENAME_COMMAND" "$CHMOD_COMMAND" \
    "$DIRNAME_COMMAND" "$LN_COMMAND" "$MKDIR_COMMAND" "$RM_COMMAND" \
    "$SHASUM_COMMAND" "$STAT_COMMAND" "$TR_COMMAND" "$WC_COMMAND"; do
    command -v "$command" >/dev/null 2>&1 || die "required command not found: $command"
done

case "$SCENARIO" in
    perf-render-idle-cursor-blink)
        warmup_ms=15000; measured_duration_ms=120000
        required_action_count=60; action_interval_ms=2000
        action_prefix=cursor-blink; plan_action=checkpoint ;;
    perf-render-text-blink)
        warmup_ms=15000; measured_duration_ms=120000
        required_action_count=60; action_interval_ms=2000
        action_prefix=text-blink; plan_action=checkpoint ;;
    perf-render-sustained-output)
        warmup_ms=30000; measured_duration_ms=180000
        required_action_count=18; action_interval_ms=10000
        action_prefix=changed-row; plan_action=checkpoint ;;
    perf-render-selection)
        warmup_ms=15000; measured_duration_ms=120000
        required_action_count=30; action_interval_ms=4000
        action_prefix=selection-overlay; plan_action=checkpoint ;;
    perf-render-marked-text)
        warmup_ms=15000; measured_duration_ms=120000
        required_action_count=24; action_interval_ms=5000
        action_prefix=marked-text-overlay; plan_action=checkpoint ;;
    perf-render-live-resize)
        warmup_ms=0; measured_duration_ms=180000
        required_action_count=180; action_interval_ms=1000
        action_prefix=resize; plan_action=resize-grid ;;
    *) die "invalid render scenario" ;;
esac

expected_driver_path="$(canonical_absent_path "$EXPECTED_DRIVER_EVENTS" \
    "expected driver events")"
final_metadata_path="$(canonical_absent_path "$FINAL_METADATA" "final metadata")"
action_video_path="$(canonical_absent_path "$ACTION_VIDEO" "action video")"
output_path="$(canonical_absent_path "$OUTPUT" "intent output")"
[[ "$expected_driver_path" != "$final_metadata_path" \
    && "$expected_driver_path" != "$action_video_path" \
    && "$expected_driver_path" != "$output_path" \
    && "$action_video_path" != "$final_metadata_path" \
    && "$action_video_path" != "$output_path" \
    && "$final_metadata_path" != "$output_path" ]] \
    || die "intent, driver, video, and final metadata paths must differ"

declare -a immutable_inputs=(
    "$PLAN" "$PLAN_METADATA" "$PAIR_METADATA" "$RUN_INTENT"
    "$COMMAND_MANIFEST" "$ENVIRONMENT_MANIFEST" "$FONT_MANIFEST"
    "$INITIAL_GRID_MANIFEST" "$SUBJECT_IDENTITY"
)
declare -a immutable_labels=(
    plan plan-metadata pair-metadata run-intent command-manifest
    environment-manifest font-manifest initial-grid-manifest subject-identity
)
declare -a immutable_hashes=()
for ((index = 0; index < ${#immutable_inputs[@]}; index += 1)); do
    immutable_hashes+=("$(immutable_hash "${immutable_inputs[index]}" \
        "${immutable_labels[index]}")")
done
plan_hash="${immutable_hashes[0]}"
plan_metadata_hash="${immutable_hashes[1]}"
pair_hash="${immutable_hashes[2]}"
run_hash="${immutable_hashes[3]}"
command_hash="${immutable_hashes[4]}"
environment_hash="${immutable_hashes[5]}"
font_hash="${immutable_hashes[6]}"
grid_hash="${immutable_hashes[7]}"
subject_hash="${immutable_hashes[8]}"

[[ "$(kv "$PLAN_METADATA" format_version)" == 2 \
    && "$(kv "$PLAN_METADATA" scenario)" == "$SCENARIO" \
    && "$(kv "$PLAN_METADATA" plan_sha256)" == "$plan_hash" \
    && "$(kv "$PLAN_METADATA" warmup_ms)" == "$warmup_ms" \
    && "$(kv "$PLAN_METADATA" measured_duration_ms)" == "$measured_duration_ms" \
    && "$(kv "$PLAN_METADATA" required_action_count)" == "$required_action_count" ]] \
    || die "plan metadata does not match the canonical render scenario"
validate_plan "$action_prefix" "$plan_action"

[[ "$(kv "$PAIR_METADATA" format_version)" == 1 \
    && "$(kv "$PAIR_METADATA" scenario)" == "$SCENARIO" \
    && "$(kv "$PAIR_METADATA" plan_sha256)" == "$plan_hash" \
    && "$(kv "$PAIR_METADATA" command_sha256)" == "$command_hash" \
    && "$(kv "$PAIR_METADATA" environment_sha256)" == "$environment_hash" \
    && "$(kv "$PAIR_METADATA" font_sha256)" == "$font_hash" \
    && "$(kv "$PAIR_METADATA" initial_grid_sha256)" == "$grid_hash" \
    && "$(kv "$PAIR_METADATA" duration_ms)" == "$measured_duration_ms" \
    && "$(kv "$PAIR_METADATA" "${SUBJECT}_subject_identity_sha256")" == "$subject_hash" ]] \
    || die "pair metadata does not bind the render inputs"
[[ "$(kv "$SUBJECT_IDENTITY" format_version)" == 1 \
    && "$(kv "$SUBJECT_IDENTITY" subject)" == "$SUBJECT" \
    && "$(kv "$SUBJECT_IDENTITY" identity_status)" == frozen ]] \
    || die "subject identity is not frozen"
process_pid="$(kv "$SUBJECT_IDENTITY" process_pid)"
process_start_identity="$(kv "$SUBJECT_IDENTITY" process_start_identity)"
[[ "$process_pid" =~ ^[1-9][0-9]*$ && -n "$process_start_identity" ]] \
    || die "subject process identity is invalid"
readonly RUN_INTENT_KEYS="format_version subject subject_identity_sha256 scenario scenario_plan_sha256 workload_sha256 command_sha256 environment_sha256 font_sha256 initial_grid_sha256 measured_duration_ms process_pid process_start_identity campaign_id session_id nonce native_provisional_observation_sha256 evidence_mode status"
exact_schema "$RUN_INTENT" "$RUN_INTENT_KEYS" 19 \
    || die "run intent is not exact19"
[[ "$(kv "$RUN_INTENT" format_version)" == 1 \
    && "$(kv "$RUN_INTENT" subject)" == "$SUBJECT" \
    && "$(kv "$RUN_INTENT" subject_identity_sha256)" == "$subject_hash" \
    && "$(kv "$RUN_INTENT" scenario)" == "$SCENARIO" \
    && "$(kv "$RUN_INTENT" scenario_plan_sha256)" == "$plan_hash" \
    && "$(kv "$RUN_INTENT" command_sha256)" == "$command_hash" \
    && "$(kv "$RUN_INTENT" environment_sha256)" == "$environment_hash" \
    && "$(kv "$RUN_INTENT" font_sha256)" == "$font_hash" \
    && "$(kv "$RUN_INTENT" initial_grid_sha256)" == "$grid_hash" \
    && "$(kv "$RUN_INTENT" measured_duration_ms)" == "$measured_duration_ms" \
    && "$(kv "$RUN_INTENT" process_pid)" == "$process_pid" \
    && "$(kv "$RUN_INTENT" process_start_identity)" == "$process_start_identity" \
    && "$(kv "$RUN_INTENT" campaign_id)" == "$CAMPAIGN_ID" \
    && "$(kv "$RUN_INTENT" session_id)" == "$SESSION_ID" \
    && "$(kv "$RUN_INTENT" nonce)" == "$NONCE" \
    && "$(kv "$RUN_INTENT" evidence_mode)" == production \
    && "$(kv "$RUN_INTENT" status)" == prepared ]] \
    || die "run intent does not bind the render inputs and subject"
if [[ "$SUBJECT" == spaceterm ]]; then
    [[ "$(kv "$RUN_INTENT" native_provisional_observation_sha256)" =~ ^[0-9a-f]{64}$ ]] \
        || die "SpaceTerm run intent lacks native launch evidence"
else
    [[ "$(kv "$RUN_INTENT" native_provisional_observation_sha256)" == not-applicable ]] \
        || die "Ghostty run intent contains SpaceTerm native evidence"
fi

hmac_body "$PLAN"
expected_driver_parent="$("$DIRNAME_COMMAND" -- "$expected_driver_path")"
action_video_parent="$("$DIRNAME_COMMAND" -- "$action_video_path")"
final_metadata_parent="$("$DIRNAME_COMMAND" -- "$final_metadata_path")"
expected_driver_parent_identity="$("$STAT_COMMAND" -f '%d:%i' "$expected_driver_parent")"
action_video_parent_identity="$("$STAT_COMMAND" -f '%d:%i' "$action_video_parent")"
final_metadata_parent_identity="$("$STAT_COMMAND" -f '%d:%i' "$final_metadata_parent")"
"$MKDIR_COMMAND" -p -- "$("$DIRNAME_COMMAND" -- "$output_path")"
TEMP="${output_path}.tmp.$$"
trap cleanup EXIT INT TERM
{
    printf 'format_version\t1\n'
    printf 'canonicalization\t%s\n' "$CANONICALIZATION"
    printf 'auth_domain\t%s\n' "$AUTH_DOMAIN"
    printf 'scenario\t%s\n' "$SCENARIO"
    printf 'subject\t%s\n' "$SUBJECT"
    printf 'campaign_id\t%s\n' "$CAMPAIGN_ID"
    printf 'session_id\t%s\n' "$SESSION_ID"
    printf 'nonce\t%s\n' "$NONCE"
    printf 'plan_sha256\t%s\n' "$plan_hash"
    printf 'plan_metadata_sha256\t%s\n' "$plan_metadata_hash"
    printf 'pair_metadata_sha256\t%s\n' "$pair_hash"
    printf 'run_intent_sha256\t%s\n' "$run_hash"
    printf 'command_sha256\t%s\n' "$command_hash"
    printf 'environment_sha256\t%s\n' "$environment_hash"
    printf 'font_sha256\t%s\n' "$font_hash"
    printf 'initial_grid_sha256\t%s\n' "$grid_hash"
    printf 'subject_identity_sha256\t%s\n' "$subject_hash"
    printf 'subject_process_pid\t%s\n' "$process_pid"
    printf 'subject_process_start_identity\t%s\n' "$process_start_identity"
    printf 'expected_driver_events_path\t%s\n' "$expected_driver_path"
    printf 'expected_driver_parent_device\t%s\n' "${expected_driver_parent_identity%%:*}"
    printf 'expected_driver_parent_inode\t%s\n' "${expected_driver_parent_identity#*:}"
    printf 'action_video_path\t%s\n' "$action_video_path"
    printf 'action_video_parent_device\t%s\n' "${action_video_parent_identity%%:*}"
    printf 'action_video_parent_inode\t%s\n' "${action_video_parent_identity#*:}"
    printf 'final_metadata_path\t%s\n' "$final_metadata_path"
    printf 'final_metadata_parent_device\t%s\n' "${final_metadata_parent_identity%%:*}"
    printf 'final_metadata_parent_inode\t%s\n' "${final_metadata_parent_identity#*:}"
    printf 'warmup_ms\t%s\n' "$warmup_ms"
    printf 'measured_duration_ms\t%s\n' "$measured_duration_ms"
    printf 'required_action_count\t%s\n' "$required_action_count"
    printf 'action_interval_ms\t%s\n' "$action_interval_ms"
    printf 'hmac_key_identifier_sha256\t%s\n' "$HMAC_KEY_IDENTIFIER"
} > "$TEMP"
hmac_body "$TEMP"
intent_hmac="$HMAC_RESULT"
[[ "$intent_hmac" =~ ^[0-9a-f]{64}$ ]] || die "could not authenticate intent"
printf 'intent_hmac_sha256\t%s\n' "$intent_hmac" >> "$TEMP"
"$CHMOD_COMMAND" 0444 "$TEMP"
[[ ! -e "$expected_driver_path" && ! -e "$action_video_path" \
    && ! -e "$final_metadata_path" \
    && "$("$STAT_COMMAND" -f '%d:%i' "$expected_driver_parent")" \
        == "$expected_driver_parent_identity" \
    && "$("$STAT_COMMAND" -f '%d:%i' "$action_video_parent")" \
        == "$action_video_parent_identity" \
    && "$("$STAT_COMMAND" -f '%d:%i' "$final_metadata_parent")" \
        == "$final_metadata_parent_identity" ]] \
    || die "reserved capture paths or parent identities changed while freezing intent"
"$LN_COMMAND" "$TEMP" "$output_path" || die "intent output path was created concurrently"
"$RM_COMMAND" -f -- "$TEMP"
TEMP=""
trap - EXIT INT TERM
printf 'render_profile_intent_sha256\t%s\n' "$(sha256 "$output_path")"
