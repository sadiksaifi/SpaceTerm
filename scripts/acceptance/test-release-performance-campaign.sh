#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
umask 077

SCRIPT_DIRECTORY="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIRECTORY
TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-performance-campaign.XXXXXX")"
readonly TEMP_ROOT
readonly HASH_A="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
readonly HASH_B="bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
readonly HASH_C="cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
readonly BASE_CONTINUOUS_NS=1000000000000
readonly CAMPAIGN_ID="campaign-43"
readonly SESSION_ID="session-43"
readonly NONCE="dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
CAMPAIGN_SECRET_FILE="$TEMP_ROOT/campaign-secret"
printf '0123456789abcdef0123456789abcdef\n' > "$CAMPAIGN_SECRET_FILE"
chmod 0600 "$CAMPAIGN_SECRET_FILE"
readonly CAMPAIGN_SECRET_FILE

cleanup() {
    chmod -R u+w "$TEMP_ROOT" 2>/dev/null || true
    rm -rf -- "$TEMP_ROOT"
}
trap cleanup EXIT INT TERM

fail() {
    echo "test failure: $*" >&2
    exit 1
}

expect_result() {
    local expected_exit="$1"
    local expected_result="$2"
    local label="$3"
    shift 3
    local output="$TEMP_ROOT/result.tsv"
    local actual_exit=0
    "$@" > "$output" 2>/dev/null || actual_exit=$?
    if [[ "$actual_exit" != "$expected_exit" ]]; then
        sed 's/^/  /' "$output" >&2
        fail "$label exit: expected $expected_exit, observed $actual_exit"
    fi
    if ! grep -Fxq $'result\t'"$expected_result" "$output"; then
        sed 's/^/  /' "$output" >&2
        fail "$label result: expected $expected_result"
    fi
}

expect_command_failure() {
    local label="$1"
    shift
    if "$@" >/dev/null 2>&1; then
        fail "$label unexpectedly succeeded"
    fi
}

sha256() {
    shasum -a 256 "$1" | awk '{ print $1 }'
}

publish_plan_start_gate() {
    local events="$1"
    local gate="$2"
    local secret="${3:-$CAMPAIGN_SECRET_FILE}"
    local ready_receipt="${4:?ready receipt path is required}"
    local start_override="${5:-}"
    for _ in {1..12000}; do
        if [[ -f "$events" && -f "$ready_receipt" ]] \
            && awk -F '\t' '$3 == "measurement-ready" { found = 1 } END { exit !found }' \
                "$events" 2>/dev/null; then
            break
        fi
        sleep 0.01
    done
    [[ -f "$events" ]] || return 1
    local start
    if [[ -n "$start_override" ]]; then
        start="$start_override"
    else
        start="$(python3 - <<'PY'
import ctypes
import ctypes.util

mach = ctypes.CDLL(ctypes.util.find_library("System"))
class Timebase(ctypes.Structure):
    _fields_ = [("numer", ctypes.c_uint32), ("denom", ctypes.c_uint32)]
mach.mach_continuous_time.restype = ctypes.c_uint64
info = Timebase()
mach.mach_timebase_info(ctypes.byref(info))
print(mach.mach_continuous_time() * info.numer // info.denom + 3_000_000_000)
PY
)"
    fi
    python3 - "$gate" "$secret" "$start" "$CAMPAIGN_ID" "$SESSION_ID" "$NONCE" \
        "$ready_receipt" <<'PY'
import hashlib
import hmac
import pathlib
import struct
import sys

gate, secret_path, start, campaign, session, nonce, ready_receipt = sys.argv[1:]
ready_hash = hashlib.sha256(pathlib.Path(ready_receipt).read_bytes()).hexdigest()
unsigned = (
    "format_version\t1\n"
    f"campaign_id\t{campaign}\n"
    f"session_id\t{session}\n"
    f"nonce\t{nonce}\n"
    f"ready_receipt_sha256\t{ready_hash}\n"
    f"plan_start_continuous_ns\t{start}\n"
).encode()
payload = (
    b"spaceterm.performance.plan-start-gate/v1\0"
    + struct.pack(">Q", len(unsigned))
    + unsigned
)
signature = hmac.new(pathlib.Path(secret_path).read_bytes(), payload, hashlib.sha256).hexdigest()
temporary = pathlib.Path(gate + ".tmp")
temporary.write_bytes(unsigned + f"start_gate_hmac_sha256\t{signature}\n".encode())
temporary.chmod(0o400)
temporary.rename(gate)
PY
}

write_ready_receipt() {
    local events="$1" identity="$2" output="$3"
    python3 - "$events" "$identity" "$CAMPAIGN_SECRET_FILE" "$output" \
        "$CAMPAIGN_ID" "$SESSION_ID" "$NONCE" <<'PY'
import hashlib, hmac, os, pathlib, struct, sys

events_path, identity_path, secret_path, output, campaign, session, nonce = sys.argv[1:]
events = pathlib.Path(events_path).read_bytes()
lines = events.splitlines(keepends=True)
ready_index = next(i for i, line in enumerate(lines) if b"\tmeasurement-ready\t" in line)
prefix = b"".join(lines[:ready_index + 1])
fields = lines[ready_index].rstrip(b"\n").split(b"\t")
metadata = os.stat(events_path)
unsigned = (
    "format_version\t1\n"
    f"campaign_id\t{campaign}\n"
    f"session_id\t{session}\n"
    f"nonce\t{nonce}\n"
    f"subject_identity_sha256\t{hashlib.sha256(pathlib.Path(identity_path).read_bytes()).hexdigest()}\n"
    "producer_pid\t456\n"
    f"producer_started_continuous_ns\t{1_000_000_000_000 - 1_000_000}\n"
    "producer_session_id\t400\n"
    "producer_process_group\t456\n"
    "tty_device\t7\n"
    "tty_inode\t8\n"
    "tty_rdev\t9\n"
    f"events_device\t{metadata.st_dev}\n"
    f"events_inode\t{metadata.st_ino}\n"
    f"events_prefix_bytes\t{len(prefix)}\n"
    f"events_prefix_sha256\t{hashlib.sha256(prefix).hexdigest()}\n"
    f"measurement_ready_continuous_ns\t{fields[1].decode()}\n"
    f"measurement_ready_byte_count\t{fields[4].decode()}\n"
    "auth_algorithm\thmac-sha256\n"
).encode()
payload = b"spaceterm.performance.workload-ready/v1\0" + struct.pack(">Q", len(unsigned)) + unsigned
signature = hmac.new(pathlib.Path(secret_path).read_bytes(), payload, hashlib.sha256).hexdigest()
pathlib.Path(output).write_bytes(unsigned + f"ready_hmac_sha256\t{signature}\n".encode())
pathlib.Path(output).chmod(0o400)
PY
}

write_subject_identity() {
    local subject="$1"
    local path="$2"
    {
        printf 'format_version\t1\n'
        printf 'subject\t%s\n' "$subject"
        printf 'app_bundle_path\t/Applications/%s.app\n' "$subject"
        printf 'bundle_identifier\tcom.example.%s\n' "$subject"
        printf 'bundle_version\t1.0+1\n'
        printf 'executable_path\t/Applications/%s.app/Contents/MacOS/%s\n' \
            "$subject" "$subject"
        printf 'executable_sha256\t%s\n' "$HASH_A"
        printf 'executable_device\t1\n'
        printf 'executable_inode\t2\n'
        printf 'executable_fsid\t1\n'
        printf 'signature_valid\ttrue\n'
        printf 'signing_identifier\tcom.example.%s\n' "$subject"
        printf 'team_identifier\tnone\n'
        printf 'cdhash\tabcd1234\n'
        printf 'process_pid\t123\n'
        printf 'process_start_identity\t1786473000:123456\n'
        printf 'identity_status\tfrozen\n'
    } > "$path"
    chmod 0444 "$path"
}

write_driver_events() {
    local plan="$1"
    local output="$2"
    awk -F '\t' -v base="$BASE_CONTINUOUS_NS" 'BEGIN { OFS = "\t" }
        NR == 1 {
            print "sequence", "continuous_ns", "event_id", "action", \
                "target_pid", "window_number", "requested_a", "requested_b", \
                "observed_a", "observed_b", "result"
            next
        }
        {
            sequence = NR - 2
            print sequence, base + 1000000 + $2 * 1000000 + sequence, $1, $3, \
                123, 44, $4, $5, 1, 1, "verified"
        }
    ' "$plan" > "$output"
}

write_workload_events() {
    local driver="$1"
    local output="$2"
    local body="$TEMP_ROOT/workload-events-body.$$"
    {
        printf '%d\tstarted\tnone\t0\t40\t100\t1000\t800\tok\n' \
            "$BASE_CONTINUOUS_NS"
        printf '%d\tgeometry\tnone\t0\t40\t100\t1000\t800\tok\n' \
            "$((BASE_CONTINUOUS_NS + 1))"
        printf '%d\tseed-complete\tnone\t1024\t40\t100\t1000\t800\tok\n' \
            "$((BASE_CONTINUOUS_NS + 2))"
        printf '%d\tmeasurement-ready\tnone\t1024\t40\t100\t1000\t800\tok\n' \
            "$((BASE_CONTINUOUS_NS + 3))"
        for ((index = 0; index <= 600; index += 1)); do
            printf '%d\tprogress\tprogress-%06d\t%d\t40\t100\t1000\t800\tok\n' \
                "$((BASE_CONTINUOUS_NS + 60000000000 + index * 1000000000))" \
                "$index" "$(((index + 1) * 1000000))"
        done
        awk -F '\t' 'NR > 1 && $4 == "input" {
            printf "%d\tinput-read\t%s\t0\t40\t100\t1000\t800\tok\n", \
                $2 + 50000000, $3
            printf "%d\tinput-ack-written\t%s\t64\t40\t100\t1000\t800\tok\n", \
                $2 + 75000000, $3
        }' "$driver"
        printf '%d\tproducer-end\tnone\t601000000\t40\t100\t1000\t800\tsuccess\n' \
            "$((BASE_CONTINUOUS_NS + 660050000000))"
    } > "$body"
    {
        printf 'sequence\tcontinuous_ns\tkind\tevent_id\tbyte_count\trows\tcolumns\tpixel_width\tpixel_height\tstatus\n'
        sort -t $'\t' -k1,1n -k2,2 "$body" \
            | awk -F '\t' 'BEGIN { OFS = "\t" } { print NR - 1, $0 }'
    } > "$output"
    rm -f -- "$body"
    chmod 0444 "$output"
}

write_workload_metadata() {
    local workload_binary="$1"
    local events="$2"
    local identity="$3"
    local output="$4"
    local ready_receipt="${5:-$READY_RECEIPT}"
    local unsigned="$TEMP_ROOT/workload-metadata-unsigned.$$"
    {
        printf 'format_version\t3\n'
        printf 'scenario\tascii\n'
        printf 'campaign_id\t%s\n' "$CAMPAIGN_ID"
        printf 'session_id\t%s\n' "$SESSION_ID"
        printf 'nonce\t%s\n' "$NONCE"
        printf 'subject_identity_sha256\t%s\n' "$(sha256 "$identity")"
        printf 'subject_process_pid\t123\n'
        printf 'subject_process_start_identity\t1786473000:123456\n'
        printf 'producer_sha256\t%s\n' "$(sha256 "$workload_binary")"
        printf 'producer_pid\t456\n'
        printf 'producer_started_continuous_ns\t%d\n' \
            "$((BASE_CONTINUOUS_NS - 1000000))"
        printf 'producer_session_id\t400\n'
        printf 'producer_process_group\t456\n'
        printf 'tty_device\t7\n'
        printf 'tty_inode\t8\n'
        printf 'tty_rdev\t9\n'
        printf 'ready_receipt_sha256\t%s\n' "$(sha256 "$ready_receipt")"
        printf 'events_sha256\t%s\n' "$(sha256 "$events")"
        printf 'auth_algorithm\thmac-sha256\n'
        printf 'seed_sha256\t%s\n' "$HASH_B"
        printf 'seed_bytes\t1024\n'
        printf 'requested_duration_ms\t600000\n'
        printf 'warmup_ms\t60000\n'
        printf 'requested_iterations\t0\n'
        printf 'requested_seed_rows\t0\n'
        printf 'emitted_bytes\t601000000\n'
        printf 'input_events\t20\n'
        printf 'plan_start_continuous_ns\t%d\n' "$BASE_CONTINUOUS_NS"
        printf 'started_continuous_ns\t%d\n' "$((BASE_CONTINUOUS_NS + 60000000000))"
        printf 'ended_continuous_ns\t%d\n' "$((BASE_CONTINUOUS_NS + 660050000000))"
        printf 'status\tcomplete\n'
    } > "$unsigned"
    python3 - "$unsigned" "$events" "$CAMPAIGN_SECRET_FILE" "$output" <<'PY'
import hashlib
import hmac
import pathlib
import struct
import sys

metadata = pathlib.Path(sys.argv[1]).read_bytes()
events = pathlib.Path(sys.argv[2]).read_bytes()
secret = pathlib.Path(sys.argv[3]).read_bytes()
payload = (
    b"spaceterm.performance.workload-auth/v1\0"
    + struct.pack(">Q", len(metadata))
    + metadata
    + struct.pack(">Q", len(events))
    + events
)
signature = hmac.new(secret, payload, hashlib.sha256).hexdigest()
pathlib.Path(sys.argv[4]).write_bytes(
    metadata + f"events_hmac_sha256\t{signature}\n".encode()
)
PY
    rm -f -- "$unsigned"
    chmod 0444 "$output"
}

resign_workload_metadata() {
    local source="$1"
    local events="$2"
    local output="$3"
    local ready_receipt="${4:-$READY_RECEIPT}"
    python3 - "$source" "$events" "$CAMPAIGN_SECRET_FILE" "$output" \
        "$ready_receipt" <<'PY'
import hashlib
import hmac
import pathlib
import struct
import sys

lines = pathlib.Path(sys.argv[1]).read_bytes().splitlines(keepends=True)
events = pathlib.Path(sys.argv[2]).read_bytes()
if not lines[-1].startswith(b"events_hmac_sha256\t"):
    raise SystemExit("missing signature row")
unsigned_lines = lines[:-1]
for index, line in enumerate(unsigned_lines):
    if line.startswith(b"events_sha256\t"):
        unsigned_lines[index] = b"events_sha256\t" + hashlib.sha256(events).hexdigest().encode() + b"\n"
    elif line.startswith(b"ready_receipt_sha256\t"):
        receipt = pathlib.Path(sys.argv[5]).read_bytes()
        unsigned_lines[index] = b"ready_receipt_sha256\t" + hashlib.sha256(receipt).hexdigest().encode() + b"\n"
unsigned = b"".join(unsigned_lines)
payload = (
    b"spaceterm.performance.workload-auth/v1\0"
    + struct.pack(">Q", len(unsigned))
    + unsigned
    + struct.pack(">Q", len(events))
    + events
)
secret = pathlib.Path(sys.argv[3]).read_bytes()
signature = hmac.new(secret, payload, hashlib.sha256).hexdigest()
pathlib.Path(sys.argv[4]).write_bytes(
    unsigned + b"events_hmac_sha256\t" + signature.encode() + b"\n"
)
PY
    chmod 0444 "$output"
}

normalize_workload_events() {
    local source="$1"
    local output="$2"
    awk -F '\t' 'BEGIN { OFS = "\t" }
        NR == 1 { print; next }
        {
            $1 = sequence++
            if ($3 == "progress") {
                $4 = sprintf("progress-%06d", progress++)
            }
            print
        }
    ' "$source" > "$output"
    chmod 0444 "$output"
}

ASSEMBLY_FAILURE_INDEX=0
expect_progress_assembly_failure() {
    local label="$1"
    local events="$2"
    local metadata="$3"
    local raw="$4"
    local identity="${5:-$GHOSTTY_IDENTITY}"
    local secret="${6:-$CAMPAIGN_SECRET_FILE}"
    local campaign="${7:-$CAMPAIGN_ID}"
    local session="${8:-$SESSION_ID}"
    local nonce="${9:-$NONCE}"
    ASSEMBLY_FAILURE_INDEX=$((ASSEMBLY_FAILURE_INDEX + 1))
    expect_command_failure "$label" \
        "$SCRIPT_DIRECTORY/assemble-release-performance-rss-v3.sh" \
            --subject-identity "$identity" \
            --scenario ascii \
            --requested-warmup-ms 60000 \
            --requested-duration-ms 600000 \
            --raw-samples "$raw" \
            --workload-events "$events" \
            --workload-metadata "$metadata" \
            --ready-receipt "$READY_RECEIPT" \
            --plan-start-gate "$PLAN_START_GATE" \
            --campaign-id "$campaign" --session-id "$session" \
            --nonce "$nonce" --campaign-secret-file "$secret" \
            --driver-events "$DRIVER_EVENTS" \
            --output "$TEMP_ROOT/adversarial-assembled-$ASSEMBLY_FAILURE_INDEX.tsv"
}

write_sustained_rss() {
    local subject_identity="$1"
    local workload_metadata="$2"
    local workload_events="$3"
    local driver_events="$4"
    local output="$5"
    local final_shift_kib="${6:-0}"
    local ready_receipt="${7:-$READY_RECEIPT}"
    local plan_start_gate="${8:-$PLAN_START_GATE}"
    {
        printf 'elapsed_ms\tcontinuous_ns\trss_kib\tworkload_bytes\tresize_count\n'
        printf '# format_version\t4\n'
        printf '# scenario\tascii\n'
        printf '# sample_interval_ms\t10000\n'
        printf '# requested_duration_ms\t600000\n'
        printf '# plan_start_continuous_ns\t%d\n' "$BASE_CONTINUOUS_NS"
        printf '# measurement_start_continuous_ns\t%d\n' \
            "$((BASE_CONTINUOUS_NS + 60000000000))"
        printf '# plan_start_gate_sha256\t%s\n' "$(sha256 "$plan_start_gate")"
        printf '# subject_identity_sha256\t%s\n' "$(sha256 "$subject_identity")"
        printf '# workload_events_sha256\t%s\n' "$(sha256 "$workload_events")"
        printf '# workload_metadata_sha256\t%s\n' "$(sha256 "$workload_metadata")"
        printf '# ready_receipt_sha256\t%s\n' "$(sha256 "$ready_receipt")"
        printf '# workload_authentication\thmac-sha256\n'
        printf '# progress_interval_ms\t1000\n'
        printf '# maximum_progress_age_ms\t2000\n'
        printf '# driver_events_sha256\t%s\n' "$(sha256 "$driver_events")"
        for ((index = 0; index <= 60; index += 1)); do
            rss_kib=$((100000 + index % 2 * 1000))
            (( index <= 30 )) || rss_kib=$((rss_kib + final_shift_kib))
            printf '%d\t%d\t%d\t%d\t0\n' \
                "$((index * 10000))" \
                "$((BASE_CONTINUOUS_NS + 60000000000 + index * 10000000000))" \
                "$rss_kib" "$((index * 10000000 + 1000000))"
        done
        printf '# status\tcomplete\n'
    } > "$output"
}

write_raw_rss() {
    local subject_identity="$1"
    local output="$2"
    local identity_hash="${3:-$(sha256 "$subject_identity")}"
    local continuous_delay_ns="${4:-0}"
    local ready_receipt="${5:-$READY_RECEIPT}"
    local plan_start_gate="${6:-$PLAN_START_GATE}"
    {
        printf 'elapsed_ms\tcontinuous_ns\trss_kib\n'
        printf '# format_version\t1\n'
        printf '# sample_interval_ms\t10000\n'
        printf '# requested_warmup_ms\t60000\n'
        printf '# requested_duration_ms\t600000\n'
        printf '# plan_start_continuous_ns\t%d\n' "$BASE_CONTINUOUS_NS"
        printf '# measurement_start_continuous_ns\t%d\n' \
            "$((BASE_CONTINUOUS_NS + 60000000000))"
        printf '# plan_start_gate_sha256\t%s\n' "$(sha256 "$plan_start_gate")"
        printf '# ready_receipt_sha256\t%s\n' "$(sha256 "$ready_receipt")"
        printf '# subject_identity_sha256\t%s\n' "$identity_hash"
        for ((index = 0; index <= 60; index += 1)); do
            printf '%d\t%d\t%d\n' "$((500 + index * 10000))" \
                "$((BASE_CONTINUOUS_NS + 60000000000 + continuous_delay_ns \
                    + 500000000 + index * 10000000000))" \
                "$((100000 + index % 2 * 1000))"
        done
        printf '# status\tcomplete\n'
    } > "$output"
}

write_trace_metadata() {
    local subject_identity="$1"
    local run_metadata="$2"
    local workload_metadata="$3"
    local output="$4"
    local target_verified="${5:-true}"
    local start_offset_ns="${6:-0}"
    local end_offset_ns="${7:-0}"
    {
        printf 'format_version\t3\n'
        printf 'capture_status\tCAPTURED\n'
        printf 'incomplete_reason\tnone\n'
        printf 'subject_identity_sha256\t%s\n' "$(sha256 "$subject_identity")"
        printf 'run_metadata_sha256\t%s\n' "$(sha256 "$run_metadata")"
        printf 'workload_metadata_sha256\t%s\n' "$(sha256 "$workload_metadata")"
        printf 'workload_ready_receipt_sha256\t%s\n' \
            "$(sha256 "${TRACE_READY_RECEIPT_OVERRIDE:-$READY_RECEIPT}")"
        printf 'supplemental_evidence_sha256\t%s\n' \
            "$(sha256 "${TRACE_PLAN_START_GATE_OVERRIDE:-$PLAN_START_GATE}")"
        printf 'requested_duration_ms\t600000\n'
        printf 'actual_duration_ms\t600001\n'
        printf 'capture_started_continuous_ns\t%d\n' \
            "$((BASE_CONTINUOUS_NS + 60000000000 + start_offset_ns))"
        printf 'capture_ended_continuous_ns\t%d\n' \
            "$((BASE_CONTINUOUS_NS + 660100000000 + end_offset_ns))"
        printf 'target_identity_verified\t%s\n' "$target_verified"
        printf 'trace_target_pid_verified\t%s\n' "$target_verified"
        printf 'time_profiler_instrument\ttrue\n'
        printf 'allocations_instrument\ttrue\n'
        printf 'hangs_instrument\ttrue\n'
        printf 'time_profiler_target_verified\t%s\n' "$target_verified"
        printf 'allocations_target_verified\t%s\n' "$target_verified"
        printf 'hangs_target_verified\t%s\n' "$target_verified"
        printf 'time_profiler_rows\t1\n'
        printf 'allocations_rows\t1\n'
        # Zero Hangs rows is valid when instrument, target, and duration bind.
        printf 'hangs_rows\t0\n'
        printf 'maximum_main_thread_hang_ms\t0\n'
        printf 'status\tcomplete\n'
    } > "$output"
}

write_manual_artifacts() {
    local output="$1"
    local result="${2:-PASS}"
    {
        printf 'format_version\t1\n'
        printf 'screenshot_sha256\t%s\n' "$(sha256 "$MANUAL_SCREENSHOT")"
        printf 'video_sha256\t%s\n' "$(sha256 "$MANUAL_VIDEO")"
        printf 'final_content_review\tPASS\n'
        printf 'anchor_review\tPASS\n'
        printf 'restoration_review\tPASS\n'
        printf 'geometry_review\tPASS\n'
        printf 'reviewer\tacceptance-operator\n'
        printf 'result\t%s\n' "$result"
    } > "$output"
}

write_native_launch() {
    local identity="$1"
    local output="$2"
    {
        printf 'schema\tspaceterm.acceptance.native-launch-proof/v4\n'
        printf 'observation.source\tproduction-app\n'
        printf 'launch.nonce\t%s\n' "$NONCE"
        printf 'run.id\t%s\n' "$CAMPAIGN_ID"
        printf 'package.app.sha256\t%s\n' "$HASH_C"
        printf 'runtime.schema\tspaceterm.acceptance.runtime-stream/v1\n'
        printf 'runtime.sample_interval_ms\t1000\n'
        printf 'runtime.transition_capacity\t64\n'
        printf 'failure.action.schema\tspaceterm.acceptance.failure-action/v1\n'
        printf 'process.pid\t123\n'
        printf 'process.pidversion\t5\n'
        printf 'process.executable.path\t%s\n' \
            "$(awk -F '\t' '$1 == "executable_path" { print $2 }' "$identity")"
        printf 'process.executable.device\t1\n'
        printf 'process.executable.inode\t2\n'
        printf 'process.executable.fsid\t1:1\n'
        printf 'process.signature.cdhash\tabcd1234\n'
        printf 'process.signature.identifier\tcom.example.spaceterm\n'
        printf 'process.signature.team_identifier\tnone\n'
        printf 'terminal_font_selected\tMenlo 12\n'
        printf 'initial_grid.rows\t40\n'
        printf 'initial_grid.columns\t100\n'
        printf 'initial_grid.logical_width\t1000\n'
        printf 'initial_grid.logical_height\t800\n'
        printf 'initial_grid.backing_pixel_width\t2000\n'
        printf 'initial_grid.backing_pixel_height\t1600\n'
        printf 'observation.complete\ttrue\n'
    } > "$output"
}

write_runtime_observation() {
    local workload_events="$1"
    local samples="$2"
    local events="$3"
    local metadata="$4"
    local failure_actions="$5"
    local drops="${6:-0}"
    local start end accepted
    start="$(awk -F '\t' '$3 == "input-read" { inputs += 1 } \
        END { print inputs + 0 }' "$workload_events")"
    accepted="$start"
    start="$((BASE_CONTINUOUS_NS + 60000000000))"
    end="$((BASE_CONTINUOUS_NS + 660100000000))"
    {
        printf '%s\n' 'sequence	continuous_ns	worker_generation	screens_published	screens_enqueued	screens_superseded	event_queue_length	event_queue_high_water	ui_dispatches	ui_screen_events	ui_drain_high_water	ui_latest_generation	render_latest_generation	next_frame_generation	next_frame_count	presentable	minimized	occluded	workspace_visible	pane_visible	live_resize	viewport_total_rows	viewport_visible_rows	viewport_offset_rows	selection_present	resize_requests	resize_notifications	resize_applied	resize_coalesced	pty_rows	pty_columns	pty_pixel_width	pty_pixel_height	terminal_inputs_accepted	lifecycle	observer_drops'
        for ((index = 0; index <= 600; index += 1)); do
            generation=$((index + 2))
            lifecycle=running
            (( index < 600 )) || lifecycle=exited
            inputs=$((accepted * index / 599))
            (( inputs <= accepted )) || inputs="$accepted"
            screen_events="$generation"
            printf '%d\t%d\t%d\t%d\t%d\t%d\t0\t2\t%d\t%d\t2\t%d\t%d\t%d\t%d\t1\t0\t0\t1\t1\t0\t500\t40\t0\t0\t0\t0\t0\t0\t40\t100\t1000\t800\t%d\t%s\t%d\n' \
                "$index" "$((start + index * 1000000000))" \
                "$generation" "$screen_events" "$generation" "$((index + 1))" \
                "$generation" "$generation" "$generation" "$generation" \
                "$generation" "$generation" "$inputs" \
                "$lifecycle" "$drops"
        done
    } > "$samples"
    {
        printf 'sequence\tcontinuous_ns\tkind\tgeneration\taux0\taux1\n'
        printf '0\t%d\tsession-exited\t602\t0\t0\n' "$end"
    } > "$events"
    {
        printf '%s\n' $'request_id\tsequence\tcase_id\taction\tresult\tpane_id\tpane_state\tfailure_class\tfailure_recoverability\tfailure_operation\tstate_revision\tlatest_generation\tlast_valid_generation\tvisible_generation\tpending_recovery\tterminal_input_usable\tsession_attached'
    } > "$failure_actions"
    {
        printf 'schema\tspaceterm.acceptance.runtime-observation-metadata/v2\n'
        printf 'observation.source\tproduction-app\n'
        printf 'run.id\t%s\n' "$CAMPAIGN_ID"
        printf 'package.app.sha256\t%s\n' "$HASH_C"
        printf 'process.pid\t123\n'
        printf 'runtime.samples.path\truntime-samples.tsv\n'
        printf 'runtime.samples.sha256\t%s\n' "$(sha256 "$samples")"
        printf 'runtime.events.path\truntime-events.tsv\n'
        printf 'runtime.events.sha256\t%s\n' "$(sha256 "$events")"
        printf 'failure.action.schema\tspaceterm.acceptance.failure-action/v1\n'
        printf 'failure.result.schema\tspaceterm.acceptance.failure-action-result/v1\n'
        printf 'failure.actions.path\tfailure-actions.tsv\n'
        printf 'failure.actions.sha256\t%s\n' "$(sha256 "$failure_actions")"
        printf 'failure.result_count\t0\n'
        printf 'observer.started_continuous_ns\t%d\n' "$start"
        printf 'observer.ended_continuous_ns\t%d\n' "$end"
        printf 'observer.sample_interval_ms\t1000\n'
        printf 'observer.transition_capacity\t64\n'
        printf 'observer.sample_count\t601\n'
        printf 'observer.event_count\t1\n'
        printf 'observer.status\tcomplete\n'
        printf 'observation.complete\ttrue\n'
    } > "$metadata"
}

run_case() {
    local subject="$1"
    local identity="$2"
    local run_metadata="$3"
    local workload_events="$4"
    local driver_events="$5"
    local rss="$6"
    local trace="$7"
    local manual="$8"
    local workload_metadata="${WORKLOAD_METADATA_OVERRIDE:-$WORKLOAD_METADATA}"
    local ready_receipt="${READY_RECEIPT_OVERRIDE:-$READY_RECEIPT}"
    local plan_start_gate="${PLAN_START_GATE_OVERRIDE:-$PLAN_START_GATE}"
    [[ "$subject" != spaceterm ]] \
        || {
            workload_metadata="$SPACETERM_WORKLOAD_METADATA"
            ready_receipt="$SPACETERM_READY_RECEIPT"
            plan_start_gate="$SPACETERM_PLAN_START_GATE"
        }
    shift 8
    "$SCRIPT_DIRECTORY/analyze-release-performance-case.sh" \
        --subject "$subject" \
        --scenario ascii \
        --plan "$PLAN" \
        --plan-metadata "$PLAN_METADATA" \
        --pair-metadata "$PAIR_METADATA" \
        --subject-identity "$identity" \
        --run-metadata "$run_metadata" \
        --workload-metadata "$workload_metadata" \
        --workload-events "$workload_events" \
        --ready-receipt "$ready_receipt" \
        --campaign-id "$CAMPAIGN_ID" \
        --session-id "$SESSION_ID" \
        --nonce "$NONCE" \
        --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
        --driver-events "$driver_events" \
        --rss-samples "$rss" \
        --trace-metadata "$trace" \
        --plan-start-gate "$plan_start_gate" \
        --manual-artifacts "$manual" \
        --manual-screenshot "$MANUAL_SCREENSHOT" \
        --manual-video "$MANUAL_VIDEO" \
        "$@"
}

run_case_with_metadata() {
    local metadata="$1"
    shift
    WORKLOAD_METADATA_OVERRIDE="$metadata" run_case "$@"
}

for command in awk bash chmod cmp cp grep mktemp python3 rm sed shasum sort; do
    command -v "$command" >/dev/null 2>&1 || fail "missing command: $command"
done
[[ -f "$SCRIPT_DIRECTORY/performance-workload.c" ]] \
    || fail "performance-workload.c is missing"
[[ -x "$SCRIPT_DIRECTORY/performance-workload.sh" ]] \
    || fail "performance-workload.sh is missing or not executable"
[[ -f "$SCRIPT_DIRECTORY/performance-driver.m" ]] \
    || fail "performance-driver.m is missing"
[[ -f "$SCRIPT_DIRECTORY/performance-rss-sampler.m" ]] \
    || fail "performance-rss-sampler.m is missing"
grep -Fq 'mach_continuous_time' "$SCRIPT_DIRECTORY/performance-workload.c" \
    || fail "workload does not use mach_continuous_time"
grep -Fq 'TIOCGWINSZ' "$SCRIPT_DIRECTORY/performance-workload.c" \
    || fail "workload does not observe exact PTY geometry"
grep -Fq 'CGEventPostToPid' "$SCRIPT_DIRECTORY/performance-driver.m" \
    || fail "native driver is not PID-targeted"
grep -Fq 'proc_pid_rusage' "$SCRIPT_DIRECTORY/performance-rss-sampler.m" \
    || fail "native RSS sampler does not use exact process rusage"
grep -Fq 'mach_continuous_time' "$SCRIPT_DIRECTORY/performance-rss-sampler.m" \
    || fail "native RSS sampler does not use a continuous clock"
grep -Fq 'scheduled_elapsed,' "$SCRIPT_DIRECTORY/performance-rss-sampler.m" \
    || fail "native RSS sampler does not emit exact scheduled elapsed cadence"

# Execute the real native producer under a PTY and verify its exact authenticated
# event stream. This is intentionally short and does not launch either app.
LIVE_PRODUCER="$TEMP_ROOT/live-performance-workload"
xcrun clang -std=c17 -O2 -Wall -Wextra -Werror -Wpedantic \
    -mmacosx-version-min=11.0 \
    "$SCRIPT_DIRECTORY/performance-workload.c" -o "$LIVE_PRODUCER"
LIVE_IDENTITY="$TEMP_ROOT/live-subject-identity.tsv"
write_subject_identity ghostty "$LIVE_IDENTITY"
LIVE_EVENTS="$TEMP_ROOT/live-workload-events.tsv"
LIVE_METADATA="$TEMP_ROOT/live-workload-metadata.tsv"
LIVE_START_GATE="$TEMP_ROOT/live-plan-start-gate.tsv"
LIVE_READY_RECEIPT="$TEMP_ROOT/live-ready-receipt.tsv"
LIVE_SECRET_LINK="$TEMP_ROOT/live-secret-link"
ln "$CAMPAIGN_SECRET_FILE" "$LIVE_SECRET_LINK"
expect_command_failure "campaign secret hard link" \
    "$LIVE_PRODUCER" --scenario ascii --events "$TEMP_ROOT/link-events.tsv" \
        --metrics "$TEMP_ROOT/link-metadata.tsv" --campaign-id "$CAMPAIGN_ID" \
        --session-id "$SESSION_ID" --nonce "$NONCE" \
        --subject-identity "$LIVE_IDENTITY" --campaign-secret-file "$LIVE_SECRET_LINK" \
        --ready-receipt "$TEMP_ROOT/link-ready.tsv" \
        --plan-start-gate "$TEMP_ROOT/link-gate.tsv" --iterations 1
rm -f -- "$LIVE_SECRET_LINK"
publish_plan_start_gate "$LIVE_EVENTS" "$LIVE_START_GATE" \
    "$CAMPAIGN_SECRET_FILE" "$LIVE_READY_RECEIPT" &
live_gate_pid=$!
sleep 2 | script -q /dev/null "$LIVE_PRODUCER" \
    --scenario ascii \
    --events "$LIVE_EVENTS" \
    --metrics "$LIVE_METADATA" \
    --campaign-id "$CAMPAIGN_ID" \
    --session-id "$SESSION_ID" \
    --nonce "$NONCE" \
    --subject-identity "$LIVE_IDENTITY" \
    --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
    --ready-receipt "$LIVE_READY_RECEIPT" \
    --plan-start-gate "$LIVE_START_GATE" \
    --iterations 100 >/dev/null
wait "$live_gate_pid" || fail "live plan-start gate publication failed"
python3 "$SCRIPT_DIRECTORY/verify-performance-workload-ready.py" \
    --ready-receipt "$LIVE_READY_RECEIPT" \
    --events "$LIVE_EVENTS" \
    --subject-identity "$LIVE_IDENTITY" \
    --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
    --campaign-id "$CAMPAIGN_ID" \
    --session-id "$SESSION_ID" \
    --nonce "$NONCE"
python3 "$SCRIPT_DIRECTORY/verify-performance-workload-auth.py" \
    --metadata "$LIVE_METADATA" \
    --events "$LIVE_EVENTS" \
    --subject-identity "$LIVE_IDENTITY" \
    --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
    --ready-receipt "$LIVE_READY_RECEIPT" \
    --campaign-id "$CAMPAIGN_ID" \
    --session-id "$SESSION_ID" \
    --nonce "$NONCE" \
    --scenario ascii \
    --requested-warmup-ms 0 \
    --requested-duration-ms 0 \
    --verified-metadata-output "$TEMP_ROOT/live-verified-metadata.tsv" \
    --verified-events-output "$TEMP_ROOT/live-verified-events.tsv" \
    --verified-subject-identity-output "$TEMP_ROOT/live-verified-subject.tsv"
cmp "$LIVE_METADATA" "$TEMP_ROOT/live-verified-metadata.tsv" \
    || fail "verified workload metadata snapshot differs"
cmp "$LIVE_EVENTS" "$TEMP_ROOT/live-verified-events.tsv" \
    || fail "verified workload event snapshot differs"
cmp "$LIVE_IDENTITY" "$TEMP_ROOT/live-verified-subject.tsv" \
    || fail "verified subject snapshot differs"
awk -F '\t' '
    $3 == "progress" {
        progress += 1
        if (progress > 1 && $5 + 0 <= previous_bytes) exit 1
        previous_bytes = $5 + 0
    }
    $3 == "producer-end" { ended += 1; final_bytes = $5 + 0 }
    END { exit !(progress == 2 && ended == 1 && previous_bytes == final_bytes) }
' "$LIVE_EVENTS" || fail "live producer progress/final accounting is invalid"

MUTATING_SECRET="$TEMP_ROOT/mutating-secret"
printf '0123456789abcdef0123456789abcdef\n' > "$MUTATING_SECRET"
chmod 0600 "$MUTATING_SECRET"
MUTATING_EVENTS="$TEMP_ROOT/mutating-workload-events.tsv"
MUTATING_METADATA="$TEMP_ROOT/mutating-workload-metadata.tsv"
MUTATING_START_GATE="$TEMP_ROOT/mutating-plan-start-gate.tsv"
MUTATING_READY_RECEIPT="$TEMP_ROOT/mutating-ready-receipt.tsv"
publish_plan_start_gate "$MUTATING_EVENTS" "$MUTATING_START_GATE" \
    "$MUTATING_SECRET" "$MUTATING_READY_RECEIPT" &
mutating_gate_pid=$!
sleep 5 | script -q /dev/null "$LIVE_PRODUCER" \
    --scenario ascii \
    --events "$MUTATING_EVENTS" \
    --metrics "$MUTATING_METADATA" \
    --campaign-id "$CAMPAIGN_ID" \
    --session-id "$SESSION_ID" \
    --nonce "$NONCE" \
    --subject-identity "$LIVE_IDENTITY" \
    --campaign-secret-file "$MUTATING_SECRET" \
    --ready-receipt "$MUTATING_READY_RECEIPT" \
    --plan-start-gate "$MUTATING_START_GATE" \
    --iterations 2000 >/dev/null &
mutating_pid=$!
for _ in {1..1200}; do
    if [[ -f "$MUTATING_EVENTS" ]] \
        && awk -F '\t' '$3 == "progress" { found = 1 } END { exit !found }' \
            "$MUTATING_EVENTS" 2>/dev/null; then
        break
    fi
    sleep 0.01
done
[[ -f "$MUTATING_EVENTS" ]] || fail "mutating-secret producer did not start"
printf 'fedcba9876543210fedcba9876543210\n' > "$MUTATING_SECRET"
if wait "$mutating_pid"; then
    fail "producer accepted a campaign secret mutation"
fi
wait "$mutating_gate_pid" || fail "mutating plan-start gate publication failed"
[[ ! -e "$MUTATING_METADATA" ]] \
    || fail "producer published metadata after a campaign secret mutation"

# Plans are deterministic, immutable, ordered, and contain the required cases.
for scenario in ascii unicode-styles scrolled hidden-occluded resize; do
    "$SCRIPT_DIRECTORY/performance-plan.sh" \
        --scenario "$scenario" \
        --plan "$TEMP_ROOT/$scenario-plan.tsv" \
        --metadata "$TEMP_ROOT/$scenario-plan-metadata.tsv" >/dev/null
    [[ ! -w "$TEMP_ROOT/$scenario-plan.tsv" ]] || fail "$scenario plan is mutable"
    [[ "$(sha256 "$TEMP_ROOT/$scenario-plan.tsv")" \
        == "$(awk -F '\t' '$1 == "plan_sha256" { print $2 }' \
            "$TEMP_ROOT/$scenario-plan-metadata.tsv")" ]] \
        || fail "$scenario plan hash mismatch"
done
[[ "$(awk -F '\t' '$3 == "resize-grid" { count += 1 } END { print count + 0 }' \
    "$TEMP_ROOT/resize-plan.tsv")" == 300 ]] || fail "resize plan is not 300 cycles"

PLAN="$TEMP_ROOT/ascii-plan.tsv"
PLAN_METADATA="$TEMP_ROOT/ascii-plan-metadata.tsv"
readonly PLAN PLAN_METADATA
WORKLOAD_BINARY="$TEMP_ROOT/performance-workload"
printf 'deterministic fake workload binary\n' > "$WORKLOAD_BINARY"
readonly WORKLOAD_BINARY
for manifest in command environment font initial-grid; do
    printf '%s-manifest-v1\n' "$manifest" > "$TEMP_ROOT/$manifest.tsv"
done
SPACETERM_IDENTITY="$TEMP_ROOT/spaceterm-identity.tsv"
GHOSTTY_IDENTITY="$TEMP_ROOT/ghostty-identity.tsv"
write_subject_identity spaceterm "$SPACETERM_IDENTITY"
write_subject_identity ghostty "$GHOSTTY_IDENTITY"
readonly SPACETERM_IDENTITY GHOSTTY_IDENTITY
PAIR_METADATA="$TEMP_ROOT/pair-metadata.tsv"
"$SCRIPT_DIRECTORY/freeze-performance-pair.sh" \
    --pair-id pair-ascii \
    --scenario ascii \
    --plan "$PLAN" \
    --plan-metadata "$PLAN_METADATA" \
    --workload-binary "$WORKLOAD_BINARY" \
    --command-manifest "$TEMP_ROOT/command.tsv" \
    --environment-manifest "$TEMP_ROOT/environment.tsv" \
    --font-manifest "$TEMP_ROOT/font.tsv" \
    --initial-grid-manifest "$TEMP_ROOT/initial-grid.tsv" \
    --spaceterm-identity "$SPACETERM_IDENTITY" \
    --ghostty-identity "$GHOSTTY_IDENTITY" \
    --output "$PAIR_METADATA" >/dev/null
readonly PAIR_METADATA

GHOSTTY_RUN="$TEMP_ROOT/ghostty-run.tsv"
"$SCRIPT_DIRECTORY/freeze-performance-run.sh" \
    --subject ghostty \
    --pair-metadata "$PAIR_METADATA" \
    --subject-identity "$GHOSTTY_IDENTITY" \
    --plan "$PLAN" \
    --workload-binary "$WORKLOAD_BINARY" \
    --command-manifest "$TEMP_ROOT/command.tsv" \
    --environment-manifest "$TEMP_ROOT/environment.tsv" \
    --font-manifest "$TEMP_ROOT/font.tsv" \
    --initial-grid-manifest "$TEMP_ROOT/initial-grid.tsv" \
    --output "$GHOSTTY_RUN" >/dev/null
readonly GHOSTTY_RUN

SPACETERM_RUN="$TEMP_ROOT/spaceterm-run.tsv"
"$SCRIPT_DIRECTORY/freeze-performance-run.sh" \
    --subject spaceterm \
    --pair-metadata "$PAIR_METADATA" \
    --subject-identity "$SPACETERM_IDENTITY" \
    --plan "$PLAN" \
    --workload-binary "$WORKLOAD_BINARY" \
    --command-manifest "$TEMP_ROOT/command.tsv" \
    --environment-manifest "$TEMP_ROOT/environment.tsv" \
    --font-manifest "$TEMP_ROOT/font.tsv" \
    --initial-grid-manifest "$TEMP_ROOT/initial-grid.tsv" \
    --output "$SPACETERM_RUN" >/dev/null
readonly SPACETERM_RUN

DRIVER_EVENTS="$TEMP_ROOT/driver-events.tsv"
WORKLOAD_EVENTS="$TEMP_ROOT/workload-events.tsv"
WORKLOAD_METADATA="$TEMP_ROOT/workload-metadata.tsv"
SPACETERM_WORKLOAD_METADATA="$TEMP_ROOT/spaceterm-workload-metadata.tsv"
READY_RECEIPT="$TEMP_ROOT/synthetic-ready-receipt.tsv"
PLAN_START_GATE="$TEMP_ROOT/synthetic-plan-start-gate.tsv"
SPACETERM_READY_RECEIPT="$TEMP_ROOT/spaceterm-ready-receipt.tsv"
SPACETERM_PLAN_START_GATE="$TEMP_ROOT/spaceterm-plan-start-gate.tsv"
GHOSTTY_RSS="$TEMP_ROOT/ghostty-rss.tsv"
GHOSTTY_TRACE="$TEMP_ROOT/ghostty-trace.tsv"
MANUAL="$TEMP_ROOT/manual.tsv"
MANUAL_SCREENSHOT="$TEMP_ROOT/manual-screenshot.png"
MANUAL_VIDEO="$TEMP_ROOT/manual-video.mov"
write_driver_events "$PLAN" "$DRIVER_EVENTS"
write_workload_events "$DRIVER_EVENTS" "$WORKLOAD_EVENTS"
write_ready_receipt "$WORKLOAD_EVENTS" "$GHOSTTY_IDENTITY" "$READY_RECEIPT"
publish_plan_start_gate "$WORKLOAD_EVENTS" "$PLAN_START_GATE" \
    "$CAMPAIGN_SECRET_FILE" "$READY_RECEIPT" "$BASE_CONTINUOUS_NS"
write_ready_receipt "$WORKLOAD_EVENTS" "$SPACETERM_IDENTITY" \
    "$SPACETERM_READY_RECEIPT"
publish_plan_start_gate "$WORKLOAD_EVENTS" "$SPACETERM_PLAN_START_GATE" \
    "$CAMPAIGN_SECRET_FILE" "$SPACETERM_READY_RECEIPT" "$BASE_CONTINUOUS_NS"
write_workload_metadata "$WORKLOAD_BINARY" "$WORKLOAD_EVENTS" \
    "$GHOSTTY_IDENTITY" "$WORKLOAD_METADATA"
write_workload_metadata "$WORKLOAD_BINARY" "$WORKLOAD_EVENTS" \
    "$SPACETERM_IDENTITY" "$SPACETERM_WORKLOAD_METADATA" \
    "$SPACETERM_READY_RECEIPT"
write_sustained_rss "$GHOSTTY_IDENTITY" "$WORKLOAD_METADATA" \
    "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$GHOSTTY_RSS"
write_trace_metadata "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
    "$WORKLOAD_METADATA" "$GHOSTTY_TRACE"
printf 'bounded fake screenshot evidence\n' > "$MANUAL_SCREENSHOT"
printf 'bounded fake video evidence\n' > "$MANUAL_VIDEO"
write_manual_artifacts "$MANUAL"
readonly DRIVER_EVENTS WORKLOAD_EVENTS WORKLOAD_METADATA SPACETERM_WORKLOAD_METADATA
readonly READY_RECEIPT
readonly PLAN_START_GATE
readonly SPACETERM_READY_RECEIPT SPACETERM_PLAN_START_GATE
readonly GHOSTTY_RSS GHOSTTY_TRACE MANUAL
readonly MANUAL_SCREENSHOT MANUAL_VIDEO

RAW_RSS="$TEMP_ROOT/raw-rss.tsv"
ASSEMBLED_RSS="$TEMP_ROOT/assembled-rss.tsv"
write_raw_rss "$GHOSTTY_IDENTITY" "$RAW_RSS"
"$SCRIPT_DIRECTORY/assemble-release-performance-rss-v3.sh" \
    --subject-identity "$GHOSTTY_IDENTITY" \
    --scenario ascii \
    --requested-warmup-ms 60000 \
    --requested-duration-ms 600000 \
    --raw-samples "$RAW_RSS" \
    --workload-events "$WORKLOAD_EVENTS" \
    --workload-metadata "$WORKLOAD_METADATA" \
    --ready-receipt "$READY_RECEIPT" \
    --plan-start-gate "$PLAN_START_GATE" \
    --campaign-id "$CAMPAIGN_ID" --session-id "$SESSION_ID" \
    --nonce "$NONCE" --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
    --driver-events "$DRIVER_EVENTS" \
    --output "$ASSEMBLED_RSS"
readonly RAW_RSS ASSEMBLED_RSS

# Force the scheduler ordering that used to race: the sampler is already
# waiting at the measurement boundary, while progress-000000 is not published
# until 50 ms later. The scheduled 500 ms sample must join that live progress.
SAMPLER_FIRST_EVENTS="$TEMP_ROOT/sampler-first-events.tsv"
SAMPLER_FIRST_BODY="$TEMP_ROOT/sampler-first-events-body.tsv"
awk -F '\t' 'BEGIN { OFS = "\t" }
    NR == 1 { next }
    $3 == "progress" || $3 == "producer-end" { $2 += 50000000 }
    { print }
' "$WORKLOAD_EVENTS" | sort -t $'\t' -k2,2n -k1,1n > "$SAMPLER_FIRST_BODY"
{
    head -n 1 "$WORKLOAD_EVENTS"
    awk -F '\t' 'BEGIN { OFS = "\t" } { $1 = NR - 1; print }' \
        "$SAMPLER_FIRST_BODY"
} > "$SAMPLER_FIRST_EVENTS"
chmod 0444 "$SAMPLER_FIRST_EVENTS"
SAMPLER_FIRST_READY="$TEMP_ROOT/sampler-first-ready.tsv"
SAMPLER_FIRST_GATE="$TEMP_ROOT/sampler-first-gate.tsv"
write_ready_receipt "$SAMPLER_FIRST_EVENTS" "$GHOSTTY_IDENTITY" \
    "$SAMPLER_FIRST_READY"
publish_plan_start_gate "$SAMPLER_FIRST_EVENTS" "$SAMPLER_FIRST_GATE" \
    "$CAMPAIGN_SECRET_FILE" "$SAMPLER_FIRST_READY" "$BASE_CONTINUOUS_NS"
SAMPLER_FIRST_SOURCE_METADATA="$TEMP_ROOT/sampler-first-source-metadata.tsv"
awk -F '\t' 'BEGIN { OFS = "\t" }
    $1 == "started_continuous_ns" || $1 == "ended_continuous_ns" {
        $2 += 50000000
    }
    { print }
' "$WORKLOAD_METADATA" > "$SAMPLER_FIRST_SOURCE_METADATA"
SAMPLER_FIRST_METADATA="$TEMP_ROOT/sampler-first-metadata.tsv"
resign_workload_metadata "$SAMPLER_FIRST_SOURCE_METADATA" \
    "$SAMPLER_FIRST_EVENTS" "$SAMPLER_FIRST_METADATA" "$SAMPLER_FIRST_READY"
SAMPLER_FIRST_RAW="$TEMP_ROOT/sampler-first-raw.tsv"
SAMPLER_FIRST_ASSEMBLED="$TEMP_ROOT/sampler-first-assembled.tsv"
write_raw_rss "$GHOSTTY_IDENTITY" "$SAMPLER_FIRST_RAW" \
    "$(sha256 "$GHOSTTY_IDENTITY")" 0 "$SAMPLER_FIRST_READY" \
    "$SAMPLER_FIRST_GATE"
"$SCRIPT_DIRECTORY/assemble-release-performance-rss-v3.sh" \
    --subject-identity "$GHOSTTY_IDENTITY" \
    --scenario ascii \
    --requested-warmup-ms 60000 \
    --requested-duration-ms 600000 \
    --raw-samples "$SAMPLER_FIRST_RAW" \
    --workload-events "$SAMPLER_FIRST_EVENTS" \
    --workload-metadata "$SAMPLER_FIRST_METADATA" \
    --ready-receipt "$SAMPLER_FIRST_READY" \
    --plan-start-gate "$SAMPLER_FIRST_GATE" \
    --campaign-id "$CAMPAIGN_ID" --session-id "$SESSION_ID" \
    --nonce "$NONCE" --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
    --driver-events "$DRIVER_EVENTS" \
    --output "$SAMPLER_FIRST_ASSEMBLED"
[[ "$(awk -F '\t' '$1 == 500 { print $4 }' "$SAMPLER_FIRST_ASSEMBLED")" == 1000000 ]] \
    || fail "sampler-first sample zero did not join live progress"

# Normal scheduling delay changes actual continuous time, never the exact
# scheduled elapsed cadence or the 500 ms post-duration boundary.
DELAYED_RAW_RSS="$TEMP_ROOT/delayed-raw-rss.tsv"
DELAYED_ASSEMBLED_RSS="$TEMP_ROOT/delayed-assembled-rss.tsv"
write_raw_rss "$GHOSTTY_IDENTITY" "$DELAYED_RAW_RSS" \
    "$(sha256 "$GHOSTTY_IDENTITY")" 400000000
[[ "$(awk -F '\t' '$1 !~ /^#/ && $1 ~ /^[0-9]+$/ { last = $1 } END { print last }' \
    "$DELAYED_RAW_RSS")" == 600500 ]] \
    || fail "delayed raw RSS changed the scheduled final boundary"
"$SCRIPT_DIRECTORY/assemble-release-performance-rss-v3.sh" \
    --subject-identity "$GHOSTTY_IDENTITY" \
    --scenario ascii \
    --requested-warmup-ms 60000 \
    --requested-duration-ms 600000 \
    --raw-samples "$DELAYED_RAW_RSS" \
    --workload-events "$WORKLOAD_EVENTS" \
    --workload-metadata "$WORKLOAD_METADATA" \
    --ready-receipt "$READY_RECEIPT" \
    --plan-start-gate "$PLAN_START_GATE" \
    --campaign-id "$CAMPAIGN_ID" --session-id "$SESSION_ID" \
    --nonce "$NONCE" --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
    --driver-events "$DRIVER_EVENTS" \
    --output "$DELAYED_ASSEMBLED_RSS"
expect_result 0 PASS "scheduled RSS cadence survives normal sampling delay" \
    run_case ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$DELAYED_ASSEMBLED_RSS" \
        "$GHOSTTY_TRACE" "$MANUAL"

expect_result 0 PASS "valid assembled raw RSS evidence" \
    run_case ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$ASSEMBLED_RSS" \
        "$GHOSTTY_TRACE" "$MANUAL"

WRONG_SUBJECT_RAW="$TEMP_ROOT/wrong-subject-raw.tsv"
write_raw_rss "$GHOSTTY_IDENTITY" "$WRONG_SUBJECT_RAW" "$HASH_C"
expect_command_failure "raw RSS subject binding mismatch" \
    "$SCRIPT_DIRECTORY/assemble-release-performance-rss-v3.sh" \
        --subject-identity "$GHOSTTY_IDENTITY" \
        --scenario ascii \
        --requested-warmup-ms 60000 \
        --requested-duration-ms 600000 \
        --raw-samples "$WRONG_SUBJECT_RAW" \
        --workload-events "$WORKLOAD_EVENTS" \
        --workload-metadata "$WORKLOAD_METADATA" \
        --ready-receipt "$READY_RECEIPT" \
        --plan-start-gate "$PLAN_START_GATE" \
        --campaign-id "$CAMPAIGN_ID" --session-id "$SESSION_ID" \
        --nonce "$NONCE" --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
        --driver-events "$DRIVER_EVENTS" \
        --output "$TEMP_ROOT/wrong-subject-assembled.tsv"

TRUNCATED_RAW="$TEMP_ROOT/truncated-raw.tsv"
sed '$d' "$RAW_RSS" > "$TRUNCATED_RAW"
expect_command_failure "raw RSS missing completion marker" \
    "$SCRIPT_DIRECTORY/assemble-release-performance-rss-v3.sh" \
        --subject-identity "$GHOSTTY_IDENTITY" \
        --scenario ascii \
        --requested-warmup-ms 60000 \
        --requested-duration-ms 600000 \
        --raw-samples "$TRUNCATED_RAW" \
        --workload-events "$WORKLOAD_EVENTS" \
        --workload-metadata "$WORKLOAD_METADATA" \
        --ready-receipt "$READY_RECEIPT" \
        --plan-start-gate "$PLAN_START_GATE" \
        --campaign-id "$CAMPAIGN_ID" --session-id "$SESSION_ID" \
        --nonce "$NONCE" --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
        --driver-events "$DRIVER_EVENTS" \
        --output "$TEMP_ROOT/truncated-assembled.tsv"

# Authenticated progress must be live, ordered, monotonic, fresh, and bound to
# this exact campaign, subject, process identity, secret, and event stream.
MISSING_READY="$TEMP_ROOT/missing-measurement-ready.tsv"
awk -F '\t' '$3 != "measurement-ready"' "$WORKLOAD_EVENTS" \
    > "$TEMP_ROOT/missing-measurement-ready-raw.tsv"
normalize_workload_events "$TEMP_ROOT/missing-measurement-ready-raw.tsv" "$MISSING_READY"
MISSING_READY_METADATA="$TEMP_ROOT/missing-measurement-ready-metadata.tsv"
resign_workload_metadata "$WORKLOAD_METADATA" "$MISSING_READY" \
    "$MISSING_READY_METADATA"
expect_progress_assembly_failure "missing measurement-ready event" "$MISSING_READY" \
    "$MISSING_READY_METADATA" "$RAW_RSS"

DUPLICATE_READY="$TEMP_ROOT/duplicate-measurement-ready.tsv"
awk -F '\t' 'BEGIN { OFS = "\t" }
    { print }
    $3 == "measurement-ready" { $2 += 1; print }
' "$WORKLOAD_EVENTS" | sort -t $'\t' -k2,2n \
    | awk -F '\t' 'BEGIN { OFS = "\t" } NR == 1 { print; next } { $1 = NR - 2; print }' \
    > "$DUPLICATE_READY"
chmod 0444 "$DUPLICATE_READY"
DUPLICATE_READY_METADATA="$TEMP_ROOT/duplicate-measurement-ready-metadata.tsv"
resign_workload_metadata "$WORKLOAD_METADATA" "$DUPLICATE_READY" \
    "$DUPLICATE_READY_METADATA"
expect_progress_assembly_failure "duplicate measurement-ready event" "$DUPLICATE_READY" \
    "$DUPLICATE_READY_METADATA" "$RAW_RSS"

LATE_READY="$TEMP_ROOT/late-measurement-ready.tsv"
awk -F '\t' 'BEGIN { OFS = "\t" }
    $3 == "measurement-ready" { $2 = 1060000000001 }
    { print }
' "$WORKLOAD_EVENTS" | sort -t $'\t' -k2,2n \
    | awk -F '\t' 'BEGIN { OFS = "\t" } NR == 1 { print; next } { $1 = NR - 2; print }' \
    > "$LATE_READY"
chmod 0444 "$LATE_READY"
LATE_READY_METADATA="$TEMP_ROOT/late-measurement-ready-metadata.tsv"
resign_workload_metadata "$WORKLOAD_METADATA" "$LATE_READY" "$LATE_READY_METADATA"
expect_progress_assembly_failure "measurement-ready after first progress" "$LATE_READY" \
    "$LATE_READY_METADATA" "$RAW_RSS"

NO_PROGRESS="$TEMP_ROOT/no-progress.tsv"
awk -F '\t' '$3 != "progress"' "$WORKLOAD_EVENTS" > "$TEMP_ROOT/no-progress-raw.tsv"
normalize_workload_events "$TEMP_ROOT/no-progress-raw.tsv" "$NO_PROGRESS"
NO_PROGRESS_METADATA="$TEMP_ROOT/no-progress-metadata.tsv"
resign_workload_metadata "$WORKLOAD_METADATA" "$NO_PROGRESS" "$NO_PROGRESS_METADATA"
expect_progress_assembly_failure "missing progress stream" "$NO_PROGRESS" \
    "$NO_PROGRESS_METADATA" "$RAW_RSS"

ONLY_FINAL_PROGRESS="$TEMP_ROOT/only-final-progress.tsv"
awk -F '\t' '$3 != "progress" || $4 == "progress-000600"' \
    "$WORKLOAD_EVENTS" > "$TEMP_ROOT/only-final-progress-raw.tsv"
normalize_workload_events "$TEMP_ROOT/only-final-progress-raw.tsv" "$ONLY_FINAL_PROGRESS"
ONLY_FINAL_METADATA="$TEMP_ROOT/only-final-progress-metadata.tsv"
resign_workload_metadata "$WORKLOAD_METADATA" "$ONLY_FINAL_PROGRESS" "$ONLY_FINAL_METADATA"
expect_progress_assembly_failure "only final progress" "$ONLY_FINAL_PROGRESS" \
    "$ONLY_FINAL_METADATA" "$RAW_RSS"

FORGED_FINAL_ROWS="$TEMP_ROOT/forged-final-progress.tsv"
awk -F '\t' 'BEGIN { OFS = "\t" } $3 == "progress" { $5 = 601000000 } { print }' \
    "$WORKLOAD_EVENTS" > "$TEMP_ROOT/forged-final-progress-raw.tsv"
normalize_workload_events "$TEMP_ROOT/forged-final-progress-raw.tsv" "$FORGED_FINAL_ROWS"
FORGED_FINAL_METADATA="$TEMP_ROOT/forged-final-progress-metadata.tsv"
resign_workload_metadata "$WORKLOAD_METADATA" "$FORGED_FINAL_ROWS" "$FORGED_FINAL_METADATA"
expect_progress_assembly_failure "forged final total in earlier progress" \
    "$FORGED_FINAL_ROWS" "$FORGED_FINAL_METADATA" "$RAW_RSS"

FUTURE_PROGRESS="$TEMP_ROOT/future-progress.tsv"
awk -F '\t' 'BEGIN { OFS = "\t" } $4 == "progress-000000" { $2 += 3000000000 } \
    { print }' "$WORKLOAD_EVENTS" > "$TEMP_ROOT/future-progress-raw.tsv"
normalize_workload_events "$TEMP_ROOT/future-progress-raw.tsv" "$FUTURE_PROGRESS"
FUTURE_METADATA="$TEMP_ROOT/future-progress-metadata.tsv"
resign_workload_metadata "$WORKLOAD_METADATA" "$FUTURE_PROGRESS" "$FUTURE_METADATA"
expect_progress_assembly_failure "future progress cannot bind sample zero" \
    "$FUTURE_PROGRESS" "$FUTURE_METADATA" "$RAW_RSS"

EARLY_RAW="$TEMP_ROOT/sample-before-first-progress.tsv"
write_raw_rss "$GHOSTTY_IDENTITY" "$EARLY_RAW" \
    "$(sha256 "$GHOSTTY_IDENTITY")" -700000000
expect_progress_assembly_failure "sample before first progress" "$WORKLOAD_EVENTS" \
    "$WORKLOAD_METADATA" "$EARLY_RAW"

MISSING_INTERVAL="$TEMP_ROOT/missing-progress-interval.tsv"
awk -F '\t' '$4 != "progress-000010" && $4 != "progress-000011"' \
    "$WORKLOAD_EVENTS" > "$TEMP_ROOT/missing-progress-interval-raw.tsv"
normalize_workload_events "$TEMP_ROOT/missing-progress-interval-raw.tsv" "$MISSING_INTERVAL"
MISSING_INTERVAL_METADATA="$TEMP_ROOT/missing-progress-interval-metadata.tsv"
resign_workload_metadata "$WORKLOAD_METADATA" "$MISSING_INTERVAL" \
    "$MISSING_INTERVAL_METADATA"
expect_progress_assembly_failure "progress gap exceeds two seconds" "$MISSING_INTERVAL" \
    "$MISSING_INTERVAL_METADATA" "$RAW_RSS"

REGRESSING_PROGRESS="$TEMP_ROOT/regressing-progress.tsv"
awk -F '\t' 'BEGIN { OFS = "\t" } $4 == "progress-000010" { $5 = 1 } \
    { print }' "$WORKLOAD_EVENTS" > "$REGRESSING_PROGRESS"
chmod 0444 "$REGRESSING_PROGRESS"
REGRESSING_METADATA="$TEMP_ROOT/regressing-progress-metadata.tsv"
resign_workload_metadata "$WORKLOAD_METADATA" "$REGRESSING_PROGRESS" \
    "$REGRESSING_METADATA"
expect_progress_assembly_failure "regressing progress bytes" "$REGRESSING_PROGRESS" \
    "$REGRESSING_METADATA" "$RAW_RSS"

BAD_PROGRESS_ID="$TEMP_ROOT/bad-progress-id.tsv"
awk -F '\t' 'BEGIN { OFS = "\t" } $4 == "progress-000010" { $4 = "progress-000009" } \
    { print }' "$WORKLOAD_EVENTS" > "$BAD_PROGRESS_ID"
chmod 0444 "$BAD_PROGRESS_ID"
BAD_PROGRESS_ID_METADATA="$TEMP_ROOT/bad-progress-id-metadata.tsv"
resign_workload_metadata "$WORKLOAD_METADATA" "$BAD_PROGRESS_ID" \
    "$BAD_PROGRESS_ID_METADATA"
expect_progress_assembly_failure "duplicate progress ID" "$BAD_PROGRESS_ID" \
    "$BAD_PROGRESS_ID_METADATA" "$RAW_RSS"

BAD_SEQUENCE="$TEMP_ROOT/bad-progress-sequence.tsv"
awk -F '\t' 'BEGIN { OFS = "\t" } NR == 20 { $1 += 1 } { print }' \
    "$WORKLOAD_EVENTS" > "$BAD_SEQUENCE"
chmod 0444 "$BAD_SEQUENCE"
BAD_SEQUENCE_METADATA="$TEMP_ROOT/bad-progress-sequence-metadata.tsv"
resign_workload_metadata "$WORKLOAD_METADATA" "$BAD_SEQUENCE" \
    "$BAD_SEQUENCE_METADATA"
expect_progress_assembly_failure "out-of-order progress sequence" "$BAD_SEQUENCE" \
    "$BAD_SEQUENCE_METADATA" "$RAW_RSS"

PROGRESS_OVER_FINAL="$TEMP_ROOT/progress-over-final.tsv"
awk -F '\t' 'BEGIN { OFS = "\t" } $4 == "progress-000500" { $5 = 700000000 } \
    { print }' "$WORKLOAD_EVENTS" > "$PROGRESS_OVER_FINAL"
chmod 0444 "$PROGRESS_OVER_FINAL"
PROGRESS_OVER_FINAL_METADATA="$TEMP_ROOT/progress-over-final-metadata.tsv"
resign_workload_metadata "$WORKLOAD_METADATA" "$PROGRESS_OVER_FINAL" \
    "$PROGRESS_OVER_FINAL_METADATA"
expect_progress_assembly_failure "progress exceeds final total" "$PROGRESS_OVER_FINAL" \
    "$PROGRESS_OVER_FINAL_METADATA" "$RAW_RSS"

FINAL_MISMATCH="$TEMP_ROOT/final-total-mismatch.tsv"
awk -F '\t' 'BEGIN { OFS = "\t" } $3 == "producer-end" { $5 -= 1 } { print }' \
    "$WORKLOAD_EVENTS" > "$FINAL_MISMATCH"
chmod 0444 "$FINAL_MISMATCH"
FINAL_MISMATCH_METADATA="$TEMP_ROOT/final-total-mismatch-metadata.tsv"
resign_workload_metadata "$WORKLOAD_METADATA" "$FINAL_MISMATCH" \
    "$FINAL_MISMATCH_METADATA"
expect_progress_assembly_failure "producer final accounting mismatch" "$FINAL_MISMATCH" \
    "$FINAL_MISMATCH_METADATA" "$RAW_RSS"

BAD_STATUS="$TEMP_ROOT/progress-bad-status.tsv"
awk -F '\t' 'BEGIN { OFS = "\t" } $4 == "progress-000010" { $10 = "forged" } \
    { print }' "$WORKLOAD_EVENTS" > "$BAD_STATUS"
chmod 0444 "$BAD_STATUS"
BAD_STATUS_METADATA="$TEMP_ROOT/progress-bad-status-metadata.tsv"
resign_workload_metadata "$WORKLOAD_METADATA" "$BAD_STATUS" "$BAD_STATUS_METADATA"
expect_progress_assembly_failure "forged progress status" "$BAD_STATUS" \
    "$BAD_STATUS_METADATA" "$RAW_RSS"

WRONG_SECRET="$TEMP_ROOT/wrong-campaign-secret"
printf 'fedcba9876543210fedcba9876543210\n' > "$WRONG_SECRET"
chmod 0600 "$WRONG_SECRET"
expect_progress_assembly_failure "wrong campaign secret" "$WORKLOAD_EVENTS" \
    "$WORKLOAD_METADATA" "$RAW_RSS" "$GHOSTTY_IDENTITY" "$WRONG_SECRET"
expect_progress_assembly_failure "stale campaign replay" "$WORKLOAD_EVENTS" \
    "$WORKLOAD_METADATA" "$RAW_RSS" "$GHOSTTY_IDENTITY" \
    "$CAMPAIGN_SECRET_FILE" stale-campaign
expect_progress_assembly_failure "stale session replay" "$WORKLOAD_EVENTS" \
    "$WORKLOAD_METADATA" "$RAW_RSS" "$GHOSTTY_IDENTITY" \
    "$CAMPAIGN_SECRET_FILE" "$CAMPAIGN_ID" stale-session
expect_progress_assembly_failure "stale nonce replay" "$WORKLOAD_EVENTS" \
    "$WORKLOAD_METADATA" "$RAW_RSS" "$GHOSTTY_IDENTITY" \
    "$CAMPAIGN_SECRET_FILE" "$CAMPAIGN_ID" "$SESSION_ID" "$HASH_A"

ALTERED_IDENTITY="$TEMP_ROOT/altered-subject-identity.tsv"
sed 's/process_pid\t123/process_pid\t124/' "$GHOSTTY_IDENTITY" > "$ALTERED_IDENTITY"
chmod 0444 "$ALTERED_IDENTITY"
expect_progress_assembly_failure "mismatched subject process identity" \
    "$WORKLOAD_EVENTS" "$WORKLOAD_METADATA" "$RAW_RSS" "$ALTERED_IDENTITY"

TAMPERED_EVENTS="$TEMP_ROOT/tampered-events.tsv"
cp "$WORKLOAD_EVENTS" "$TAMPERED_EVENTS"
chmod u+w "$TAMPERED_EVENTS"
sed -i '' 's/progress-000010/progress-000099/' "$TAMPERED_EVENTS"
chmod 0444 "$TAMPERED_EVENTS"
expect_progress_assembly_failure "events altered after authentication" "$TAMPERED_EVENTS" \
    "$WORKLOAD_METADATA" "$RAW_RSS"

WORLD_SECRET="$TEMP_ROOT/world-readable-secret"
cp "$CAMPAIGN_SECRET_FILE" "$WORLD_SECRET"
chmod 0644 "$WORLD_SECRET"
expect_progress_assembly_failure "world-readable campaign secret" "$WORKLOAD_EVENTS" \
    "$WORKLOAD_METADATA" "$RAW_RSS" "$GHOSTTY_IDENTITY" "$WORLD_SECRET"

SECRET_SYMLINK="$TEMP_ROOT/secret-symlink"
ln -s "$CAMPAIGN_SECRET_FILE" "$SECRET_SYMLINK"
expect_progress_assembly_failure "campaign secret symlink" "$WORKLOAD_EVENTS" \
    "$WORKLOAD_METADATA" "$RAW_RSS" "$GHOSTTY_IDENTITY" "$SECRET_SYMLINK"

expect_result 0 PASS "valid Ghostty case with zero Hangs" \
    run_case ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$GHOSTTY_RSS" "$GHOSTTY_TRACE" "$MANUAL"

SPACETERM_RSS="$TEMP_ROOT/spaceterm-rss.tsv"
SPACETERM_TRACE="$TEMP_ROOT/spaceterm-trace.tsv"
NATIVE_LAUNCH="$TEMP_ROOT/native-launch.tsv"
RUNTIME_SAMPLES="$TEMP_ROOT/runtime-samples.tsv"
RUNTIME_EVENTS="$TEMP_ROOT/runtime-events.tsv"
RUNTIME_METADATA="$TEMP_ROOT/runtime-metadata.tsv"
FAILURE_ACTIONS="$TEMP_ROOT/failure-actions.tsv"
write_sustained_rss "$SPACETERM_IDENTITY" "$SPACETERM_WORKLOAD_METADATA" "$WORKLOAD_EVENTS" \
    "$DRIVER_EVENTS" "$SPACETERM_RSS" 0 "$SPACETERM_READY_RECEIPT" \
    "$SPACETERM_PLAN_START_GATE"
TRACE_READY_RECEIPT_OVERRIDE="$SPACETERM_READY_RECEIPT" \
TRACE_PLAN_START_GATE_OVERRIDE="$SPACETERM_PLAN_START_GATE" \
    write_trace_metadata "$SPACETERM_IDENTITY" "$SPACETERM_RUN" \
        "$SPACETERM_WORKLOAD_METADATA" "$SPACETERM_TRACE"
write_native_launch "$SPACETERM_IDENTITY" "$NATIVE_LAUNCH"
write_runtime_observation "$WORKLOAD_EVENTS" "$RUNTIME_SAMPLES" \
    "$RUNTIME_EVENTS" "$RUNTIME_METADATA" "$FAILURE_ACTIONS"
readonly SPACETERM_RSS SPACETERM_TRACE NATIVE_LAUNCH
readonly RUNTIME_SAMPLES RUNTIME_EVENTS RUNTIME_METADATA
readonly FAILURE_ACTIONS

expect_result 0 PASS "valid authenticated SpaceTerm runtime case" \
    run_case spaceterm "$SPACETERM_IDENTITY" "$SPACETERM_RUN" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$SPACETERM_RSS" \
        "$SPACETERM_TRACE" "$MANUAL" \
        --runtime-samples "$RUNTIME_SAMPLES" \
        --runtime-events "$RUNTIME_EVENTS" \
        --runtime-metadata "$RUNTIME_METADATA" \
        --failure-actions "$FAILURE_ACTIONS" \
        --native-launch-observation "$NATIVE_LAUNCH"

ALTERED_FONT="$TEMP_ROOT/altered-font.tsv"
printf 'different-font-manifest\n' > "$ALTERED_FONT"
expect_command_failure "paired font mismatch" \
    "$SCRIPT_DIRECTORY/freeze-performance-run.sh" \
        --subject ghostty \
        --pair-metadata "$PAIR_METADATA" \
        --subject-identity "$GHOSTTY_IDENTITY" \
        --plan "$PLAN" \
        --workload-binary "$WORKLOAD_BINARY" \
        --command-manifest "$TEMP_ROOT/command.tsv" \
        --environment-manifest "$TEMP_ROOT/environment.tsv" \
        --font-manifest "$ALTERED_FONT" \
        --initial-grid-manifest "$TEMP_ROOT/initial-grid.tsv" \
        --output "$TEMP_ROOT/invalid-run.tsv"

BAD_DURATION_RUN="$TEMP_ROOT/bad-duration-run.tsv"
sed 's/measured_duration_ms\t600000/measured_duration_ms\t599000/' \
    "$GHOSTTY_RUN" > "$BAD_DURATION_RUN"
expect_result 2 NOT-RUN "paired duration mismatch" \
    run_case ghostty "$GHOSTTY_IDENTITY" "$BAD_DURATION_RUN" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$GHOSTTY_RSS" "$GHOSTTY_TRACE" "$MANUAL"

SLOW_WORKLOAD="$TEMP_ROOT/slow-workload.tsv"
awk -F '\t' 'BEGIN { OFS = "\t" }
    $3 == "input-ack-written" && !changed { $2 += 300000000; changed = 1 }
    { print }
' "$WORKLOAD_EVENTS" > "$SLOW_WORKLOAD"
chmod 0444 "$SLOW_WORKLOAD"
SLOW_WORKLOAD_METADATA="$TEMP_ROOT/slow-workload-metadata.tsv"
SLOW_READY_RECEIPT="$TEMP_ROOT/slow-ready-receipt.tsv"
SLOW_PLAN_START_GATE="$TEMP_ROOT/slow-plan-start-gate.tsv"
write_ready_receipt "$SLOW_WORKLOAD" "$GHOSTTY_IDENTITY" "$SLOW_READY_RECEIPT"
publish_plan_start_gate "$SLOW_WORKLOAD" "$SLOW_PLAN_START_GATE" \
    "$CAMPAIGN_SECRET_FILE" "$SLOW_READY_RECEIPT" "$BASE_CONTINUOUS_NS"
resign_workload_metadata "$WORKLOAD_METADATA" "$SLOW_WORKLOAD" \
    "$SLOW_WORKLOAD_METADATA" "$SLOW_READY_RECEIPT"
SLOW_RSS="$TEMP_ROOT/slow-rss.tsv"
write_sustained_rss "$GHOSTTY_IDENTITY" "$SLOW_WORKLOAD_METADATA" \
    "$SLOW_WORKLOAD" "$DRIVER_EVENTS" "$SLOW_RSS" 0 \
    "$SLOW_READY_RECEIPT" "$SLOW_PLAN_START_GATE"
READY_RECEIPT_OVERRIDE="$SLOW_READY_RECEIPT" \
PLAN_START_GATE_OVERRIDE="$SLOW_PLAN_START_GATE" \
expect_result 1 FAIL "input acknowledgement over 250 ms" \
    run_case_with_metadata "$SLOW_WORKLOAD_METADATA" \
        ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$SLOW_WORKLOAD" "$DRIVER_EVENTS" "$SLOW_RSS" "$GHOSTTY_TRACE" "$MANUAL"

MISSING_END="$TEMP_ROOT/missing-producer-end.tsv"
sed '$d' "$WORKLOAD_EVENTS" > "$MISSING_END"
chmod 0444 "$MISSING_END"
MISSING_END_METADATA="$TEMP_ROOT/missing-end-metadata.tsv"
resign_workload_metadata "$WORKLOAD_METADATA" "$MISSING_END" \
    "$MISSING_END_METADATA"
MISSING_END_RSS="$TEMP_ROOT/missing-end-rss.tsv"
write_sustained_rss "$GHOSTTY_IDENTITY" "$MISSING_END_METADATA" \
    "$MISSING_END" "$DRIVER_EVENTS" "$MISSING_END_RSS"
expect_result 2 NOT-RUN "missing producer end" \
    run_case_with_metadata "$MISSING_END_METADATA" \
        ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$MISSING_END" "$DRIVER_EVENTS" "$MISSING_END_RSS" "$GHOSTTY_TRACE" "$MANUAL"

BAD_DRIVER="$TEMP_ROOT/bad-driver.tsv"
awk -F '\t' 'BEGIN { OFS = "\t" } NR == 3 { $3 = "duplicate-event" } { print }' \
    "$DRIVER_EVENTS" > "$BAD_DRIVER"
BAD_DRIVER_RSS="$TEMP_ROOT/bad-driver-rss.tsv"
write_sustained_rss "$GHOSTTY_IDENTITY" "$WORKLOAD_METADATA" \
    "$WORKLOAD_EVENTS" "$BAD_DRIVER" "$BAD_DRIVER_RSS"
expect_result 2 NOT-RUN "driver/plan event mismatch" \
    run_case ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$WORKLOAD_EVENTS" "$BAD_DRIVER" "$BAD_DRIVER_RSS" "$GHOSTTY_TRACE" "$MANUAL"

NONMONOTONIC_DRIVER="$TEMP_ROOT/nonmonotonic-driver.tsv"
awk -F '\t' 'BEGIN { OFS = "\t" } NR == 4 { $2 = 1 } { print }' \
    "$DRIVER_EVENTS" > "$NONMONOTONIC_DRIVER"
NONMONOTONIC_RSS="$TEMP_ROOT/nonmonotonic-driver-rss.tsv"
write_sustained_rss "$GHOSTTY_IDENTITY" "$WORKLOAD_METADATA" "$WORKLOAD_EVENTS" \
    "$NONMONOTONIC_DRIVER" "$NONMONOTONIC_RSS"
expect_result 2 NOT-RUN "nonmonotonic driver event time" \
    run_case ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$WORKLOAD_EVENTS" "$NONMONOTONIC_DRIVER" "$NONMONOTONIC_RSS" \
        "$GHOSTTY_TRACE" "$MANUAL"

BAD_TRACE="$TEMP_ROOT/bad-trace.tsv"
write_trace_metadata "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
    "$WORKLOAD_METADATA" "$BAD_TRACE" false
expect_result 2 NOT-RUN "trace without exact target binding" \
    run_case ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$GHOSTTY_RSS" "$BAD_TRACE" "$MANUAL"

TRACE_WITHOUT_DURATION="$TEMP_ROOT/trace-without-duration.tsv"
sed '/^actual_duration_ms\t/d' "$GHOSTTY_TRACE" > "$TRACE_WITHOUT_DURATION"
expect_result 2 NOT-RUN "trace schema without duration" \
    run_case ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$GHOSTTY_RSS" \
        "$TRACE_WITHOUT_DURATION" "$MANUAL"

TRACE_WITHOUT_INCOMPLETE_REASON="$TEMP_ROOT/trace-without-incomplete-reason.tsv"
sed '/^incomplete_reason\t/d' "$GHOSTTY_TRACE" > "$TRACE_WITHOUT_INCOMPLETE_REASON"
expect_result 2 NOT-RUN "trace schema missing incomplete reason" \
    run_case ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$GHOSTTY_RSS" \
        "$TRACE_WITHOUT_INCOMPLETE_REASON" "$MANUAL"

TRACE_WITH_INCOMPLETE_REASON="$TEMP_ROOT/trace-with-incomplete-reason.tsv"
sed 's/^incomplete_reason\tnone$/incomplete_reason\tworkload-ended-early/' \
    "$GHOSTTY_TRACE" > "$TRACE_WITH_INCOMPLETE_REASON"
expect_result 2 NOT-RUN "captured trace reports incomplete reason" \
    run_case ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$GHOSTTY_RSS" \
        "$TRACE_WITH_INCOMPLETE_REASON" "$MANUAL"

TRACE_WITHOUT_READY_HASH="$TEMP_ROOT/trace-without-ready-hash.tsv"
sed '/^workload_ready_receipt_sha256\t/d' "$GHOSTTY_TRACE" > "$TRACE_WITHOUT_READY_HASH"
expect_result 2 NOT-RUN "trace schema missing readiness hash" \
    run_case ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$GHOSTTY_RSS" \
        "$TRACE_WITHOUT_READY_HASH" "$MANUAL"

MISMATCHED_GATE_TRACE="$TEMP_ROOT/mismatched-gate-trace.tsv"
sed "s/^supplemental_evidence_sha256.*/supplemental_evidence_sha256\t$HASH_C/" \
    "$GHOSTTY_TRACE" > "$MISMATCHED_GATE_TRACE"
expect_result 2 NOT-RUN "trace signed gate hash mismatch" \
    run_case ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$GHOSTTY_RSS" \
        "$MISMATCHED_GATE_TRACE" "$MANUAL"

REUSED_TRACE="$TEMP_ROOT/reused-trace.tsv"
cp "$GHOSTTY_TRACE" "$REUSED_TRACE"
chmod u+w "$REUSED_TRACE"
sed -i '' "s/run_metadata_sha256.*/run_metadata_sha256\t$HASH_C/" "$REUSED_TRACE"
expect_result 2 NOT-RUN "trace reused from another run" \
    run_case ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$GHOSTTY_RSS" \
        "$REUSED_TRACE" "$MANUAL"

SKEWED_TRACE="$TEMP_ROOT/skewed-trace.tsv"
write_trace_metadata "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
    "$WORKLOAD_METADATA" "$SKEWED_TRACE" true 3000000000 3000000000
expect_result 2 NOT-RUN "trace starts after workload window" \
    run_case ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$GHOSTTY_RSS" \
        "$SKEWED_TRACE" "$MANUAL"

UNREVIEWED="$TEMP_ROOT/unreviewed.tsv"
write_manual_artifacts "$UNREVIEWED" NOT-REVIEWED
expect_result 2 NOT-RUN "automated success without manual artifacts" \
    run_case ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$GHOSTTY_RSS" "$GHOSTTY_TRACE" "$UNREVIEWED"

SHIFTED_RSS="$TEMP_ROOT/shifted-rss.tsv"
write_sustained_rss "$GHOSTTY_IDENTITY" "$WORKLOAD_METADATA" \
    "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" \
    "$SHIFTED_RSS" 300000
expect_result 1 FAIL "equal RSS range shifted upward" \
    awk -f "$SCRIPT_DIRECTORY/analyze-release-performance-sustained.awk" "$SHIFTED_RSS"

UNKNOWN_RSS="$TEMP_ROOT/unknown-rss.tsv"
awk '1; /^# scenario/ { print "# invented_field\t1" }' "$GHOSTTY_RSS" > "$UNKNOWN_RSS"
expect_result 2 NOT-RUN "unknown RSS metadata" \
    awk -f "$SCRIPT_DIRECTORY/analyze-release-performance-sustained.awk" "$UNKNOWN_RSS"

# Static guards cover SpaceTerm-only false passes; complete authenticated
# runtime fixtures live with the production observation seam that owns them.
for required_guard in \
    'runtime-backlog-bound-exceeded' \
    'runtime-observer-does-not-cover-workload' \
    'runtime-produced-no-superseded-screen-evidence' \
    'stale-generation-presented-after-restore' \
    'hidden-state-or-no-frame-proof-failed' \
    'final-screen-was-not-presented-before-exit' \
    'runtime-pty-geometry-does-not-match-producer-tiocgwinsz' \
    'runtime-does-not-prove-10000-retained-rows'; do
    grep -Fq "$required_guard" "$SCRIPT_DIRECTORY/analyze-release-performance-case.sh" \
        || fail "missing analyzer guard: $required_guard"
done

NO_RESIZE_VARIANCE="$TEMP_ROOT/no-resize-variance.tsv"
{
    printf 'elapsed_ms\tcontinuous_ns\trss_kib\tworkload_bytes\tresize_count\n'
    printf '# format_version\t4\n# scenario\tresize\n# sample_interval_ms\t10000\n'
    printf '# requested_duration_ms\t300000\n'
    printf '# plan_start_continuous_ns\t%d\n' "$BASE_CONTINUOUS_NS"
    printf '# measurement_start_continuous_ns\t%d\n' "$BASE_CONTINUOUS_NS"
    printf '# plan_start_gate_sha256\t%s\n' "$HASH_C"
    printf '# subject_identity_sha256\t%s\n' "$HASH_A"
    printf '# workload_events_sha256\t%s\n' "$HASH_B"
    printf '# workload_metadata_sha256\t%s\n' "$HASH_A"
    printf '# ready_receipt_sha256\t%s\n' "$HASH_B"
    printf '# workload_authentication\thmac-sha256\n'
    printf '# progress_interval_ms\t1000\n# maximum_progress_age_ms\t2000\n'
    printf '# driver_events_sha256\t%s\n' "$HASH_C"
    printf '# distinct_geometry_count\t1\n# geometry_change_count\t300\n'
    printf '# completed_resize_cycles\t300\n# geometry_correlated\ttrue\n'
    for ((index = 0; index <= 30; index += 1)); do
        printf '%d\t%d\t100000\t%d\t0\n' "$((index * 10000))" \
            "$((BASE_CONTINUOUS_NS + index * 10000000000))" "$((index * 1000000))"
    done
    printf '# status\tcomplete\n'
} > "$NO_RESIZE_VARIANCE"
expect_result 2 NOT-RUN "resize count with constant geometry" \
    awk -f "$SCRIPT_DIRECTORY/analyze-release-performance-resize.awk" "$NO_RESIZE_VARIANCE"

echo "release performance campaign fixtures passed"
