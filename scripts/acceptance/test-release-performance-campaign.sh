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
readonly SPACETERM_SESSION_ID="session-43-spaceterm"
readonly SPACETERM_NONCE="eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
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
    "$@" > "$output" || actual_exit=$?
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

trace_tree_sha256() {
    python3 - "$1" <<'PY'
import hashlib, pathlib, struct, sys, unicodedata
root = pathlib.Path(sys.argv[1])
if not root.is_dir() or root.is_symlink(): raise SystemExit(1)
digest = hashlib.sha256(b"spaceterm.performance.trace-tree/v1\0")
entries = []
for path in root.rglob("*"):
    if path.is_symlink() or (path.exists() and not path.is_file() and not path.is_dir()):
        raise SystemExit(1)
    if path.is_file():
        relative = unicodedata.normalize("NFC", path.relative_to(root).as_posix())
        if relative != path.relative_to(root).as_posix(): raise SystemExit(1)
        entries.append((relative.encode(), path))
for encoded, path in sorted(entries):
    data = path.read_bytes()
    digest.update(struct.pack(">Q", len(encoded))); digest.update(encoded)
    digest.update(struct.pack(">Q", len(data))); digest.update(data)
print(digest.hexdigest())
PY
}

freeze_case_report() {
    local output="$1" label="$2"
    shift 2
    local temporary="$output.tmp" actual_exit=0
    "$@" > "$temporary" || actual_exit=$?
    if [[ "$actual_exit" != 0 \
        || "$(wc -l < "$temporary" | tr -d ' ')" != 14 \
        || "$(awk -F '\t' '$1 == "format_version" {print $2}' "$temporary")" != 2 \
        || "$(awk -F '\t' '$1 == "result" {print $2}' "$temporary")" != CASE-COMPLETE ]]; then
        sed 's/^/  /' "$temporary" >&2
        fail "$label did not produce an exact14 v2 CASE-COMPLETE report"
    fi
    chmod 0400 "$temporary"
    mv -- "$temporary" "$output"
}

publish_plan_start_gate() {
    local events="$1"
    local gate="$2"
    local secret="${3:-$CAMPAIGN_SECRET_FILE}"
    local ready_receipt="${4:?ready receipt path is required}"
    local start_override="${5:-}"
    local session="${6:-$SESSION_ID}" nonce="${7:-$NONCE}"
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
    python3 - "$gate" "$secret" "$start" "$CAMPAIGN_ID" "$session" "$nonce" \
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
    local session="${4:-$SESSION_ID}" nonce="${5:-$NONCE}"
    python3 - "$events" "$identity" "$CAMPAIGN_SECRET_FILE" "$output" \
        "$CAMPAIGN_ID" "$session" "$nonce" <<'PY'
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
    chmod 0400 "$output"
}

write_window_identity() {
    local subject="$1" identity="$2" output="$3"
    {
        printf 'format_version\t1\n'
        printf 'subject_identity_sha256\t%s\n' "$(sha256 "$identity")"
        printf 'subject\t%s\n' "$subject"
        printf 'process_pid\t123\n'
        printf 'process_start_identity\t1786473000:123456\n'
        printf 'bundle_identifier\tcom.example.%s\n' "$subject"
        printf 'executable_sha256\t%s\n' "$HASH_A"
        printf 'window_number\t44\n'
        printf 'window_owner_pid_verified\ttrue\n'
        printf 'window_layer\t0\n'
        printf 'window_onscreen\ttrue\n'
        printf 'window_minimized\tfalse\n'
        printf 'window_x\t0\n'
        printf 'window_y\t0\n'
        printf 'window_width\t1000\n'
        printf 'window_height\t800\n'
        printf 'resolved_continuous_ns\t%d\n' "$BASE_CONTINUOUS_NS"
        printf 'selector_kind\tfrontmost-normal-window\n'
        printf 'status\tfrozen\n'
    } > "$output"
    chmod 0400 "$output"
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
    local session="${6:-$SESSION_ID}" nonce="${7:-$NONCE}"
    local unsigned="$TEMP_ROOT/workload-metadata-unsigned.$$"
    {
        printf 'format_version\t3\n'
        printf 'scenario\tascii\n'
        printf 'campaign_id\t%s\n' "$CAMPAIGN_ID"
        printf 'session_id\t%s\n' "$session"
        printf 'nonce\t%s\n' "$nonce"
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
    chmod 0400 "$output"
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
    chmod 0400 "$output"
}

write_native_provisional() {
    local identity="$1"
    local output="$2"
    local nonce="${3:-$NONCE}"
    {
        printf 'schema\tspaceterm.acceptance.native-launch-proof/v5\n'
        printf 'observation.source\tproduction-app\n'
        printf 'launch.nonce\t%s\n' "$nonce"
        printf 'run.id\t%s\n' "$CAMPAIGN_ID"
        printf 'package.app.sha256\t%s\n' "$HASH_C"
        printf 'runtime.schema\tspaceterm.acceptance.runtime-stream/v1\n'
        printf 'runtime.sample_interval_ms\t1000\n'
        printf 'runtime.transition_capacity\t64\n'
        printf 'failure.action.schema\tspaceterm.acceptance.failure-action/v1\n'
        printf 'failure.action.enabled\tfalse\n'
        printf 'process.pid\t123\n'
        printf 'process.pidversion\t5\n'
        printf 'process.executable.path\t%s\n' \
            "$(awk -F '\t' '$1 == "executable_path" { print $2 }' "$identity")"
        printf 'process.executable.device\t1\n'
        printf 'process.executable.inode\t2\n'
        printf 'process.executable.fsid\t1:1\n'
        printf 'process.signature.cdhash\tabcd1234\n'
        printf 'process.signature.identifier\tcom.example.spaceterm\n'
        printf 'process.signature.team_identifier\t\n'
        printf 'terminal_font_selected\tMenlo 12\n'
        printf 'initial_grid.rows\t40\n'
        printf 'initial_grid.columns\t100\n'
        printf 'initial_grid.logical_width\t1000\n'
        printf 'initial_grid.logical_height\t800\n'
        printf 'initial_grid.backing_pixel_width\t2000\n'
        printf 'initial_grid.backing_pixel_height\t1600\n'
        printf 'observation.complete\ttrue\n'
    } > "$output"
    chmod 0400 "$output"
}

write_native_final() {
    local provisional="$1"
    local runtime_metadata="$2"
    local failure_actions="$3"
    local output="$4"
    sed '$d' "$provisional" > "$output"
    {
        printf 'provisional.observation.sha256\t%s\n' "$(sha256 "$provisional")"
        printf 'runtime.metadata.schema\tspaceterm.acceptance.runtime-observation-metadata/v3\n'
        printf 'runtime.metadata.path\truntime-metadata.tsv\n'
        printf 'runtime.metadata.sha256\t%s\n' "$(sha256 "$runtime_metadata")"
        printf 'failure.result.schema\tspaceterm.acceptance.failure-action-result/v2\n'
        printf 'failure.actions.path\tfailure-actions.tsv\n'
        printf 'failure.actions.sha256\t%s\n' "$(sha256 "$failure_actions")"
        printf 'failure.request_count\t0\n'
        printf 'failure.result_count\t0\n'
        printf 'observation.complete\ttrue\n'
    } >> "$output"
    chmod 0400 "$output"
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
            sample_ns="$((start + index * 1000000000))"
            (( index < 600 )) || sample_ns="$end"
            printf '%d\t%d\t%d\t%d\t%d\t%d\t0\t2\t%d\t%d\t2\t%d\t%d\t%d\t%d\t1\t0\t0\t1\t1\t0\t500\t40\t0\t0\t0\t0\t0\t0\t40\t100\t1000\t800\t%d\t%s\t%d\n' \
                "$index" "$sample_ns" \
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
        printf '%s\n' $'request_id\tsequence\tcase_id\taction\tresult\tpane_id\tpane_state\tfailure_class\tfailure_recoverability\tfailure_operation\tstate_revision\tlatest_generation\tlast_valid_generation\tvisible_generation\tpending_recovery\tterminal_input_usable\tsession_attached\tresource_staged_count\tresource_staged_bytes\tresource_rolled_back_count\tresource_rolled_back_bytes'
    } > "$failure_actions"
    {
        printf 'schema\tspaceterm.acceptance.runtime-observation-metadata/v3\n'
        printf 'observation.source\tproduction-app\n'
        printf 'run.id\t%s\n' "$CAMPAIGN_ID"
        printf 'package.app.sha256\t%s\n' "$HASH_C"
        printf 'process.pid\t123\n'
        printf 'runtime.samples.path\truntime-samples.tsv\n'
        printf 'runtime.samples.sha256\t%s\n' "$(sha256 "$samples")"
        printf 'runtime.events.path\truntime-events.tsv\n'
        printf 'runtime.events.sha256\t%s\n' "$(sha256 "$events")"
        printf 'failure.action.schema\tspaceterm.acceptance.failure-action/v1\n'
        printf 'failure.action.enabled\tfalse\n'
        printf 'failure.result.schema\tspaceterm.acceptance.failure-action-result/v2\n'
        printf 'failure.actions.path\tfailure-actions.tsv\n'
        printf 'failure.actions.sha256\t%s\n' "$(sha256 "$failure_actions")"
        printf 'failure.request_count\t0\n'
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
    chmod 0400 "$samples" "$events" "$metadata" "$failure_actions"
}

freeze_run_intent() {
    local subject="$1" identity="$2" output="$3"
    local provisional="${4:-}"
    local session="${5:-$SESSION_ID}" nonce="${6:-$NONCE}"
    local -a arguments=(
        --subject "$subject"
        --pair-metadata "$PAIR_METADATA"
        --subject-identity "$identity"
        --plan "$PLAN"
        --workload-binary "$WORKLOAD_BINARY"
        --command-manifest "$TEMP_ROOT/command.tsv"
        --environment-manifest "$TEMP_ROOT/environment.tsv"
        --font-manifest "$TEMP_ROOT/font.tsv"
        --initial-grid-manifest "$TEMP_ROOT/initial-grid.tsv"
        --campaign-id "$CAMPAIGN_ID"
        --session-id "$session"
        --nonce "$nonce"
        --output "$output"
    )
    [[ -z "$provisional" ]] \
        || arguments+=(--native-provisional-observation "$provisional")
    "$SCRIPT_DIRECTORY/freeze-performance-run-intent.sh" "${arguments[@]}" >/dev/null
}

write_trace_provisional() {
    local identity="$1" intent="$2" workload_metadata="$3"
    local ready_receipt="$4" plan_start_gate="$5" output="$6" trace_archive="$7"
    python3 - "$identity" "$intent" "$workload_metadata" "$ready_receipt" \
        "$plan_start_gate" "$CAMPAIGN_SECRET_FILE" "$output" \
        "$BASE_CONTINUOUS_NS" "$trace_archive" <<'PY'
import hashlib
import hmac
import pathlib
import struct
import sys

identity, intent, workload, ready, gate, secret, output = map(pathlib.Path, sys.argv[1:8])
base = int(sys.argv[8])
trace_archive = pathlib.Path(sys.argv[9])
digest = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
tree = hashlib.sha256(b"spaceterm.performance.trace-tree/v1\0")
entries = []
for path in trace_archive.rglob("*"):
    if path.is_file():
        entries.append((path.relative_to(trace_archive).as_posix().encode(), path))
for encoded, path in sorted(entries):
    data = path.read_bytes()
    tree.update(struct.pack(">Q", len(encoded))); tree.update(encoded)
    tree.update(struct.pack(">Q", len(data))); tree.update(data)
rows = [
    ("format_version", "1"),
    ("subject_identity_sha256", digest(identity)),
    ("run_intent_sha256", digest(intent)),
    ("workload_metadata_sha256", digest(workload)),
    ("workload_ready_receipt_sha256", digest(ready)),
    ("supplemental_evidence_sha256", digest(gate)),
    ("capture_status", "CAPTURED"),
    ("requested_duration_ms", "600000"),
    ("actual_duration_ms", "600001"),
    ("capture_started_continuous_ns", str(base + 60_000_000_000)),
    ("capture_ended_continuous_ns", str(base + 660_100_000_000)),
    ("trace_bundle_tree_sha256", tree.hexdigest()),
    ("toc_sha256", "b" * 64),
    ("time_profile_export_sha256", "c" * 64),
    ("allocations_export_sha256", "d" * 64),
    ("hangs_export_sha256", "e" * 64),
    ("trace_verification_sha256", "f" * 64),
    ("verifier_sha256", "1" * 64),
    ("evidence_mode", "production"),
    ("status", "complete"),
    ("auth_algorithm", "hmac-sha256"),
]
unsigned = b"".join(f"{key}\t{value}\n".encode() for key, value in rows)
payload = b"spaceterm.performance.trace-provisional/v1\0" + struct.pack(">Q", len(unsigned)) + unsigned
signature = hmac.new(secret.read_bytes(), payload, hashlib.sha256).hexdigest()
output.write_bytes(unsigned + f"provisional_hmac_sha256\t{signature}\n".encode())
output.chmod(0o400)
PY
}

write_normal_exit_closure() {
    local subject="$1" identity="$2" intent="$3" tail="$4" quit="$5"
    local exit_receipt="$6" native_observation="${7:-}"
    local terminator_source="$8" terminator_binary="$9" lifecycle_ready="${10}"
    local quit_token tail_ns request_ns exit_ns
    local session nonce
    session="$(awk -F '\t' '$1 == "session_id" {print $2}' "$intent")"
    nonce="$(awk -F '\t' '$1 == "nonce" {print $2}' "$intent")"
    quit_token="$(awk -F '\t' '$1 == "quit_token" { print $2 }' "$tail")"
    tail_ns="$(awk -F '\t' '$1 == "tail_completed_continuous_ns" { print $2 }' "$tail")"
    request_ns="$((tail_ns + 100))"
    exit_ns="$((request_ns + 100))"
    {
        printf 'format_version\t1\n'
        printf 'campaign_id\t%s\n' "$CAMPAIGN_ID"
        printf 'session_id\t%s\n' "$session"
        printf 'nonce\t%s\n' "$nonce"
        printf 'run_intent_sha256\t%s\n' "$(sha256 "$intent")"
        printf 'subject_process_pid\t123\n'
        printf 'subject_process_start_identity\t1786473000:123456\n'
        printf 'quit_token\t%s\n' "$quit_token"
        printf 'request_continuous_ns\t%s\n' "$request_ns"
        printf 'exit_continuous_ns\t%s\n' "$exit_ns"
        printf 'termination_method\tappkit-terminate\n'
        printf 'runtime_closure_status\tconfirmed\n'
        for key in lifecycle_helper_device lifecycle_helper_inode lifecycle_helper_sha256 \
            process_inspector_device process_inspector_inode process_inspector_sha256 \
            appkit_terminator_process_pid appkit_terminator_process_start_identity; do
            printf '%s\t%s\n' "$key" "$(awk -F '\t' -v wanted="$key" '$1 == wanted {print $2}' "$lifecycle_ready")"
        done
        printf 'appkit_terminator_source_device\t%s\n' "$(stat -f '%d' "$terminator_source")"
        printf 'appkit_terminator_source_inode\t%s\n' "$(stat -f '%i' "$terminator_source")"
        printf 'appkit_terminator_source_sha256\t%s\n' "$(sha256 "$terminator_source")"
        printf 'appkit_terminator_binary_device\t%s\n' "$(stat -f '%d' "$terminator_binary")"
        printf 'appkit_terminator_binary_inode\t%s\n' "$(stat -f '%i' "$terminator_binary")"
        printf 'appkit_terminator_binary_sha256\t%s\n' "$(sha256 "$terminator_binary")"
        printf 'evidence_mode\tproduction\n'
        printf 'status\tcompleted\n'
    } > "$quit"
    chmod 0400 "$quit"
    python3 - "$subject" "$identity" "$intent" "$tail" "$quit" "$exit_receipt" \
        "$CAMPAIGN_SECRET_FILE" "$request_ns" "$exit_ns" "$native_observation" \
        "$terminator_source" "$terminator_binary" "$lifecycle_ready" <<'PY'
import hashlib
import hmac
import pathlib
import struct
import sys

subject, identity_name, intent_name, tail_name, quit_name, output_name, secret_name, requested, exited, native_name, source_name, binary_name, ready_name = sys.argv[1:]
identity = pathlib.Path(identity_name)
intent = pathlib.Path(intent_name)
tail = pathlib.Path(tail_name)
quit_receipt = pathlib.Path(quit_name)
output = pathlib.Path(output_name)
secret = pathlib.Path(secret_name)
digest = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
intent_values = dict(line.split("\t", 1) for line in intent.read_text().splitlines())
ready_values = dict(line.split("\t", 1) for line in pathlib.Path(ready_name).read_text().splitlines())
native_hash = digest(pathlib.Path(native_name)) if subject == "spaceterm" else "not-applicable"
rows = [
    ("schema", "spaceterm.acceptance.performance-subject-exit/v1"),
    ("subject", subject),
    ("campaign_id", intent_values["campaign_id"]),
    ("session_id", intent_values["session_id"]),
    ("nonce", intent_values["nonce"]),
    ("run_intent_sha256", digest(intent)),
    ("subject_identity_sha256", digest(identity)),
    ("process_pid", intent_values["process_pid"]),
    ("process_start_identity", intent_values["process_start_identity"]),
    ("tail_receipt_sha256", digest(tail)),
    ("quit_receipt_sha256", digest(quit_receipt)),
    ("exit_requested_continuous_ns", requested),
    ("process_exited_continuous_ns", exited),
    ("exit_status", "normal"),
    ("native_observation_sha256", native_hash),
    *((key, ready_values[key]) for key in (
        "lifecycle_helper_device", "lifecycle_helper_inode", "lifecycle_helper_sha256",
        "process_inspector_device", "process_inspector_inode", "process_inspector_sha256",
        "appkit_terminator_process_pid", "appkit_terminator_process_start_identity")),
    ("appkit_terminator_source_device", str(pathlib.Path(source_name).stat().st_dev)),
    ("appkit_terminator_source_inode", str(pathlib.Path(source_name).stat().st_ino)),
    ("appkit_terminator_source_sha256", digest(pathlib.Path(source_name))),
    ("appkit_terminator_binary_device", str(pathlib.Path(binary_name).stat().st_dev)),
    ("appkit_terminator_binary_inode", str(pathlib.Path(binary_name).stat().st_ino)),
    ("appkit_terminator_binary_sha256", digest(pathlib.Path(binary_name))),
    ("evidence_mode", "production"),
    ("auth_algorithm", "hmac-sha256"),
]
status = ("status", "complete")
unsigned = b"".join(f"{key}\t{value}\n".encode() for key, value in rows + [status])
payload = b"spaceterm.acceptance.performance-subject-exit/v1\0" + struct.pack(">Q", len(unsigned)) + unsigned
signature = hmac.new(secret.read_bytes(), payload, hashlib.sha256).hexdigest()
output.write_bytes(
    b"".join(f"{key}\t{value}\n".encode() for key, value in rows)
    + f"receipt_hmac_sha256\t{signature}\nstatus\tcomplete\n".encode()
)
output.chmod(0o400)
PY
}

write_lifecycle_receipts() {
    local subject="$1" identity="$2" intent="$3" tail="$4" workload="$5" events="$6"
    local ready="$7" quit="$8" exit_receipt="$9" native="${10}"
    local source="${11}" binary="${12}" token="${13}" ready_output="${14}" registration_output="${15}"
    local helper="${16}"
    local inspector="$SCRIPT_DIRECTORY/../inspect-release-performance-process.py"
    python3 - "$subject" "$identity" "$intent" "$tail" "$workload" "$events" "$ready" \
        "$quit" "$exit_receipt" "$native" "$source" "$binary" "$helper" "$inspector" "$token" \
        "$ready_output" "$registration_output" "$CAMPAIGN_SECRET_FILE" <<'PY'
import hashlib,hmac,pathlib,struct,sys
(subject,identity_name,intent_name,tail_name,workload_name,events_name,ready_name,
 quit_name,exit_name,native_name,source_name,binary_name,helper_name,inspector_name,token,ready_output,
 registration_output,secret_name)=sys.argv[1:]
paths=[pathlib.Path(value) for value in (identity_name,intent_name,tail_name,workload_name,
 events_name,ready_name,quit_name,exit_name,source_name,binary_name,helper_name,inspector_name)]
identity,intent,tail,workload,events,ready,quit_receipt,exit_receipt,source,binary,helper,inspector=paths
secret=pathlib.Path(secret_name).read_bytes(); digest=lambda path:hashlib.sha256(path.read_bytes()).hexdigest()
values=dict(line.split("\t",1) for line in identity.read_text().splitlines())
intent_values=dict(line.split("\t",1) for line in intent.read_text().splitlines())
tool=[("lifecycle_helper_device",str(helper.stat().st_dev)),
 ("lifecycle_helper_inode",str(helper.stat().st_ino)),
 ("lifecycle_helper_sha256",digest(helper)),
 ("process_inspector_device",str(inspector.stat().st_dev)),
 ("process_inspector_inode",str(inspector.stat().st_ino)),
 ("process_inspector_sha256",digest(inspector)),
 ("appkit_terminator_process_pid","99"),
 ("appkit_terminator_process_start_identity","10:20"),
 ("appkit_terminator_source_device",str(source.stat().st_dev)),
 ("appkit_terminator_source_inode",str(source.stat().st_ino)),
 ("appkit_terminator_source_sha256",digest(source)),
 ("appkit_terminator_binary_device",str(binary.stat().st_dev)),
 ("appkit_terminator_binary_inode",str(binary.stat().st_ino)),
 ("appkit_terminator_binary_sha256",digest(binary))]
def signed(rows,field,magic):
 unsigned=b"".join(f"{k}\t{v}\n".encode() for k,v in rows)
 signature=hmac.new(secret,magic+struct.pack(">Q",len(unsigned))+unsigned,hashlib.sha256).hexdigest()
 return b"".join(f"{k}\t{v}\n".encode() for k,v in rows[:-1])+f"{field}\t{signature}\n".encode()+f"{rows[-1][0]}\t{rows[-1][1]}\n".encode()
ready_rows=[("schema","spaceterm.acceptance.performance-lifecycle-ready/v1"),
 ("subject",subject),("campaign_id",intent_values["campaign_id"]),
 ("session_id",intent_values["session_id"]),("nonce",intent_values["nonce"]),
 ("subject_identity_sha256",digest(identity)),
 ("process_pid",values["process_pid"]),("process_start_identity",values["process_start_identity"]),
 ("executable_sha256",values["executable_sha256"]),("ready_continuous_ns","1"),
 ("registration_control_device","1"),("registration_control_inode","2")]+tool+[
 ("evidence_mode","production"),("auth_algorithm","hmac-sha256"),("status","ready")]
pathlib.Path(ready_output).write_bytes(signed(ready_rows,"receipt_hmac_sha256",b"spaceterm.acceptance.performance-lifecycle-ready/v1\0"))
native="not-applicable" if subject=="ghostty" else str(pathlib.Path(native_name).resolve())
registration=[("format_version","1"),("campaign_id",intent_values["campaign_id"]),
 ("session_id",intent_values["session_id"]),("nonce",intent_values["nonce"]),
 ("registration_token",token),("subject_identity_sha256",digest(identity)),
 ("process_pid",values["process_pid"]),("process_start_identity",values["process_start_identity"]),
 ("run_intent_path",str(intent.resolve())),("run_intent_sha256",digest(intent)),
 ("tail_receipt_path",str(tail.resolve())),("workload_metadata_path",str(workload.resolve())),
 ("workload_events_path",str(events.resolve())),("workload_ready_receipt_path",str(ready.resolve())),
 ("quit_receipt_path",str(quit_receipt.resolve())),("subject_exit_receipt_path",str(exit_receipt.resolve())),
 ("native_observation_path",native)]+tool+[("evidence_mode","production"),
 ("auth_algorithm","hmac-sha256"),("status","registered")]
pathlib.Path(registration_output).write_bytes(signed(registration,"registration_hmac_sha256",b"spaceterm.acceptance.performance-lifecycle-registration/v1\0"))
PY
    chmod 0400 "$ready_output" "$registration_output"
}

build_causal_closure() {
    local prefix="$1" subject="$2" identity="$3" intent="$4"
    local workload_metadata="$5" workload_events="$6" ready_receipt="$7"
    local driver_events="$8" rss_samples="$9" plan_start_gate="${10}"
    local native_provisional="${11:-}" native_observation="${12:-}"
    local runtime_metadata="${13:-}" runtime_samples="${14:-}"
    local runtime_events="${15:-}" failure_actions="${16:-}"
    local driver_intent="$TEMP_ROOT/$prefix-driver-intent.tsv"
    local driver_receipt="$TEMP_ROOT/$prefix-driver-receipt.tsv"
    local window_identity="$TEMP_ROOT/$prefix-window-identity.tsv"
    local driver_binary="$TEMP_ROOT/$prefix-performance-driver"
    local trace_provisional="$TEMP_ROOT/$prefix-trace-provisional.tsv"
    local tail_receipt="$TEMP_ROOT/$prefix-tail.tsv"
    local quit_receipt="$TEMP_ROOT/$prefix-quit.tsv"
    local exit_receipt="$TEMP_ROOT/$prefix-exit.tsv"
    local lifecycle_ready="$TEMP_ROOT/$prefix-lifecycle-ready.tsv"
    local lifecycle_registration="$TEMP_ROOT/$prefix-lifecycle-registration.tsv"
    local lifecycle_helper="$TEMP_ROOT/$prefix-lifecycle-helper.py"
    local run_metadata="$TEMP_ROOT/$prefix-run.tsv"
    local trace_metadata="$TEMP_ROOT/$prefix-trace.tsv"
    local trace_archive="$TEMP_ROOT/$subject-ascii.trace"
    local run_session run_nonce
    run_session="$(awk -F '\t' '$1 == "session_id" {print $2}' "$intent")"
    run_nonce="$(awk -F '\t' '$1 == "nonce" {print $2}' "$intent")"
    cp -- "$SCRIPT_DIRECTORY/performance-subject-lifecycle.py" "$lifecycle_helper"
    chmod 0500 "$lifecycle_helper"
    printf 'fixture native driver binary: %s\n' "$prefix" > "$driver_binary"
    chmod 0500 "$driver_binary"
    write_window_identity "$subject" "$identity" "$window_identity"
    local held_driver_events="$TEMP_ROOT/$prefix-driver-events-held.tsv"
    mv -- "$driver_events" "$held_driver_events"
    "$SCRIPT_DIRECTORY/performance-driver-receipt.py" intent \
        --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
        --campaign-id "$CAMPAIGN_ID" --session-id "$run_session" --nonce "$run_nonce" \
        --driver-output "$driver_events" --driver-binary "$driver_binary" \
        --driver-source "$SCRIPT_DIRECTORY/performance-driver.m" \
        --controller "$SCRIPT_DIRECTORY/run-native-performance-scenario.sh" \
        --scenario-plan "$PLAN" --plan-start-continuous-ns "$BASE_CONTINUOUS_NS" \
        --subject-identity "$identity" --window-identity "$window_identity" \
        --output "$driver_intent"
    mv -- "$held_driver_events" "$driver_events"
    "$SCRIPT_DIRECTORY/performance-driver-receipt.py" finalize \
        --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
        --campaign-id "$CAMPAIGN_ID" --session-id "$run_session" --nonce "$run_nonce" \
        --driver-output "$driver_events" --driver-binary "$driver_binary" \
        --driver-source "$SCRIPT_DIRECTORY/performance-driver.m" \
        --controller "$SCRIPT_DIRECTORY/run-native-performance-scenario.sh" \
        --scenario-plan "$PLAN" --plan-start-continuous-ns "$BASE_CONTINUOUS_NS" \
        --subject-identity "$identity" --window-identity "$window_identity" \
        --intent "$driver_intent" --receipt-output "$driver_receipt"
    if [[ ! -d "$trace_archive" ]]; then
        mkdir -m 0700 "$trace_archive"
        printf 'fixture trace payload: %s\n' "$subject" > "$trace_archive/payload.bin"
        chmod 0400 "$trace_archive/payload.bin"
    fi
    write_trace_provisional "$identity" "$intent" "$workload_metadata" \
        "$ready_receipt" "$plan_start_gate" "$trace_provisional" "$trace_archive"
    local tail_ns quit_token
    tail_ns="$(( $(awk -F '\t' '$1 == "ended_continuous_ns" { print $2 }' "$workload_metadata") + 5000000000 ))"
    quit_token="$(sha256 "$driver_receipt")"
    local terminator_binary="$TEMP_ROOT/performance-appkit-terminate"
    if [[ ! -e "$terminator_binary" ]]; then
        printf 'fixture AppKit terminator binary\n' > "$terminator_binary"
        chmod 0500 "$terminator_binary"
    fi
    write_lifecycle_receipts "$subject" "$identity" "$intent" "$tail_receipt" \
        "$workload_metadata" "$workload_events" "$ready_receipt" "$quit_receipt" \
        "$exit_receipt" "$native_observation" \
        "$SCRIPT_DIRECTORY/performance-appkit-terminate.m" "$terminator_binary" \
        "$quit_token" "$lifecycle_ready" "$lifecycle_registration" "$lifecycle_helper"
    "$SCRIPT_DIRECTORY/performance-tail-receipt.py" create \
        --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
        --campaign-id "$CAMPAIGN_ID" --session-id "$run_session" --nonce "$run_nonce" \
        --quit-token "$quit_token" --run-intent "$intent" \
        --subject-identity "$identity" --driver-receipt "$driver_receipt" \
        --driver-events "$driver_events" --workload-metadata "$workload_metadata" \
        --workload-events "$workload_events" --workload-ready-receipt "$ready_receipt" \
        --rss-samples "$rss_samples" --trace-provisional-receipt "$trace_provisional" \
        --lifecycle-ready-receipt "$lifecycle_ready" \
        --appkit-terminator-source \
            "$SCRIPT_DIRECTORY/performance-appkit-terminate.m" \
        --appkit-terminator-binary "$terminator_binary" \
        --tail-completed-continuous-ns "$tail_ns" --output "$tail_receipt"
    write_normal_exit_closure "$subject" "$identity" "$intent" "$tail_receipt" \
        "$quit_receipt" "$exit_receipt" "$native_observation" \
        "$SCRIPT_DIRECTORY/performance-appkit-terminate.m" "$terminator_binary" \
        "$lifecycle_ready"
    local -a final_arguments=(
        --run-intent "$intent"
        --subject-identity "$identity"
        --campaign-secret-file "$CAMPAIGN_SECRET_FILE"
        --trace-provisional-receipt "$trace_provisional"
        --performance-tail-receipt "$tail_receipt"
        --performance-quit-receipt "$quit_receipt"
        --subject-exit-receipt "$exit_receipt"
        --driver-intent "$driver_intent"
        --driver-receipt "$driver_receipt"
        --driver-events "$driver_events"
        --window-identity "$window_identity"
        --driver-binary "$driver_binary"
        --driver-source "$SCRIPT_DIRECTORY/performance-driver.m"
        --driver-controller "$SCRIPT_DIRECTORY/run-native-performance-scenario.sh"
        --scenario-plan "$PLAN"
        --plan-start-gate "$plan_start_gate"
        --workload-metadata "$workload_metadata"
        --workload-events "$workload_events"
        --workload-ready-receipt "$ready_receipt"
        --rss-samples "$rss_samples"
        --performance-lifecycle-ready-receipt "$lifecycle_ready"
        --performance-lifecycle-registration "$lifecycle_registration"
        --subject-lifecycle-helper "$lifecycle_helper"
        --common-lifecycle-helper "$SCRIPT_DIRECTORY/performance-subject-lifecycle.py"
        --expected-common-lifecycle-helper-device \
            "$(stat -f '%d' "$SCRIPT_DIRECTORY/performance-subject-lifecycle.py")"
        --expected-common-lifecycle-helper-inode \
            "$(stat -f '%i' "$SCRIPT_DIRECTORY/performance-subject-lifecycle.py")"
        --expected-common-lifecycle-helper-sha256 \
            "$(sha256 "$SCRIPT_DIRECTORY/performance-subject-lifecycle.py")"
        --appkit-terminator-source "$SCRIPT_DIRECTORY/performance-appkit-terminate.m"
        --appkit-terminator-binary "$terminator_binary"
        --output "$run_metadata"
    )
    for artifact in "$CAMPAIGN_SECRET_FILE" "$trace_provisional" "$tail_receipt" \
        "$quit_receipt" "$exit_receipt" "$driver_intent" "$driver_receipt" "$driver_events" \
        "$window_identity" "$driver_binary" "$lifecycle_ready" "$lifecycle_registration" \
        "$workload_metadata" "$workload_events" "$ready_receipt" "$rss_samples"; do
        [[ -f "$artifact" && ! -L "$artifact" ]] \
            || fail "causal fixture artifact is unavailable: $artifact"
    done
    if [[ "$subject" == spaceterm ]]; then
        final_arguments+=(
            --native-provisional-observation "$native_provisional"
            --native-observation "$native_observation"
            --native-runtime-metadata "$runtime_metadata"
            --native-runtime-samples "$runtime_samples"
            --native-runtime-events "$runtime_events"
            --native-failure-actions "$failure_actions"
        )
    fi
    "$SCRIPT_DIRECTORY/freeze-performance-run.sh" "${final_arguments[@]}" >/dev/null
    TRACE_READY_RECEIPT_OVERRIDE="$ready_receipt" \
    TRACE_PLAN_START_GATE_OVERRIDE="$plan_start_gate" \
        write_trace_metadata "$identity" "$run_metadata" "$workload_metadata" \
            "$trace_metadata"
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
    local run_intent causal_prefix driver_receipt trace_provisional
    local tail_receipt quit_receipt exit_receipt lifecycle_ready lifecycle_registration
    local native_provisional=""
    local -a native_case_arguments=()
    causal_prefix="${CAUSAL_PREFIX_OVERRIDE:-$subject}"
    [[ "$subject" != spaceterm ]] \
        || {
            workload_metadata="$SPACETERM_WORKLOAD_METADATA"
            ready_receipt="$SPACETERM_READY_RECEIPT"
            plan_start_gate="$SPACETERM_PLAN_START_GATE"
            native_provisional="$NATIVE_PROVISIONAL"
        }
    if [[ "$subject" == spaceterm ]]; then
        run_intent="${RUN_INTENT_OVERRIDE:-$SPACETERM_INTENT}"
    else
        run_intent="${RUN_INTENT_OVERRIDE:-$GHOSTTY_INTENT}"
    fi
    [[ -z "$native_provisional" ]] \
        || native_case_arguments+=(--native-provisional-observation "$native_provisional")
    driver_receipt="$TEMP_ROOT/$causal_prefix-driver-receipt.tsv"
    trace_provisional="$TEMP_ROOT/$causal_prefix-trace-provisional.tsv"
    tail_receipt="$TEMP_ROOT/$causal_prefix-tail.tsv"
    quit_receipt="$TEMP_ROOT/$causal_prefix-quit.tsv"
    exit_receipt="$TEMP_ROOT/$causal_prefix-exit.tsv"
    lifecycle_ready="$TEMP_ROOT/$causal_prefix-lifecycle-ready.tsv"
    lifecycle_registration="$TEMP_ROOT/$causal_prefix-lifecycle-registration.tsv"
    local run_session run_nonce
    run_session="$(awk -F '\t' '$1 == "session_id" {print $2}' "$run_intent")"
    run_nonce="$(awk -F '\t' '$1 == "nonce" {print $2}' "$run_intent")"
    shift 8
    ( "$SCRIPT_DIRECTORY/analyze-release-performance-case.sh" \
        --subject "$subject" \
        --scenario ascii \
        --plan "$PLAN" \
        --plan-metadata "$PLAN_METADATA" \
        --pair-metadata "$PAIR_METADATA" \
        --subject-identity "$identity" \
        --run-intent "$run_intent" \
        --run-metadata "$run_metadata" \
        --workload-metadata "$workload_metadata" \
        --workload-events "$workload_events" \
        --ready-receipt "$ready_receipt" \
        --campaign-id "$CAMPAIGN_ID" \
        --session-id "$run_session" \
        --nonce "$run_nonce" \
        --campaign-secret-file "$CAMPAIGN_SECRET_FILE" \
        --driver-events "$driver_events" \
        --driver-intent "$TEMP_ROOT/$causal_prefix-driver-intent.tsv" \
        --driver-receipt "$driver_receipt" \
        --window-identity "$TEMP_ROOT/$causal_prefix-window-identity.tsv" \
        --driver-binary "$TEMP_ROOT/$causal_prefix-performance-driver" \
        --driver-source "$SCRIPT_DIRECTORY/performance-driver.m" \
        --driver-controller "$SCRIPT_DIRECTORY/run-native-performance-scenario.sh" \
        --rss-samples "$rss" \
        --trace-metadata "$trace" \
        --trace-provisional-receipt "$trace_provisional" \
        --performance-tail-receipt "$tail_receipt" \
        --performance-quit-receipt "$quit_receipt" \
        --subject-exit-receipt "$exit_receipt" \
        --performance-lifecycle-ready-receipt "$lifecycle_ready" \
        --performance-lifecycle-registration "$lifecycle_registration" \
        --subject-lifecycle-helper "$TEMP_ROOT/$causal_prefix-lifecycle-helper.py" \
        --common-lifecycle-helper "$SCRIPT_DIRECTORY/performance-subject-lifecycle.py" \
        --appkit-terminator-source "$SCRIPT_DIRECTORY/performance-appkit-terminate.m" \
        --appkit-terminator-binary "$TEMP_ROOT/performance-appkit-terminate" \
        --plan-start-gate "$plan_start_gate" \
        --manual-artifacts "$manual" \
        --manual-screenshot "$MANUAL_SCREENSHOT" \
        --manual-video "$MANUAL_VIDEO" \
        ${native_case_arguments[@]+"${native_case_arguments[@]}"} \
        "$@" )
    return $?
}

run_case_with_metadata() {
    local metadata="$1"
    shift
    WORKLOAD_METADATA_OVERRIDE="$metadata" run_case "$@"
}

run_case_with_causal_prefix() {
    local prefix="$1"
    shift
    CAUSAL_PREFIX_OVERRIDE="$prefix" run_case "$@"
}

run_case_with_metadata_and_prefix() {
    local metadata="$1" prefix="$2"
    shift 2
    WORKLOAD_METADATA_OVERRIDE="$metadata" CAUSAL_PREFIX_OVERRIDE="$prefix" run_case "$@"
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

DRIVER_EVENTS="$TEMP_ROOT/driver-events.tsv"
WORKLOAD_EVENTS="$TEMP_ROOT/workload-events.tsv"
WORKLOAD_METADATA="$TEMP_ROOT/workload-metadata.tsv"
SPACETERM_WORKLOAD_METADATA="$TEMP_ROOT/spaceterm-workload-metadata.tsv"
READY_RECEIPT="$TEMP_ROOT/synthetic-ready-receipt.tsv"
PLAN_START_GATE="$TEMP_ROOT/synthetic-plan-start-gate.tsv"
SPACETERM_READY_RECEIPT="$TEMP_ROOT/spaceterm-ready-receipt.tsv"
SPACETERM_PLAN_START_GATE="$TEMP_ROOT/spaceterm-plan-start-gate.tsv"
GHOSTTY_RSS="$TEMP_ROOT/ghostty-rss.tsv"
MANUAL="$TEMP_ROOT/manual.tsv"
MANUAL_SCREENSHOT="$TEMP_ROOT/manual-screenshot.png"
MANUAL_VIDEO="$TEMP_ROOT/manual-video.mov"
write_driver_events "$PLAN" "$DRIVER_EVENTS"
write_workload_events "$DRIVER_EVENTS" "$WORKLOAD_EVENTS"
write_ready_receipt "$WORKLOAD_EVENTS" "$GHOSTTY_IDENTITY" "$READY_RECEIPT"
publish_plan_start_gate "$WORKLOAD_EVENTS" "$PLAN_START_GATE" \
    "$CAMPAIGN_SECRET_FILE" "$READY_RECEIPT" "$BASE_CONTINUOUS_NS"
write_ready_receipt "$WORKLOAD_EVENTS" "$SPACETERM_IDENTITY" \
    "$SPACETERM_READY_RECEIPT" "$SPACETERM_SESSION_ID" "$SPACETERM_NONCE"
publish_plan_start_gate "$WORKLOAD_EVENTS" "$SPACETERM_PLAN_START_GATE" \
    "$CAMPAIGN_SECRET_FILE" "$SPACETERM_READY_RECEIPT" "$BASE_CONTINUOUS_NS" \
    "$SPACETERM_SESSION_ID" "$SPACETERM_NONCE"
write_workload_metadata "$WORKLOAD_BINARY" "$WORKLOAD_EVENTS" \
    "$GHOSTTY_IDENTITY" "$WORKLOAD_METADATA"
write_workload_metadata "$WORKLOAD_BINARY" "$WORKLOAD_EVENTS" \
    "$SPACETERM_IDENTITY" "$SPACETERM_WORKLOAD_METADATA" \
    "$SPACETERM_READY_RECEIPT" "$SPACETERM_SESSION_ID" "$SPACETERM_NONCE"
write_sustained_rss "$GHOSTTY_IDENTITY" "$WORKLOAD_METADATA" \
    "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$GHOSTTY_RSS"
printf 'bounded fake screenshot evidence\n' > "$MANUAL_SCREENSHOT"
printf 'bounded fake video evidence\n' > "$MANUAL_VIDEO"
write_manual_artifacts "$MANUAL"
readonly DRIVER_EVENTS WORKLOAD_EVENTS WORKLOAD_METADATA SPACETERM_WORKLOAD_METADATA
readonly READY_RECEIPT
readonly PLAN_START_GATE
readonly SPACETERM_READY_RECEIPT SPACETERM_PLAN_START_GATE
readonly GHOSTTY_RSS MANUAL
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

# Freeze pre-action intents only after each subject identity and SpaceTerm's
# provisional native launch observation exist. Final run metadata is produced
# below from the complete authenticated causal closure.
NATIVE_PROVISIONAL="$TEMP_ROOT/native-provisional.tsv"
write_native_provisional "$SPACETERM_IDENTITY" "$NATIVE_PROVISIONAL" "$SPACETERM_NONCE"
GHOSTTY_INTENT="$TEMP_ROOT/ghostty-intent.tsv"
SPACETERM_INTENT="$TEMP_ROOT/spaceterm-intent.tsv"
freeze_run_intent ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_INTENT"
freeze_run_intent spaceterm "$SPACETERM_IDENTITY" "$SPACETERM_INTENT" \
    "$NATIVE_PROVISIONAL" "$SPACETERM_SESSION_ID" "$SPACETERM_NONCE"

# The baseline Ghostty closure is built from the exact assembled evidence.
build_causal_closure ghostty ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_INTENT" \
    "$WORKLOAD_METADATA" "$WORKLOAD_EVENTS" "$READY_RECEIPT" \
    "$DRIVER_EVENTS" "$GHOSTTY_RSS" "$PLAN_START_GATE"
GHOSTTY_RUN="$TEMP_ROOT/ghostty-run.tsv"
GHOSTTY_TRACE="$TEMP_ROOT/ghostty-trace.tsv"
readonly NATIVE_PROVISIONAL GHOSTTY_INTENT SPACETERM_INTENT
readonly GHOSTTY_RUN GHOSTTY_TRACE

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
build_causal_closure delayed ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_INTENT" \
    "$WORKLOAD_METADATA" "$WORKLOAD_EVENTS" "$READY_RECEIPT" \
    "$DRIVER_EVENTS" "$DELAYED_ASSEMBLED_RSS" "$PLAN_START_GATE"
expect_result 0 CASE-COMPLETE "scheduled RSS cadence survives normal sampling delay" \
    run_case_with_causal_prefix delayed ghostty "$GHOSTTY_IDENTITY" \
        "$TEMP_ROOT/delayed-run.tsv" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$DELAYED_ASSEMBLED_RSS" \
        "$TEMP_ROOT/delayed-trace.tsv" "$MANUAL"

build_causal_closure assembled ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_INTENT" \
    "$WORKLOAD_METADATA" "$WORKLOAD_EVENTS" "$READY_RECEIPT" \
    "$DRIVER_EVENTS" "$ASSEMBLED_RSS" "$PLAN_START_GATE"
expect_result 0 CASE-COMPLETE "valid assembled raw RSS evidence" \
    run_case_with_causal_prefix assembled ghostty "$GHOSTTY_IDENTITY" \
        "$TEMP_ROOT/assembled-run.tsv" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$ASSEMBLED_RSS" \
        "$TEMP_ROOT/assembled-trace.tsv" "$MANUAL"

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
chmod 0400 "$TAMPERED_EVENTS"
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

expect_result 0 CASE-COMPLETE "valid Ghostty case with zero Hangs" \
    run_case ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$GHOSTTY_RSS" "$GHOSTTY_TRACE" "$MANUAL"

SPACETERM_RSS="$TEMP_ROOT/spaceterm-rss.tsv"
SPACETERM_TRACE="$TEMP_ROOT/spaceterm-trace.tsv"
NATIVE_LAUNCH="$TEMP_ROOT/native-final.tsv"
RUNTIME_SAMPLES="$TEMP_ROOT/runtime-samples.tsv"
RUNTIME_EVENTS="$TEMP_ROOT/runtime-events.tsv"
RUNTIME_METADATA="$TEMP_ROOT/runtime-metadata.tsv"
FAILURE_ACTIONS="$TEMP_ROOT/failure-actions.tsv"
write_sustained_rss "$SPACETERM_IDENTITY" "$SPACETERM_WORKLOAD_METADATA" "$WORKLOAD_EVENTS" \
    "$DRIVER_EVENTS" "$SPACETERM_RSS" 0 "$SPACETERM_READY_RECEIPT" \
    "$SPACETERM_PLAN_START_GATE"
write_runtime_observation "$WORKLOAD_EVENTS" "$RUNTIME_SAMPLES" \
    "$RUNTIME_EVENTS" "$RUNTIME_METADATA" "$FAILURE_ACTIONS"
write_native_final "$NATIVE_PROVISIONAL" "$RUNTIME_METADATA" "$FAILURE_ACTIONS" \
    "$NATIVE_LAUNCH"
build_causal_closure spaceterm spaceterm "$SPACETERM_IDENTITY" "$SPACETERM_INTENT" \
    "$SPACETERM_WORKLOAD_METADATA" "$WORKLOAD_EVENTS" "$SPACETERM_READY_RECEIPT" \
    "$DRIVER_EVENTS" "$SPACETERM_RSS" "$SPACETERM_PLAN_START_GATE" \
    "$NATIVE_PROVISIONAL" "$NATIVE_LAUNCH" "$RUNTIME_METADATA" \
    "$RUNTIME_SAMPLES" "$RUNTIME_EVENTS" "$FAILURE_ACTIONS"
SPACETERM_RUN="$TEMP_ROOT/spaceterm-run.tsv"
readonly SPACETERM_RSS SPACETERM_TRACE NATIVE_LAUNCH
readonly RUNTIME_SAMPLES RUNTIME_EVENTS RUNTIME_METADATA
readonly FAILURE_ACTIONS

expect_result 0 CASE-COMPLETE "valid authenticated SpaceTerm runtime case" \
    run_case spaceterm "$SPACETERM_IDENTITY" "$SPACETERM_RUN" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$SPACETERM_RSS" \
        "$SPACETERM_TRACE" "$MANUAL" \
        --runtime-samples "$RUNTIME_SAMPLES" \
        --runtime-events "$RUNTIME_EVENTS" \
        --runtime-metadata "$RUNTIME_METADATA" \
        --failure-actions "$FAILURE_ACTIONS" \
        --native-launch-observation "$NATIVE_LAUNCH"

# Freeze the two exact14 content-free reports before constructing the pair
# result. The pair HMAC then binds the immutable evidence that each analyzer
# actually used, rather than files that could be swapped between analyses.
GHOSTTY_CASE_REPORT="$TEMP_ROOT/ghostty-case-report.tsv"
SPACETERM_CASE_REPORT="$TEMP_ROOT/spaceterm-case-report.tsv"
freeze_case_report "$GHOSTTY_CASE_REPORT" "Ghostty case report" \
    run_case ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$GHOSTTY_RSS" "$GHOSTTY_TRACE" "$MANUAL"
freeze_case_report "$SPACETERM_CASE_REPORT" "SpaceTerm case report" \
    run_case spaceterm "$SPACETERM_IDENTITY" "$SPACETERM_RUN" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$SPACETERM_RSS" \
        "$SPACETERM_TRACE" "$MANUAL" \
        --runtime-samples "$RUNTIME_SAMPLES" --runtime-events "$RUNTIME_EVENTS" \
        --runtime-metadata "$RUNTIME_METADATA" --failure-actions "$FAILURE_ACTIONS" \
        --native-launch-observation "$NATIVE_LAUNCH"

# Final release acceptance is pair-scoped. Reuse both complete production-mode
# subject bundles above, whose distinct session/nonce values prevent replay.
PAIR_RESULT="$TEMP_ROOT/pair-result.tsv"
pair_result_arguments=(
    --campaign-secret-file "$CAMPAIGN_SECRET_FILE" --campaign-id "$CAMPAIGN_ID"
    --pair-metadata "$PAIR_METADATA" --scenario-plan "$PLAN"
)
for pair_subject in spaceterm ghostty; do
    if [[ "$pair_subject" == spaceterm ]]; then
        pair_identity="$SPACETERM_IDENTITY"; pair_intent="$SPACETERM_INTENT"
        pair_run="$SPACETERM_RUN"; pair_gate="$SPACETERM_PLAN_START_GATE"
        pair_workload="$SPACETERM_WORKLOAD_METADATA"; pair_ready="$SPACETERM_READY_RECEIPT"
        pair_trace="$SPACETERM_TRACE"; pair_case_report="$SPACETERM_CASE_REPORT"
    else
        pair_identity="$GHOSTTY_IDENTITY"; pair_intent="$GHOSTTY_INTENT"
        pair_run="$GHOSTTY_RUN"; pair_gate="$PLAN_START_GATE"
        pair_workload="$WORKLOAD_METADATA"; pair_ready="$READY_RECEIPT"
        pair_trace="$GHOSTTY_TRACE"; pair_case_report="$GHOSTTY_CASE_REPORT"
    fi
    pair_result_arguments+=(
        "--$pair_subject-subject-identity" "$pair_identity"
        "--$pair_subject-run-intent" "$pair_intent"
        "--$pair_subject-run-metadata" "$pair_run"
        "--$pair_subject-window-identity" "$TEMP_ROOT/$pair_subject-window-identity.tsv"
        "--$pair_subject-driver-intent" "$TEMP_ROOT/$pair_subject-driver-intent.tsv"
        "--$pair_subject-driver-events" "$DRIVER_EVENTS"
        "--$pair_subject-driver-receipt" "$TEMP_ROOT/$pair_subject-driver-receipt.tsv"
        "--$pair_subject-driver-binary" "$TEMP_ROOT/$pair_subject-performance-driver"
        "--$pair_subject-driver-source" "$SCRIPT_DIRECTORY/performance-driver.m"
        "--$pair_subject-driver-controller" "$SCRIPT_DIRECTORY/run-native-performance-scenario.sh"
        "--$pair_subject-plan-start-gate" "$pair_gate"
        "--$pair_subject-trace-provisional-receipt" "$TEMP_ROOT/$pair_subject-trace-provisional.tsv"
        "--$pair_subject-workload-metadata" "$pair_workload"
        "--$pair_subject-workload-events" "$WORKLOAD_EVENTS"
        "--$pair_subject-workload-ready-receipt" "$pair_ready"
        "--$pair_subject-lifecycle-ready-receipt" "$TEMP_ROOT/$pair_subject-lifecycle-ready.tsv"
        "--$pair_subject-lifecycle-registration" "$TEMP_ROOT/$pair_subject-lifecycle-registration.tsv"
        "--$pair_subject-lifecycle-helper" "$TEMP_ROOT/$pair_subject-lifecycle-helper.py"
        "--$pair_subject-tail-receipt" "$TEMP_ROOT/$pair_subject-tail.tsv"
        "--$pair_subject-quit-receipt" "$TEMP_ROOT/$pair_subject-quit.tsv"
        "--$pair_subject-exit-receipt" "$TEMP_ROOT/$pair_subject-exit.tsv"
        "--$pair_subject-case-report" "$pair_case_report"
        "--$pair_subject-trace-metadata" "$pair_trace"
        "--$pair_subject-trace-archive" "$TEMP_ROOT/$pair_subject-ascii.trace"
        "--$pair_subject-manual-artifacts" "$MANUAL"
        "--$pair_subject-manual-screenshot" "$MANUAL_SCREENSHOT"
        "--$pair_subject-manual-video" "$MANUAL_VIDEO"
    )
done
pair_result_arguments+=(
    --spaceterm-native-provisional-observation "$NATIVE_PROVISIONAL"
    --spaceterm-native-observation "$NATIVE_LAUNCH"
    --spaceterm-native-runtime-metadata "$RUNTIME_METADATA"
    --spaceterm-native-runtime-samples "$RUNTIME_SAMPLES"
    --spaceterm-native-runtime-events "$RUNTIME_EVENTS"
    --spaceterm-native-failure-actions "$FAILURE_ACTIONS"
    --common-lifecycle-helper "$SCRIPT_DIRECTORY/performance-subject-lifecycle.py"
    --appkit-terminator-source "$SCRIPT_DIRECTORY/performance-appkit-terminate.m"
    --appkit-terminator-binary "$TEMP_ROOT/performance-appkit-terminate"
)
chmod 0400 "$DRIVER_EVENTS" "$WORKLOAD_EVENTS" "$WORKLOAD_METADATA" \
    "$SPACETERM_WORKLOAD_METADATA" "$READY_RECEIPT" "$SPACETERM_READY_RECEIPT" \
    "$MANUAL_SCREENSHOT" "$MANUAL_VIDEO"
"$SCRIPT_DIRECTORY/performance-pair-result.py" create \
    "${pair_result_arguments[@]}" --output "$PAIR_RESULT"

pair_analyzer_arguments=(
    --campaign-id "$CAMPAIGN_ID" --campaign-secret-file "$CAMPAIGN_SECRET_FILE"
    --scenario ascii --plan "$PLAN" --plan-metadata "$PLAN_METADATA"
    --pair-metadata "$PAIR_METADATA" --pair-result "$PAIR_RESULT"
)
for pair_subject in spaceterm ghostty; do
    if [[ "$pair_subject" == spaceterm ]]; then
        pair_identity="$SPACETERM_IDENTITY"; pair_intent="$SPACETERM_INTENT"
        pair_run="$SPACETERM_RUN"; pair_workload="$SPACETERM_WORKLOAD_METADATA"
        pair_ready="$SPACETERM_READY_RECEIPT"; pair_session="$SPACETERM_SESSION_ID"
        pair_nonce="$SPACETERM_NONCE"; pair_rss="$SPACETERM_RSS"
        pair_trace="$SPACETERM_TRACE"; pair_gate="$SPACETERM_PLAN_START_GATE"
    else
        pair_identity="$GHOSTTY_IDENTITY"; pair_intent="$GHOSTTY_INTENT"
        pair_run="$GHOSTTY_RUN"; pair_workload="$WORKLOAD_METADATA"
        pair_ready="$READY_RECEIPT"; pair_session="$SESSION_ID"; pair_nonce="$NONCE"
        pair_rss="$GHOSTTY_RSS"; pair_trace="$GHOSTTY_TRACE"; pair_gate="$PLAN_START_GATE"
    fi
    pair_analyzer_arguments+=(
        "--$pair_subject-subject-identity" "$pair_identity"
        "--$pair_subject-run-intent" "$pair_intent"
        "--$pair_subject-run-metadata" "$pair_run"
        "--$pair_subject-workload-metadata" "$pair_workload"
        "--$pair_subject-workload-events" "$WORKLOAD_EVENTS"
        "--$pair_subject-ready-receipt" "$pair_ready"
        "--$pair_subject-session-id" "$pair_session" "--$pair_subject-nonce" "$pair_nonce"
        "--$pair_subject-driver-events" "$DRIVER_EVENTS"
        "--$pair_subject-driver-intent" "$TEMP_ROOT/$pair_subject-driver-intent.tsv"
        "--$pair_subject-driver-receipt" "$TEMP_ROOT/$pair_subject-driver-receipt.tsv"
        "--$pair_subject-window-identity" "$TEMP_ROOT/$pair_subject-window-identity.tsv"
        "--$pair_subject-driver-binary" "$TEMP_ROOT/$pair_subject-performance-driver"
        "--$pair_subject-driver-source" "$SCRIPT_DIRECTORY/performance-driver.m"
        "--$pair_subject-driver-controller" "$SCRIPT_DIRECTORY/run-native-performance-scenario.sh"
        "--$pair_subject-rss-samples" "$pair_rss"
        "--$pair_subject-trace-metadata" "$pair_trace"
        "--$pair_subject-trace-provisional-receipt" "$TEMP_ROOT/$pair_subject-trace-provisional.tsv"
        "--$pair_subject-performance-tail-receipt" "$TEMP_ROOT/$pair_subject-tail.tsv"
        "--$pair_subject-performance-quit-receipt" "$TEMP_ROOT/$pair_subject-quit.tsv"
        "--$pair_subject-subject-exit-receipt" "$TEMP_ROOT/$pair_subject-exit.tsv"
        "--$pair_subject-plan-start-gate" "$pair_gate"
        "--$pair_subject-manual-artifacts" "$MANUAL"
        "--$pair_subject-manual-screenshot" "$MANUAL_SCREENSHOT"
        "--$pair_subject-manual-video" "$MANUAL_VIDEO"
        "--$pair_subject-lifecycle-ready-receipt" "$TEMP_ROOT/$pair_subject-lifecycle-ready.tsv"
        "--$pair_subject-lifecycle-registration" "$TEMP_ROOT/$pair_subject-lifecycle-registration.tsv"
        "--$pair_subject-lifecycle-helper" "$TEMP_ROOT/$pair_subject-lifecycle-helper.py"
        "--$pair_subject-case-report" \
            "$([[ "$pair_subject" == spaceterm ]] && printf '%s' "$SPACETERM_CASE_REPORT" || printf '%s' "$GHOSTTY_CASE_REPORT")"
    )
done
pair_analyzer_arguments+=(
    --spaceterm-runtime-samples "$RUNTIME_SAMPLES"
    --spaceterm-runtime-events "$RUNTIME_EVENTS"
    --spaceterm-runtime-metadata "$RUNTIME_METADATA"
    --spaceterm-failure-actions "$FAILURE_ACTIONS"
    --spaceterm-native-launch-observation "$NATIVE_LAUNCH"
    --spaceterm-native-provisional-observation "$NATIVE_PROVISIONAL"
    --common-lifecycle-helper "$SCRIPT_DIRECTORY/performance-subject-lifecycle.py"
    --appkit-terminator-source "$SCRIPT_DIRECTORY/performance-appkit-terminate.m"
    --appkit-terminator-binary "$TEMP_ROOT/performance-appkit-terminate"
)
expect_result 0 PASS "authenticated completed pair" \
    "$SCRIPT_DIRECTORY/analyze-release-performance-pair.sh" "${pair_analyzer_arguments[@]}"
run_pair_with_replacement() {
    local old="$1" replacement="$2" index
    local -a replaced_arguments=("${pair_analyzer_arguments[@]}")
    for index in "${!replaced_arguments[@]}"; do
        [[ "${replaced_arguments[$index]}" != "$old" ]] \
            || replaced_arguments[index]="$replacement"
    done
    "$SCRIPT_DIRECTORY/analyze-release-performance-pair.sh" \
        "${replaced_arguments[@]}"
}
expect_result 2 NOT-RUN "missing pair result" \
    run_pair_with_replacement "$PAIR_RESULT" "$TEMP_ROOT/missing-pair-result.tsv"
expect_result 2 NOT-RUN "one-sided pair evidence" \
    run_pair_with_replacement "$GHOSTTY_RUN" "$TEMP_ROOT/missing-ghostty-run.tsv"
expect_result 2 NOT-RUN "cross-run pair evidence replay" \
    run_pair_with_replacement "$TEMP_ROOT/ghostty-tail.tsv" \
        "$TEMP_ROOT/spaceterm-tail.tsv"

MISMATCHED_NATIVE_TEAM="$TEMP_ROOT/mismatched-native-team.tsv"
sed $'s/^process.signature.team_identifier\t$/process.signature.team_identifier\tFORGEDTEAM/' \
    "$NATIVE_LAUNCH" > "$MISMATCHED_NATIVE_TEAM"
chmod 0400 "$MISMATCHED_NATIVE_TEAM"
expect_result 2 NOT-RUN "native team identifier mismatch" \
    run_case spaceterm "$SPACETERM_IDENTITY" "$SPACETERM_RUN" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$SPACETERM_RSS" \
        "$SPACETERM_TRACE" "$MANUAL" \
        --runtime-samples "$RUNTIME_SAMPLES" \
        --runtime-events "$RUNTIME_EVENTS" \
        --runtime-metadata "$RUNTIME_METADATA" \
        --failure-actions "$FAILURE_ACTIONS" \
        --native-launch-observation "$MISMATCHED_NATIVE_TEAM"

ALTERED_FONT="$TEMP_ROOT/altered-font.tsv"
printf 'different-font-manifest\n' > "$ALTERED_FONT"
expect_command_failure "paired font mismatch" \
    "$SCRIPT_DIRECTORY/freeze-performance-run-intent.sh" \
        --subject ghostty \
        --pair-metadata "$PAIR_METADATA" \
        --subject-identity "$GHOSTTY_IDENTITY" \
        --plan "$PLAN" \
        --workload-binary "$WORKLOAD_BINARY" \
        --command-manifest "$TEMP_ROOT/command.tsv" \
        --environment-manifest "$TEMP_ROOT/environment.tsv" \
        --font-manifest "$ALTERED_FONT" \
        --initial-grid-manifest "$TEMP_ROOT/initial-grid.tsv" \
        --campaign-id "$CAMPAIGN_ID" --session-id "$SESSION_ID" --nonce "$NONCE" \
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
build_causal_closure slow ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_INTENT" \
    "$SLOW_WORKLOAD_METADATA" "$SLOW_WORKLOAD" "$SLOW_READY_RECEIPT" \
    "$DRIVER_EVENTS" "$SLOW_RSS" "$SLOW_PLAN_START_GATE"
READY_RECEIPT_OVERRIDE="$SLOW_READY_RECEIPT" \
PLAN_START_GATE_OVERRIDE="$SLOW_PLAN_START_GATE" \
expect_result 1 FAIL "input acknowledgement over 250 ms" \
    run_case_with_metadata_and_prefix "$SLOW_WORKLOAD_METADATA" slow \
        ghostty "$GHOSTTY_IDENTITY" "$TEMP_ROOT/slow-run.tsv" \
        "$SLOW_WORKLOAD" "$DRIVER_EVENTS" "$SLOW_RSS" \
        "$TEMP_ROOT/slow-trace.tsv" "$MANUAL"

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
chmod 0400 "$REUSED_TRACE"
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
