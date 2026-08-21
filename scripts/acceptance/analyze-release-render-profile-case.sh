#!/bin/bash
# shellcheck disable=SC2016 # Awk programs intentionally use literal dollar fields.

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
readonly TRUSTED_SYSTEM_PATH="/usr/bin:/bin:/usr/sbin:/sbin"
export PATH="$TRUSTED_SYSTEM_PATH"

SUBJECT=""
SCENARIO=""
PLAN=""
PLAN_METADATA=""
PAIR_METADATA=""
PAIR_RESULT=""
CASE_REPORT=""
SUBJECT_IDENTITY=""
COMPARISON_SUBJECT_IDENTITY=""
RUN_METADATA=""
RENDER_INTENT=""
RENDER_EVIDENCE=""
RENDER_WORKLOAD_METADATA=""
WORKLOAD_METADATA=""
WORKLOAD_EVENTS=""
WORKLOAD_READY_RECEIPT=""
CAMPAIGN_SECRET_FILE=""
DRIVER_EVENTS=""
TRACE_INDEX=""
TRACE_METADATA=""
TRACE_ARTIFACT=""
TRACE_TOC=""
TRACE_VERIFICATION=""
TRACE_RECEIPT=""
TRACE_ANCHOR_RECEIPT=""
CAMPAIGN_MANIFEST=""
RENDER_TOOL_BUNDLE_MANIFEST=""
EXPECTED_SOURCE_COMMIT=""
TRUSTED_SOURCE_REPOSITORY=""
DRIVER_INTENT=""
DRIVER_RECEIPT=""
DRIVER_BINARY=""
DRIVER_SOURCE=""
DRIVER_CONTROLLER=""
WINDOW_IDENTITY=""
DRIVER_PLAN_START_CONTINUOUS_NS=""
TIME_PROFILER_ARTIFACT=""
ALLOCATIONS_ARTIFACT=""
HANGS_ARTIFACT=""
MANUAL_REVIEW=""
STACK_SCREENSHOT=""
ACTION_VIDEO=""
FFPROBE_ARGUMENT=""
RENDER_SECRET_FINGERPRINT=""
RENDER_HMAC_KEY_IDENTIFIER=""

readonly PLAN_HEADER=$'event_id\toffset_ms\taction\targ0\targ1'
readonly DRIVER_HEADER=$'sequence\tcontinuous_ns\tevent_id\taction\ttarget_pid\twindow_number\trequested_a\trequested_b\tobserved_a\tobserved_b\tresult'
readonly TRACE_INDEX_HEADER=$'scenario\tsubject\tsubject_identity_sha256\tpair_metadata_sha256\tcampaign_id\tsession_id\tnonce\tcampaign_manifest_sha256\ttrace_anchor_receipt_sha256\tdriver_intent_sha256\tdriver_receipt_sha256\ttrace_receipt_sha256\ttrace_metadata_sha256\ttrace_artifact_sha256\ttrace_toc_sha256\ttrace_verification_sha256\ttrace_verifier_sha256\ttime_profiler_artifact_sha256\tallocations_artifact_sha256\thangs_artifact_sha256\trepresentative_stack_screenshot_sha256\taction_video_sha256'
readonly RENDER_SCENARIOS="perf-render-idle-cursor-blink perf-render-text-blink perf-render-sustained-output perf-render-selection perf-render-marked-text perf-render-live-resize"
readonly ZERO_SHA256="0000000000000000000000000000000000000000000000000000000000000000"
readonly AWK_COMMAND="/usr/bin/awk"
readonly BASENAME_COMMAND="/usr/bin/basename"
readonly HEAD_COMMAND="/usr/bin/head"
readonly ID_COMMAND="/usr/bin/id"
readonly MKTEMP_COMMAND="/usr/bin/mktemp"
readonly PYTHON_COMMAND="/usr/bin/python3"
readonly RM_COMMAND="/bin/rm"
readonly SHASUM_COMMAND="/usr/bin/shasum"
readonly STAT_COMMAND="/usr/bin/stat"
readonly TR_COMMAND="/usr/bin/tr"
readonly WC_COMMAND="/usr/bin/wc"
XCRUN_COMMAND=""
SIPS_COMMAND=""
FFPROBE_COMMAND=""
TRACE_VERIFIER=""
TRACE_ARCHIVE_VERIFIER=""
ACTION_VIDEO_VERIFIER=""
TRACE_RECEIPT_VERIFIER=""
DRIVER_RECEIPT_VERIFIER=""
RENDER_TOOL_IDENTITY_SNAPSHOT=""
EVIDENCE_IDENTITY_SNAPSHOT=""
declare -a EVIDENCE_SNAPSHOT_ARGUMENTS=()
TEST_OVERRIDES_ACTIVE=false
EVIDENCE_SNAPSHOT_TEST_HOOK="${SPACETERM_RENDER_PROFILE_TEST_EVIDENCE_SNAPSHOT_HOOK:-}"
[[ -z "${SPACETERM_RENDER_PROFILE_XCRUN:-}${SPACETERM_RENDER_PROFILE_SIPS:-}${SPACETERM_RENDER_PROFILE_FFPROBE:-}${SPACETERM_RENDER_PROFILE_TRACE_VERIFIER:-}${EVIDENCE_SNAPSHOT_TEST_HOOK}" ]] \
    || TEST_OVERRIDES_ACTIVE=true
readonly TEST_OVERRIDES_ACTIVE EVIDENCE_SNAPSHOT_TEST_HOOK
VALIDATION_ROOT=""

usage() {
    cat <<EOF
Usage: $("$BASENAME_COMMAND" -- "$0") --subject spaceterm|ghostty --scenario NAME \\
  --plan FILE --plan-metadata FILE --pair-metadata FILE \\
  --pair-result FILE --case-report FILE \\
  --subject-identity FILE --comparison-subject-identity FILE \\
  --run-metadata FILE --render-intent FILE --render-evidence FILE \\
  --render-workload-metadata FILE --driver-events FILE \\
  [--workload-metadata FILE --workload-events FILE \\
   --workload-ready-receipt FILE] \\
  --campaign-secret-file FILE \\
  --trace-index FILE --trace-metadata FILE --trace-artifact FILE \\
  --trace-toc FILE --trace-verification FILE --trace-receipt FILE \\
  --trace-anchor-receipt FILE \\
  --campaign-manifest FILE --render-tool-bundle-manifest FILE \\
  --expected-source-commit SHA1 --trusted-source-repository DIRECTORY \\
  --driver-intent FILE --driver-receipt FILE \\
  --driver-binary FILE --driver-source FILE --driver-controller FILE \\
  --window-identity FILE --driver-plan-start-continuous-ns UINT \\
  --time-profiler-artifact FILE --allocations-artifact FILE \\
  --hangs-artifact FILE --manual-review FILE --stack-screenshot FILE \\
  --action-video FILE --ffprobe ABSOLUTE_FILE

Analyze one paired issue #43 render-profile case. PASS requires a complete
six-scenario/two-subject trace index, exact frozen identities and plan, a
full-duration Time Profiler + Allocations + Hangs capture, and a manual call-
tree review bound to the actual artifacts. Source inspection is not evidence.
EOF
}

verdict() {
    local result="$1"
    local reason="$2"
    printf 'format_version\t1\n'
    printf 'subject\t%s\n' "${SUBJECT:-unknown}"
    printf 'scenario\t%s\n' "${SCENARIO:-unknown}"
    printf 'result\t%s\n' "$result"
    printf 'reason\t%s\n' "$reason"
    case "$result" in
        PASS) exit 0 ;;
        FAIL) exit 1 ;;
        NOT-RUN) exit 2 ;;
        *) exit 3 ;;
    esac
}

not_run() { verdict NOT-RUN "$1"; }
fail() { verdict FAIL "$1"; }

sha256() {
    "$SHASUM_COMMAND" -a 256 "$1" | "$AWK_COMMAND" '{ print $1 }'
}

evidence_identity_snapshot() {
    "$PYTHON_COMMAND" - "$@" <<'PY'
import hashlib
import os
import pathlib
import stat
import sys

arguments = sys.argv[1:]
if not arguments or len(arguments) % 2:
    raise SystemExit(1)

for label, raw in zip(arguments[0::2], arguments[1::2]):
    try:
        if not label or any(character in label for character in "\t\r\n"):
            raise ValueError
        if not raw or any(character in raw for character in "\t\r\n"):
            raise ValueError
        path = pathlib.Path(raw)
        before = path.lstat()
        resolved = path.resolve(strict=True)
        if (not path.is_absolute() or path.is_symlink() or str(resolved) != raw
                or not stat.S_ISREG(before.st_mode) or before.st_size <= 0
                or before.st_nlink != 1 or before.st_mode & 0o222):
            raise ValueError
        descriptor = os.open(raw, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
        opened = os.fstat(descriptor)
        digest = hashlib.sha256()
        identity = lambda details: (
            details.st_dev, details.st_ino, stat.S_IMODE(details.st_mode),
            details.st_nlink, details.st_size, details.st_mtime_ns,
            details.st_ctime_ns,
        )
        if identity(before) != identity(opened):
            raise ValueError
        with os.fdopen(descriptor, "rb", closefd=False) as stream:
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(block)
        opened_after = os.fstat(descriptor)
        after = path.lstat()
        if not (identity(before) == identity(opened_after) == identity(after)):
            raise ValueError
    except (OSError, RuntimeError, ValueError):
        raise SystemExit(1)
    finally:
        if "descriptor" in locals():
            os.close(descriptor)
            del descriptor
    print("\t".join((
        label, str(before.st_dev), str(before.st_ino),
        format(stat.S_IMODE(before.st_mode), "04o"), str(before.st_nlink),
        str(before.st_size), str(before.st_mtime_ns), str(before.st_ctime_ns),
        digest.hexdigest(),
    )))
PY
}

render_tool_identity_snapshot() {
    local tool
    for tool in "$XCRUN_COMMAND" "$SIPS_COMMAND" "$PYTHON_COMMAND" \
        "$FFPROBE_COMMAND" "$TRACE_VERIFIER" "$TRACE_ARCHIVE_VERIFIER" \
        "$ACTION_VIDEO_VERIFIER" "$TRACE_RECEIPT_VERIFIER" \
        "$DRIVER_RECEIPT_VERIFIER" "$HMAC_HELPER" "$PROCESS_INSPECTOR" \
        "$COMMAND_RUNNER"; do
        [[ -f "$tool" && ! -L "$tool" ]] || return 1
        printf '%s\t%s\t%s\n' "$tool" \
            "$("$STAT_COMMAND" -f '%d:%i:%z:%m:%c' "$tool")" \
            "$(sha256 "$tool")"
    done
}

verify_render_tool_bundle() {
    "$PYTHON_COMMAND" - "$RENDER_TOOL_BUNDLE_MANIFEST" "$EXPECTED_SOURCE_COMMIT" \
        "$TRUSTED_SOURCE_REPOSITORY" \
        "${BASH_SOURCE[0]}" "$HMAC_HELPER" "$TRACE_RECEIPT_VERIFIER" \
        "$TRACE_ARCHIVE_VERIFIER" "$ACTION_VIDEO_VERIFIER" "$TRACE_VERIFIER" \
        "$PROCESS_INSPECTOR" "$COMMAND_RUNNER" <<'PY'
import hashlib
import pathlib
import stat
import subprocess
import sys

manifest_raw, expected_commit, repository_raw, analyzer_raw, hmac_raw, receipt_raw, archive_raw, video_raw, \
    trace_raw, inspector_raw, runner_raw = sys.argv[1:]
names = (
    "record_release_performance_trace", "freeze_render_profile_intent",
    "finalize_render_profile_evidence", "render_profile_hmac", "render_trace_receipt",
    "analyze_release_render_profile_case", "archive_render_trace",
    "verify_render_action_video", "verify_render_trace_archive",
    "verify_release_performance_trace", "inspect_release_performance_process",
    "run_release_performance_command",
    "freeze_render_profile_tool_bundle",
)
relatives = (
    "scripts/record-release-performance-trace.sh",
    "scripts/acceptance/freeze-render-profile-intent.sh",
    "scripts/acceptance/finalize-render-profile-evidence.sh",
    "scripts/acceptance/render-profile-hmac.py",
    "scripts/acceptance/render-trace-receipt.py",
    "scripts/acceptance/analyze-release-render-profile-case.sh",
    "scripts/acceptance/archive-render-trace.py",
    "scripts/acceptance/verify-render-action-video.py",
    "scripts/acceptance/verify-render-trace-archive.py",
    "scripts/verify-release-performance-trace.py",
    "scripts/inspect-release-performance-process.py",
    "scripts/run-release-performance-command.py",
    "scripts/acceptance/freeze-render-profile-tool-bundle.sh",
)
keys = ["format_version", "schema", "source_commit", "tool_count"]
for name in names:
    keys.extend((f"{name}_source_path", f"{name}_source_sha256",
                 f"{name}_bundle_path", f"{name}_bundle_sha256"))

def exact_file(raw, executable=False):
    path = pathlib.Path(raw)
    before = path.lstat()
    if (not path.is_absolute() or path.is_symlink() or path.resolve(strict=True) != path
            or not stat.S_ISREG(before.st_mode) or before.st_nlink != 1
            or before.st_mode & 0o222 or (executable and not before.st_mode & 0o111)):
        raise SystemExit(1)
    payload = path.read_bytes()
    after = path.lstat()
    if (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns) != (
        after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns
    ):
        raise SystemExit(1)
    return path, payload

manifest_path, payload = exact_file(manifest_raw)
lines = payload.splitlines()
if not payload.endswith(b"\n") or len(lines) != len(keys):
    raise SystemExit(1)
values = {}
for key, line in zip(keys, lines):
    try:
        actual, value = line.split(b"\t", 1)
        actual = actual.decode("ascii")
        value = value.decode("utf-8")
    except (ValueError, UnicodeDecodeError):
        raise SystemExit(1)
    if actual != key or not value or "\t" in value or "\r" in value:
        raise SystemExit(1)
    values[key] = value
repository = pathlib.Path(repository_raw)
if (not repository.is_absolute() or repository.is_symlink()
        or repository.resolve(strict=True) != repository or not repository.is_dir()
        or values["format_version"] != "1"
        or values["schema"] != "spaceterm.render-profile-tool-bundle/v1"
        or values["source_commit"] != expected_commit
        or values["tool_count"] != str(len(names))):
    raise SystemExit(1)
for name, relative in zip(names, relatives):
    bundle, tool_payload = exact_file(values[f"{name}_bundle_path"], executable=True)
    digest = hashlib.sha256(tool_payload).hexdigest()
    blob = subprocess.run(
        ["/usr/bin/git", "--no-replace-objects", "-C", str(repository), "show",
         f"{expected_commit}:{relative}"], check=False, capture_output=True,
        env={"PATH": "/usr/bin:/bin", "HOME": "/var/empty",
             "GIT_NO_REPLACE_OBJECTS": "1", "LC_ALL": "C"},
    )
    blob_hash = hashlib.sha256(blob.stdout).hexdigest()
    if (blob.returncode != 0 or values[f"{name}_bundle_sha256"] != digest
            or values[f"{name}_source_sha256"] != blob_hash or digest != blob_hash
            or pathlib.Path(values[f"{name}_source_path"]) != repository / relative):
        raise SystemExit(1)
expected_paths = {
    "analyze_release_render_profile_case": analyzer_raw,
    "render_profile_hmac": hmac_raw, "render_trace_receipt": receipt_raw,
    "verify_render_trace_archive": archive_raw, "verify_render_action_video": video_raw,
    "verify_release_performance_trace": trace_raw,
    "inspect_release_performance_process": inspector_raw,
    "run_release_performance_command": runner_raw,
}
if any(values[f"{name}_bundle_path"] != str(pathlib.Path(raw).resolve(strict=True))
       for name, raw in expected_paths.items()):
    raise SystemExit(1)
PY
}

kv() {
    local file="$1"
    local key="$2"
    "$AWK_COMMAND" -F '\t' -v wanted="$key" '
        NF != 2 { bad = 1 }
        $1 == wanted { count += 1; value = $2 }
        END { if (!bad && count == 1) print value }
    ' "$file"
}

required_kv() {
    local value
    value="$(kv "$1" "$2")"
    [[ -n "$value" ]] || not_run "missing-or-duplicate-$3-$2"
    printf '%s' "$value"
}

require_uint() {
    [[ "$1" =~ ^[0-9]+$ ]] || not_run "invalid-$2"
}

require_hash() {
    [[ "$1" =~ ^[0-9a-f]{64}$ ]] || not_run "invalid-$2"
}

require_artifact() {
    local label="$1"
    local path="$2"
    [[ -f "$path" && ! -L "$path" && -r "$path" && -s "$path" ]] \
        || not_run "missing-or-empty-$label"
}

reject_unknown_kv() {
    local file="$1"
    local allowed="$2"
    local label="$3"
    "$AWK_COMMAND" -F '\t' -v allowed="$allowed" '
        BEGIN {
            count = split(allowed, keys, " ")
            for (i = 1; i <= count; i += 1) accepted[keys[i]] = 1
        }
        NF != 2 || !($1 in accepted) || seen[$1]++ { exit 1 }
        END {
            for (i = 1; i <= count; i += 1) {
                if (seen[keys[i]] != 1) exit 1
            }
        }
    ' "$file" || not_run "invalid-$label-schema"
}

accept_render_hmac_output() {
    local output="$1"
    local fingerprint identifier digest
    [[ "$(printf '%s\n' "$output" | "$WC_COMMAND" -l | "$TR_COMMAND" -d ' ')" == 3 ]] \
        || return 1
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

inspect_render_secret() {
    local output
    output="$("$PYTHON_COMMAND" "$HMAC_HELPER" --secret "$CAMPAIGN_SECRET_FILE" \
        --domain SPACETERM_RENDER_PROFILE_SECRET_PROBE_V1 --body "$1")" \
        || return 1
    accept_render_hmac_output "$output"
}

verify_render_hmac() {
    local output expected
    output="$("$PYTHON_COMMAND" "$HMAC_HELPER" --secret "$CAMPAIGN_SECRET_FILE" \
        --domain "$2" --artifact "$1" --last-key "$3")" || return 1
    accept_render_hmac_output "$output" || return 1
    expected="$(required_kv "$1" "$3" render-hmac)"
    [[ "$RENDER_HMAC_DIGEST" == "$expected" ]]
}

verify_sustained_workload_v3() {
    "$PYTHON_COMMAND" - "$WORKLOAD_METADATA" "$WORKLOAD_EVENTS" \
        "$WORKLOAD_READY_RECEIPT" "$CAMPAIGN_SECRET_FILE" "$SCENARIO" \
        "$1" "$2" "$3" "$4" "$5" "$6" "$7" "$8" "$9" <<'PY'
import hashlib
import hmac
import os
import pathlib
import struct
import sys

(
    metadata_name, events_name, ready_name, secret_name, scenario,
    campaign_id, session_id, nonce, subject_hash, subject_pid,
    subject_start, producer_hash, expected_duration_ms, expected_warmup_ms,
) = sys.argv[1:]
metadata_path = pathlib.Path(metadata_name)
events_path = pathlib.Path(events_name)
ready_path = pathlib.Path(ready_name)
secret = pathlib.Path(secret_name).read_bytes()
metadata = metadata_path.read_bytes()
events = events_path.read_bytes()
ready = ready_path.read_bytes()

metadata_keys = [
    "format_version", "scenario", "campaign_id", "session_id", "nonce",
    "subject_identity_sha256", "subject_process_pid",
    "subject_process_start_identity", "producer_sha256", "producer_pid",
    "producer_started_continuous_ns", "producer_session_id",
    "producer_process_group", "tty_device", "tty_inode", "tty_rdev",
    "ready_receipt_sha256", "events_sha256", "auth_algorithm", "seed_sha256",
    "seed_bytes", "requested_duration_ms", "warmup_ms", "requested_iterations",
    "requested_seed_rows", "emitted_bytes", "input_events",
    "plan_start_continuous_ns", "started_continuous_ns", "ended_continuous_ns",
    "status", "events_hmac_sha256",
]
ready_keys = [
    "format_version", "campaign_id", "session_id", "nonce",
    "subject_identity_sha256", "producer_pid", "producer_started_continuous_ns",
    "producer_session_id", "producer_process_group", "tty_device", "tty_inode",
    "tty_rdev", "events_device", "events_inode", "events_prefix_bytes",
    "events_prefix_sha256", "measurement_ready_continuous_ns",
    "measurement_ready_byte_count", "auth_algorithm", "ready_hmac_sha256",
]

def parse_exact(contents: bytes, keys: list[str]) -> tuple[dict[str, str], list[bytes]]:
    if not contents.endswith(b"\n"):
        raise ValueError
    lines = contents.splitlines(keepends=True)
    parsed: list[tuple[str, str]] = []
    for line in lines:
        pieces = line[:-1].split(b"\t")
        if len(pieces) != 2 or not pieces[0] or not pieces[1]:
            raise ValueError
        parsed.append((pieces[0].decode("ascii"), pieces[1].decode("ascii")))
    if [key for key, _ in parsed] != keys:
        raise ValueError
    return dict(parsed), lines

try:
    fields, metadata_lines = parse_exact(metadata, metadata_keys)
    ready_fields, ready_lines = parse_exact(ready, ready_keys)
except (UnicodeDecodeError, ValueError):
    raise SystemExit(1)

bindings = {
    "format_version": "3",
    "scenario": scenario,
    "campaign_id": campaign_id,
    "session_id": session_id,
    "nonce": nonce,
    "subject_identity_sha256": subject_hash,
    "subject_process_pid": subject_pid,
    "subject_process_start_identity": subject_start,
    "producer_sha256": producer_hash,
    "requested_duration_ms": expected_duration_ms,
    "warmup_ms": expected_warmup_ms,
    "ready_receipt_sha256": hashlib.sha256(ready).hexdigest(),
    "events_sha256": hashlib.sha256(events).hexdigest(),
    "auth_algorithm": "hmac-sha256",
    "status": "complete",
}
if any(fields.get(key) != value for key, value in bindings.items()):
    raise SystemExit(1)
ready_bindings = {
    "format_version": "1",
    "campaign_id": campaign_id,
    "session_id": session_id,
    "nonce": nonce,
    "subject_identity_sha256": subject_hash,
    "auth_algorithm": "hmac-sha256",
}
if any(ready_fields.get(key) != value for key, value in ready_bindings.items()):
    raise SystemExit(1)
for key in (
    "producer_pid", "producer_started_continuous_ns", "producer_session_id",
    "producer_process_group", "tty_device", "tty_inode", "tty_rdev",
):
    if fields[key] != ready_fields[key]:
        raise SystemExit(1)

numeric_metadata = (
    "producer_pid", "producer_started_continuous_ns", "producer_session_id",
    "producer_process_group", "tty_device", "tty_inode", "tty_rdev", "seed_bytes",
    "requested_duration_ms", "warmup_ms", "requested_iterations",
    "requested_seed_rows", "emitted_bytes", "input_events",
    "plan_start_continuous_ns", "started_continuous_ns", "ended_continuous_ns",
)
numeric_ready = (
    "producer_pid", "producer_started_continuous_ns", "producer_session_id",
    "producer_process_group", "tty_device", "tty_inode", "tty_rdev",
    "events_device", "events_inode", "events_prefix_bytes",
    "measurement_ready_continuous_ns", "measurement_ready_byte_count",
)
if any(not fields[key].isascii() or not fields[key].isdecimal()
       for key in numeric_metadata) or any(
           not ready_fields[key].isascii() or not ready_fields[key].isdecimal()
           for key in numeric_ready
       ):
    raise SystemExit(1)
numbers = {key: int(fields[key]) for key in numeric_metadata}
ready_numbers = {key: int(ready_fields[key]) for key in numeric_ready}
events_stat = os.stat(events_path, follow_symlinks=False)
prefix_bytes = ready_numbers["events_prefix_bytes"]
if not (
    numbers["producer_pid"] > 0
    and numbers["producer_session_id"] > 0
    and numbers["producer_process_group"] > 0
    and numbers["seed_bytes"] > 0
    and numbers["emitted_bytes"] > numbers["seed_bytes"]
    and numbers["requested_iterations"] == 0
    and numbers["requested_seed_rows"] == 0
    and numbers["input_events"] == 0
    and 0 < ready_numbers["measurement_ready_byte_count"] < numbers["emitted_bytes"]
    and 0 < prefix_bytes <= len(events)
    and ready_numbers["events_device"] == events_stat.st_dev
    and ready_numbers["events_inode"] == events_stat.st_ino
    and ready_fields["events_prefix_sha256"]
        == hashlib.sha256(events[:prefix_bytes]).hexdigest()
):
    raise SystemExit(1)

unsigned_ready = b"".join(ready_lines[:-1])
ready_authenticated = (
    b"spaceterm.performance.workload-ready/v1\0"
    + struct.pack(">Q", len(unsigned_ready))
    + unsigned_ready
)
actual_ready_hmac = hmac.new(secret, ready_authenticated, hashlib.sha256).hexdigest()
if not hmac.compare_digest(ready_fields["ready_hmac_sha256"], actual_ready_hmac):
    raise SystemExit(1)
unsigned_metadata = b"".join(metadata_lines[:-1])
metadata_authenticated = (
    b"spaceterm.performance.workload-auth/v1\0"
    + struct.pack(">Q", len(unsigned_metadata))
    + unsigned_metadata
    + struct.pack(">Q", len(events))
    + events
)
actual_metadata_hmac = hmac.new(secret, metadata_authenticated, hashlib.sha256).hexdigest()
if not hmac.compare_digest(fields["events_hmac_sha256"], actual_metadata_hmac):
    raise SystemExit(1)

measurement_start = numbers["plan_start_continuous_ns"] + numbers["warmup_ms"] * 1_000_000
duration = numbers["ended_continuous_ns"] - numbers["started_continuous_ns"]
if not (
    numbers["producer_started_continuous_ns"]
    <= ready_numbers["measurement_ready_continuous_ns"]
    <= numbers["plan_start_continuous_ns"]
    <= measurement_start
    <= numbers["started_continuous_ns"]
    and numbers["started_continuous_ns"] - measurement_start <= 100_000_000
    and int(expected_duration_ms) * 1_000_000
        <= duration <= (int(expected_duration_ms) + 2_000) * 1_000_000
):
    raise SystemExit(1)

event_lines = events.splitlines()
expected_header = (
    b"sequence\tcontinuous_ns\tkind\tevent_id\tbyte_count\trows\tcolumns\t"
    b"pixel_width\tpixel_height\tstatus"
)
if not events.endswith(b"\n") or not event_lines or event_lines[0] != expected_header:
    raise SystemExit(1)
rows: list[list[bytes]] = []
for sequence, line in enumerate(event_lines[1:]):
    columns = line.split(b"\t")
    if len(columns) != 10 or any(not columns[index].isdigit()
                                 for index in (0, 1, 4, 5, 6, 7, 8)):
        raise SystemExit(1)
    if int(columns[0]) != sequence or int(columns[5]) <= 0 or int(columns[6]) <= 0:
        raise SystemExit(1)
    rows.append(columns)
kinds = [row[2] for row in rows]
if not rows or any(
    int(rows[index][1]) <= int(rows[index - 1][1])
    for index in range(1, len(rows))
) or (
    len(rows) < 4
    or rows[0][2] != b"started" or rows[0][3] != b"none"
    or rows[0][4] != b"0" or rows[0][9] != b"ok"
    or kinds[1] != b"geometry" or kinds.count(b"seed-complete") != 1
    or kinds[-1] != b"producer-end" or rows[-1][3] != b"none"
    or rows[-1][9] != b"success"
    or any(kind not in {b"started", b"geometry", b"seed-complete", b"producer-end"}
           for kind in kinds)
    or int(rows[kinds.index(b"seed-complete")][4]) != numbers["seed_bytes"]
    or int(rows[-1][4]) != numbers["emitted_bytes"]
    or int(rows[0][1]) != numbers["producer_started_continuous_ns"]
    or int(rows[-1][1]) != numbers["ended_continuous_ns"]
    or not (int(rows[kinds.index(b"seed-complete")][1])
            < numbers["started_continuous_ns"] < numbers["ended_continuous_ns"])
):
    raise SystemExit(1)
PY
}

while (( $# > 0 )); do
    case "$1" in
        --subject) SUBJECT="${2:-}"; shift ;;
        --scenario) SCENARIO="${2:-}"; shift ;;
        --plan) PLAN="${2:-}"; shift ;;
        --plan-metadata) PLAN_METADATA="${2:-}"; shift ;;
        --pair-metadata) PAIR_METADATA="${2:-}"; shift ;;
        --pair-result) PAIR_RESULT="${2:-}"; shift ;;
        --case-report) CASE_REPORT="${2:-}"; shift ;;
        --subject-identity) SUBJECT_IDENTITY="${2:-}"; shift ;;
        --comparison-subject-identity) COMPARISON_SUBJECT_IDENTITY="${2:-}"; shift ;;
        --run-metadata) RUN_METADATA="${2:-}"; shift ;;
        --render-intent) RENDER_INTENT="${2:-}"; shift ;;
        --render-evidence) RENDER_EVIDENCE="${2:-}"; shift ;;
        --render-workload-metadata) RENDER_WORKLOAD_METADATA="${2:-}"; shift ;;
        --workload-metadata) WORKLOAD_METADATA="${2:-}"; shift ;;
        --workload-events) WORKLOAD_EVENTS="${2:-}"; shift ;;
        --workload-ready-receipt) WORKLOAD_READY_RECEIPT="${2:-}"; shift ;;
        --campaign-secret-file) CAMPAIGN_SECRET_FILE="${2:-}"; shift ;;
        --driver-events) DRIVER_EVENTS="${2:-}"; shift ;;
        --trace-index) TRACE_INDEX="${2:-}"; shift ;;
        --trace-metadata) TRACE_METADATA="${2:-}"; shift ;;
        --trace-artifact) TRACE_ARTIFACT="${2:-}"; shift ;;
        --trace-toc) TRACE_TOC="${2:-}"; shift ;;
        --trace-verification) TRACE_VERIFICATION="${2:-}"; shift ;;
        --trace-receipt) TRACE_RECEIPT="${2:-}"; shift ;;
        --trace-anchor-receipt) TRACE_ANCHOR_RECEIPT="${2:-}"; shift ;;
        --campaign-manifest) CAMPAIGN_MANIFEST="${2:-}"; shift ;;
        --render-tool-bundle-manifest) RENDER_TOOL_BUNDLE_MANIFEST="${2:-}"; shift ;;
        --expected-source-commit) EXPECTED_SOURCE_COMMIT="${2:-}"; shift ;;
        --trusted-source-repository) TRUSTED_SOURCE_REPOSITORY="${2:-}"; shift ;;
        --driver-intent) DRIVER_INTENT="${2:-}"; shift ;;
        --driver-receipt) DRIVER_RECEIPT="${2:-}"; shift ;;
        --driver-binary) DRIVER_BINARY="${2:-}"; shift ;;
        --driver-source) DRIVER_SOURCE="${2:-}"; shift ;;
        --driver-controller) DRIVER_CONTROLLER="${2:-}"; shift ;;
        --window-identity) WINDOW_IDENTITY="${2:-}"; shift ;;
        --driver-plan-start-continuous-ns) DRIVER_PLAN_START_CONTINUOUS_NS="${2:-}"; shift ;;
        --time-profiler-artifact) TIME_PROFILER_ARTIFACT="${2:-}"; shift ;;
        --allocations-artifact) ALLOCATIONS_ARTIFACT="${2:-}"; shift ;;
        --hangs-artifact) HANGS_ARTIFACT="${2:-}"; shift ;;
        --manual-review) MANUAL_REVIEW="${2:-}"; shift ;;
        --stack-screenshot) STACK_SCREENSHOT="${2:-}"; shift ;;
        --action-video) ACTION_VIDEO="${2:-}"; shift ;;
        --ffprobe) FFPROBE_ARGUMENT="${2:-}"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; not_run unknown-argument ;;
    esac
    shift
done

[[ "$SUBJECT" == spaceterm || "$SUBJECT" == ghostty ]] || not_run invalid-subject
case "$SCENARIO" in
    perf-render-idle-cursor-blink|perf-render-text-blink \
        |perf-render-sustained-output|perf-render-selection \
        |perf-render-marked-text|perf-render-live-resize) ;;
    *) not_run invalid-render-scenario ;;
esac
if [[ "$SCENARIO" == perf-render-sustained-output ]]; then
    [[ -n "$WORKLOAD_METADATA" && -n "$WORKLOAD_EVENTS" \
        && -n "$WORKLOAD_READY_RECEIPT" ]] \
        || not_run sustained-output-workload-v3-evidence-missing
else
    [[ -z "$WORKLOAD_METADATA$WORKLOAD_EVENTS$WORKLOAD_READY_RECEIPT" ]] \
        || not_run unexpected-workload-v3-evidence-for-render-scenario
fi
SCRIPT_DIRECTORY="$(cd -- "$(/usr/bin/dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIRECTORY
HMAC_HELPER="$SCRIPT_DIRECTORY/render-profile-hmac.py"
PROCESS_INSPECTOR="$SCRIPT_DIRECTORY/../inspect-release-performance-process.py"
COMMAND_RUNNER="$SCRIPT_DIRECTORY/../run-release-performance-command.py"
readonly HMAC_HELPER PROCESS_INSPECTOR COMMAND_RUNNER
XCRUN_COMMAND="${SPACETERM_RENDER_PROFILE_XCRUN:-/usr/bin/xcrun}"
SIPS_COMMAND="${SPACETERM_RENDER_PROFILE_SIPS:-/usr/bin/sips}"
FFPROBE_COMMAND="${SPACETERM_RENDER_PROFILE_FFPROBE:-$FFPROBE_ARGUMENT}"
TRACE_VERIFIER="${SPACETERM_RENDER_PROFILE_TRACE_VERIFIER:-$SCRIPT_DIRECTORY/../verify-release-performance-trace.py}"
[[ "$FFPROBE_COMMAND" == /* && -f "$FFPROBE_COMMAND" \
    && ! -L "$FFPROBE_COMMAND" && -x "$FFPROBE_COMMAND" ]] \
    || not_run render-ffprobe-path-invalid
if [[ "$TEST_OVERRIDES_ACTIVE" == false ]]; then
    [[ -n "$TRACE_RECEIPT" && -n "$TRACE_ANCHOR_RECEIPT" \
        && -n "$CAMPAIGN_MANIFEST" \
        && -n "$RENDER_TOOL_BUNDLE_MANIFEST" \
        && "$EXPECTED_SOURCE_COMMIT" =~ ^[0-9a-f]{40}$ \
        && -d "$TRUSTED_SOURCE_REPOSITORY" && ! -L "$TRUSTED_SOURCE_REPOSITORY" \
        && -n "$DRIVER_INTENT" && -n "$DRIVER_RECEIPT" \
        && -f "$TRACE_RECEIPT" && ! -L "$TRACE_RECEIPT" \
        && -r "$TRACE_RECEIPT" && -s "$TRACE_RECEIPT" ]] \
        || not_run render-trace-receipt-required
fi
[[ -x "$XCRUN_COMMAND" && -x "$SIPS_COMMAND" && -x "$PYTHON_COMMAND" ]] \
    || not_run trusted-render-analysis-toolchain-unavailable
readonly XCRUN_COMMAND SIPS_COMMAND FFPROBE_COMMAND
readonly TRACE_VERIFIER
TRACE_ARCHIVE_VERIFIER="$SCRIPT_DIRECTORY/verify-render-trace-archive.py"
ACTION_VIDEO_VERIFIER="$SCRIPT_DIRECTORY/verify-render-action-video.py"
TRACE_RECEIPT_VERIFIER="$SCRIPT_DIRECTORY/render-trace-receipt.py"
DRIVER_RECEIPT_VERIFIER="$SCRIPT_DIRECTORY/performance-driver-receipt.py"
readonly TRACE_ARCHIVE_VERIFIER ACTION_VIDEO_VERIFIER TRACE_RECEIPT_VERIFIER \
    DRIVER_RECEIPT_VERIFIER
for command in "$AWK_COMMAND" "$BASENAME_COMMAND" "$HEAD_COMMAND" \
    "$ID_COMMAND" "$MKTEMP_COMMAND" "$PYTHON_COMMAND" "$RM_COMMAND" \
    "$SHASUM_COMMAND" "$STAT_COMMAND" "$TR_COMMAND" "$WC_COMMAND" \
    "$XCRUN_COMMAND" "$SIPS_COMMAND" "$FFPROBE_COMMAND"; do
    command -v "$command" >/dev/null 2>&1 || not_run "missing-command-$command"
done
[[ -f "$TRACE_VERIFIER" && -x "$TRACE_VERIFIER" ]] \
    || not_run trace-verifier-unavailable
[[ -f "$TRACE_ARCHIVE_VERIFIER" && -x "$TRACE_ARCHIVE_VERIFIER" \
    && -f "$ACTION_VIDEO_VERIFIER" && -x "$ACTION_VIDEO_VERIFIER" \
    && -f "$TRACE_RECEIPT_VERIFIER" && -x "$TRACE_RECEIPT_VERIFIER" \
    && ( "$TEST_OVERRIDES_ACTIVE" == true \
        || ( -f "$DRIVER_RECEIPT_VERIFIER" && -x "$DRIVER_RECEIPT_VERIFIER" ) ) ]] \
    || not_run render-evidence-verifier-unavailable
[[ -f "$HMAC_HELPER" && ! -L "$HMAC_HELPER" ]] \
    || not_run render-authentication-helper-unavailable
[[ -f "$PROCESS_INSPECTOR" && -x "$PROCESS_INSPECTOR" \
    && -f "$COMMAND_RUNNER" && -x "$COMMAND_RUNNER" ]] \
    || not_run render-recorder-tooling-unavailable

require_artifact scenario-plan "$PLAN"
require_artifact plan-metadata "$PLAN_METADATA"
require_artifact pair-metadata "$PAIR_METADATA"
require_artifact pair-result "$PAIR_RESULT"
require_artifact performance-case-report "$CASE_REPORT"
require_artifact subject-identity "$SUBJECT_IDENTITY"
require_artifact comparison-subject-identity "$COMPARISON_SUBJECT_IDENTITY"
require_artifact subject-run-metadata "$RUN_METADATA"
require_artifact render-profile-intent "$RENDER_INTENT"
require_artifact render-profile-evidence "$RENDER_EVIDENCE"
require_artifact render-workload-metadata "$RENDER_WORKLOAD_METADATA"
if [[ "$SCENARIO" == perf-render-sustained-output ]]; then
    require_artifact workload-v3-metadata "$WORKLOAD_METADATA"
    require_artifact workload-v3-events "$WORKLOAD_EVENTS"
    require_artifact workload-v3-ready-receipt "$WORKLOAD_READY_RECEIPT"
fi
require_artifact campaign-secret "$CAMPAIGN_SECRET_FILE"
require_artifact native-driver-events "$DRIVER_EVENTS"
require_artifact campaign-trace-index "$TRACE_INDEX"
require_artifact trace-metadata "$TRACE_METADATA"
require_artifact trace-capture "$TRACE_ARTIFACT"
require_artifact trace-toc "$TRACE_TOC"
require_artifact trace-verification "$TRACE_VERIFICATION"
require_artifact time-profiler-export "$TIME_PROFILER_ARTIFACT"
require_artifact allocations-export "$ALLOCATIONS_ARTIFACT"
require_artifact hangs-export "$HANGS_ARTIFACT"
require_artifact manual-render-review "$MANUAL_REVIEW"
require_artifact representative-stack-screenshot "$STACK_SCREENSHOT"
require_artifact render-profile-action-video "$ACTION_VIDEO"
if [[ "$TEST_OVERRIDES_ACTIVE" == false ]]; then
    require_artifact render-campaign-manifest "$CAMPAIGN_MANIFEST"
    require_artifact render-tool-bundle-manifest "$RENDER_TOOL_BUNDLE_MANIFEST"
    require_artifact native-driver-intent "$DRIVER_INTENT"
    require_artifact native-driver-receipt "$DRIVER_RECEIPT"
    require_artifact native-driver-binary "$DRIVER_BINARY"
    require_artifact native-driver-source "$DRIVER_SOURCE"
    require_artifact native-driver-controller "$DRIVER_CONTROLLER"
    require_artifact native-driver-window-identity "$WINDOW_IDENTITY"
    require_uint "$DRIVER_PLAN_START_CONTINUOUS_NS" \
        native-driver-plan-start-continuous-ns
    (( DRIVER_PLAN_START_CONTINUOUS_NS > 0 )) \
        || not_run invalid-native-driver-plan-start-continuous-ns
    require_artifact authenticated-render-trace-anchor-receipt "$TRACE_ANCHOR_RECEIPT"
    require_artifact authenticated-render-trace-receipt "$TRACE_RECEIPT"
fi
[[ "$TRACE_ARTIFACT" == *.zip ]] || not_run trace-artifact-is-not-a-zip-archive
[[ ! -w "$PLAN" && ! -w "$PLAN_METADATA" && ! -w "$DRIVER_EVENTS" \
    && ! -w "$PAIR_RESULT" && ! -w "$CASE_REPORT" \
    && ! -w "$RENDER_INTENT" && ! -w "$RENDER_EVIDENCE" \
    && ! -w "$RENDER_WORKLOAD_METADATA" \
    && ! -w "$TRACE_INDEX" ]] \
    || not_run immutable-evidence-is-writable
if [[ "$SCENARIO" == perf-render-sustained-output ]]; then
    [[ ! -w "$WORKLOAD_METADATA" && ! -w "$WORKLOAD_EVENTS" \
        && ! -w "$WORKLOAD_READY_RECEIPT" \
        && ! -L "$WORKLOAD_METADATA" && ! -L "$WORKLOAD_EVENTS" \
        && ! -L "$WORKLOAD_READY_RECEIPT" \
        && "$("$STAT_COMMAND" -f '%l' "$WORKLOAD_METADATA")" == 1 \
        && "$("$STAT_COMMAND" -f '%l' "$WORKLOAD_EVENTS")" == 1 \
        && "$("$STAT_COMMAND" -f '%l' "$WORKLOAD_READY_RECEIPT")" == 1 ]] \
        || not_run workload-v3-evidence-is-writable
fi
EVIDENCE_SNAPSHOT_ARGUMENTS=(
    scenario-plan "$PLAN"
    plan-metadata "$PLAN_METADATA"
    pair-metadata "$PAIR_METADATA"
    pair-result "$PAIR_RESULT"
    performance-case-report "$CASE_REPORT"
    subject-identity "$SUBJECT_IDENTITY"
    comparison-subject-identity "$COMPARISON_SUBJECT_IDENTITY"
    subject-run-metadata "$RUN_METADATA"
    render-profile-intent "$RENDER_INTENT"
    render-profile-evidence "$RENDER_EVIDENCE"
    render-workload-metadata "$RENDER_WORKLOAD_METADATA"
    native-driver-events "$DRIVER_EVENTS"
    campaign-trace-index "$TRACE_INDEX"
    render-campaign-manifest "$CAMPAIGN_MANIFEST"
    native-driver-intent "$DRIVER_INTENT"
    native-driver-receipt "$DRIVER_RECEIPT"
    authenticated-render-trace-anchor-receipt "$TRACE_ANCHOR_RECEIPT"
    authenticated-render-trace-receipt "$TRACE_RECEIPT"
    trace-metadata "$TRACE_METADATA"
    trace-capture "$TRACE_ARTIFACT"
    trace-toc "$TRACE_TOC"
    trace-verification "$TRACE_VERIFICATION"
    time-profiler-export "$TIME_PROFILER_ARTIFACT"
    allocations-export "$ALLOCATIONS_ARTIFACT"
    hangs-export "$HANGS_ARTIFACT"
    manual-render-review "$MANUAL_REVIEW"
    representative-stack-screenshot "$STACK_SCREENSHOT"
    render-profile-action-video "$ACTION_VIDEO"
)
if [[ "$SCENARIO" == perf-render-sustained-output ]]; then
    EVIDENCE_SNAPSHOT_ARGUMENTS+=(
        workload-v3-metadata "$WORKLOAD_METADATA"
        workload-v3-events "$WORKLOAD_EVENTS"
        workload-v3-ready-receipt "$WORKLOAD_READY_RECEIPT"
    )
fi
if [[ "$TEST_OVERRIDES_ACTIVE" == false ]]; then
    EVIDENCE_SNAPSHOT_ARGUMENTS+=(
        render-tool-bundle-manifest "$RENDER_TOOL_BUNDLE_MANIFEST"
        native-driver-binary "$DRIVER_BINARY"
        native-driver-source "$DRIVER_SOURCE"
        native-driver-controller "$DRIVER_CONTROLLER"
        native-driver-window-identity "$WINDOW_IDENTITY"
    )
fi
EVIDENCE_IDENTITY_SNAPSHOT="$(
    evidence_identity_snapshot "${EVIDENCE_SNAPSHOT_ARGUMENTS[@]}"
)" || not_run render-evidence-is-not-canonical-singleton-immutable
[[ -n "$EVIDENCE_IDENTITY_SNAPSHOT" ]] \
    || not_run render-evidence-identity-snapshot-failed
if [[ "$TEST_OVERRIDES_ACTIVE" == false ]]; then
    verify_render_tool_bundle || not_run render-tool-bundle-invalid
fi
secret_mode="$("$STAT_COMMAND" -f '%Lp' "$CAMPAIGN_SECRET_FILE" 2>/dev/null || true)"
[[ ! -L "$CAMPAIGN_SECRET_FILE" && "$("$STAT_COMMAND" -f '%l' "$CAMPAIGN_SECRET_FILE")" == 1 \
    && "$("$STAT_COMMAND" -f '%u' "$CAMPAIGN_SECRET_FILE")" == "$("$ID_COMMAND" -u)" \
    && "$secret_mode" =~ ^[0-7]{3,4}$ ]] \
    || not_run render-campaign-secret-is-not-private-singleton
(( (8#$secret_mode & 077) == 0 && (8#$secret_mode & 0200) == 0 \
    && (8#$secret_mode & 0400) != 0 )) \
    || not_run render-campaign-secret-is-not-private-singleton
inspect_render_secret "$PLAN" \
    || not_run render-campaign-secret-format-invalid

if [[ "$TEST_OVERRIDES_ACTIVE" == false ]]; then
    "$PYTHON_COMMAND" - \
        "$TRACE_RECEIPT" "$TRACE_ANCHOR_RECEIPT" \
        "$CAMPAIGN_SECRET_FILE" "$CAMPAIGN_MANIFEST" \
        "$SUBJECT_IDENTITY" "$RUN_METADATA" "$RENDER_INTENT" \
        "$RENDER_EVIDENCE" "$DRIVER_INTENT" "$DRIVER_RECEIPT" \
        "$TRACE_METADATA" "$TRACE_ARTIFACT" "$TRACE_TOC" \
        "$TIME_PROFILER_ARTIFACT" "$ALLOCATIONS_ARTIFACT" "$HANGS_ARTIFACT" \
        "$TRACE_VERIFICATION" "$XCRUN_COMMAND" "$SIPS_COMMAND" \
        "$PYTHON_COMMAND" "$FFPROBE_COMMAND" "$TRACE_VERIFIER" \
        "$TRACE_ARCHIVE_VERIFIER" "$ACTION_VIDEO_VERIFIER" \
        "$TRACE_RECEIPT_VERIFIER" "$STACK_SCREENSHOT" "$ACTION_VIDEO" \
        "$DRIVER_RECEIPT_VERIFIER" "$HMAC_HELPER" \
        "$TRACE_RECEIPT_VERIFIER" "$PROCESS_INSPECTOR" "$COMMAND_RUNNER" \
        "$RENDER_TOOL_BUNDLE_MANIFEST" "$EXPECTED_SOURCE_COMMIT" \
        "$TRUSTED_SOURCE_REPOSITORY" \
        "$SCENARIO" "$SUBJECT" "${WORKLOAD_METADATA:-}" \
        "${WORKLOAD_EVENTS:-}" "${WORKLOAD_READY_RECEIPT:-}" <<'PY' \
        || not_run render-trace-receipt-authentication-or-binding-invalid
import hashlib
import hmac
import os
from pathlib import Path
import re
import stat
import sys

(
    receipt_raw, anchor_raw, secret_raw, manifest_raw, subject_identity_raw,
    run_metadata_raw, render_intent_raw, render_evidence_raw,
    driver_intent_raw, driver_receipt_raw, trace_metadata_raw,
    trace_archive_raw, trace_toc_raw, time_profiler_raw, allocations_raw,
    hangs_raw, trace_verification_raw, xcrun_raw, sips_raw, python_raw,
    ffprobe_raw, trace_verifier_raw, trace_archive_verifier_raw,
    action_video_verifier_raw, receipt_verifier_raw, screenshot_raw,
    video_raw, driver_receipt_verifier_raw, render_profile_hmac_raw,
    receipt_helper_raw, process_inspector_raw, command_runner_raw,
    tool_bundle_manifest_raw, expected_source_commit, trusted_source_repository_raw,
    scenario, subject, workload_metadata_raw,
    workload_events_raw, workload_ready_raw,
) = sys.argv[1:]

keys = (
    "format_version canonicalization auth_domain campaign_id session_id nonce scenario subject "
    "campaign_manifest_sha256 trace_anchor_receipt_sha256 subject_identity_sha256 subject_process_pid "
    "subject_process_start_sec subject_process_start_usec subject_code_identity_token "
    "run_metadata_sha256 render_intent_sha256 render_evidence_sha256 driver_intent_sha256 "
    "driver_receipt_sha256 evidence_mode driver_receipt_verifier_path "
    "driver_receipt_verifier_device driver_receipt_verifier_inode "
    "driver_receipt_verifier_sha256 render_tool_bundle_manifest_path "
    "render_tool_bundle_manifest_device render_tool_bundle_manifest_inode "
    "render_tool_bundle_manifest_sha256 render_tool_bundle_source_commit "
    "render_profile_hmac_path render_profile_hmac_device "
    "render_profile_hmac_inode render_profile_hmac_sha256 render_trace_receipt_helper_path "
    "render_trace_receipt_helper_device render_trace_receipt_helper_inode "
    "render_trace_receipt_helper_sha256 process_inspector_path process_inspector_device "
    "process_inspector_inode process_inspector_sha256 command_runner_path command_runner_device "
    "command_runner_inode command_runner_sha256 workload_metadata_sha256 workload_events_sha256 "
    "workload_ready_receipt_sha256 trace_metadata_sha256 trace_archive_sha256 trace_toc_sha256 "
    "time_profiler_artifact_sha256 allocations_artifact_sha256 hangs_artifact_sha256 "
    "action_video_sha256 representative_stack_screenshot_sha256 trace_verification_sha256 "
    "capture_started_continuous_ns capture_ended_continuous_ns trace_started_epoch_ns "
    "trace_ended_epoch_ns start_anchor_continuous_ns start_anchor_epoch_ns start_anchor_width_ns "
    "end_anchor_continuous_ns end_anchor_epoch_ns end_anchor_width_ns "
    "xcrun_path xcrun_device xcrun_inode xcrun_sha256 sips_path sips_device sips_inode sips_sha256 "
    "python_path python_device python_inode python_sha256 ffprobe_path ffprobe_device ffprobe_inode "
    "ffprobe_sha256 trace_verifier_path trace_verifier_device trace_verifier_inode "
    "trace_verifier_sha256 trace_archive_verifier_path trace_archive_verifier_device "
    "trace_archive_verifier_inode trace_archive_verifier_sha256 action_video_verifier_path "
    "action_video_verifier_device action_video_verifier_inode action_video_verifier_sha256 "
    "render_trace_receipt_verifier_path render_trace_receipt_verifier_device "
    "render_trace_receipt_verifier_inode render_trace_receipt_verifier_sha256 "
    "hmac_key_identifier_sha256 result receipt_hmac_sha256"
).split()
receipt_path = Path(receipt_raw)
receipt = receipt_path.read_bytes()
lines = receipt.splitlines(keepends=True)
if not receipt.endswith(b"\n") or len(lines) != len(keys):
    raise SystemExit(1)
values = {}
for expected, line in zip(keys, lines):
    try:
        key_raw, value_raw = line[:-1].split(b"\t", 1)
        key = key_raw.decode("ascii")
        value = value_raw.decode("utf-8")
    except (ValueError, UnicodeDecodeError):
        raise SystemExit(1)
    if key != expected or not value or "\t" in value or "\r" in value:
        raise SystemExit(1)
    values[key] = value
if (values["format_version"] != "1"
        or values["canonicalization"] != "utf8-lf-tab-kv-fixed-order-domain-nul-v1"
        or values["auth_domain"] != "SPACETERM_RENDER_TRACE_RECEIPT_V1"):
    raise SystemExit(1)
secret_path = Path(secret_raw)
secret_before = secret_path.lstat()
if (not stat.S_ISREG(secret_before.st_mode) or secret_path.is_symlink()
        or secret_before.st_uid != os.geteuid() or secret_before.st_nlink != 1
        or secret_before.st_size != 65 or secret_before.st_mode & 0o7777 != 0o400):
    raise SystemExit(1)
secret_fd = os.open(
    secret_path,
    os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0),
)
try:
    secret_opened = os.fstat(secret_fd)
    secret = os.read(secret_fd, 66)
    secret_after_fd = os.fstat(secret_fd)
finally:
    os.close(secret_fd)
secret_after_path = secret_path.lstat()
stable_fields = (
    "st_dev", "st_ino", "st_mode", "st_uid", "st_nlink", "st_size",
    "st_mtime_ns", "st_ctime_ns",
)
if (re.fullmatch(rb"[0-9a-f]{64}\n", secret) is None
        or any(getattr(secret_before, field) != getattr(secret_opened, field)
               for field in stable_fields)
        or any(getattr(secret_before, field) != getattr(secret_after_fd, field)
               for field in stable_fields)
        or any(getattr(secret_before, field) != getattr(secret_after_path, field)
               for field in stable_fields)):
    raise SystemExit(1)
key = bytes.fromhex(secret[:-1].decode("ascii"))
unsigned = b"".join(lines[:-1])
actual_hmac = hmac.new(
    key, b"SPACETERM_RENDER_TRACE_RECEIPT_V1\0" + unsigned, hashlib.sha256
).hexdigest()
if not hmac.compare_digest(actual_hmac, values["receipt_hmac_sha256"]):
    raise SystemExit(1)
if values["hmac_key_identifier_sha256"] != hashlib.sha256(secret[:-1]).hexdigest():
    raise SystemExit(1)

manifest_keys = (
    "format_version canonicalization auth_domain campaign_id session_id nonce scenario subject "
    "subject_identity_sha256 render_intent_path render_evidence_path driver_intent_path "
    "driver_receipt_path trace_anchor_receipt_path trace_anchor_receipt_parent_device "
    "trace_anchor_receipt_parent_inode trace_receipt_path trace_receipt_parent_device "
    "trace_receipt_parent_inode campaign_secret_device campaign_secret_inode "
    "render_tool_bundle_manifest_path render_tool_bundle_manifest_device "
    "render_tool_bundle_manifest_inode render_tool_bundle_manifest_sha256 "
    "render_tool_bundle_source_commit "
    "render_profile_hmac_path render_profile_hmac_device render_profile_hmac_inode "
    "render_profile_hmac_sha256 render_trace_receipt_helper_path "
    "render_trace_receipt_helper_device render_trace_receipt_helper_inode "
    "render_trace_receipt_helper_sha256 process_inspector_path process_inspector_device "
    "process_inspector_inode process_inspector_sha256 trace_verifier_path "
    "trace_verifier_device trace_verifier_inode trace_verifier_sha256 command_runner_path "
    "command_runner_device command_runner_inode command_runner_sha256 "
    "hmac_key_identifier_sha256 manifest_hmac_sha256"
).split()
manifest = Path(manifest_raw).read_bytes()
manifest_lines = manifest.splitlines(keepends=True)
if not manifest.endswith(b"\n") or len(manifest_lines) != len(manifest_keys):
    raise SystemExit(1)
manifest_values = {}
for expected, line in zip(manifest_keys, manifest_lines):
    try:
        key_raw, value_raw = line[:-1].split(b"\t", 1)
        key_name = key_raw.decode("ascii")
        value = value_raw.decode("utf-8")
    except (ValueError, UnicodeDecodeError):
        raise SystemExit(1)
    if key_name != expected or not value or "\t" in value or "\r" in value:
        raise SystemExit(1)
    manifest_values[key_name] = value
manifest_hmac = hmac.new(
    key, b"SPACETERM_RENDER_CAMPAIGN_CASE_MANIFEST_V1\0"
    + b"".join(manifest_lines[:-1]), hashlib.sha256,
).hexdigest()
bundle_path = Path(tool_bundle_manifest_raw)
bundle_stat = bundle_path.lstat()
if (not bundle_path.is_absolute() or bundle_path.is_symlink()
        or bundle_path.resolve(strict=True) != bundle_path
        or not stat.S_ISREG(bundle_stat.st_mode) or bundle_stat.st_nlink != 1
        or bundle_stat.st_mode & 0o222):
    raise SystemExit(1)
if (not hmac.compare_digest(manifest_hmac, manifest_values["manifest_hmac_sha256"])
        or values["campaign_manifest_sha256"] != hashlib.sha256(manifest).hexdigest()
        or manifest_values["campaign_secret_device"] != str(secret_before.st_dev)
        or manifest_values["campaign_secret_inode"] != str(secret_before.st_ino)
        or manifest_values["trace_anchor_receipt_path"] != anchor_raw
        or manifest_values["trace_receipt_path"] != receipt_raw
        or manifest_values["render_intent_path"] != render_intent_raw
        or manifest_values["render_evidence_path"] != render_evidence_raw
        or manifest_values["driver_intent_path"] != driver_intent_raw
        or manifest_values["driver_receipt_path"] != driver_receipt_raw
        or manifest_values["render_tool_bundle_manifest_path"] != tool_bundle_manifest_raw
        or manifest_values["render_tool_bundle_manifest_device"] != str(bundle_stat.st_dev)
        or manifest_values["render_tool_bundle_manifest_inode"] != str(bundle_stat.st_ino)
        or manifest_values["render_tool_bundle_manifest_sha256"]
            != hashlib.sha256(bundle_path.read_bytes()).hexdigest()
        or manifest_values["render_tool_bundle_source_commit"] != expected_source_commit):
    raise SystemExit(1)
for shared_key in (
    "campaign_id", "session_id", "nonce", "scenario", "subject",
    "subject_identity_sha256", "render_tool_bundle_manifest_path",
    "render_tool_bundle_manifest_device", "render_tool_bundle_manifest_inode",
    "render_tool_bundle_manifest_sha256", "render_tool_bundle_source_commit",
    "hmac_key_identifier_sha256",
):
    if manifest_values[shared_key] != values[shared_key]:
        raise SystemExit(1)

def exact_ordered(path_raw, expected_keys):
    payload = Path(path_raw).read_bytes()
    rows = payload.splitlines(keepends=True)
    if not payload.endswith(b"\n") or len(rows) != len(expected_keys):
        raise SystemExit(1)
    parsed = {}
    for expected, row in zip(expected_keys, rows):
        try:
            key_raw, value_raw = row[:-1].split(b"\t", 1)
            key_name = key_raw.decode("ascii")
            value = value_raw.decode("utf-8")
        except (ValueError, UnicodeDecodeError):
            raise SystemExit(1)
        if key_name != expected or not value or "\t" in value or "\r" in value:
            raise SystemExit(1)
        parsed[key_name] = value
    return parsed, payload

intent_keys = (
    "format_version canonicalization auth_domain scenario subject campaign_id session_id nonce "
    "plan_sha256 plan_metadata_sha256 pair_metadata_sha256 run_intent_sha256 command_sha256 "
    "environment_sha256 font_sha256 initial_grid_sha256 subject_identity_sha256 "
    "subject_process_pid subject_process_start_identity expected_driver_events_path "
    "expected_driver_parent_device expected_driver_parent_inode action_video_path "
    "action_video_parent_device action_video_parent_inode final_metadata_path "
    "final_metadata_parent_device final_metadata_parent_inode warmup_ms measured_duration_ms "
    "required_action_count action_interval_ms hmac_key_identifier_sha256 intent_hmac_sha256"
).split()
evidence_keys = (
    "format_version canonicalization auth_domain intent_sha256 scenario subject campaign_id "
    "session_id nonce subject_identity_sha256 subject_process_pid subject_process_start_identity "
    "driver_events_path driver_events_device driver_events_inode driver_events_sha256 "
    "action_video_path action_video_device action_video_inode action_video_sha256 "
    "render_workload_metadata_sha256 required_action_count completed_action_count "
    "action_interval_ms started_continuous_ns ended_continuous_ns measured_span_ns result "
    "hmac_key_identifier_sha256 evidence_hmac_sha256"
).split()
intent_values, intent_payload = exact_ordered(render_intent_raw, intent_keys)
evidence_values, _ = exact_ordered(render_evidence_raw, evidence_keys)
for tuple_key in ("campaign_id", "session_id", "nonce", "scenario", "subject"):
    if not (
        values[tuple_key]
        == manifest_values[tuple_key]
        == intent_values[tuple_key]
        == evidence_values[tuple_key]
    ):
        raise SystemExit(1)
if (
    intent_values["subject_identity_sha256"] != values["subject_identity_sha256"]
    or evidence_values["subject_identity_sha256"] != values["subject_identity_sha256"]
    or intent_values["final_metadata_path"] != render_evidence_raw
    or evidence_values["intent_sha256"] != hashlib.sha256(intent_payload).hexdigest()
):
    raise SystemExit(1)

anchor_keys = (
    "format_version canonicalization auth_domain campaign_id session_id nonce scenario subject "
    "campaign_manifest_sha256 subject_identity_sha256 run_metadata_sha256 render_intent_sha256 "
    "render_evidence_sha256 trace_metadata_sha256 capture_started_continuous_ns "
    "capture_ended_continuous_ns trace_started_epoch_ns trace_ended_epoch_ns "
    "start_anchor_continuous_ns start_anchor_epoch_ns start_anchor_width_ns "
    "end_anchor_continuous_ns end_anchor_epoch_ns end_anchor_width_ns "
    "hmac_key_identifier_sha256 result anchor_hmac_sha256"
).split()
anchor = Path(anchor_raw).read_bytes()
anchor_lines = anchor.splitlines(keepends=True)
if not anchor.endswith(b"\n") or len(anchor_lines) != len(anchor_keys):
    raise SystemExit(1)
anchor_values = {}
for expected, line in zip(anchor_keys, anchor_lines):
    try:
        key_raw, value_raw = line[:-1].split(b"\t", 1)
        key_name = key_raw.decode("ascii")
        value = value_raw.decode("utf-8")
    except (ValueError, UnicodeDecodeError):
        raise SystemExit(1)
    if key_name != expected or not value or "\t" in value or "\r" in value:
        raise SystemExit(1)
    anchor_values[key_name] = value
anchor_hmac = hmac.new(
    key, b"SPACETERM_RENDER_TRACE_ANCHOR_V1\0" + b"".join(anchor_lines[:-1]),
    hashlib.sha256,
).hexdigest()
if (not hmac.compare_digest(anchor_hmac, anchor_values["anchor_hmac_sha256"])
        or values["trace_anchor_receipt_sha256"] != hashlib.sha256(anchor).hexdigest()
        or anchor_values["result"] != "PASS"):
    raise SystemExit(1)
for shared_key in (
    "campaign_id", "session_id", "nonce", "scenario", "subject",
    "campaign_manifest_sha256", "subject_identity_sha256", "run_metadata_sha256",
    "render_intent_sha256", "render_evidence_sha256", "trace_metadata_sha256",
    "capture_started_continuous_ns", "capture_ended_continuous_ns",
    "trace_started_epoch_ns", "trace_ended_epoch_ns", "start_anchor_continuous_ns",
    "start_anchor_epoch_ns", "start_anchor_width_ns", "end_anchor_continuous_ns",
    "end_anchor_epoch_ns", "end_anchor_width_ns", "hmac_key_identifier_sha256",
):
    if anchor_values[shared_key] != values[shared_key]:
        raise SystemExit(1)

def digest(raw: str) -> str:
    return hashlib.sha256(Path(raw).read_bytes()).hexdigest()

hash_paths = {
    "campaign_manifest": manifest_raw, "subject_identity": subject_identity_raw,
    "run_metadata": run_metadata_raw, "render_intent": render_intent_raw,
    "render_evidence": render_evidence_raw, "driver_intent": driver_intent_raw,
    "driver_receipt": driver_receipt_raw, "trace_metadata": trace_metadata_raw,
    "trace_archive": trace_archive_raw, "trace_toc": trace_toc_raw,
    "time_profiler_artifact": time_profiler_raw, "allocations_artifact": allocations_raw,
    "hangs_artifact": hangs_raw, "trace_verification": trace_verification_raw,
    "representative_stack_screenshot": screenshot_raw, "action_video": video_raw,
}
for prefix, raw in hash_paths.items():
    if values[f"{prefix}_sha256"] != digest(raw):
        raise SystemExit(1)

tool_paths = {
    "driver_receipt_verifier": driver_receipt_verifier_raw,
    "render_profile_hmac": render_profile_hmac_raw,
    "render_trace_receipt_helper": receipt_helper_raw,
    "process_inspector": process_inspector_raw,
    "command_runner": command_runner_raw,
    "xcrun": xcrun_raw, "sips": sips_raw, "python": python_raw,
    "ffprobe": ffprobe_raw, "trace_verifier": trace_verifier_raw,
    "trace_archive_verifier": trace_archive_verifier_raw,
    "action_video_verifier": action_video_verifier_raw,
    "render_trace_receipt_verifier": receipt_verifier_raw,
}
for prefix, raw in tool_paths.items():
    path = Path(raw)
    before = path.lstat()
    sealed_system_tool = (
        str(path).startswith(("/usr/bin/", "/bin/", "/usr/sbin/", "/sbin/"))
        and before.st_uid == 0 and before.st_nlink >= 1
        and not before.st_mode & 0o022
    )
    if (not path.is_absolute() or path.is_symlink() or not stat.S_ISREG(before.st_mode)
            or (before.st_nlink != 1 and not sealed_system_tool)
            or (before.st_mode & 0o222 and not sealed_system_tool)
            or not before.st_mode & 0o111):
        raise SystemExit(1)
    if (values[f"{prefix}_path"] != str(path.resolve(strict=True))
            or values[f"{prefix}_device"] != str(before.st_dev)
            or values[f"{prefix}_inode"] != str(before.st_ino)
            or values[f"{prefix}_sha256"] != digest(raw)):
        raise SystemExit(1)
    after = path.lstat()
    if (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns) != (
        after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns
    ):
        raise SystemExit(1)

for prefix in (
    "render_profile_hmac", "render_trace_receipt_helper", "process_inspector",
    "trace_verifier", "command_runner",
):
    for suffix in ("path", "device", "inode", "sha256"):
        if manifest_values[f"{prefix}_{suffix}"] != values[f"{prefix}_{suffix}"]:
            raise SystemExit(1)

for numeric_key in (
    "subject_process_pid", "subject_process_start_sec", "subject_process_start_usec",
    "capture_started_continuous_ns", "capture_ended_continuous_ns",
    "trace_started_epoch_ns", "trace_ended_epoch_ns", "start_anchor_continuous_ns",
    "start_anchor_epoch_ns", "start_anchor_width_ns", "end_anchor_continuous_ns",
    "end_anchor_epoch_ns", "end_anchor_width_ns",
):
    if not values[numeric_key].isascii() or not values[numeric_key].isdecimal():
        raise SystemExit(1)
if (int(values["subject_process_pid"]) <= 0
        or int(values["subject_process_start_usec"]) > 999_999
        or int(values["capture_ended_continuous_ns"]) <= int(values["capture_started_continuous_ns"])
        or int(values["trace_ended_epoch_ns"]) <= int(values["trace_started_epoch_ns"])
        or int(values["start_anchor_width_ns"]) > 10_000_000
        or int(values["end_anchor_width_ns"]) > 10_000_000):
    raise SystemExit(1)
start_offset = int(values["start_anchor_continuous_ns"]) - int(values["start_anchor_epoch_ns"])
end_offset = int(values["end_anchor_continuous_ns"]) - int(values["end_anchor_epoch_ns"])
mapping = (start_offset + end_offset) // 2
if (abs(start_offset - end_offset) > 50_000_000
        or abs(int(values["trace_started_epoch_ns"]) + mapping
               - int(values["capture_started_continuous_ns"])) > 50_000_000
        or abs(int(values["trace_ended_epoch_ns"]) + mapping
               - int(values["capture_ended_continuous_ns"])) > 50_000_000
        or int(values["start_anchor_continuous_ns"])
            > int(values["capture_started_continuous_ns"])
        or int(values["end_anchor_continuous_ns"])
            < int(values["capture_ended_continuous_ns"])):
    raise SystemExit(1)

zero = "0" * 64
if scenario == "perf-render-sustained-output":
    workload_mode = "sustained-output-v3"
    workload_paths = (workload_metadata_raw, workload_events_raw, workload_ready_raw)
    if not all(workload_paths):
        raise SystemExit(1)
    workload_hashes = tuple(digest(path) for path in workload_paths)
else:
    workload_mode = "zero-workload"
    if any((workload_metadata_raw, workload_events_raw, workload_ready_raw)):
        raise SystemExit(1)
    workload_hashes = (zero, zero, zero)
if (values["scenario"] != scenario or values["subject"] != subject
        or values["evidence_mode"] != workload_mode
        or tuple(values[key] for key in (
            "workload_metadata_sha256", "workload_events_sha256",
            "workload_ready_receipt_sha256")) != workload_hashes
        or values["result"] != "PASS"):
    raise SystemExit(1)
PY
    RENDER_TOOL_IDENTITY_SNAPSHOT="$(render_tool_identity_snapshot)" \
        || not_run render-analysis-tool-identity-snapshot-failed
fi

reject_unknown_kv "$PLAN_METADATA" \
    "format_version scenario plan_sha256 warmup_ms measured_duration_ms profile_kind required_action required_action_count required_unchanged_row_windows required_changed_row_windows required_overlay_change_windows required_resize_cycles trace_instruments manual_review_schema" \
    render-plan-metadata
[[ "$(required_kv "$PLAN_METADATA" format_version plan)" == 2 ]] \
    || not_run unsupported-render-plan-format
[[ "$(required_kv "$PLAN_METADATA" scenario plan)" == "$SCENARIO" ]] \
    || not_run render-plan-scenario-mismatch
plan_hash="$(required_kv "$PLAN_METADATA" plan_sha256 plan)"
require_hash "$plan_hash" render-plan-sha256
[[ "$(sha256 "$PLAN")" == "$plan_hash" ]] || not_run render-plan-hash-mismatch
[[ "$("$HEAD_COMMAND" -n 1 "$PLAN")" == "$PLAN_HEADER" ]] || not_run invalid-render-plan-header
[[ "$(required_kv "$PLAN_METADATA" trace_instruments plan)" \
        == Time-Profiler,Allocations,Hangs \
    && "$(required_kv "$PLAN_METADATA" manual_review_schema plan)" \
        == render-call-tree-review-v1 ]] \
    || not_run render-plan-evidence-requirements-weakened

case "$SCENARIO" in
    perf-render-idle-cursor-blink)
        expected_kind=idle-cursor-blink
        expected_warmup=15000
        expected_duration=120000
        expected_action=cursor-blink-observation
        expected_count=60
        expected_unchanged=60
        expected_changed=0
        expected_overlay=0
        expected_resize=0
        expected_prefix=cursor-blink
        expected_interval=2000
        expected_plan_action=checkpoint
        ;;
    perf-render-text-blink)
        expected_kind=text-blink
        expected_warmup=15000
        expected_duration=120000
        expected_action=text-blink-observation
        expected_count=60
        expected_unchanged=60
        expected_changed=0
        expected_overlay=0
        expected_resize=0
        expected_prefix=text-blink
        expected_interval=2000
        expected_plan_action=checkpoint
        ;;
    perf-render-sustained-output)
        expected_kind=sustained-output
        expected_warmup=30000
        expected_duration=180000
        expected_action=changed-row-observation
        expected_count=18
        expected_unchanged=0
        expected_changed=18
        expected_overlay=0
        expected_resize=0
        expected_prefix=changed-row
        expected_interval=10000
        expected_plan_action=checkpoint
        ;;
    perf-render-selection)
        expected_kind=selection
        expected_warmup=15000
        expected_duration=120000
        expected_action=selection-overlay-cycle
        expected_count=30
        expected_unchanged=30
        expected_changed=0
        expected_overlay=30
        expected_resize=0
        expected_prefix=selection-overlay
        expected_interval=4000
        expected_plan_action=checkpoint
        ;;
    perf-render-marked-text)
        expected_kind=marked-text
        expected_warmup=15000
        expected_duration=120000
        expected_action=marked-text-overlay-cycle
        expected_count=24
        expected_unchanged=24
        expected_changed=0
        expected_overlay=24
        expected_resize=0
        expected_prefix=marked-text-overlay
        expected_interval=5000
        expected_plan_action=checkpoint
        ;;
    perf-render-live-resize)
        expected_kind=live-resize
        expected_warmup=0
        expected_duration=180000
        expected_action=live-resize-cycle
        expected_count=180
        expected_unchanged=0
        expected_changed=180
        expected_overlay=0
        expected_resize=180
        expected_prefix=resize
        expected_interval=1000
        expected_plan_action=resize-grid
        ;;
esac

for field_and_value in \
    "profile_kind:$expected_kind" \
    "warmup_ms:$expected_warmup" \
    "measured_duration_ms:$expected_duration" \
    "required_action:$expected_action" \
    "required_action_count:$expected_count" \
    "required_unchanged_row_windows:$expected_unchanged" \
    "required_changed_row_windows:$expected_changed" \
    "required_overlay_change_windows:$expected_overlay" \
    "required_resize_cycles:$expected_resize"; do
    field="${field_and_value%%:*}"
    expected="${field_and_value#*:}"
    [[ "$(required_kv "$PLAN_METADATA" "$field" plan)" == "$expected" ]] \
        || not_run "render-plan-$field-weakened"
done

"$AWK_COMMAND" -F '\t' -v warmup="$expected_warmup" -v duration="$expected_duration" \
    -v count="$expected_count" -v prefix="$expected_prefix" \
    -v interval="$expected_interval" -v plan_action="$expected_plan_action" \
    -v is_resize="$([[ "$SCENARIO" == perf-render-live-resize ]] && printf 1 || printf 0)" '
    BEGIN { OFS = "\t" }
    NR == 1 { next }
    NF != 5 || $1 !~ /^[A-Za-z0-9][A-Za-z0-9._-]*$/ \
        || $2 !~ /^[0-9]+$/ || $4 !~ /^-?[0-9]+$/ || $5 !~ /^-?[0-9]+$/ \
        || seen[$1]++ || (NR > 2 && $2 + 0 < prior) { bad = 1 }
    { prior = $2 + 0 }
    $1 ~ ("^" prefix "-[0-9][0-9][0-9]$") {
        action_index = action_count
        expected_id = sprintf("%s-%03d", prefix, action_index)
        if ($1 != expected_id || $2 + 0 != warmup + action_index * interval \
            || $3 != plan_action) bad = 1
        if (!is_resize && ($4 + 0 != action_index || $5 != "0")) bad = 1
        if (is_resize) {
            sign = action_index % 2 == 0 ? 1 : -1
            mode = action_index % 3
            expected_a = mode == 0 ? sign * 120 : (mode == 2 ? sign * 96 : 0)
            expected_b = mode == 1 ? sign * 80 : (mode == 2 ? sign * 64 : 0)
            if ($4 + 0 != expected_a || $5 + 0 != expected_b) bad = 1
        }
        action_count += 1
        next
    }
    $1 == "profile-warmup-start" {
        if (is_resize || $2 != "0" || $3 != "checkpoint" || $4 != "0" || $5 != "0") bad = 1
        warmup_start += 1
        next
    }
    $1 == "measured-start" {
        if ($2 + 0 != warmup || $3 != "checkpoint" || $4 != "0" || $5 != "0") bad = 1
        measured_start += 1
        next
    }
    $1 == "measured-end" {
        if ($2 + 0 != warmup + duration || $3 != "checkpoint" || $4 != "0" || $5 != "0") bad = 1
        measured_end += 1
        next
    }
    $1 == "stop" {
        if ($2 + 0 != warmup + duration || $3 != "stop" || $4 != "0" || $5 != "0") bad = 1
        stop += 1
        stop_row = NR
        next
    }
    { bad = 1 }
    END {
        expected_rows = count + (is_resize ? 3 : 4)
        exit bad || action_count != count || measured_start != 1 || measured_end != 1 \
            || stop != 1 || stop_row != NR || warmup_start != (is_resize ? 0 : 1) \
            || NR - 1 != expected_rows
    }
' "$PLAN" || not_run render-plan-actions-or-cadence-mismatch

reject_unknown_kv "$PAIR_METADATA" \
    "format_version pair_id scenario plan_sha256 workload_sha256 command_sha256 environment_sha256 font_sha256 initial_grid_sha256 duration_ms spaceterm_subject_identity_sha256 ghostty_subject_identity_sha256" \
    pair-metadata
[[ "$(required_kv "$PAIR_METADATA" format_version pair)" == 1 \
    && "$(required_kv "$PAIR_METADATA" scenario pair)" == "$SCENARIO" \
    && "$(required_kv "$PAIR_METADATA" plan_sha256 pair)" == "$plan_hash" \
    && "$(required_kv "$PAIR_METADATA" duration_ms pair)" == "$expected_duration" ]] \
    || not_run render-pair-plan-or-duration-mismatch
pair_id="$(required_kv "$PAIR_METADATA" pair_id pair)"
[[ "$pair_id" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] || not_run invalid-render-pair-id
for pair_hash_key in workload_sha256 command_sha256 environment_sha256 font_sha256 \
    initial_grid_sha256 spaceterm_subject_identity_sha256 \
    ghostty_subject_identity_sha256; do
    require_hash "$(required_kv "$PAIR_METADATA" "$pair_hash_key" pair)" \
        "pair-$pair_hash_key"
done

identity_schema="format_version subject app_bundle_path bundle_identifier bundle_version executable_path executable_sha256 executable_device executable_inode executable_fsid signature_valid signing_identifier team_identifier cdhash process_pid process_start_identity identity_status"
reject_unknown_kv "$SUBJECT_IDENTITY" "$identity_schema" subject-identity
reject_unknown_kv "$COMPARISON_SUBJECT_IDENTITY" "$identity_schema" comparison-subject-identity
comparison_subject=ghostty
[[ "$SUBJECT" == spaceterm ]] || comparison_subject=spaceterm
for identity_tuple in \
    "$SUBJECT:$SUBJECT_IDENTITY" \
    "$comparison_subject:$COMPARISON_SUBJECT_IDENTITY"; do
    identity_subject="${identity_tuple%%:*}"
    identity_file="${identity_tuple#*:}"
    [[ "$(required_kv "$identity_file" format_version identity)" == 1 \
        && "$(required_kv "$identity_file" subject identity)" == "$identity_subject" \
        && "$(required_kv "$identity_file" signature_valid identity)" == true \
        && "$(required_kv "$identity_file" identity_status identity)" == frozen ]] \
        || not_run "${identity_subject}-identity-is-not-frozen"
    for identity_key in app_bundle_path bundle_identifier bundle_version executable_path \
        executable_sha256 executable_device executable_inode executable_fsid \
        signing_identifier team_identifier cdhash process_pid process_start_identity; do
        identity_value="$(required_kv "$identity_file" "$identity_key" identity)"
        [[ -n "$identity_value" ]] || not_run "missing-${identity_subject}-$identity_key"
    done
    require_hash "$(required_kv "$identity_file" executable_sha256 identity)" \
        "${identity_subject}-executable-sha256"
    for numeric_identity_key in executable_device executable_inode executable_fsid process_pid; do
        require_uint "$(required_kv "$identity_file" "$numeric_identity_key" identity)" \
            "${identity_subject}-$numeric_identity_key"
    done
    [[ "$(sha256 "$identity_file")" \
        == "$(required_kv "$PAIR_METADATA" \
            "${identity_subject}_subject_identity_sha256" pair)" ]] \
        || not_run "paired-${identity_subject}-identity-mismatch"
done
subject_hash="$(sha256 "$SUBJECT_IDENTITY")"
comparison_subject_hash="$(sha256 "$COMPARISON_SUBJECT_IDENTITY")"
pair_hash="$(sha256 "$PAIR_METADATA")"

reject_unknown_kv "$RUN_METADATA" \
    "format_version subject subject_identity_sha256 scenario scenario_plan_sha256 workload_sha256 command_sha256 environment_sha256 font_sha256 initial_grid_sha256 measured_duration_ms process_pid process_start_identity run_intent_sha256 native_observation_sha256 native_runtime_metadata_sha256 native_failure_actions_sha256 native_failure_action_enabled native_failure_request_count native_failure_result_count native_failure_resource_staged_count native_failure_resource_staged_bytes native_failure_resource_rolled_back_count native_failure_resource_rolled_back_bytes trace_provisional_receipt_sha256 performance_tail_receipt_sha256 performance_quit_receipt_sha256 subject_exit_receipt_sha256 lifecycle_ready_receipt_sha256 lifecycle_registration_receipt_sha256 lifecycle_helper_sha256 terminator_source_sha256 terminator_binary_sha256 evidence_mode status" \
    run-metadata
[[ "$(required_kv "$RUN_METADATA" format_version run)" == 4 \
    && "$(required_kv "$RUN_METADATA" subject run)" == "$SUBJECT" \
    && "$(required_kv "$RUN_METADATA" subject_identity_sha256 run)" \
        == "$subject_hash" \
    && "$(required_kv "$RUN_METADATA" scenario run)" == "$SCENARIO" \
    && "$(required_kv "$RUN_METADATA" scenario_plan_sha256 run)" == "$plan_hash" \
    && "$(required_kv "$RUN_METADATA" measured_duration_ms run)" \
        == "$expected_duration" \
    && "$(required_kv "$RUN_METADATA" evidence_mode run)" == production \
    && "$(required_kv "$RUN_METADATA" status run)" == complete ]] \
    || not_run render-run-metadata-mismatch
for lifecycle_hash in trace_provisional_receipt_sha256 performance_tail_receipt_sha256 \
    performance_quit_receipt_sha256 subject_exit_receipt_sha256 \
    lifecycle_ready_receipt_sha256 lifecycle_registration_receipt_sha256 \
    lifecycle_helper_sha256 terminator_source_sha256 terminator_binary_sha256; do
    require_hash "$(required_kv "$RUN_METADATA" "$lifecycle_hash" run)" \
        "render-run-$lifecycle_hash"
done
if [[ "$SUBJECT" == spaceterm ]]; then
    for native_hash in native_observation_sha256 native_runtime_metadata_sha256 \
        native_failure_actions_sha256; do
        require_hash "$(required_kv "$RUN_METADATA" "$native_hash" run)" \
            "render-run-$native_hash"
    done
    [[ "$(required_kv "$RUN_METADATA" native_failure_action_enabled run)" == false ]] \
        || not_run render-run-failure-controller-enabled
    for counter in native_failure_request_count native_failure_result_count \
        native_failure_resource_staged_count native_failure_resource_staged_bytes \
        native_failure_resource_rolled_back_count \
        native_failure_resource_rolled_back_bytes; do
        [[ "$(required_kv "$RUN_METADATA" "$counter" run)" == 0 ]] \
            || not_run "render-run-$counter-is-not-zero"
    done
else
    for native_field in native_observation_sha256 native_runtime_metadata_sha256 \
        native_failure_actions_sha256 native_failure_action_enabled \
        native_failure_request_count native_failure_result_count \
        native_failure_resource_staged_count native_failure_resource_staged_bytes \
        native_failure_resource_rolled_back_count \
        native_failure_resource_rolled_back_bytes; do
        [[ "$(required_kv "$RUN_METADATA" "$native_field" run)" == not-applicable ]] \
            || not_run "ghostty-render-run-$native_field-is-not-applicable"
    done
fi
for run_pair_key in workload_sha256 command_sha256 environment_sha256 font_sha256 \
    initial_grid_sha256; do
    [[ "$(required_kv "$RUN_METADATA" "$run_pair_key" run)" \
        == "$(required_kv "$PAIR_METADATA" "$run_pair_key" pair)" ]] \
        || not_run "render-run-$run_pair_key-mismatch"
done
[[ "$(required_kv "$RUN_METADATA" process_pid run)" \
        == "$(required_kv "$SUBJECT_IDENTITY" process_pid identity)" \
    && "$(required_kv "$RUN_METADATA" process_start_identity run)" \
        == "$(required_kv "$SUBJECT_IDENTITY" process_start_identity identity)" ]] \
    || not_run render-run-process-identity-mismatch
process_pid="$(required_kv "$SUBJECT_IDENTITY" process_pid identity)"

reject_unknown_kv "$RENDER_WORKLOAD_METADATA" \
    "format_version scenario subject subject_identity_sha256 pair_metadata_sha256 driver_events_sha256 action_video_sha256 required_action_count completed_action_count started_continuous_ns ended_continuous_ns status" \
    render-workload-metadata
[[ "$(required_kv "$RENDER_WORKLOAD_METADATA" format_version workload)" == 1 \
    && "$(required_kv "$RENDER_WORKLOAD_METADATA" scenario workload)" == "$SCENARIO" \
    && "$(required_kv "$RENDER_WORKLOAD_METADATA" subject workload)" == "$SUBJECT" \
    && "$(required_kv "$RENDER_WORKLOAD_METADATA" subject_identity_sha256 workload)" \
        == "$subject_hash" \
    && "$(required_kv "$RENDER_WORKLOAD_METADATA" pair_metadata_sha256 workload)" \
        == "$pair_hash" \
    && "$(required_kv "$RENDER_WORKLOAD_METADATA" driver_events_sha256 workload)" \
        == "$(sha256 "$DRIVER_EVENTS")" \
    && "$(required_kv "$RENDER_WORKLOAD_METADATA" action_video_sha256 workload)" \
        == "$(sha256 "$ACTION_VIDEO")" \
    && "$(required_kv "$RENDER_WORKLOAD_METADATA" required_action_count workload)" \
        == "$expected_count" \
    && "$(required_kv "$RENDER_WORKLOAD_METADATA" completed_action_count workload)" \
        == "$expected_count" \
    && "$(required_kv "$RENDER_WORKLOAD_METADATA" status workload)" == complete ]] \
    || not_run render-workload-metadata-mismatch
reject_unknown_kv "$RENDER_INTENT" \
    "format_version canonicalization auth_domain scenario subject campaign_id session_id nonce plan_sha256 plan_metadata_sha256 pair_metadata_sha256 run_intent_sha256 command_sha256 environment_sha256 font_sha256 initial_grid_sha256 subject_identity_sha256 subject_process_pid subject_process_start_identity expected_driver_events_path expected_driver_parent_device expected_driver_parent_inode action_video_path action_video_parent_device action_video_parent_inode final_metadata_path final_metadata_parent_device final_metadata_parent_inode warmup_ms measured_duration_ms required_action_count action_interval_ms hmac_key_identifier_sha256 intent_hmac_sha256" \
    render-profile-intent
verify_render_hmac "$RENDER_INTENT" SPACETERM_RENDER_PROFILE_INTENT_V1 \
    intent_hmac_sha256 || not_run render-profile-intent-authentication-invalid
[[ "$(required_kv "$RENDER_INTENT" format_version intent)" == 1 \
    && "$(required_kv "$RENDER_INTENT" canonicalization intent)" \
        == utf8-lf-tab-kv-fixed-order-domain-nul-v1 \
    && "$(required_kv "$RENDER_INTENT" auth_domain intent)" \
        == SPACETERM_RENDER_PROFILE_INTENT_V1 \
    && "$(required_kv "$RENDER_INTENT" scenario intent)" == "$SCENARIO" \
    && "$(required_kv "$RENDER_INTENT" subject intent)" == "$SUBJECT" \
    && "$(required_kv "$RENDER_INTENT" plan_sha256 intent)" == "$plan_hash" \
    && "$(required_kv "$RENDER_INTENT" plan_metadata_sha256 intent)" \
        == "$(sha256 "$PLAN_METADATA")" \
    && "$(required_kv "$RENDER_INTENT" pair_metadata_sha256 intent)" == "$pair_hash" \
    && "$(required_kv "$RENDER_INTENT" run_intent_sha256 intent)" \
        == "$(required_kv "$RUN_METADATA" run_intent_sha256 run)" \
    && "$(required_kv "$RENDER_INTENT" subject_identity_sha256 intent)" \
        == "$subject_hash" \
    && "$(required_kv "$RENDER_INTENT" subject_process_pid intent)" == "$process_pid" \
    && "$(required_kv "$RENDER_INTENT" subject_process_start_identity intent)" \
        == "$(required_kv "$SUBJECT_IDENTITY" process_start_identity identity)" \
    && "$(required_kv "$RENDER_INTENT" warmup_ms intent)" == "$expected_warmup" \
    && "$(required_kv "$RENDER_INTENT" measured_duration_ms intent)" \
        == "$expected_duration" \
    && "$(required_kv "$RENDER_INTENT" required_action_count intent)" \
        == "$expected_count" \
    && "$(required_kv "$RENDER_INTENT" action_interval_ms intent)" \
        == "$expected_interval" ]] \
    || not_run render-profile-intent-does-not-bind-run
for intent_hash_field in nonce run_intent_sha256 command_sha256 environment_sha256 font_sha256 \
    initial_grid_sha256 hmac_key_identifier_sha256 intent_hmac_sha256; do
    require_hash "$(required_kv "$RENDER_INTENT" "$intent_hash_field" intent)" \
        "render-intent-$intent_hash_field"
done
[[ "$(required_kv "$RENDER_INTENT" hmac_key_identifier_sha256 intent)" \
    == "$RENDER_HMAC_KEY_IDENTIFIER" ]] \
    || not_run render-profile-intent-hmac-key-mismatch
"$PYTHON_COMMAND" - "$PAIR_RESULT" "$CAMPAIGN_SECRET_FILE" "$CASE_REPORT" \
    "$SUBJECT" "$SCENARIO" "$RUN_METADATA" "$PAIR_METADATA" "$RENDER_INTENT" \
    "$TRACE_METADATA" "$TRACE_ARTIFACT" "$MANUAL_REVIEW" "$STACK_SCREENSHOT" \
    "$ACTION_VIDEO" <<'PY' \
    || not_run performance-pair-result-or-case-report-invalid
import hashlib, hmac, pathlib, stat, struct, sys, unicodedata, zipfile

(pair_name, secret_name, case_name, subject, scenario, run_name, pair_metadata_name,
 intent_name, trace_name, archive_name, manual_name, screenshot_name,
 video_name) = sys.argv[1:]
result_keys = (
 "format_version campaign_id pair_metadata_sha256 scenario_plan_sha256 workload_sha256 "
 "command_sha256 environment_sha256 font_sha256 initial_grid_sha256 "
 "spaceterm_session_id spaceterm_nonce spaceterm_run_intent_sha256 "
 "spaceterm_run_metadata_sha256 spaceterm_driver_intent_sha256 "
 "spaceterm_driver_events_sha256 spaceterm_driver_receipt_sha256 "
 "spaceterm_window_identity_sha256 spaceterm_driver_binary_sha256 "
 "spaceterm_driver_source_sha256 spaceterm_driver_controller_sha256 "
 "spaceterm_plan_start_gate_sha256 spaceterm_tail_receipt_sha256 "
 "spaceterm_quit_receipt_sha256 spaceterm_exit_receipt_sha256 "
 "spaceterm_case_report_sha256 spaceterm_trace_metadata_sha256 "
 "spaceterm_trace_archive_sha256 spaceterm_manual_artifacts_sha256 "
 "spaceterm_manual_screenshot_sha256 spaceterm_manual_video_sha256 "
 "ghostty_session_id ghostty_nonce ghostty_run_intent_sha256 "
 "ghostty_run_metadata_sha256 ghostty_driver_intent_sha256 "
 "ghostty_driver_events_sha256 ghostty_driver_receipt_sha256 "
 "ghostty_window_identity_sha256 ghostty_driver_binary_sha256 "
 "ghostty_driver_source_sha256 ghostty_driver_controller_sha256 "
 "ghostty_plan_start_gate_sha256 ghostty_tail_receipt_sha256 "
 "ghostty_quit_receipt_sha256 ghostty_exit_receipt_sha256 "
 "ghostty_case_report_sha256 ghostty_trace_metadata_sha256 "
 "ghostty_trace_archive_sha256 ghostty_manual_artifacts_sha256 "
 "ghostty_manual_screenshot_sha256 ghostty_manual_video_sha256 "
 "spaceterm_lifecycle_ready_receipt_sha256 "
 "spaceterm_lifecycle_registration_receipt_sha256 "
 "ghostty_lifecycle_ready_receipt_sha256 "
 "ghostty_lifecycle_registration_receipt_sha256 lifecycle_helper_sha256 "
 "terminator_source_sha256 terminator_binary_sha256 evidence_mode status "
 "auth_algorithm pair_result_hmac_sha256"
).split()
case_keys = (
 "format_version subject scenario session_id nonce run_intent_sha256 "
 "run_metadata_sha256 trace_metadata_sha256 trace_archive_sha256 "
 "manual_artifacts_sha256 manual_screenshot_sha256 manual_video_sha256 result reason"
).split()
def exact(path, keys):
 data = pathlib.Path(path).read_bytes()
 rows = data.splitlines(keepends=True)
 if not data.endswith(b"\n") or len(rows) != len(keys):
  raise SystemExit(1)
 values = {}
 for expected, row in zip(keys, rows):
  fields = row[:-1].split(b"\t", 1)
  if len(fields) != 2 or fields[0].decode("ascii") != expected or not fields[1]:
   raise SystemExit(1)
  values[expected] = fields[1].decode()
 return values, data, b"".join(rows[:-1])
def digest(path):
 return hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest()
def trace_tree(path):
 root = None
 entries = {}
 with zipfile.ZipFile(path) as archive:
  for info in archive.infolist():
   name = info.filename.rstrip("/")
   parts = pathlib.PurePosixPath(name).parts
   if not parts or any(part in ("", ".", "..") for part in parts):
    raise SystemExit(1)
   root = parts[0] if root is None else root
   if parts[0] != root or not root.endswith(".trace"):
    raise SystemExit(1)
   mode = (info.external_attr >> 16) & 0xFFFF
   if stat.S_IFMT(mode) == stat.S_IFLNK:
    raise SystemExit(1)
   if info.is_dir():
    continue
   relative = pathlib.PurePosixPath(*parts[1:]).as_posix()
   if not relative or unicodedata.normalize("NFC", relative) != relative or relative in entries:
    raise SystemExit(1)
   entries[relative] = archive.read(info)
 if not entries:
  raise SystemExit(1)
 value = hashlib.sha256(b"spaceterm.performance.trace-tree/v1\0")
 for relative, data in sorted(entries.items()):
  encoded = relative.encode()
  value.update(struct.pack(">Q", len(encoded))); value.update(encoded)
  value.update(struct.pack(">Q", len(data))); value.update(data)
 return value.hexdigest()
values, pair_data, pair_unsigned = exact(pair_name, result_keys)
case, _, _ = exact(case_name, case_keys)
run = dict(line.split("\t", 1) for line in pathlib.Path(run_name).read_text().splitlines())
intent = dict(line.split("\t", 1) for line in pathlib.Path(intent_name).read_text().splitlines())
pair_metadata = dict(
 line.split("\t", 1) for line in pathlib.Path(pair_metadata_name).read_text().splitlines()
)
tree_hash = trace_tree(archive_name)
secret = pathlib.Path(secret_name).read_bytes()
authenticated = (b"spaceterm.performance.pair-result/v3\0"
 + struct.pack(">Q", len(pair_unsigned)) + pair_unsigned)
expected_hmac = hmac.new(secret, authenticated, hashlib.sha256).hexdigest()
prefix = subject
expected = {
 "format_version": "3", "campaign_id": intent["campaign_id"],
 "pair_metadata_sha256": digest(pair_metadata_name),
 "scenario_plan_sha256": pair_metadata["plan_sha256"],
 "workload_sha256": pair_metadata["workload_sha256"],
 "command_sha256": pair_metadata["command_sha256"],
 "environment_sha256": pair_metadata["environment_sha256"],
 "font_sha256": pair_metadata["font_sha256"],
 "initial_grid_sha256": pair_metadata["initial_grid_sha256"],
 f"{prefix}_session_id": intent["session_id"], f"{prefix}_nonce": intent["nonce"],
 f"{prefix}_run_intent_sha256": run["run_intent_sha256"],
 f"{prefix}_run_metadata_sha256": digest(run_name),
 f"{prefix}_case_report_sha256": digest(case_name),
 f"{prefix}_trace_metadata_sha256": digest(trace_name),
 f"{prefix}_trace_archive_sha256": tree_hash,
 f"{prefix}_manual_artifacts_sha256": digest(manual_name),
 f"{prefix}_manual_screenshot_sha256": digest(screenshot_name),
 f"{prefix}_manual_video_sha256": digest(video_name),
 f"{prefix}_lifecycle_ready_receipt_sha256": run["lifecycle_ready_receipt_sha256"],
 f"{prefix}_lifecycle_registration_receipt_sha256":
     run["lifecycle_registration_receipt_sha256"],
 "lifecycle_helper_sha256": run["lifecycle_helper_sha256"],
 "terminator_source_sha256": run["terminator_source_sha256"],
 "terminator_binary_sha256": run["terminator_binary_sha256"],
 "evidence_mode": "production", "status": "complete", "auth_algorithm": "hmac-sha256",
}
if (not hmac.compare_digest(values["pair_result_hmac_sha256"], expected_hmac)
 or any(values[key] != value for key, value in expected.items())
 or case != {
  "format_version": "2", "subject": subject, "scenario": scenario,
  "session_id": intent["session_id"], "nonce": intent["nonce"],
  "run_intent_sha256": run["run_intent_sha256"],
  "run_metadata_sha256": digest(run_name),
  "trace_metadata_sha256": digest(trace_name), "trace_archive_sha256": tree_hash,
  "manual_artifacts_sha256": digest(manual_name),
  "manual_screenshot_sha256": digest(screenshot_name),
  "manual_video_sha256": digest(video_name), "result": "CASE-COMPLETE",
  "reason": "all-required-evidence-complete",
 }):
 raise SystemExit(1)
PY
if [[ "$TEST_OVERRIDES_ACTIVE" == false ]]; then
    "$PYTHON_COMMAND" "$DRIVER_RECEIPT_VERIFIER" verify \
        --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
        --campaign-id "$(required_kv "$RENDER_INTENT" campaign_id intent)" \
        --session-id "$(required_kv "$RENDER_INTENT" session_id intent)" \
        --nonce "$(required_kv "$RENDER_INTENT" nonce intent)" \
        --driver-output "$DRIVER_EVENTS" \
        --driver-binary "$DRIVER_BINARY" \
        --driver-source "$DRIVER_SOURCE" \
        --controller "$DRIVER_CONTROLLER" \
        --scenario-plan "$PLAN" \
        --plan-start-continuous-ns "$DRIVER_PLAN_START_CONTINUOUS_NS" \
        --subject-identity "$SUBJECT_IDENTITY" \
        --window-identity "$WINDOW_IDENTITY" \
        --intent "$DRIVER_INTENT" \
        --receipt "$DRIVER_RECEIPT" \
        >/dev/null 2>&1 \
        || not_run native-driver-receipt-authentication-or-binding-invalid
fi
for intent_pair_field in command environment font initial_grid; do
    [[ "$(required_kv "$RENDER_INTENT" "${intent_pair_field}_sha256" intent)" \
        == "$(required_kv "$PAIR_METADATA" "${intent_pair_field}_sha256" pair)" ]] \
        || not_run "render-intent-$intent_pair_field-hash-mismatch"
done
reject_unknown_kv "$RENDER_EVIDENCE" \
    "format_version canonicalization auth_domain intent_sha256 scenario subject campaign_id session_id nonce subject_identity_sha256 subject_process_pid subject_process_start_identity driver_events_path driver_events_device driver_events_inode driver_events_sha256 action_video_path action_video_device action_video_inode action_video_sha256 render_workload_metadata_sha256 required_action_count completed_action_count action_interval_ms started_continuous_ns ended_continuous_ns measured_span_ns result hmac_key_identifier_sha256 evidence_hmac_sha256" \
    render-profile-evidence
verify_render_hmac "$RENDER_EVIDENCE" SPACETERM_RENDER_PROFILE_EVIDENCE_V1 \
    evidence_hmac_sha256 || not_run render-profile-evidence-authentication-invalid
[[ "$(required_kv "$RENDER_EVIDENCE" format_version evidence)" == 1 \
    && "$(required_kv "$RENDER_EVIDENCE" canonicalization evidence)" \
        == utf8-lf-tab-kv-fixed-order-domain-nul-v1 \
    && "$(required_kv "$RENDER_EVIDENCE" auth_domain evidence)" \
        == SPACETERM_RENDER_PROFILE_EVIDENCE_V1 \
    && "$(required_kv "$RENDER_EVIDENCE" intent_sha256 evidence)" \
        == "$(sha256 "$RENDER_INTENT")" \
    && "$(required_kv "$RENDER_EVIDENCE" scenario evidence)" == "$SCENARIO" \
    && "$(required_kv "$RENDER_EVIDENCE" subject evidence)" == "$SUBJECT" \
    && "$(required_kv "$RENDER_EVIDENCE" campaign_id evidence)" \
        == "$(required_kv "$RENDER_INTENT" campaign_id intent)" \
    && "$(required_kv "$RENDER_EVIDENCE" session_id evidence)" \
        == "$(required_kv "$RENDER_INTENT" session_id intent)" \
    && "$(required_kv "$RENDER_EVIDENCE" nonce evidence)" \
        == "$(required_kv "$RENDER_INTENT" nonce intent)" \
    && "$(required_kv "$RENDER_EVIDENCE" subject_identity_sha256 evidence)" \
        == "$subject_hash" \
    && "$(required_kv "$RENDER_EVIDENCE" subject_process_pid evidence)" \
        == "$process_pid" \
    && "$(required_kv "$RENDER_EVIDENCE" driver_events_sha256 evidence)" \
        == "$(sha256 "$DRIVER_EVENTS")" \
    && "$(required_kv "$RENDER_EVIDENCE" action_video_sha256 evidence)" \
        == "$(sha256 "$ACTION_VIDEO")" \
    && "$(required_kv "$RENDER_EVIDENCE" render_workload_metadata_sha256 evidence)" \
        == "$(sha256 "$RENDER_WORKLOAD_METADATA")" \
    && "$(required_kv "$RENDER_EVIDENCE" required_action_count evidence)" \
        == "$expected_count" \
    && "$(required_kv "$RENDER_EVIDENCE" completed_action_count evidence)" \
        == "$expected_count" \
    && "$(required_kv "$RENDER_EVIDENCE" action_interval_ms evidence)" \
        == "$expected_interval" \
    && "$(required_kv "$RENDER_EVIDENCE" result evidence)" == verified ]] \
    || not_run render-profile-evidence-does-not-bind-capture
for evidence_hash_field in hmac_key_identifier_sha256 evidence_hmac_sha256; do
    require_hash "$(required_kv "$RENDER_EVIDENCE" "$evidence_hash_field" evidence)" \
        "render-evidence-$evidence_hash_field"
done
[[ "$(required_kv "$RENDER_EVIDENCE" hmac_key_identifier_sha256 evidence)" \
    == "$(required_kv "$RENDER_INTENT" hmac_key_identifier_sha256 intent)" ]] \
    || not_run render-profile-evidence-hmac-key-mismatch
if [[ "$SCENARIO" == perf-render-sustained-output ]]; then
    verify_sustained_workload_v3 \
        "$(required_kv "$RENDER_INTENT" campaign_id intent)" \
        "$(required_kv "$RENDER_INTENT" session_id intent)" \
        "$(required_kv "$RENDER_INTENT" nonce intent)" \
        "$subject_hash" "$process_pid" \
        "$(required_kv "$SUBJECT_IDENTITY" process_start_identity identity)" \
        "$(required_kv "$RUN_METADATA" workload_sha256 run)" \
        "$expected_duration" "$expected_warmup" \
        || not_run sustained-output-workload-v3-authentication-invalid
fi
[[ "$("$HEAD_COMMAND" -n 1 "$DRIVER_EVENTS")" == "$DRIVER_HEADER" ]] \
    || not_run invalid-render-driver-event-header
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
' "$PLAN" "$DRIVER_EVENTS" || not_run render-driver-plan-or-process-mismatch
workload_started="$(required_kv "$RENDER_WORKLOAD_METADATA" started_continuous_ns workload)"
workload_ended="$(required_kv "$RENDER_WORKLOAD_METADATA" ended_continuous_ns workload)"
require_uint "$workload_started" render-workload-started-continuous-ns
require_uint "$workload_ended" render-workload-ended-continuous-ns
(( workload_ended >= workload_started + expected_duration * 1000000 \
    && workload_ended <= workload_started + (expected_duration + 2000) * 1000000 )) \
    || not_run render-workload-duration-incomplete
evidence_started="$(required_kv "$RENDER_EVIDENCE" started_continuous_ns evidence)"
evidence_ended="$(required_kv "$RENDER_EVIDENCE" ended_continuous_ns evidence)"
evidence_span="$(required_kv "$RENDER_EVIDENCE" measured_span_ns evidence)"
require_uint "$evidence_started" render-evidence-started-continuous-ns
require_uint "$evidence_ended" render-evidence-ended-continuous-ns
require_uint "$evidence_span" render-evidence-measured-span-ns
(( evidence_started == workload_started && evidence_ended == workload_ended \
    && evidence_span == evidence_ended - evidence_started )) \
    || not_run render-evidence-and-workload-clocks-disagree
if [[ "$SCENARIO" == perf-render-sustained-output ]]; then
    common_workload_started="$(required_kv "$WORKLOAD_METADATA" started_continuous_ns common-workload)"
    common_workload_ended="$(required_kv "$WORKLOAD_METADATA" ended_continuous_ns common-workload)"
    require_uint "$common_workload_started" sustained-output-workload-started-continuous-ns
    require_uint "$common_workload_ended" sustained-output-workload-ended-continuous-ns
    start_delta=$((common_workload_started - workload_started))
    end_delta=$((common_workload_ended - workload_ended))
    (( start_delta >= 0 )) || start_delta=$((-start_delta))
    (( end_delta >= 0 )) || end_delta=$((-end_delta))
    (( start_delta <= 250000000 && end_delta <= 250000000 )) \
        || not_run sustained-output-workload-and-render-clocks-disagree
fi
[[ "$("$AWK_COMMAND" -F '\t' '$3 == "measured-start" { count += 1; value = $2 } \
        END { if (count == 1) print value }' "$DRIVER_EVENTS")" == "$workload_started" \
    && "$("$AWK_COMMAND" -F '\t' '$3 == "measured-end" { count += 1; value = $2 } \
        END { if (count == 1) print value }' "$DRIVER_EVENTS")" == "$workload_ended" ]] \
    || not_run render-workload-clock-does-not-match-driver
driver_action_count="$("$AWK_COMMAND" -F '\t' -v prefix="^${expected_prefix}-[0-9][0-9][0-9]$" \
    '$3 ~ prefix && $11 == "verified" { count += 1 } END { print count + 0 }' \
    "$DRIVER_EVENTS")"
(( driver_action_count == expected_count )) || not_run render-driver-action-count-incomplete

[[ "$("$HEAD_COMMAND" -n 1 "$TRACE_INDEX")" == "$TRACE_INDEX_HEADER" ]] \
    || not_run invalid-campaign-trace-index-header
"$AWK_COMMAND" -F '\t' -v scenarios="$RENDER_SCENARIOS" '
    BEGIN {
        scenario_count = split(scenarios, ordered, " ")
    }
    NR == 1 { next }
    {
        row = NR - 1
        scenario_index = int((row - 1) / 2) + 1
        expected_subject = row % 2 == 1 ? "spaceterm" : "ghostty"
        if (NF != 22 || scenario_index > scenario_count \
            || $1 != ordered[scenario_index] || $2 != expected_subject) bad = 1
        if ($5 !~ /^[A-Za-z0-9][A-Za-z0-9._-]*$/ \
            || $6 !~ /^[A-Za-z0-9][A-Za-z0-9._-]*$/ \
            || $7 !~ /^[0-9a-f]{64}$/) bad = 1
        for (field = 3; field <= 22; field += 1) {
            if (field >= 5 && field <= 7) continue
            if ($field !~ /^[0-9a-f]{64}$/) bad = 1
        }
        if (identity[$2] != "" && identity[$2] != $3) bad = 1
        identity[$2] = $3
        if (pair[$1] != "" && pair[$1] != $4) bad = 1
        pair[$1] = $4
        if (campaign == "") campaign = $5
        if (session == "") session = $6
        if ($5 != campaign || $6 != session || seen_nonce[$7]++) bad = 1
        export_set = $18 ":" $19 ":" $20
        if (seen_manifest[$8]++ \
            || seen_anchor[$9]++ || seen_driver_intent[$10]++ \
            || seen_driver_receipt[$11]++ || seen_receipt[$12]++ \
            || seen_trace[$14]++ || seen_export_set[export_set]++ \
            || seen_screenshot[$21]++ || seen_video[$22]++) reused = 1
    }
    END { exit bad || reused || NR != scenario_count * 2 + 1 }
' "$TRACE_INDEX" || not_run campaign-trace-index-incomplete-mismatched-or-reused

trace_index_row="$("$AWK_COMMAND" -F '\t' -v scenario="$SCENARIO" -v subject="$SUBJECT" '
    $1 == scenario && $2 == subject { count += 1; row = $0 }
    END { if (count == 1) print row }
' "$TRACE_INDEX")"
[[ -n "$trace_index_row" ]] || not_run current-trace-index-row-missing
IFS=$'\t' read -r index_scenario index_subject index_identity index_pair \
    index_campaign_id index_session_id index_nonce \
    index_manifest index_anchor index_driver_intent index_driver_receipt index_receipt \
    index_metadata index_trace index_toc index_verification index_verifier \
    index_time index_allocations index_hangs index_screenshot index_video \
    <<< "$trace_index_row"
[[ "$index_scenario" == "$SCENARIO" && "$index_subject" == "$SUBJECT" \
    && "$index_identity" == "$subject_hash" && "$index_pair" == "$pair_hash" \
    && "$index_campaign_id" == "$(required_kv "$RENDER_INTENT" campaign_id intent)" \
    && "$index_session_id" == "$(required_kv "$RENDER_INTENT" session_id intent)" \
    && "$index_nonce" == "$(required_kv "$RENDER_INTENT" nonce intent)" \
    && "$index_manifest" == "$(sha256 "$CAMPAIGN_MANIFEST")" \
    && "$index_anchor" == "$(sha256 "$TRACE_ANCHOR_RECEIPT")" \
    && "$index_driver_intent" == "$(sha256 "$DRIVER_INTENT")" \
    && "$index_driver_receipt" == "$(sha256 "$DRIVER_RECEIPT")" \
    && "$index_receipt" == "$(sha256 "$TRACE_RECEIPT")" \
    && "$index_metadata" == "$(sha256 "$TRACE_METADATA")" \
    && "$index_trace" == "$(sha256 "$TRACE_ARTIFACT")" \
    && "$index_toc" == "$(sha256 "$TRACE_TOC")" \
    && "$index_verification" == "$(sha256 "$TRACE_VERIFICATION")" \
    && "$index_verifier" == "$(sha256 "$TRACE_VERIFIER")" \
    && "$index_time" == "$(sha256 "$TIME_PROFILER_ARTIFACT")" \
    && "$index_allocations" == "$(sha256 "$ALLOCATIONS_ARTIFACT")" \
    && "$index_hangs" == "$(sha256 "$HANGS_ARTIFACT")" \
    && "$index_screenshot" == "$(sha256 "$STACK_SCREENSHOT")" \
    && "$index_video" == "$(sha256 "$ACTION_VIDEO")" ]] \
    || not_run current-trace-artifacts-do-not-match-campaign-index
[[ "$("$AWK_COMMAND" -F '\t' -v scenario="$SCENARIO" -v subject="$comparison_subject" \
        '$1 == scenario && $2 == subject { print $3 }' "$TRACE_INDEX")" \
        == "$comparison_subject_hash" ]] \
    || not_run comparison-identity-does-not-match-campaign-index

reject_unknown_kv "$TRACE_METADATA" \
    "format_version capture_status incomplete_reason subject_identity_sha256 run_metadata_sha256 workload_metadata_sha256 workload_ready_receipt_sha256 supplemental_evidence_sha256 requested_duration_ms actual_duration_ms capture_started_continuous_ns capture_ended_continuous_ns target_identity_verified trace_target_pid_verified time_profiler_instrument allocations_instrument hangs_instrument time_profiler_target_verified allocations_target_verified hangs_target_verified time_profiler_rows allocations_rows hangs_rows maximum_main_thread_hang_ms status" \
    trace-metadata
expected_trace_workload_hash="$ZERO_SHA256"
expected_trace_ready_hash="$ZERO_SHA256"
if [[ "$SCENARIO" == perf-render-sustained-output ]]; then
    expected_trace_workload_hash="$(sha256 "$WORKLOAD_METADATA")"
    expected_trace_ready_hash="$(sha256 "$WORKLOAD_READY_RECEIPT")"
fi
[[ "$(required_kv "$TRACE_METADATA" format_version trace)" == 3 \
    && "$(required_kv "$TRACE_METADATA" capture_status trace)" == CAPTURED \
    && "$(required_kv "$TRACE_METADATA" incomplete_reason trace)" == none \
    && "$(required_kv "$TRACE_METADATA" status trace)" == complete \
    && "$(required_kv "$TRACE_METADATA" subject_identity_sha256 trace)" \
        == "$subject_hash" \
    && "$(required_kv "$TRACE_METADATA" run_metadata_sha256 trace)" \
        == "$(sha256 "$RUN_METADATA")" \
    && "$(required_kv "$TRACE_METADATA" workload_metadata_sha256 trace)" \
        == "$expected_trace_workload_hash" \
    && "$(required_kv "$TRACE_METADATA" workload_ready_receipt_sha256 trace)" \
        == "$expected_trace_ready_hash" \
    && "$(required_kv "$TRACE_METADATA" supplemental_evidence_sha256 trace)" \
        == "$(sha256 "$RENDER_EVIDENCE")" \
    && "$(required_kv "$TRACE_METADATA" target_identity_verified trace)" == true \
    && "$(required_kv "$TRACE_METADATA" trace_target_pid_verified trace)" == true ]] \
    || not_run render-trace-is-incomplete-or-mismatched
trace_requested="$(required_kv "$TRACE_METADATA" requested_duration_ms trace)"
trace_actual="$(required_kv "$TRACE_METADATA" actual_duration_ms trace)"
require_uint "$trace_requested" render-trace-requested-duration
require_uint "$trace_actual" render-trace-actual-duration
(( trace_requested == expected_duration && trace_actual >= expected_duration \
    && trace_actual <= expected_duration + 4000 )) \
    || not_run render-trace-duration-incomplete
capture_started="$(required_kv "$TRACE_METADATA" capture_started_continuous_ns trace)"
capture_ended="$(required_kv "$TRACE_METADATA" capture_ended_continuous_ns trace)"
require_uint "$capture_started" render-trace-capture-started-continuous-ns
require_uint "$capture_ended" render-trace-capture-ended-continuous-ns
(( capture_started <= workload_started \
    && workload_started - capture_started <= 2000000000 \
    && capture_ended >= workload_ended \
    && capture_ended - workload_ended <= 2000000000 \
    && capture_ended > capture_started )) \
    || not_run render-trace-does-not-bracket-workload
if [[ "$SCENARIO" == perf-render-sustained-output ]]; then
    (( capture_started <= common_workload_started \
        && common_workload_started - capture_started <= 2000000000 \
        && capture_ended >= common_workload_ended \
        && capture_ended - common_workload_ended <= 2000000000 )) \
        || not_run render-trace-does-not-bracket-sustained-output-workload
fi
capture_span_ms=$(((capture_ended - capture_started) / 1000000))
(( capture_span_ms >= trace_actual - 250 && capture_span_ms <= trace_actual + 250 )) \
    || not_run render-trace-duration-and-continuous-span-disagree

# shellcheck disable=SC2329 # Invoked by the EXIT/INT/TERM trap below.
cleanup_validation() {
    [[ -z "$VALIDATION_ROOT" ]] || "$RM_COMMAND" -rf -- "$VALIDATION_ROOT"
}
VALIDATION_ROOT="$("$MKTEMP_COMMAND" -d "${TMPDIR:-/tmp}/spaceterm-render-validation.XXXXXX")"
trap cleanup_validation EXIT INT TERM
process_name="$("$BASENAME_COMMAND" -- "$(required_kv "$SUBJECT_IDENTITY" executable_path identity)")"
command_elapsed_seconds="$("$AWK_COMMAND" -v milliseconds="$trace_actual" \
    'BEGIN { printf "%.6f", milliseconds / 1000 + 1 }')"
archive_verdict="$VALIDATION_ROOT/archive-verdict.tsv"
archive_exit=0
"$PYTHON_COMMAND" "$TRACE_ARCHIVE_VERIFIER" --archive "$TRACE_ARTIFACT" \
    --output-directory "$VALIDATION_ROOT/archive" --xcrun "$XCRUN_COMMAND" \
    --toc "$TRACE_TOC" --time-profile "$TIME_PROFILER_ARTIFACT" \
    --allocations "$ALLOCATIONS_ARTIFACT" --hangs "$HANGS_ARTIFACT" \
    --trace-verifier "$TRACE_VERIFIER" --python "$PYTHON_COMMAND" \
    --verification "$TRACE_VERIFICATION" \
    --pid "$process_pid" --process-name "$process_name" \
    --requested-seconds "$((expected_duration / 1000))" \
    --command-elapsed-seconds "$command_elapsed_seconds" \
    > "$archive_verdict" 2>/dev/null || archive_exit=$?
archive_result="$(kv "$archive_verdict" result 2>/dev/null || true)"
archive_reason="$(kv "$archive_verdict" reason 2>/dev/null || true)"
[[ "$archive_exit" == 0 && "$archive_result" == PASS \
    && "$archive_reason" == trace-archive-and-regenerated-exports-verified ]] \
    || not_run "${archive_reason:-render-trace-archive-validator-error}"
verification_metric() {
    local primary="$1"
    local fallback="${2:-}"
    local value
    value="$(kv "$TRACE_VERIFICATION" "$primary")"
    [[ -n "$value" || -z "$fallback" ]] || value="$(kv "$TRACE_VERIFICATION" "$fallback")"
    printf '%s' "$value"
}
verified_duration_seconds="$(verification_metric actual_record_duration_seconds)"
[[ "$verified_duration_seconds" =~ ^[0-9]+([.][0-9]+)?$ ]] \
    || not_run render-trace-verification-duration-missing
"$AWK_COMMAND" -v seconds="$verified_duration_seconds" -v milliseconds="$trace_actual" '
    BEGIN { delta = seconds * 1000 - milliseconds; if (delta < 0) delta = -delta; exit delta > 1 }
' || not_run render-trace-verification-duration-mismatch
for trace_count in time_profiler allocations hangs; do
    verified_count="$(verification_metric "${trace_count}_rows" \
        "${trace_count}_sample_count")"
    [[ -n "$verified_count" ]] || verified_count="$(verification_metric \
        "${trace_count}_rows" "${trace_count}_event_count")"
    require_uint "$verified_count" "verified-$trace_count-rows"
    [[ "$verified_count" == "$(required_kv "$TRACE_METADATA" \
        "${trace_count}_rows" trace)" ]] \
        || not_run "render-trace-$trace_count-row-count-mismatch"
done
verified_hang_ms="$(verification_metric maximum_main_thread_hang_ms)"
[[ "$verified_hang_ms" =~ ^[0-9]+([.][0-9]+)?$ ]] \
    || not_run render-trace-verification-hang-duration-missing
[[ "$verified_hang_ms" == "$(required_kv "$TRACE_METADATA" \
    maximum_main_thread_hang_ms trace)" ]] \
    || not_run render-trace-hang-duration-mismatch

screenshot_inspection="$($SIPS_COMMAND -g format -g pixelWidth -g pixelHeight \
    "$STACK_SCREENSHOT" 2>/dev/null || true)"
screenshot_format="$("$AWK_COMMAND" '/format:/ { print tolower($2) }' \
    <<< "$screenshot_inspection")"
screenshot_width="$("$AWK_COMMAND" '/pixelWidth:/ { print $2 }' \
    <<< "$screenshot_inspection")"
screenshot_height="$("$AWK_COMMAND" '/pixelHeight:/ { print $2 }' \
    <<< "$screenshot_inspection")"
[[ "$screenshot_format" == png && "$screenshot_width" =~ ^[1-9][0-9]*$ \
    && "$screenshot_height" =~ ^[1-9][0-9]*$ ]] \
    || not_run representative-stack-screenshot-is-not-valid-png
video_verdict="$VALIDATION_ROOT/video-verdict.tsv"
video_exit=0
"$PYTHON_COMMAND" "$ACTION_VIDEO_VERIFIER" --video "$ACTION_VIDEO" \
    --ffprobe "$FFPROBE_COMMAND" \
    --minimum-duration-ms "$expected_duration" \
    --maximum-duration-ms "$((expected_duration + 60000))" \
    > "$video_verdict" 2>/dev/null || video_exit=$?
video_result="$(kv "$video_verdict" result 2>/dev/null || true)"
video_reason="$(kv "$video_verdict" reason 2>/dev/null || true)"
[[ "$video_exit" == 0 && "$video_result" == PASS \
    && "$video_reason" == render-action-video-stream-and-duration-verified ]] \
    || not_run "${video_reason:-render-action-video-validator-error}"
for instrument in time_profiler allocations hangs; do
    [[ "$(required_kv "$TRACE_METADATA" "${instrument}_instrument" trace)" == true \
        && "$(required_kv "$TRACE_METADATA" \
            "${instrument}_target_verified" trace)" == true ]] \
        || not_run "render-trace-$instrument-not-proven"
    rows="$(required_kv "$TRACE_METADATA" "${instrument}_rows" trace)"
    require_uint "$rows" "render-trace-$instrument-rows"
    if [[ "$instrument" == time_profiler ]]; then
        (( rows >= 2 )) || not_run render-time-profiler-samples-insufficient
    elif [[ "$instrument" == allocations ]]; then
        (( rows >= 1 )) || not_run render-allocation-events-missing
    fi
done
hang_ms="$(required_kv "$TRACE_METADATA" maximum_main_thread_hang_ms trace)"
[[ "$hang_ms" =~ ^[0-9]+([.][0-9]+)?$ ]] \
    || not_run invalid-render-main-thread-hang-duration
if "$AWK_COMMAND" -v hang="$hang_ms" 'BEGIN { exit !(hang + 0 > 250) }'; then
    fail render-main-thread-hang-exceeds-250ms
fi

reject_unknown_kv "$MANUAL_REVIEW" \
    "format_version scenario subject plan_sha256 pair_metadata_sha256 run_metadata_sha256 render_intent_sha256 render_workload_metadata_sha256 render_evidence_sha256 campaign_manifest_sha256 trace_anchor_receipt_sha256 driver_intent_sha256 driver_receipt_sha256 trace_receipt_sha256 trace_index_sha256 trace_metadata_sha256 trace_artifact_sha256 trace_toc_sha256 trace_verification_sha256 trace_verifier_sha256 time_profiler_artifact_sha256 allocations_artifact_sha256 hangs_artifact_sha256 representative_stack_screenshot_sha256 action_video_sha256 instruments_version sampling_settings render_root_symbol render_root_sample_count call_tree_filters action_video_review time_profiler_call_tree_checked allocations_call_tree_checked hangs_timeline_checked render_root_text_shaping_stack_present render_root_path_construction_stack_present render_root_symbol_plan_construction_stack_present render_root_row_plan_construction_stack_present render_root_image_placement_geometry_stack_present render_root_normal_frame_allocation_stack_present unchanged_row_reshaping_present changed_row_proportionality_result overlay_change_proportionality_result completed_action_count exceptional_error_allocations_excluded reviewer result" \
    manual-render-review
[[ "$(required_kv "$MANUAL_REVIEW" format_version manual)" == 1 \
    && "$(required_kv "$MANUAL_REVIEW" scenario manual)" == "$SCENARIO" \
    && "$(required_kv "$MANUAL_REVIEW" subject manual)" == "$SUBJECT" \
    && "$(required_kv "$MANUAL_REVIEW" plan_sha256 manual)" == "$plan_hash" \
    && "$(required_kv "$MANUAL_REVIEW" pair_metadata_sha256 manual)" == "$pair_hash" \
    && "$(required_kv "$MANUAL_REVIEW" run_metadata_sha256 manual)" \
        == "$(sha256 "$RUN_METADATA")" \
    && "$(required_kv "$MANUAL_REVIEW" render_intent_sha256 manual)" \
        == "$(sha256 "$RENDER_INTENT")" \
    && "$(required_kv "$MANUAL_REVIEW" render_workload_metadata_sha256 manual)" \
        == "$(sha256 "$RENDER_WORKLOAD_METADATA")" \
    && "$(required_kv "$MANUAL_REVIEW" render_evidence_sha256 manual)" \
        == "$(sha256 "$RENDER_EVIDENCE")" \
    && "$(required_kv "$MANUAL_REVIEW" campaign_manifest_sha256 manual)" \
        == "$(sha256 "$CAMPAIGN_MANIFEST")" \
    && "$(required_kv "$MANUAL_REVIEW" trace_anchor_receipt_sha256 manual)" \
        == "$(sha256 "$TRACE_ANCHOR_RECEIPT")" \
    && "$(required_kv "$MANUAL_REVIEW" driver_intent_sha256 manual)" \
        == "$(sha256 "$DRIVER_INTENT")" \
    && "$(required_kv "$MANUAL_REVIEW" driver_receipt_sha256 manual)" \
        == "$(sha256 "$DRIVER_RECEIPT")" \
    && "$(required_kv "$MANUAL_REVIEW" trace_receipt_sha256 manual)" \
        == "$(sha256 "$TRACE_RECEIPT")" \
    && "$(required_kv "$MANUAL_REVIEW" trace_index_sha256 manual)" \
        == "$(sha256 "$TRACE_INDEX")" \
    && "$(required_kv "$MANUAL_REVIEW" trace_metadata_sha256 manual)" \
        == "$(sha256 "$TRACE_METADATA")" \
    && "$(required_kv "$MANUAL_REVIEW" trace_artifact_sha256 manual)" \
        == "$(sha256 "$TRACE_ARTIFACT")" \
    && "$(required_kv "$MANUAL_REVIEW" trace_toc_sha256 manual)" \
        == "$(sha256 "$TRACE_TOC")" \
    && "$(required_kv "$MANUAL_REVIEW" trace_verification_sha256 manual)" \
        == "$(sha256 "$TRACE_VERIFICATION")" \
    && "$(required_kv "$MANUAL_REVIEW" trace_verifier_sha256 manual)" \
        == "$(sha256 "$TRACE_VERIFIER")" \
    && "$(required_kv "$MANUAL_REVIEW" time_profiler_artifact_sha256 manual)" \
        == "$(sha256 "$TIME_PROFILER_ARTIFACT")" \
    && "$(required_kv "$MANUAL_REVIEW" allocations_artifact_sha256 manual)" \
        == "$(sha256 "$ALLOCATIONS_ARTIFACT")" \
    && "$(required_kv "$MANUAL_REVIEW" hangs_artifact_sha256 manual)" \
        == "$(sha256 "$HANGS_ARTIFACT")" \
    && "$(required_kv "$MANUAL_REVIEW" \
        representative_stack_screenshot_sha256 manual)" \
        == "$(sha256 "$STACK_SCREENSHOT")" \
    && "$(required_kv "$MANUAL_REVIEW" action_video_sha256 manual)" \
        == "$(sha256 "$ACTION_VIDEO")" ]] \
    || not_run manual-render-review-artifact-binding-mismatch

for descriptive_field in instruments_version sampling_settings render_root_symbol call_tree_filters \
    exceptional_error_allocations_excluded reviewer; do
    description="$(required_kv "$MANUAL_REVIEW" "$descriptive_field" manual)"
    [[ "$description" != unchecked && "$description" != none ]] \
        || not_run "manual-$descriptive_field-is-not-reviewed"
done
render_root="$(required_kv "$MANUAL_REVIEW" render_root_symbol manual)"
if [[ "$SUBJECT" == spaceterm ]]; then
    [[ "$render_root" == TerminalGridElement::paint ]] \
        || not_run spaceterm-render-root-is-not-terminal-grid-paint
fi
call_tree_filters="$(required_kv "$MANUAL_REVIEW" call_tree_filters manual)"
[[ "$call_tree_filters" == *"$render_root"* ]] \
    || not_run manual-call-tree-filter-does-not-name-render-root
render_root_samples="$(required_kv "$MANUAL_REVIEW" render_root_sample_count manual)"
require_uint "$render_root_samples" render-root-sample-count
(( render_root_samples > 0 )) || not_run render-root-has-no-positive-sample-evidence
for review_gate in action_video_review time_profiler_call_tree_checked \
    allocations_call_tree_checked hangs_timeline_checked; do
    [[ "$(required_kv "$MANUAL_REVIEW" "$review_gate" manual)" == PASS ]] \
        || not_run "manual-$review_gate-missing"
done
completed_actions="$(required_kv "$MANUAL_REVIEW" completed_action_count manual)"
require_uint "$completed_actions" completed-render-profile-actions
(( completed_actions == expected_count )) || not_run render-profile-actions-incomplete

manual_result="$(required_kv "$MANUAL_REVIEW" result manual)"
[[ "$manual_result" != FAIL ]] || fail manual-render-review-failed
[[ "$manual_result" == PASS ]] || not_run manual-render-review-not-approved
for forbidden_field in render_root_text_shaping_stack_present \
    render_root_path_construction_stack_present \
    render_root_symbol_plan_construction_stack_present \
    render_root_row_plan_construction_stack_present \
    render_root_image_placement_geometry_stack_present \
    render_root_normal_frame_allocation_stack_present \
    unchanged_row_reshaping_present; do
    forbidden_value="$(required_kv "$MANUAL_REVIEW" "$forbidden_field" manual)"
    [[ "$forbidden_value" != true ]] || fail "$forbidden_field"
    [[ "$forbidden_value" == false ]] || not_run "invalid-$forbidden_field"
done
for proportionality_field in changed_row_proportionality_result \
    overlay_change_proportionality_result; do
    proportionality="$(required_kv "$MANUAL_REVIEW" "$proportionality_field" manual)"
    [[ "$proportionality" != FAIL ]] || fail "$proportionality_field"
    [[ "$proportionality" == PASS ]] || not_run "manual-$proportionality_field-missing"
done

if [[ -n "$EVIDENCE_SNAPSHOT_TEST_HOOK" ]]; then
    [[ "$EVIDENCE_SNAPSHOT_TEST_HOOK" == /* \
        && -f "$EVIDENCE_SNAPSHOT_TEST_HOOK" \
        && ! -L "$EVIDENCE_SNAPSHOT_TEST_HOOK" \
        && -x "$EVIDENCE_SNAPSHOT_TEST_HOOK" ]] \
        || not_run render-evidence-snapshot-test-hook-invalid
    "$EVIDENCE_SNAPSHOT_TEST_HOOK" "$MANUAL_REVIEW" \
        || not_run render-evidence-snapshot-test-hook-failed
fi
if [[ "$TEST_OVERRIDES_ACTIVE" == false ]]; then
    [[ "$(render_tool_identity_snapshot)" == "$RENDER_TOOL_IDENTITY_SNAPSHOT" ]] \
        || not_run render-analysis-tool-identity-changed-during-validation
fi
[[ "$(evidence_identity_snapshot "${EVIDENCE_SNAPSHOT_ARGUMENTS[@]}")" \
    == "$EVIDENCE_IDENTITY_SNAPSHOT" ]] \
    || not_run render-evidence-identity-changed-during-validation
[[ "$TEST_OVERRIDES_ACTIVE" != true ]] || not_run render-profile-test-overrides-active

verdict PASS render-profile-trace-and-manual-review-passed
