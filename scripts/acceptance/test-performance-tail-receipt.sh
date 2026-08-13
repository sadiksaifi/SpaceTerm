#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
umask 077
export SPACETERM_PERFORMANCE_TEST_MODE=1

SCRIPT_DIRECTORY="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
TOOL="$SCRIPT_DIRECTORY/performance-tail-receipt.py"
TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-tail-receipt.XXXXXX")"
trap 'rm -rf -- "$TEMP_ROOT"' EXIT INT TERM
NONCE=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
TOKEN=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb

fail() { echo "test failure: $*" >&2; exit 1; }
sha256() { shasum -a 256 "$1" | awk '{print $1}'; }
expect_failure() { local label="$1"; shift; "$@" >/dev/null 2>&1 && fail "$label unexpectedly succeeded" || true; }

SECRET="$TEMP_ROOT/secret"
SUBJECT="$TEMP_ROOT/subject.tsv"
INTENT="$TEMP_ROOT/intent.tsv"
printf '%064d' 0 > "$SECRET"
chmod 0600 "$SECRET"
cat > "$SUBJECT" <<'EOF'
format_version	1
subject	spaceterm
app_bundle_path	/Applications/SpaceTerm.app
bundle_identifier	dev.spaceterm.SpaceTerm
bundle_version	1+1
executable_path	/Applications/SpaceTerm.app/Contents/MacOS/SpaceTerm
executable_sha256	cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
executable_device	1
executable_inode	2
executable_fsid	1
signature_valid	true
signing_identifier	dev.spaceterm.SpaceTerm
team_identifier	none
cdhash	abc123
process_pid	4242
process_start_identity	100:200
identity_status	frozen
EOF
chmod 0400 "$SUBJECT"
cat > "$INTENT" <<EOF
format_version	1
subject	spaceterm
subject_identity_sha256	$(sha256 "$SUBJECT")
scenario	ascii
scenario_plan_sha256	cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
workload_sha256	cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
command_sha256	cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
environment_sha256	cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
font_sha256	cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
initial_grid_sha256	cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
measured_duration_ms	1000
process_pid	4242
process_start_identity	100:200
campaign_id	campaign-a
session_id	session-a
nonce	$NONCE
native_provisional_observation_sha256	cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
evidence_mode	test-only
status	prepared
EOF
chmod 0400 "$INTENT"
for name in driver-receipt driver-events rss-samples; do
    printf '%s\n' "$name" > "$TEMP_ROOT/$name.tsv"
    chmod 0400 "$TEMP_ROOT/$name.tsv"
done
WORKLOAD_EVENTS="$TEMP_ROOT/workload-events.tsv"
WORKLOAD_METADATA="$TEMP_ROOT/workload-metadata.tsv"
WORKLOAD_READY="$TEMP_ROOT/workload-ready.tsv"
python3 - "$WORKLOAD_EVENTS" "$WORKLOAD_METADATA" "$WORKLOAD_READY" "$SECRET" "$SUBJECT" <<'PY'
import hashlib, hmac, os, pathlib, struct, sys
events_path, metadata_path, ready_path, secret_path, subject_path = map(pathlib.Path, sys.argv[1:])
secret = secret_path.read_bytes()
sha = lambda data: hashlib.sha256(data).hexdigest()
header = "sequence\tcontinuous_ns\tkind\tevent_id\tbyte_count\trows\tcolumns\tpixel_width\tpixel_height\tstatus\n"
prefix = (header + "0\t1000\tseed-complete\tnone\t1\t24\t80\t800\t600\tok\n"
    + "1\t1100\tmeasurement-ready\tnone\t1\t24\t80\t800\t600\tok\n").encode()
events = prefix + b"2\t3000\tproducer-end\tnone\t1\t24\t80\t800\t600\tsuccess\n"
events_path.write_bytes(events)
event_stat = events_path.stat()
subject_hash = sha(subject_path.read_bytes())
ready_rows = [
    ("format_version", "1"), ("campaign_id", "campaign-a"), ("session_id", "session-a"),
    ("nonce", "a" * 64), ("subject_identity_sha256", subject_hash),
    ("producer_pid", "50"), ("producer_started_continuous_ns", "500"),
    ("producer_session_id", "50"), ("producer_process_group", "50"),
    ("tty_device", "1"), ("tty_inode", "2"), ("tty_rdev", "3"),
    ("events_device", str(event_stat.st_dev)), ("events_inode", str(event_stat.st_ino)),
    ("events_prefix_bytes", str(len(prefix))), ("events_prefix_sha256", sha(prefix)),
    ("measurement_ready_continuous_ns", "1100"), ("measurement_ready_byte_count", "1"),
    ("auth_algorithm", "hmac-sha256"),
]
unsigned_ready = b"".join(f"{k}\t{v}\n".encode() for k, v in ready_rows)
ready_hmac = hmac.new(secret, b"spaceterm.performance.workload-ready/v1\0"
    + struct.pack(">Q", len(unsigned_ready)) + unsigned_ready, hashlib.sha256).hexdigest()
ready = unsigned_ready + f"ready_hmac_sha256\t{ready_hmac}\n".encode()
ready_path.write_bytes(ready)
rows = [
    ("format_version", "3"), ("scenario", "ascii"), ("campaign_id", "campaign-a"),
    ("session_id", "session-a"), ("nonce", "a" * 64),
    ("subject_identity_sha256", subject_hash), ("subject_process_pid", "4242"),
    ("subject_process_start_identity", "100:200"), ("producer_sha256", "c" * 64),
    ("producer_pid", "50"), ("producer_started_continuous_ns", "500"),
    ("producer_session_id", "50"), ("producer_process_group", "50"),
    ("tty_device", "1"), ("tty_inode", "2"), ("tty_rdev", "3"),
    ("ready_receipt_sha256", sha(ready)), ("events_sha256", sha(events)),
    ("auth_algorithm", "hmac-sha256"), ("seed_sha256", "c" * 64),
    ("seed_bytes", "1"), ("requested_duration_ms", "1"), ("warmup_ms", "0"),
    ("requested_iterations", "1"), ("requested_seed_rows", "1"),
    ("emitted_bytes", "1"), ("input_events", "0"), ("plan_start_continuous_ns", "1000"),
    ("started_continuous_ns", "1000"), ("ended_continuous_ns", "3000"),
    ("status", "complete"),
]
unsigned = b"".join(f"{k}\t{v}\n".encode() for k, v in rows)
payload = (b"spaceterm.performance.workload-auth/v1\0" + struct.pack(">Q", len(unsigned))
    + unsigned + struct.pack(">Q", len(events)) + events)
signature = hmac.new(secret, payload, hashlib.sha256).hexdigest()
metadata_path.write_bytes(unsigned + f"events_hmac_sha256\t{signature}\n".encode())
PY
chmod 0400 "$WORKLOAD_EVENTS" "$WORKLOAD_METADATA" "$WORKLOAD_READY"
TRACE="$TEMP_ROOT/trace-provisional.tsv"
python3 - "$TRACE" "$SECRET" "$SUBJECT" "$INTENT" "$WORKLOAD_METADATA" "$WORKLOAD_READY" <<'PY'
import hashlib, hmac, pathlib, struct, sys
output, secret_path, subject_path, intent_path, workload_path, ready_path = map(pathlib.Path, sys.argv[1:])
sha = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
rows = [
    ("format_version", "1"), ("subject_identity_sha256", sha(subject_path)),
    ("run_intent_sha256", sha(intent_path)), ("workload_metadata_sha256", sha(workload_path)),
    ("workload_ready_receipt_sha256", sha(ready_path)),
    ("supplemental_evidence_sha256", "c" * 64), ("capture_status", "CAPTURED"),
    ("requested_duration_ms", "1000"), ("actual_duration_ms", "1000"),
    ("capture_started_continuous_ns", "1000"),
    ("capture_ended_continuous_ns", "2000"), ("trace_bundle_tree_sha256", "c" * 64),
    ("toc_sha256", "c" * 64), ("time_profile_export_sha256", "c" * 64),
    ("allocations_export_sha256", "c" * 64), ("hangs_export_sha256", "c" * 64),
    ("trace_verification_sha256", "c" * 64), ("verifier_sha256", "c" * 64),
    ("evidence_mode", "test-only"),
    ("status", "complete"), ("auth_algorithm", "hmac-sha256"),
]
unsigned = b"".join(f"{key}\t{value}\n".encode() for key, value in rows)
payload = b"spaceterm.performance.trace-provisional/v1\0" + struct.pack(">Q", len(unsigned)) + unsigned
signature = hmac.new(secret_path.read_bytes(), payload, hashlib.sha256).hexdigest()
output.write_bytes(unsigned + f"provisional_hmac_sha256\t{signature}\n".encode())
PY
chmod 0400 "$TRACE"
RECEIPT="$TEMP_ROOT/tail.tsv"
TERMINATOR_SOURCE="$TEMP_ROOT/performance-appkit-terminate.m"
TERMINATOR_BINARY="$TEMP_ROOT/performance-appkit-terminate"
printf '%s\n' '// fixture' > "$TERMINATOR_SOURCE"
printf '%s\n' '#!/bin/sh' 'exit 0' > "$TERMINATOR_BINARY"
chmod 0500 "$TERMINATOR_BINARY"
common=(
    --campaign-secret-file "$SECRET" --campaign-id campaign-a --session-id session-a
    --nonce "$NONCE" --quit-token "$TOKEN" --run-intent "$INTENT"
    --subject-identity "$SUBJECT" --driver-receipt "$TEMP_ROOT/driver-receipt.tsv"
    --driver-events "$TEMP_ROOT/driver-events.tsv"
    --workload-metadata "$WORKLOAD_METADATA" --workload-events "$WORKLOAD_EVENTS"
    --workload-ready-receipt "$WORKLOAD_READY" --rss-samples "$TEMP_ROOT/rss-samples.tsv"
    --trace-provisional-receipt "$TRACE"
    --tail-completed-continuous-ns 5000003000
    --appkit-terminator-source "$TERMINATOR_SOURCE"
    --appkit-terminator-binary "$TERMINATOR_BINARY"
)
"$TOOL" create "${common[@]}" --output "$RECEIPT"
"$TOOL" verify "${common[@]}" --receipt "$RECEIPT"
expect_failure "correctly signed early tail" "$TOOL" create \
    "${common[@]/5000003000/4000003000}" --output "$TEMP_ROOT/early-tail.tsv"
expect_failure "correctly signed stale tail" "$TOOL" create \
    "${common[@]/5000003000/16000003000}" --output "$TEMP_ROOT/stale-tail.tsv"
expect_failure "replayed token" "$TOOL" verify \
    "${common[@]/$TOKEN/dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd}" \
    --receipt "$RECEIPT"
expect_failure "cross-session replay" "$TOOL" verify \
    "${common[@]/session-a/session-b}" --receipt "$RECEIPT"
chmod 0600 "$TEMP_ROOT/driver-events.tsv"
printf 'mutated\n' > "$TEMP_ROOT/driver-events.tsv"
chmod 0400 "$TEMP_ROOT/driver-events.tsv"
expect_failure "mutated driver evidence" "$TOOL" verify "${common[@]}" --receipt "$RECEIPT"
BAD="$TEMP_ROOT/bad-tail.tsv"
sed 's/terminal_status\ttail-complete/terminal_status\tpremature/' "$RECEIPT" > "$BAD"
chmod 0400 "$BAD"
expect_failure "premature tail" "$TOOL" verify "${common[@]}" --receipt "$BAD"
BAD_TRACE="$TEMP_ROOT/bad-trace.tsv"
sed 's/capture_status\tCAPTURED/capture_status\tINCOMPLETE/' "$TRACE" > "$BAD_TRACE"
chmod 0400 "$BAD_TRACE"
expect_failure "forged trace provisional" "$TOOL" verify \
    "${common[@]/$TRACE/$BAD_TRACE}" --receipt "$RECEIPT"

echo "performance tail receipt tests passed"
