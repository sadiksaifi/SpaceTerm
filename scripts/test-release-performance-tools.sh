#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C

SCRIPT_DIRECTORY="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIRECTORY
TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-performance-tools.XXXXXX")"
readonly TEMP_ROOT
TARGET_PIDS=()
CLEANED_UP=false

cleanup() {
    local pid
    [[ "$CLEANED_UP" == false ]] || return 0
    CLEANED_UP=true
    for pid in "${TARGET_PIDS[@]}"; do
        [[ -n "$pid" ]] || continue
        if kill -0 "$pid" 2>/dev/null; then
            kill "$pid" 2>/dev/null || true
        fi
        wait "$pid" 2>/dev/null || true
    done
    rm -rf -- "$TEMP_ROOT"
}

handle_signal() {
    cleanup
    trap - EXIT INT TERM
    exit 130
}

trap cleanup EXIT
trap handle_signal INT TERM

fail() {
    echo "test failure: $*" >&2
    exit 1
}

forget_target_pid() {
    local completed_pid="$1"
    local index
    for index in "${!TARGET_PIDS[@]}"; do
        if [[ "${TARGET_PIDS[$index]}" == "$completed_pid" ]]; then
            TARGET_PIDS[index]=""
        fi
    done
}

metric() {
    local path="$1"
    local key="$2"
    awk -F '\t' -v key="$key" '$1 == key { print $2 }' "$path"
}

assert_equal() {
    local expected="$1"
    local actual="$2"
    local label="$3"
    [[ "$actual" == "$expected" ]] \
        || fail "$label: expected '$expected', observed '$actual'"
}

assert_workload_accounting() {
    local output="$1"
    local metrics="$2"
    local actual_bytes
    actual_bytes="$(wc -c < "$output" | tr -d '[:space:]')"
    assert_equal "$(metric "$metrics" emitted_bytes)" "$actual_bytes" \
        "workload emitted-byte accounting"
    grep -aFq "SPACETERM-PERF-END" "$output" \
        || fail "workload output did not contain the final sentinel"
}

sign_evidence() {
    local path="$1"
    local secret="$2"
    local key="$3"
    local digest
    digest="$(python3 - "$path" "$secret" <<'PY'
import hashlib
import hmac
import pathlib
import sys

print(hmac.new(pathlib.Path(sys.argv[2]).read_bytes(),
               pathlib.Path(sys.argv[1]).read_bytes(), hashlib.sha256).hexdigest())
PY
)"
    printf '%s\t%s\n' "$key" "$digest" >> "$path"
}

write_rss_fixture() {
    local path="$1"
    local requested_duration="$2"
    local status="$3"
    local cadence_mode="$4"
    local first_min="$5"
    local first_max="$6"
    local final_min="$7"
    local final_max="$8"
    local last_elapsed="${9:-$requested_duration}"
    local workload_bytes="${10:-1073741824}"
    local nominal_elapsed observed_elapsed observed_epoch rss

    {
        printf 'elapsed_seconds\tepoch_seconds\trss_kib\n'
        printf '# format_version\t2\n'
        printf '# sample_interval_seconds\t10\n'
        printf '# requested_duration_seconds\t%s\n' "$requested_duration"
        printf '# started_epoch_seconds\t1000\n'
        printf '# campaign_id\ttest-campaign\n'
        printf '# scenario\tascii\n'
        printf '# session_id\ttest-session\n'
        printf '# process_identity_sha256\t%s\n' \
            1111111111111111111111111111111111111111111111111111111111111111
        printf '# sampler_tool_sha256\t%s\n' \
            2222222222222222222222222222222222222222222222222222222222222222
        printf '# workload_tool_sha256\t%s\n' \
            3333333333333333333333333333333333333333333333333333333333333333
        printf '# analyzer_tool_sha256\t%s\n' \
            4444444444444444444444444444444444444444444444444444444444444444
        printf '# process_inspector_tool_sha256\t%s\n' \
            7777777777777777777777777777777777777777777777777777777777777777
        for ((nominal_elapsed = 0; nominal_elapsed <= last_elapsed; nominal_elapsed += 10)); do
            observed_elapsed="$nominal_elapsed"
            case "$cadence_mode:$nominal_elapsed" in
                late:20)
                    observed_elapsed=22
                    ;;
                duplicate:20)
                    observed_elapsed=10
                    ;;
            esac
            observed_epoch=$((1000 + observed_elapsed))
            if [[ "$cadence_mode:$nominal_elapsed" == "epoch-late:20" ]]; then
                observed_epoch=$((observed_epoch + 2))
            fi
            if (( nominal_elapsed < 300 )); then
                if (( nominal_elapsed % 20 == 0 )); then
                    rss="$first_min"
                else
                    rss="$first_max"
                fi
            elif (( nominal_elapsed >= requested_duration - 300 )); then
                if (( nominal_elapsed % 20 == 0 )); then
                    rss="$final_min"
                else
                    rss="$final_max"
                fi
            else
                rss="$first_min"
            fi
            printf '%s\t%s\t%s\n' \
                "$observed_elapsed" "$observed_epoch" "$rss"
        done
        if [[ "$workload_bytes" != "missing" ]]; then
            printf '# workload_emitted_bytes\t%s\n' "$workload_bytes"
            printf '# workload_metrics_sha256\t%s\n' \
                5555555555555555555555555555555555555555555555555555555555555555
            printf '# output_receipt_sha256\t%s\n' \
                6666666666666666666666666666666666666666666666666666666666666666
        fi
        if [[ "$status" != "missing" ]]; then
            printf '# status\t%s\n' "$status"
        fi
    } > "$path"
}

ascii_output="$TEMP_ROOT/ascii.out"
ascii_metrics="$TEMP_ROOT/ascii.tsv"
"$SCRIPT_DIRECTORY/release-performance-workload.sh" \
    --scenario ascii \
    --iterations 2 \
    --metrics "$ascii_metrics" \
    > "$ascii_output"
assert_workload_accounting "$ascii_output" "$ascii_metrics"
assert_equal "2" "$(metric "$ascii_metrics" iterations)" "ASCII iterations"

unicode_output="$TEMP_ROOT/unicode.out"
unicode_metrics="$TEMP_ROOT/unicode.tsv"
"$SCRIPT_DIRECTORY/release-performance-workload.sh" \
    --scenario unicode-styles \
    --iterations 1 \
    --metrics "$unicode_metrics" \
    > "$unicode_output"
assert_workload_accounting "$unicode_output" "$unicode_metrics"
grep -aFq "https://example.test/spaceterm/" "$unicode_output" \
    || fail "Unicode workload did not contain its OSC 8 target"

input_output="$TEMP_ROOT/input.out"
input_metrics="$TEMP_ROOT/input.tsv"
printf 'PRIVATE-PROBE\n' \
    | "$SCRIPT_DIRECTORY/release-performance-workload.sh" \
        --scenario ascii \
        --iterations 32 \
        --metrics "$input_metrics" \
        > "$input_output"
assert_workload_accounting "$input_output" "$input_metrics"
assert_equal "1" "$(metric "$input_metrics" input_events)" "input event count"
assert_equal "13" "$(metric "$input_metrics" input_bytes)" "input byte count"
! grep -aFq "PRIVATE-PROBE" "$input_output" \
    || fail "input acknowledgement exposed terminal input"
! grep -Fq "PRIVATE-PROBE" "$input_metrics" \
    || fail "workload metrics exposed terminal input"

resize_output="$TEMP_ROOT/resize.out"
resize_metrics="$TEMP_ROOT/resize.tsv"
"$SCRIPT_DIRECTORY/release-performance-workload.sh" \
    --scenario resize-seed \
    --metrics "$resize_metrics" \
    > "$resize_output"
assert_workload_accounting "$resize_output" "$resize_metrics"
assert_equal "10000" "$(metric "$resize_metrics" resize_seed_lines)" \
    "resize seed line count"

cleanup_tmp="$TEMP_ROOT/workload-cleanup-tmp"
cleanup_fifo="$TEMP_ROOT/workload-cleanup.fifo"
cleanup_metrics="$TEMP_ROOT/workload-cleanup.tsv"
cleanup_reader_pid_path="$TEMP_ROOT/workload-cleanup-reader.pid"
campaign_secret="$TEMP_ROOT/campaign-secret"
printf '0123456789abcdef0123456789abcdef\n' > "$campaign_secret"
chmod 600 "$campaign_secret"
pty_metrics="$TEMP_ROOT/workload-pty.tsv"
pty_output="$TEMP_ROOT/workload-pty.out"
script -q "$pty_output" "$SCRIPT_DIRECTORY/release-performance-workload.sh" \
    --scenario ascii \
    --duration-seconds 1 \
    --campaign-id pty-campaign \
    --session-id pty-session \
    --campaign-secret-file "$campaign_secret" \
    --metrics "$pty_metrics" </dev/null >/dev/null
assert_equal "complete" "$(metric "$pty_metrics" status)" \
    "PTY workload completion"
assert_equal "pty-no-opost" "$(metric "$pty_metrics" output_mode)" \
    "PTY workload mode"
assert_equal "true" "$(metric "$pty_metrics" opost_disabled)" \
    "PTY workload output processing"
mkdir -p -- "$cleanup_tmp"
mkfifo "$cleanup_fifo"
exec 9<>"$cleanup_fifo"
TMPDIR="$cleanup_tmp" SPACETERM_TEST_ALLOW_REGULAR_OUTPUT=1 \
    SPACETERM_TEST_READER_PID_PATH="$cleanup_reader_pid_path" \
    "$SCRIPT_DIRECTORY/release-performance-workload.sh" \
        --scenario ascii \
        --duration-seconds 1 \
        --campaign-id cleanup-campaign \
        --session-id cleanup-session \
        --campaign-secret-file "$campaign_secret" \
        --metrics "$cleanup_metrics" \
        <&9 >/dev/null &
cleanup_workload_pid=$!
TARGET_PIDS+=("$cleanup_workload_pid")
cleanup_reader_pid=""
for _ in 1 2 3 4 5 6 7 8 9 10; do
    [[ ! -s "$cleanup_reader_pid_path" ]] \
        || cleanup_reader_pid="$(< "$cleanup_reader_pid_path")"
    [[ -z "$cleanup_reader_pid" ]] || break
    sleep 0.1
done
[[ -n "$cleanup_reader_pid" ]] || fail "workload input reader was not observed"
wait "$cleanup_workload_pid"
forget_target_pid "$cleanup_workload_pid"
exec 9>&-
! kill -0 "$cleanup_reader_pid" 2>/dev/null \
    || fail "workload left its input reader running"
[[ -z "$(find "$cleanup_tmp" -name 'spaceterm-performance-input.*' -print -quit)" ]] \
    || fail "workload leaked its private input queue"

signal_tmp="$TEMP_ROOT/workload-signal-tmp"
signal_fifo="$TEMP_ROOT/workload-signal.fifo"
signal_metrics="$TEMP_ROOT/workload-signal.tsv"
signal_reader_pid_path="$TEMP_ROOT/workload-signal-reader.pid"
mkdir -p -- "$signal_tmp"
mkfifo "$signal_fifo"
exec 8<>"$signal_fifo"
TMPDIR="$signal_tmp" SPACETERM_TEST_ALLOW_REGULAR_OUTPUT=1 \
    SPACETERM_TEST_READER_PID_PATH="$signal_reader_pid_path" \
    "$SCRIPT_DIRECTORY/release-performance-workload.sh" \
        --scenario ascii \
        --duration-seconds 10 \
        --campaign-id signal-campaign \
        --session-id signal-session \
        --campaign-secret-file "$campaign_secret" \
        --metrics "$signal_metrics" \
        <&8 >/dev/null &
signal_workload_pid=$!
TARGET_PIDS+=("$signal_workload_pid")
signal_reader_pid=""
for _ in 1 2 3 4 5 6 7 8 9 10; do
    [[ ! -s "$signal_reader_pid_path" ]] \
        || signal_reader_pid="$(< "$signal_reader_pid_path")"
    [[ -z "$signal_reader_pid" ]] || break
    sleep 0.1
done
[[ -n "$signal_reader_pid" ]] || fail "signal cleanup reader was not observed"
kill -TERM "$signal_workload_pid"
signal_status=0
wait "$signal_workload_pid" || signal_status=$?
forget_target_pid "$signal_workload_pid"
exec 8>&-
assert_equal "130" "$signal_status" "workload signal exit status"
! kill -0 "$signal_reader_pid" 2>/dev/null \
    || fail "signaled workload left its input reader running"
[[ -z "$(find "$signal_tmp" -name 'spaceterm-performance-input.*' -print -quit)" ]] \
    || fail "signaled workload leaked its private input queue"

test_app="$TEMP_ROOT/SpaceTerm.app"
test_executable="$test_app/Contents/MacOS/SpaceTerm"
mkdir -p -- "$test_app/Contents/MacOS"
ln -s /bin/sleep "$test_executable"
cat > "$test_app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>SpaceTerm</string>
  <key>CFBundleIdentifier</key><string>io.github.sadiksaifi.spaceterm</string>
  <key>CFBundleShortVersionString</key><string>0.0.0-test</string>
  <key>CFBundleVersion</key><string>1</string>
</dict>
</plist>
PLIST
test_executable_sha256="$(shasum -a 256 "$test_executable" | awk '{ print $1 }')"
test_commit="$(git -C "$SCRIPT_DIRECTORY/.." rev-parse HEAD)"
test_cargo_lock_sha256="$(shasum -a 256 "$SCRIPT_DIRECTORY/../Cargo.lock" | awk '{print $1}')"
fake_process_inspector="$TEMP_ROOT/fake-process-inspector"
cat > "$fake_process_inspector" <<'EOF'
#!/bin/bash
set -euo pipefail
if [[ -n "${FAKE_INSPECTOR_COUNTER:-}" ]]; then
    count=0
    [[ ! -f "$FAKE_INSPECTOR_COUNTER" ]] || count="$(< "$FAKE_INSPECTOR_COUNTER")"
    ((count += 1))
    printf '%d\n' "$count" > "$FAKE_INSPECTOR_COUNTER"
    if (( count >= ${FAKE_INSPECTOR_FAIL_AFTER:-999999} )); then
        exit 1
    fi
    if (( count >= ${FAKE_INSPECTOR_CHANGE_AFTER:-999999} )); then
        printf 'identity_token\tchanged-process-generation\n'
        exit 0
    fi
fi
printf 'identity_token\tstable-process-generation\n'
if [[ "${FAKE_INSPECTOR_LIVE_CODE:-0}" == 1 ]]; then
    printf 'live_code_identity_verified\ttrue\n'
fi
EOF
chmod +x "$fake_process_inspector"

/bin/sleep 15 &
rss_target_pid=$!
TARGET_PIDS+=("$rss_target_pid")
rss_samples="$TEMP_ROOT/rss.tsv"
rss_workload_metrics="$TEMP_ROOT/rss-workload.tsv"
fixture_now="$(date +%s)"
{
    printf 'format_version\t2\n'
    printf 'status\tcomplete\n'
    printf 'campaign_id\ttest-campaign\n'
    printf 'scenario\tascii\n'
    printf 'session_id\ttest-session\n'
    printf 'output_mode\tpty-no-opost\n'
    printf 'opost_disabled\ttrue\n'
    printf 'requested_duration_seconds\t60\n'
    printf 'emitted_bytes\t1073741824\n'
    printf 'seed_sha256\t%s\n' \
        aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
    printf 'started_epoch_seconds\t%s\n' "$((fixture_now - 10))"
    printf 'finished_epoch_seconds\t%s\n' "$((fixture_now + 60))"
} > "$rss_workload_metrics"
sign_evidence "$rss_workload_metrics" "$campaign_secret" metrics_hmac_sha256
fake_identity_sha256="$(printf '%s' stable-process-generation | shasum -a 256 | awk '{print $1}')"
rss_output_receipt="$TEMP_ROOT/rss-output-receipt.tsv"
{
    printf 'format_version\t1\n'
    printf 'source\tspaceterm-terminal-ingestion\n'
    printf 'campaign_id\ttest-campaign\n'
    printf 'scenario\tascii\n'
    printf 'session_id\ttest-session\n'
    printf 'subject_identity_sha256\t%s\n' "$fake_identity_sha256"
    printf 'emitted_bytes\t1073741824\n'
    printf 'seed_sha256\t%s\n' \
        aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
    printf 'started_epoch_seconds\t%s\n' "$((fixture_now - 10))"
    printf 'finished_epoch_seconds\t%s\n' "$((fixture_now + 60))"
    printf 'status\tcomplete\n'
} > "$rss_output_receipt"
sign_evidence "$rss_output_receipt" "$campaign_secret" receipt_hmac_sha256
if SPACETERM_PROCESS_INSPECTOR="$fake_process_inspector" \
    "$SCRIPT_DIRECTORY/sample-release-performance-rss.sh" \
    --pid "$rss_target_pid" \
    --duration-seconds 1 \
    --interval-seconds 1 \
    --output "$rss_samples" \
    --app-bundle "$test_app" \
    --bundle-identifier io.github.sadiksaifi.spaceterm \
    --expected-executable-sha256 "$test_executable_sha256" \
    --expected-commit "$test_commit" \
    --expected-marketing-version 0.0.0-test \
    --expected-build-version 1 \
    --expected-cargo-lock-sha256 "$test_cargo_lock_sha256" \
    --workload-metrics "$rss_workload_metrics" \
    --campaign-id test-campaign \
    --scenario ascii \
    --session-id test-session \
    --campaign-secret-file "$campaign_secret" \
    --output-receipt "$rss_output_receipt" >/dev/null 2>&1; then
    fail "RSS sampler let test overrides produce acceptance evidence"
fi
sample_count="$(awk '!/^#/ && NR > 1 { count += 1 } END { print count + 0 }' "$rss_samples")"
(( sample_count >= 2 )) || fail "RSS sampler produced fewer than two samples"
assert_equal "test-overrides-active" \
    "$(awk -F '\t' '$1 == "# status" { print $2 }' "$rss_samples")" \
    "RSS sampler override status"

misaligned_samples="$TEMP_ROOT/misaligned-rss.tsv"
if SPACETERM_PROCESS_INSPECTOR="$fake_process_inspector" \
    "$SCRIPT_DIRECTORY/sample-release-performance-rss.sh" \
    --pid "$rss_target_pid" \
    --duration-seconds 11 \
    --interval-seconds 10 \
    --output "$misaligned_samples" \
    --app-bundle "$test_app" \
    --bundle-identifier io.github.sadiksaifi.spaceterm \
    --expected-executable-sha256 "$test_executable_sha256" \
    --expected-commit "$test_commit" \
    --expected-marketing-version 0.0.0-test \
    --expected-build-version 1 \
    --expected-cargo-lock-sha256 "$test_cargo_lock_sha256" \
    --workload-metrics "$rss_workload_metrics" \
    --campaign-id test-campaign \
    --scenario ascii \
    --session-id test-session \
    --campaign-secret-file "$campaign_secret" \
    --output-receipt "$rss_output_receipt" >/dev/null 2>&1; then
    fail "RSS sampler accepted a duration not aligned to its interval"
fi

fake_inspector_counter="$TEMP_ROOT/fake-inspector-counter"
reused_pid_samples="$TEMP_ROOT/reused-pid-rss.tsv"
if FAKE_INSPECTOR_COUNTER="$fake_inspector_counter" \
    FAKE_INSPECTOR_CHANGE_AFTER=3 \
    SPACETERM_PROCESS_INSPECTOR="$fake_process_inspector" \
    "$SCRIPT_DIRECTORY/sample-release-performance-rss.sh" \
        --pid "$rss_target_pid" \
        --duration-seconds 1 \
        --interval-seconds 1 \
        --output "$reused_pid_samples" \
        --app-bundle "$test_app" \
        --bundle-identifier io.github.sadiksaifi.spaceterm \
        --expected-executable-sha256 "$test_executable_sha256" \
        --expected-commit "$test_commit" \
        --expected-marketing-version 0.0.0-test \
        --expected-build-version 1 \
        --expected-cargo-lock-sha256 "$test_cargo_lock_sha256" \
        --workload-metrics "$rss_workload_metrics" \
        --campaign-id test-campaign \
        --scenario ascii \
        --session-id test-session \
        --campaign-secret-file "$campaign_secret" \
        --output-receipt "$rss_output_receipt" >/dev/null 2>&1; then
    fail "RSS sampler accepted changed PID start identity"
fi
assert_equal "target-identity-changed-after-rss-read" \
    "$(awk -F '\t' '$1 == "# status" { print $2 }' "$reused_pid_samples")" \
    "RSS PID-reuse status"

fake_unavailable_counter="$TEMP_ROOT/fake-unavailable-counter"
zombie_samples="$TEMP_ROOT/zombie-rss.tsv"
if FAKE_INSPECTOR_COUNTER="$fake_unavailable_counter" \
    FAKE_INSPECTOR_FAIL_AFTER=2 \
    SPACETERM_PROCESS_INSPECTOR="$fake_process_inspector" \
    "$SCRIPT_DIRECTORY/sample-release-performance-rss.sh" \
        --pid "$rss_target_pid" \
        --duration-seconds 1 \
        --interval-seconds 1 \
        --output "$zombie_samples" \
        --app-bundle "$test_app" \
        --bundle-identifier io.github.sadiksaifi.spaceterm \
        --expected-executable-sha256 "$test_executable_sha256" \
        --expected-commit "$test_commit" \
        --expected-marketing-version 0.0.0-test \
        --expected-build-version 1 \
        --expected-cargo-lock-sha256 "$test_cargo_lock_sha256" \
        --workload-metrics "$rss_workload_metrics" \
        --campaign-id test-campaign \
        --scenario ascii \
        --session-id test-session \
        --campaign-secret-file "$campaign_secret" \
        --output-receipt "$rss_output_receipt" >/dev/null 2>&1; then
    fail "RSS sampler accepted a zombie target"
fi
assert_equal "target-identity-changed" \
    "$(awk -F '\t' '$1 == "# status" { print $2 }' "$zombie_samples")" \
    "RSS zombie status"

passing_rss="$TEMP_ROOT/passing-rss.tsv"
write_rss_fixture "$passing_rss" 600 complete exact 100000 100000 100000 100000
passing_report="$TEMP_ROOT/passing-report.tsv"
if awk -f "$SCRIPT_DIRECTORY/analyze-release-performance-rss.awk" \
    "$passing_rss" > "$passing_report"; then
    fail "RSS analyzer accepted operator-authored ingestion evidence"
fi
assert_equal "NOT-RUN" "$(metric "$passing_report" result)" \
    "unattested plateau result"
assert_equal "app-owned-ingestion-attestation-unavailable" \
    "$(metric "$passing_report" reason)" \
    "unattested plateau reason"

midpoint_spike_rss="$TEMP_ROOT/midpoint-spike-rss.tsv"
awk -F '\t' 'BEGIN {OFS = FS} $1 == 300 {$3 = 9999999} {print}' \
    "$passing_rss" > "$midpoint_spike_rss"
midpoint_spike_report="$TEMP_ROOT/midpoint-spike-report.tsv"
if awk -f "$SCRIPT_DIRECTORY/analyze-release-performance-rss.awk" \
    "$midpoint_spike_rss" > "$midpoint_spike_report"; then
    fail "RSS analyzer accepted a spike at the five-minute boundary"
fi
assert_equal "FAIL" "$(metric "$midpoint_spike_report" result)" \
    "five-minute-boundary spike result"

shifted_rss="$TEMP_ROOT/shifted-rss.tsv"
write_rss_fixture "$shifted_rss" 600 complete exact 100000 200000 400000 500000
shifted_report="$TEMP_ROOT/shifted-report.tsv"
if awk -f "$SCRIPT_DIRECTORY/analyze-release-performance-rss.awk" \
    "$shifted_rss" > "$shifted_report"; then
    fail "RSS analyzer accepted an equal-width upward level shift"
fi
assert_equal "FAIL" "$(metric "$shifted_report" result)" \
    "equal-width upward shift result"
assert_equal "true" "$(metric "$shifted_report" range_plateau)" \
    "equal-width range plateau"
assert_equal "false" "$(metric "$shifted_report" no_growth_with_bytes)" \
    "equal-width no-growth rejection"

byte_slope_rss="$TEMP_ROOT/byte-slope-rss.tsv"
write_rss_fixture "$byte_slope_rss" 600 complete exact \
    100000 110000 140000 150000 600 1048576
byte_slope_report="$TEMP_ROOT/byte-slope-report.tsv"
if awk -f "$SCRIPT_DIRECTORY/analyze-release-performance-rss.awk" \
    "$byte_slope_rss" > "$byte_slope_report"; then
    fail "RSS analyzer accepted growth large relative to exact emitted bytes"
fi
assert_equal "false" "$(metric "$byte_slope_report" no_growth_with_bytes)" \
    "absolute no-growth tolerance"
assert_equal "false" "$(metric "$byte_slope_report" byte_normalized_no_growth)" \
    "byte-normalized growth rejection"

failing_rss="$TEMP_ROOT/failing-rss.tsv"
write_rss_fixture "$failing_rss" 600 complete exact 100000 900000 400000 410000
failing_report="$TEMP_ROOT/failing-report.tsv"
if awk -f "$SCRIPT_DIRECTORY/analyze-release-performance-rss.awk" \
    "$failing_rss" > "$failing_report"; then
    fail "RSS analyzer accepted a narrowed final range outside the issue tolerance"
fi
assert_equal "FAIL" "$(metric "$failing_report" result)" "plateau fail result"
assert_equal "790000" "$(metric "$failing_report" range_change_rss_kib)" \
    "false-pass range regression"

missing_status_rss="$TEMP_ROOT/missing-status-rss.tsv"
write_rss_fixture "$missing_status_rss" 600 missing exact 100000 200000 100000 200000
missing_status_report="$TEMP_ROOT/missing-status-report.tsv"
if awk -f "$SCRIPT_DIRECTORY/analyze-release-performance-rss.awk" \
    "$missing_status_rss" > "$missing_status_report" 2>/dev/null; then
    fail "RSS analyzer accepted evidence without complete status"
fi
assert_equal "NOT-RUN" "$(metric "$missing_status_report" result)" \
    "missing-status result"

short_duration_rss="$TEMP_ROOT/short-duration-rss.tsv"
write_rss_fixture "$short_duration_rss" 590 complete exact 100000 200000 100000 200000
short_duration_report="$TEMP_ROOT/short-duration-report.tsv"
if awk -f "$SCRIPT_DIRECTORY/analyze-release-performance-rss.awk" \
    "$short_duration_rss" > "$short_duration_report" 2>/dev/null; then
    fail "RSS analyzer accepted evidence shorter than two five-minute windows"
fi
assert_equal "NOT-RUN" "$(metric "$short_duration_report" result)" \
    "short-duration result"

missing_bytes_rss="$TEMP_ROOT/missing-bytes-rss.tsv"
write_rss_fixture "$missing_bytes_rss" 600 complete exact \
    100000 200000 100000 200000 600 missing
missing_bytes_report="$TEMP_ROOT/missing-bytes-report.tsv"
if awk -f "$SCRIPT_DIRECTORY/analyze-release-performance-rss.awk" \
    "$missing_bytes_rss" > "$missing_bytes_report" 2>/dev/null; then
    fail "RSS analyzer accepted evidence without exact emitted-byte accounting"
fi
assert_equal "NOT-RUN" "$(metric "$missing_bytes_report" result)" \
    "missing workload bytes result"

misaligned_duration_rss="$TEMP_ROOT/misaligned-duration-rss.tsv"
write_rss_fixture "$misaligned_duration_rss" 601 complete exact \
    100000 200000 100000 200000
misaligned_duration_report="$TEMP_ROOT/misaligned-duration-report.tsv"
if awk -f "$SCRIPT_DIRECTORY/analyze-release-performance-rss.awk" \
    "$misaligned_duration_rss" > "$misaligned_duration_report" 2>/dev/null; then
    fail "RSS analyzer accepted duration not aligned to sample cadence"
fi
assert_equal "NOT-RUN" "$(metric "$misaligned_duration_report" result)" \
    "misaligned-duration result"

bad_cadence_rss="$TEMP_ROOT/bad-cadence-rss.tsv"
write_rss_fixture "$bad_cadence_rss" 600 complete late 100000 200000 100000 200000
bad_cadence_report="$TEMP_ROOT/bad-cadence-report.tsv"
if awk -f "$SCRIPT_DIRECTORY/analyze-release-performance-rss.awk" \
    "$bad_cadence_rss" > "$bad_cadence_report" 2>/dev/null; then
    fail "RSS analyzer accepted samples outside the documented cadence tolerance"
fi
assert_equal "NOT-RUN" "$(metric "$bad_cadence_report" result)" \
    "bad-cadence result"

bad_epoch_cadence_rss="$TEMP_ROOT/bad-epoch-cadence-rss.tsv"
write_rss_fixture "$bad_epoch_cadence_rss" 600 complete epoch-late \
    100000 200000 100000 200000
bad_epoch_cadence_report="$TEMP_ROOT/bad-epoch-cadence-report.tsv"
if awk -f "$SCRIPT_DIRECTORY/analyze-release-performance-rss.awk" \
    "$bad_epoch_cadence_rss" > "$bad_epoch_cadence_report" 2>/dev/null; then
    fail "RSS analyzer accepted epoch samples outside the cadence tolerance"
fi
assert_equal "NOT-RUN" "$(metric "$bad_epoch_cadence_report" result)" \
    "bad-epoch-cadence result"

non_monotonic_rss="$TEMP_ROOT/non-monotonic-rss.tsv"
write_rss_fixture "$non_monotonic_rss" 600 complete duplicate 100000 200000 100000 200000
non_monotonic_report="$TEMP_ROOT/non-monotonic-report.tsv"
if awk -f "$SCRIPT_DIRECTORY/analyze-release-performance-rss.awk" \
    "$non_monotonic_rss" > "$non_monotonic_report" 2>/dev/null; then
    fail "RSS analyzer accepted non-monotonic samples"
fi
assert_equal "NOT-RUN" "$(metric "$non_monotonic_report" result)" \
    "non-monotonic result"

short_coverage_rss="$TEMP_ROOT/short-coverage-rss.tsv"
write_rss_fixture "$short_coverage_rss" 600 complete exact 100000 200000 100000 200000 590
short_coverage_report="$TEMP_ROOT/short-coverage-report.tsv"
if awk -f "$SCRIPT_DIRECTORY/analyze-release-performance-rss.awk" \
    "$short_coverage_rss" > "$short_coverage_report" 2>/dev/null; then
    fail "RSS analyzer accepted incomplete requested-duration coverage"
fi
assert_equal "NOT-RUN" "$(metric "$short_coverage_report" result)" \
    "short-coverage result"

runner_child="$TEMP_ROOT/runner-child"
cat > "$runner_child" <<'EOF'
#!/bin/bash
set -euo pipefail
printf '%s\n' "$$" > "${RUNNER_CHILD_PID_PATH:?}"
exec /bin/sleep 30
EOF
chmod +x "$runner_child"
runner_elapsed="$TEMP_ROOT/runner-elapsed.txt"
runner_child_pid_path="$TEMP_ROOT/runner-child.pid"
RUNNER_CHILD_PID_PATH="$runner_child_pid_path" \
    "$SCRIPT_DIRECTORY/run-release-performance-command.py" \
        "$runner_elapsed" "$runner_child" &
runner_pid=$!
TARGET_PIDS+=("$runner_pid")
for _ in 1 2 3 4 5 6 7 8 9 10; do
    [[ -s "$runner_child_pid_path" ]] && break
    sleep 0.1
done
[[ -s "$runner_child_pid_path" ]] || fail "command runner child was not observed"
runner_child_pid="$(< "$runner_child_pid_path")"
TARGET_PIDS+=("$runner_child_pid")
kill -TERM "$runner_pid"
runner_status=0
wait "$runner_pid" || runner_status=$?
forget_target_pid "$runner_pid"
(( runner_status != 0 )) || fail "terminated command runner exited successfully"
! kill -0 "$runner_child_pid" 2>/dev/null \
    || fail "command runner left its child running after TERM"
forget_target_pid "$runner_child_pid"
[[ -s "$runner_elapsed" ]] || fail "command runner did not persist elapsed time"

fake_xcrun="$TEMP_ROOT/fake-xcrun"
cat > "$fake_xcrun" <<'EOF'
#!/bin/bash
set -euo pipefail

: "${FAKE_XCRUN_LOG:?}"
{
    printf 'call'
    for argument in "$@"; do
        printf '\t%s' "$argument"
    done
    printf '\n'
} >> "$FAKE_XCRUN_LOG"

if [[ "$1" == "xcodebuild" ]]; then
    printf 'Xcode 99.0\nBuild version TEST\n'
    exit 0
fi

[[ "$1" == "xctrace" ]] || exit 64
case "$2" in
    list)
        printf 'Time Profiler\nAllocations\nHangs\n'
        ;;
    record)
        output=""
        duration=""
        attached_pid=""
        shift 2
        while (( $# > 0 )); do
            case "$1" in
                --output)
                    output="$2"
                    shift
                    ;;
                --time-limit)
                    duration="${2%s}"
                    shift
                    ;;
                --attach)
                    attached_pid="$2"
                    shift
                    ;;
            esac
            shift
        done
        [[ -n "$output" && -n "$duration" && -n "$attached_pid" ]]
        [[ -z "${FAKE_XCRUN_REQUIRE_NOTIFICATION:-}" \
            || -f "$FAKE_XCRUN_REQUIRE_NOTIFICATION" ]]
        if [[ "${FAKE_XCRUN_KILL_TARGET:-0}" == "1" ]]; then
            kill "$attached_pid"
        fi
        if [[ -n "${FAKE_XCRUN_MUTATE_PLIST:-}" ]]; then
            plutil -replace "${FAKE_XCRUN_MUTATE_PLIST_KEY:-CFBundleIdentifier}" \
                -string "${FAKE_XCRUN_MUTATE_PLIST_VALUE:-io.github.sadiksaifi.changed}" \
                "$FAKE_XCRUN_MUTATE_PLIST"
        fi
        if [[ -n "${FAKE_XCRUN_REPLACE_EXECUTABLE_SOURCE:-}" ]]; then
            mv -- "$FAKE_XCRUN_REPLACE_EXECUTABLE_SOURCE" \
                "${FAKE_XCRUN_REPLACE_EXECUTABLE_TARGET:?}"
        fi
        if [[ -n "${FAKE_XCRUN_REPLACE_FILE_SOURCE:-}" ]]; then
            mv -- "$FAKE_XCRUN_REPLACE_FILE_SOURCE" \
                "${FAKE_XCRUN_REPLACE_FILE_TARGET:?}"
        fi
        if [[ -n "${FAKE_XCRUN_REPLACE_FILE_SOURCE_2:-}" ]]; then
            mv -- "$FAKE_XCRUN_REPLACE_FILE_SOURCE_2" \
                "${FAKE_XCRUN_REPLACE_FILE_TARGET_2:?}"
        fi
        if [[ -n "${FAKE_XCRUN_PUBLISH_RUN_SOURCE:-}" \
            && "${FAKE_XCRUN_SKIP_RUN_PUBLISH:-0}" != 1 ]]; then
            mv -- "$FAKE_XCRUN_PUBLISH_RUN_SOURCE" \
                "${FAKE_XCRUN_PUBLISH_RUN_TARGET:?}"
        fi
        if [[ -n "${FAKE_XCRUN_REPLACE_PUBLISHED_RUN_SOURCE:-}" ]]; then
            mv -- "$FAKE_XCRUN_REPLACE_PUBLISHED_RUN_SOURCE" \
                "${FAKE_XCRUN_PUBLISH_RUN_TARGET:?}"
        fi
        if [[ -n "${FAKE_XCRUN_MUTATE_FILE:-}" ]]; then
            chmod 0600 "$FAKE_XCRUN_MUTATE_FILE"
            printf 'mutated\ttrue\n' >> "$FAKE_XCRUN_MUTATE_FILE"
            chmod 0400 "$FAKE_XCRUN_MUTATE_FILE"
        fi
        if [[ -n "${FAKE_XCRUN_MUTATE_HMAC_FILE:-}" ]]; then
            chmod 0600 "$FAKE_XCRUN_MUTATE_HMAC_FILE"
            sed -i '' $'s/^events_hmac_sha256\t.*/events_hmac_sha256\t0000000000000000000000000000000000000000000000000000000000000000/' \
                "$FAKE_XCRUN_MUTATE_HMAC_FILE"
            chmod 0400 "$FAKE_XCRUN_MUTATE_HMAC_FILE"
        fi
        if [[ "${FAKE_XCRUN_SHORT_CAPTURE:-0}" != "1" ]]; then
            sleep "${FAKE_XCRUN_SLEEP_SECONDS:-$duration}"
        fi
        mkdir -p -- "$output"
        if [[ "${FAKE_XCRUN_EMPTY_TRACE:-0}" != 1 ]]; then
            printf 'fake trace data\n' > "$output/data"
        fi
        ;;
    export)
        output=""
        mode=""
        xpath=""
        shift 2
        while (( $# > 0 )); do
            case "$1" in
                --output) output="$2"; shift ;;
                --toc) mode=toc ;;
                --xpath) mode=xpath; xpath="$2"; shift ;;
            esac
            shift
        done
        [[ -n "$output" ]]
        if [[ "${FAKE_XCRUN_MISSING_TABLES:-0}" == "1" && "$mode" == xpath ]]; then
            exit 65
        elif [[ "${FAKE_XCRUN_MISSING_TABLES:-0}" == "1" ]]; then
            cat > "$output" <<XML
<trace-toc><run number="1"><info><target><process type="attached" name="SpaceTerm" pid="${FAKE_XCRUN_TARGET_PID:?}"/></target><summary><start-date>2026-08-12T00:00:00Z</start-date><end-date>2026-08-12T00:00:01Z</end-date><duration>1</duration></summary></info><processes><process name="SpaceTerm" pid="$FAKE_XCRUN_TARGET_PID"/></processes><data/></run></trace-toc>
XML
        elif [[ "$mode" == toc ]]; then
            duration="${FAKE_XCRUN_TRACE_DURATION:-1.000000}"
            end_date="${FAKE_XCRUN_END_DATE:-2026-08-12T00:00:01Z}"
            toc_pid="${FAKE_XCRUN_TOC_PID:-${FAKE_XCRUN_TARGET_PID:?}}"
            extra_time_profile=""
            extra_hangs=""
            [[ "${FAKE_XCRUN_DUPLICATE_TIME_PROFILE:-0}" != 1 ]] \
                || extra_time_profile='<table schema="time-profile"/>'
            [[ "${FAKE_XCRUN_DUPLICATE_HANGS:-0}" != 1 ]] \
                || extra_hangs='<table schema="potential-hangs"/>'
            cat > "$output" <<XML
<trace-toc>
  <run number="1">
    <info>
      <target><process type="attached" name="SpaceTerm" pid="$toc_pid"/></target>
      <summary><start-date>2026-08-12T00:00:00Z</start-date><end-date>$end_date</end-date><duration>$duration</duration></summary>
    </info>
    <processes><process name="SpaceTerm" pid="$toc_pid"/></processes>
    <data>
      <table schema="time-profile"/>
      $extra_time_profile
      <table schema="potential-hangs"/>
      $extra_hangs
    </data>
    <tracks><track name="Allocations"><details><detail name="Allocations List"/></details></track></tracks>
  </run>
</trace-toc>
XML
        else
            case "$xpath" in
                *time-profile*)
                    if [[ "${FAKE_XCRUN_SCHEMA_ONLY:-0}" == 1 ]]; then
                        printf '<trace-query-result><node><schema name="time-profile"/></node></trace-query-result>\n' > "$output"
                    else
                        row_pid="${FAKE_XCRUN_ROW_PID:-$FAKE_XCRUN_TARGET_PID}"
                        if [[ "${FAKE_XCRUN_ONE_SAMPLE:-0}" == 1 ]]; then
                            printf '<trace-query-result><node><schema name="time-profile"/><row><sample-time>0</sample-time><weight>1</weight><process id="7"><pid>%s</pid></process></row></node></trace-query-result>\n' "$row_pid" > "$output"
                        elif [[ "${FAKE_XCRUN_FULL_ENVELOPE_SAMPLES:-0}" == 1 ]]; then
                            printf '<trace-query-result><node><schema name="time-profile"/><row><sample-time>0</sample-time><weight>1</weight><process id="7"><pid>%s</pid></process></row><row><sample-time>1000000000</sample-time><weight>1</weight><process ref="7"/></row><row><sample-time>2000000000</sample-time><weight>1</weight><process ref="7"/></row><row><sample-time>3000000000</sample-time><weight>1</weight><process ref="7"/></row></node></trace-query-result>\n' "$row_pid" > "$output"
                        else
                            printf '<trace-query-result><node><schema name="time-profile"/><row><sample-time>0</sample-time><weight>1</weight><process id="7"><pid>%s</pid></process></row><row><sample-time>1000000000</sample-time><weight>1</weight><process ref="7"/></row></node></trace-query-result>\n' "$row_pid" > "$output"
                        fi
                    fi
                    ;;
                *Allocations*)
                    if [[ "${FAKE_XCRUN_SCHEMA_ONLY:-0}" == 1 ]]; then
                        printf '<trace-query-result><node/></trace-query-result>\n' > "$output"
                    else
                        row_pid="${FAKE_XCRUN_ALLOCATIONS_PID:-$FAKE_XCRUN_TARGET_PID}"
                        if [[ "${FAKE_XCRUN_EMPTY_ALLOCATIONS:-0}" == 1 ]]; then
                            printf '<trace-query-result><node><process><pid>%s</pid></process></node></trace-query-result>\n' "$row_pid" > "$output"
                        else
                            foreign=""
                            [[ -z "${FAKE_XCRUN_ALLOCATIONS_FOREIGN_PID:-}" ]] \
                                || foreign="<process><pid>${FAKE_XCRUN_ALLOCATIONS_FOREIGN_PID}</pid></process>"
                            printf '<trace-query-result><node><process><pid>%s</pid></process>%s<row timestamp="00:00.1" identifier="1" size="80"/></node></trace-query-result>\n' "$row_pid" "$foreign" > "$output"
                        fi
                    fi
                    ;;
                *potential-hangs*)
                    if [[ "${FAKE_XCRUN_SCHEMA_ONLY:-0}" == 1 ]]; then
                        printf '<trace-query-result><node><schema name="potential-hangs"/></node></trace-query-result>\n' > "$output"
                    else
                        if [[ -n "${FAKE_XCRUN_HANG_PID:-}${FAKE_XCRUN_HANG_DURATION:-}" ]]; then
                            hang_pid="${FAKE_XCRUN_HANG_PID:-$FAKE_XCRUN_TARGET_PID}"
                            hang_duration="${FAKE_XCRUN_HANG_DURATION:-1}"
                            printf '<trace-query-result><node><schema name="potential-hangs"/><process><pid>%s</pid></process><row><start-time>1</start-time><duration>%s</duration><hang-type>potential</hang-type><process><pid>%s</pid></process></row></node></trace-query-result>\n' "$FAKE_XCRUN_TARGET_PID" "$hang_duration" "$hang_pid" > "$output"
                        else
                            row_pid="${FAKE_XCRUN_HANGS_TARGET_PID:-$FAKE_XCRUN_TARGET_PID}"
                            foreign=""
                            [[ -z "${FAKE_XCRUN_HANGS_FOREIGN_PID:-}" ]] \
                                || foreign="<process><pid>${FAKE_XCRUN_HANGS_FOREIGN_PID}</pid></process>"
                            printf '<trace-query-result><node><schema name="potential-hangs"/><process><pid>%s</pid></process>%s</node></trace-query-result>\n' "$row_pid" "$foreign" > "$output"
                        fi
                    fi
                    ;;
                *) exit 65 ;;
            esac
        fi
        ;;
    *)
        exit 64
        ;;
esac
EOF
chmod +x "$fake_xcrun"

"$test_executable" 30 &
trace_target_pid=$!
TARGET_PIDS+=("$trace_target_pid")

# Legacy v2 recorder coverage is retained below as protocol history. The v3
# fail-closed contract is exercised by the adversarial block that follows it.
if false; then
trace_directory="$TEMP_ROOT/trace"
fake_xcrun_log="$TEMP_ROOT/fake-xcrun.log"
if FAKE_XCRUN_LOG="$fake_xcrun_log" FAKE_XCRUN_TARGET_PID="$trace_target_pid" \
SPACETERM_XCRUN="$fake_xcrun" \
    "$SCRIPT_DIRECTORY/record-release-performance-trace.sh" \
        --pid "$trace_target_pid" \
        --application spaceterm \
        --scenario ascii \
        --duration-seconds 1 \
        --output-directory "$trace_directory" \
        --app-bundle "$test_app" \
        --bundle-identifier io.github.sadiksaifi.spaceterm \
        --expected-executable-sha256 "$test_executable_sha256" \
        --expected-commit "$test_commit" \
        --expected-marketing-version 0.0.0-test \
        --expected-build-version 1 \
        --expected-cargo-lock-sha256 "$test_cargo_lock_sha256" \
        >/dev/null 2>&1; then
    fail "trace recorder let test overrides produce acceptance evidence"
fi
trace_metadata="$trace_directory/spaceterm-ascii-trace-metadata.tsv"
assert_equal "INCOMPLETE" "$(metric "$trace_metadata" capture_status)" \
    "trace override capture status"
assert_equal "test-overrides-active" "$(metric "$trace_metadata" incomplete_reason)" \
    "trace override reason"
assert_equal "Allocations,Hangs" "$(metric "$trace_metadata" required_trace_instruments)" \
    "trace instruments"
assert_equal "true" "$(metric "$trace_metadata" trace_tables_verified)" \
    "trace tables verification"
assert_equal "true" "$(metric "$trace_metadata" trace_target_pid_verified)" \
    "trace target PID verification"
assert_equal "2" "$(metric "$trace_metadata" time_profiler_sample_count)" \
    "time profiler sample count"
assert_equal "1" "$(metric "$trace_metadata" allocations_event_count)" \
    "allocations event count"
assert_equal "0" "$(metric "$trace_metadata" hangs_event_count)" \
    "hangs event count"
assert_equal "true" "$(metric "$trace_metadata" target_survived_duration)" \
    "trace target survival"
assert_equal "$test_executable_sha256" "$(metric "$trace_metadata" executable_sha256)" \
    "frozen packaged executable hash"
assert_equal "$test_commit" "$(metric "$trace_metadata" commit)" \
    "frozen packaged commit"
assert_equal "0.0.0-test" "$(metric "$trace_metadata" bundle_marketing_version)" \
    "frozen marketing version"
assert_equal "1" "$(metric "$trace_metadata" bundle_build_version)" \
    "frozen build version"
assert_equal "$test_cargo_lock_sha256" "$(metric "$trace_metadata" cargo_lock_sha256)" \
    "frozen Cargo.lock hash"
[[ -d "$trace_directory/spaceterm-ascii.trace" ]] \
    || fail "trace recorder did not preserve the trace bundle"
[[ -f "$trace_directory/spaceterm-ascii-trace-toc.xml" ]] \
    || fail "trace recorder did not export the trace table of contents"
grep -Fq $'\t--attach\t'"$trace_target_pid" "$fake_xcrun_log" \
    || fail "trace recorder did not attach to the requested process"
grep -Fq $'\t--template\tTime Profiler' "$fake_xcrun_log" \
    || fail "trace recorder did not request the Time Profiler template"
grep -Fq $'\t--instrument\tAllocations' "$fake_xcrun_log" \
    || fail "trace recorder did not request Allocations"
grep -Fq $'\t--instrument\tHangs' "$fake_xcrun_log" \
    || fail "trace recorder did not request Hangs"
! grep -Fq -- $'\t--launch\t' "$fake_xcrun_log" \
    || fail "trace recorder unexpectedly launched an executable"
if awk -F '\t' '$1 == "command" || $1 == "environment" \
    || $1 == "executable_path" || $1 == "terminal_content" {found = 1} \
    END {exit !found}' "$trace_metadata"; then
    fail "trace metadata included a disallowed content-bearing field"
fi
! grep -Fq $'PASS' "$trace_metadata" \
    || fail "trace capture metadata claimed a performance pass"

assert_incomplete_trace() {
    local scenario="$1"
    local expected_reason="$2"
    shift 2
    local directory="$TEMP_ROOT/$scenario-trace"
    if env \
        FAKE_XCRUN_LOG="$fake_xcrun_log" \
        FAKE_XCRUN_TARGET_PID="$trace_target_pid" \
        SPACETERM_XCRUN="$fake_xcrun" \
        "$@" \
        "$SCRIPT_DIRECTORY/record-release-performance-trace.sh" \
            --pid "$trace_target_pid" \
            --application spaceterm \
            --scenario "$scenario" \
            --duration-seconds 1 \
            --output-directory "$directory" \
            --app-bundle "$test_app" \
            --bundle-identifier io.github.sadiksaifi.spaceterm \
            --expected-executable-sha256 "$test_executable_sha256" \
            --expected-commit "$test_commit" \
            --expected-marketing-version 0.0.0-test \
            --expected-build-version 1 \
            --expected-cargo-lock-sha256 "$test_cargo_lock_sha256" \
            >/dev/null 2>&1; then
        fail "trace recorder accepted incomplete $scenario evidence"
    fi
    assert_equal "$expected_reason" \
        "$(metric "$directory/spaceterm-$scenario-trace-metadata.tsv" incomplete_reason)" \
        "$scenario trace reason"
}

assert_incomplete_trace one-sample time-profile-samples-insufficient \
    FAKE_XCRUN_ONE_SAMPLE=1
assert_incomplete_trace wrong-sample-pid time-profile-row-target-mismatch \
    FAKE_XCRUN_ROW_PID=999999
assert_incomplete_trace wrong-hang-pid hang-row-target-mismatch \
    FAKE_XCRUN_HANG_PID=999999
assert_incomplete_trace inconsistent-duration trace-summary-duration-is-inconsistent \
    FAKE_XCRUN_TRACE_DURATION=1.5
trace_generation_counter="$TEMP_ROOT/trace-generation-counter"
assert_incomplete_trace changed-generation target-did-not-survive-duration \
    FAKE_INSPECTOR_COUNTER="$trace_generation_counter" \
    FAKE_INSPECTOR_CHANGE_AFTER=2 \
    SPACETERM_PROCESS_INSPECTOR="$fake_process_inspector"

wrong_trace_pid_directory="$TEMP_ROOT/wrong-trace-pid"
if FAKE_XCRUN_LOG="$fake_xcrun_log" FAKE_XCRUN_TARGET_PID="$trace_target_pid" \
    FAKE_XCRUN_TOC_PID=999999 \
    SPACETERM_XCRUN="$fake_xcrun" \
    "$SCRIPT_DIRECTORY/record-release-performance-trace.sh" \
        --pid "$trace_target_pid" \
        --application spaceterm \
        --scenario wrong-trace-pid \
        --duration-seconds 1 \
        --output-directory "$wrong_trace_pid_directory" \
        --app-bundle "$test_app" \
        --bundle-identifier io.github.sadiksaifi.spaceterm \
        --expected-executable-sha256 "$test_executable_sha256" \
        --expected-commit "$test_commit" \
        --expected-marketing-version 0.0.0-test \
        --expected-build-version 1 \
        --expected-cargo-lock-sha256 "$test_cargo_lock_sha256" \
        >/dev/null 2>&1; then
    fail "trace recorder accepted trace metadata for a different PID"
fi
assert_equal "trace-target-identity-mismatch" \
    "$(metric "$wrong_trace_pid_directory/spaceterm-wrong-trace-pid-trace-metadata.tsv" incomplete_reason)" \
    "wrong trace PID reason"

identity_directory="$TEMP_ROOT/wrong-identity-trace"
if FAKE_XCRUN_LOG="$fake_xcrun_log" FAKE_XCRUN_TARGET_PID="$trace_target_pid" \
    SPACETERM_XCRUN="$fake_xcrun" \
    "$SCRIPT_DIRECTORY/record-release-performance-trace.sh" \
        --pid "$trace_target_pid" \
        --application spaceterm \
        --scenario wrong-identity \
        --duration-seconds 1 \
        --output-directory "$identity_directory" \
        --app-bundle "$test_app" \
        --bundle-identifier io.github.sadiksaifi.wrong \
        --expected-executable-sha256 "$test_executable_sha256" \
        --expected-commit "$test_commit" \
        --expected-marketing-version 0.0.0-test \
        --expected-build-version 1 \
        --expected-cargo-lock-sha256 "$test_cargo_lock_sha256" \
        >/dev/null 2>&1; then
    fail "trace recorder accepted a mismatched bundle identity"
fi

hash_directory="$TEMP_ROOT/wrong-hash-trace"
if FAKE_XCRUN_LOG="$fake_xcrun_log" FAKE_XCRUN_TARGET_PID="$trace_target_pid" \
    SPACETERM_XCRUN="$fake_xcrun" \
    "$SCRIPT_DIRECTORY/record-release-performance-trace.sh" \
        --pid "$trace_target_pid" \
        --application spaceterm \
        --scenario wrong-hash \
        --duration-seconds 1 \
        --output-directory "$hash_directory" \
        --app-bundle "$test_app" \
        --bundle-identifier io.github.sadiksaifi.spaceterm \
        --expected-executable-sha256 \
            0000000000000000000000000000000000000000000000000000000000000000 \
        --expected-commit "$test_commit" \
        --expected-marketing-version 0.0.0-test \
        --expected-build-version 1 \
        --expected-cargo-lock-sha256 "$test_cargo_lock_sha256" \
        >/dev/null 2>&1; then
    fail "trace recorder accepted a mismatched packaged executable hash"
fi

commit_directory="$TEMP_ROOT/wrong-commit-trace"
if FAKE_XCRUN_LOG="$fake_xcrun_log" FAKE_XCRUN_TARGET_PID="$trace_target_pid" \
    SPACETERM_XCRUN="$fake_xcrun" \
    "$SCRIPT_DIRECTORY/record-release-performance-trace.sh" \
        --pid "$trace_target_pid" \
        --application spaceterm \
        --scenario wrong-commit \
        --duration-seconds 1 \
        --output-directory "$commit_directory" \
        --app-bundle "$test_app" \
        --bundle-identifier io.github.sadiksaifi.spaceterm \
        --expected-executable-sha256 "$test_executable_sha256" \
        --expected-commit 0000000000000000000000000000000000000000 \
        --expected-marketing-version 0.0.0-test \
        --expected-build-version 1 \
        --expected-cargo-lock-sha256 "$test_cargo_lock_sha256" \
        >/dev/null 2>&1; then
    fail "trace recorder accepted a mismatched repository commit"
fi

short_trace_directory="$TEMP_ROOT/short-trace"
if FAKE_XCRUN_LOG="$fake_xcrun_log" FAKE_XCRUN_TARGET_PID="$trace_target_pid" \
    FAKE_XCRUN_SHORT_CAPTURE=1 \
    FAKE_XCRUN_TRACE_DURATION=0.5 \
    FAKE_XCRUN_END_DATE=2026-08-12T00:00:00.500000Z \
    SPACETERM_XCRUN="$fake_xcrun" \
    "$SCRIPT_DIRECTORY/record-release-performance-trace.sh" \
        --pid "$trace_target_pid" \
        --application spaceterm \
        --scenario short \
        --duration-seconds 1 \
        --output-directory "$short_trace_directory" \
        --app-bundle "$test_app" \
        --bundle-identifier io.github.sadiksaifi.spaceterm \
        --expected-executable-sha256 "$test_executable_sha256" \
        --expected-commit "$test_commit" \
        --expected-marketing-version 0.0.0-test \
        --expected-build-version 1 \
        --expected-cargo-lock-sha256 "$test_cargo_lock_sha256" \
        >/dev/null 2>&1; then
    fail "trace recorder accepted a capture shorter than the requested duration"
fi
short_trace_metadata="$short_trace_directory/spaceterm-short-trace-metadata.tsv"
assert_equal "INCOMPLETE" "$(metric "$short_trace_metadata" capture_status)" \
    "short trace status"
assert_equal "requested-duration-not-covered" \
    "$(metric "$short_trace_metadata" incomplete_reason)" \
    "short trace reason"

missing_tables_directory="$TEMP_ROOT/missing-tables-trace"
if FAKE_XCRUN_LOG="$fake_xcrun_log" FAKE_XCRUN_TARGET_PID="$trace_target_pid" \
    FAKE_XCRUN_MISSING_TABLES=1 \
    SPACETERM_XCRUN="$fake_xcrun" \
    "$SCRIPT_DIRECTORY/record-release-performance-trace.sh" \
        --pid "$trace_target_pid" \
        --application spaceterm \
        --scenario missing-tables \
        --duration-seconds 1 \
        --output-directory "$missing_tables_directory" \
        --app-bundle "$test_app" \
        --bundle-identifier io.github.sadiksaifi.spaceterm \
        --expected-executable-sha256 "$test_executable_sha256" \
        --expected-commit "$test_commit" \
        --expected-marketing-version 0.0.0-test \
        --expected-build-version 1 \
        --expected-cargo-lock-sha256 "$test_cargo_lock_sha256" \
        >/dev/null 2>&1; then
    fail "trace recorder accepted a TOC without the required tables"
fi
missing_tables_metadata="$missing_tables_directory/spaceterm-missing-tables-trace-metadata.tsv"
assert_equal "INCOMPLETE" "$(metric "$missing_tables_metadata" capture_status)" \
    "missing-tables trace status"
assert_equal "trace-table-export-failed" \
    "$(metric "$missing_tables_metadata" incomplete_reason)" \
    "missing-tables trace reason"

mutated_app="$TEMP_ROOT/Mutated.app"
mkdir -p -- "$mutated_app/Contents/MacOS"
ln -s /bin/sleep "$mutated_app/Contents/MacOS/SpaceTerm"
cp "$test_app/Contents/Info.plist" "$mutated_app/Contents/Info.plist"
mutated_package_directory="$TEMP_ROOT/mutated-package-trace"
if FAKE_XCRUN_LOG="$fake_xcrun_log" FAKE_XCRUN_TARGET_PID="$trace_target_pid" \
    FAKE_XCRUN_MUTATE_PLIST="$mutated_app/Contents/Info.plist" \
    SPACETERM_XCRUN="$fake_xcrun" \
    "$SCRIPT_DIRECTORY/record-release-performance-trace.sh" \
        --pid "$trace_target_pid" \
        --application spaceterm \
        --scenario mutated-package \
        --duration-seconds 1 \
        --output-directory "$mutated_package_directory" \
        --app-bundle "$mutated_app" \
        --bundle-identifier io.github.sadiksaifi.spaceterm \
        --expected-executable-sha256 "$test_executable_sha256" \
        --expected-commit "$test_commit" \
        --expected-marketing-version 0.0.0-test \
        --expected-build-version 1 \
        --expected-cargo-lock-sha256 "$test_cargo_lock_sha256" \
        >/dev/null 2>&1; then
    fail "trace recorder accepted a package identity that changed during capture"
fi
mutated_package_metadata="$mutated_package_directory/spaceterm-mutated-package-trace-metadata.tsv"
assert_equal "INCOMPLETE" "$(metric "$mutated_package_metadata" capture_status)" \
    "mutated-package trace status"
assert_equal "package-identity-changed" \
    "$(metric "$mutated_package_metadata" incomplete_reason)" \
    "mutated-package trace reason"

for mutation in marketing build; do
    version_app="$TEMP_ROOT/Version-$mutation.app"
    mkdir -p -- "$version_app/Contents/MacOS"
    ln -s /bin/sleep "$version_app/Contents/MacOS/SpaceTerm"
    cp "$test_app/Contents/Info.plist" "$version_app/Contents/Info.plist"
    version_directory="$TEMP_ROOT/version-$mutation-trace"
    if [[ "$mutation" == marketing ]]; then
        plist_key=CFBundleShortVersionString
    else
        plist_key=CFBundleVersion
    fi
    if FAKE_XCRUN_LOG="$fake_xcrun_log" FAKE_XCRUN_TARGET_PID="$trace_target_pid" \
        FAKE_XCRUN_MUTATE_PLIST="$version_app/Contents/Info.plist" \
        FAKE_XCRUN_MUTATE_PLIST_KEY="$plist_key" \
        FAKE_XCRUN_MUTATE_PLIST_VALUE=changed \
        SPACETERM_XCRUN="$fake_xcrun" \
        "$SCRIPT_DIRECTORY/record-release-performance-trace.sh" \
            --pid "$trace_target_pid" \
            --application spaceterm \
            --scenario "version-$mutation" \
            --duration-seconds 1 \
            --output-directory "$version_directory" \
            --app-bundle "$version_app" \
            --bundle-identifier io.github.sadiksaifi.spaceterm \
            --expected-executable-sha256 "$test_executable_sha256" \
            --expected-commit "$test_commit" \
            --expected-marketing-version 0.0.0-test \
            --expected-build-version 1 \
            --expected-cargo-lock-sha256 "$test_cargo_lock_sha256" \
            >/dev/null 2>&1; then
        fail "trace recorder accepted changed $mutation version"
    fi
    assert_equal "package-identity-changed" \
        "$(metric "$version_directory/spaceterm-version-$mutation-trace-metadata.tsv" incomplete_reason)" \
        "$mutation-version trace reason"
done

fake_shasum="$TEMP_ROOT/fake-shasum"
fake_shasum_counter="$TEMP_ROOT/fake-shasum-counter"
cat > "$fake_shasum" <<'EOF'
#!/bin/bash
set -euo pipefail
: "${FAKE_SHASUM_COUNTER:?}"
path="${*: -1}"
if [[ "$path" == */Cargo.lock ]]; then
    count=0
    [[ ! -f "$FAKE_SHASUM_COUNTER" ]] || count="$(< "$FAKE_SHASUM_COUNTER")"
    ((count += 1))
    printf '%d\n' "$count" > "$FAKE_SHASUM_COUNTER"
    if (( count >= 2 )); then
        printf '0000000000000000000000000000000000000000000000000000000000000000  %s\n' "$path"
        exit 0
    fi
fi
exec /usr/bin/shasum "$@"
EOF
chmod +x "$fake_shasum"
lock_directory="$TEMP_ROOT/cargo-lock-mutation-trace"
if FAKE_XCRUN_LOG="$fake_xcrun_log" FAKE_XCRUN_TARGET_PID="$trace_target_pid" \
    FAKE_SHASUM_COUNTER="$fake_shasum_counter" SPACETERM_SHASUM="$fake_shasum" \
    SPACETERM_XCRUN="$fake_xcrun" \
    "$SCRIPT_DIRECTORY/record-release-performance-trace.sh" \
        --pid "$trace_target_pid" \
        --application spaceterm \
        --scenario cargo-lock-mutation \
        --duration-seconds 1 \
        --output-directory "$lock_directory" \
        --app-bundle "$test_app" \
        --bundle-identifier io.github.sadiksaifi.spaceterm \
        --expected-executable-sha256 "$test_executable_sha256" \
        --expected-commit "$test_commit" \
        --expected-marketing-version 0.0.0-test \
        --expected-build-version 1 \
        --expected-cargo-lock-sha256 "$test_cargo_lock_sha256" \
        >/dev/null 2>&1; then
    fail "trace recorder accepted changed Cargo.lock"
fi
assert_equal "package-identity-changed" \
    "$(metric "$lock_directory/spaceterm-cargo-lock-mutation-trace-metadata.tsv" incomplete_reason)" \
    "Cargo.lock-mutation trace reason"

schema_only_directory="$TEMP_ROOT/schema-only-trace"
if FAKE_XCRUN_LOG="$fake_xcrun_log" FAKE_XCRUN_TARGET_PID="$trace_target_pid" \
    FAKE_XCRUN_SCHEMA_ONLY=1 \
    SPACETERM_XCRUN="$fake_xcrun" \
    "$SCRIPT_DIRECTORY/record-release-performance-trace.sh" \
        --pid "$trace_target_pid" \
        --application spaceterm \
        --scenario schema-only \
        --duration-seconds 1 \
        --output-directory "$schema_only_directory" \
        --app-bundle "$test_app" \
        --bundle-identifier io.github.sadiksaifi.spaceterm \
        --expected-executable-sha256 "$test_executable_sha256" \
        --expected-commit "$test_commit" \
        --expected-marketing-version 0.0.0-test \
        --expected-build-version 1 \
        --expected-cargo-lock-sha256 "$test_cargo_lock_sha256" \
        >/dev/null 2>&1; then
    fail "trace recorder accepted schema-only exports without actual events"
fi
assert_equal "time-profile-samples-insufficient" \
    "$(metric "$schema_only_directory/spaceterm-schema-only-trace-metadata.tsv" incomplete_reason)" \
    "schema-only trace reason"

dying_app="$TEMP_ROOT/Dying.app"
mkdir -p -- "$dying_app/Contents/MacOS"
ln -s /bin/sleep "$dying_app/Contents/MacOS/SpaceTerm"
cp "$test_app/Contents/Info.plist" "$dying_app/Contents/Info.plist"
dying_hash="$(shasum -a 256 "$dying_app/Contents/MacOS/SpaceTerm" | awk '{ print $1 }')"
/bin/sleep 30 &
dying_target_pid=$!
TARGET_PIDS+=("$dying_target_pid")
dying_target_directory="$TEMP_ROOT/dying-target-trace"
if FAKE_XCRUN_LOG="$fake_xcrun_log" FAKE_XCRUN_TARGET_PID="$dying_target_pid" \
    FAKE_XCRUN_KILL_TARGET=1 \
    SPACETERM_XCRUN="$fake_xcrun" \
    "$SCRIPT_DIRECTORY/record-release-performance-trace.sh" \
        --pid "$dying_target_pid" \
        --application spaceterm \
        --scenario dying-target \
        --duration-seconds 1 \
        --output-directory "$dying_target_directory" \
        --app-bundle "$dying_app" \
        --bundle-identifier io.github.sadiksaifi.spaceterm \
        --expected-executable-sha256 "$dying_hash" \
        --expected-commit "$test_commit" \
        --expected-marketing-version 0.0.0-test \
        --expected-build-version 1 \
        --expected-cargo-lock-sha256 "$test_cargo_lock_sha256" \
        >/dev/null 2>&1; then
    fail "trace recorder accepted a target that did not survive the duration"
fi
dying_target_metadata="$dying_target_directory/spaceterm-dying-target-trace-metadata.tsv"
assert_equal "INCOMPLETE" "$(metric "$dying_target_metadata" capture_status)" \
    "dying-target trace status"
assert_equal "target-did-not-survive-duration" \
    "$(metric "$dying_target_metadata" incomplete_reason)" \
    "dying-target trace reason"
fi

# Strict v3 trace recorder fixtures.
trace_app="$TEMP_ROOT/Trace.app"
trace_executable="$trace_app/Contents/MacOS/SpaceTerm"
mkdir -p -- "$trace_app/Contents/MacOS"
cp /bin/sleep "$trace_executable"
chmod 0555 "$trace_executable"
cp "$test_app/Contents/Info.plist" "$trace_app/Contents/Info.plist"
codesign --force --sign - --identifier io.github.sadiksaifi.spaceterm \
    "$trace_app" >/dev/null 2>&1
"$trace_executable" 90 &
v3_target_pid=$!
TARGET_PIDS+=("$v3_target_pid")
v3_start_identity="$("$SCRIPT_DIRECTORY/inspect-release-performance-process.py" \
    --pid "$v3_target_pid" --print-start-identity \
    | awk -F '\t' '$1 == "process_start_identity" { print $2 }')"
v3_hash="$(shasum -a 256 "$trace_executable" | awk '{print $1}')"
v3_device="$(stat -f '%d' "$(realpath "$trace_executable")")"
v3_inode="$(stat -f '%i' "$(realpath "$trace_executable")")"
v3_signature="$(codesign -d --verbose=4 "$trace_executable" 2>&1 | awk -F= '
    $1 == "Identifier" { identifier = $2 }
    $1 == "TeamIdentifier" { team = $2 }
    $1 == "CDHash" { cdhash = tolower($2) }
    END { if (team == "" || team == "not set") team = "none";
          print identifier "\t" team "\t" cdhash }
')"
IFS=$'\t' read -r v3_identifier v3_team v3_cdhash <<< "$v3_signature"
readonly V3_HASH_A=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
readonly V3_HASH_B=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
readonly V3_HASH_C=cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
readonly V3_HASH_D=dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd
readonly V3_HASH_E=eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee
readonly V3_HASH_F=ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff

v3_subject="$TEMP_ROOT/v3-subject.tsv"
{
    printf 'format_version\t1\nsubject\tspaceterm\napp_bundle_path\t%s\n' "$trace_app"
    printf 'bundle_identifier\tio.github.sadiksaifi.spaceterm\n'
    printf 'bundle_version\t0.0.0-test+1\nexecutable_path\t%s\n' "$trace_executable"
    printf 'executable_sha256\t%s\nexecutable_device\t%s\n' "$v3_hash" "$v3_device"
    printf 'executable_inode\t%s\nexecutable_fsid\t%s\n' "$v3_inode" "$v3_device"
    printf 'signature_valid\ttrue\nsigning_identifier\t%s\n' "$v3_identifier"
    printf 'team_identifier\t%s\ncdhash\t%s\n' "$v3_team" "$v3_cdhash"
    printf 'process_pid\t%s\nprocess_start_identity\t%s\nidentity_status\tfrozen\n' \
        "$v3_target_pid" "$v3_start_identity"
} > "$v3_subject"
chmod 0444 "$v3_subject"
v3_subject_hash="$(shasum -a 256 "$v3_subject" | awk '{print $1}')"
v3_run_intent="$TEMP_ROOT/v3-run-intent.tsv"
{
    printf 'format_version\t1\nsubject\tspaceterm\nsubject_identity_sha256\t%s\n' "$v3_subject_hash"
    printf 'scenario\tascii\nscenario_plan_sha256\t%s\n' "$V3_HASH_A"
    printf 'workload_sha256\t%s\ncommand_sha256\t%s\n' "$V3_HASH_B" "$V3_HASH_C"
    printf 'environment_sha256\t%s\nfont_sha256\t%s\n' "$V3_HASH_D" "$V3_HASH_E"
    printf 'initial_grid_sha256\t%s\nmeasured_duration_ms\t1000\n' "$V3_HASH_F"
    printf 'process_pid\t%s\nprocess_start_identity\t%s\n' \
        "$v3_target_pid" "$v3_start_identity"
    printf 'campaign_id\ttrace-campaign\nsession_id\ttrace-session\n'
    printf 'nonce\t1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef\n'
    printf 'native_provisional_observation_sha256\t%s\nstatus\tprepared\n' "$V3_HASH_A"
} > "$v3_run_intent"
chmod 0444 "$v3_run_intent"
v3_run_intent_hash="$(shasum -a 256 "$v3_run_intent" | awk '{ print $1 }')"

fake_clock="$TEMP_ROOT/fake-continuous-clock"
cat > "$fake_clock" <<'EOF'
#!/bin/bash
set -euo pipefail
counter="${FAKE_CLOCK_COUNTER:?}"
count=0
[[ ! -e "$counter" ]] || count="$(< "$counter")"
((count += 1))
printf '%s\n' "$count" > "$counter"
if (( count == 1 )); then
    printf '%s\t%s\t%s\n' "${FAKE_CLOCK_START_NS:-1000000000}" \
        "${FAKE_CLOCK_START_EPOCH_NS:-1786492800000000000}" \
        "${FAKE_CLOCK_START_WIDTH_NS:-0}"
else
    printf '%s\t%s\t%s\n' "${FAKE_CLOCK_END_NS:-2000000000}" \
        "${FAKE_CLOCK_END_EPOCH_NS:-1786492801000000000}" \
        "${FAKE_CLOCK_END_WIDTH_NS:-0}"
fi
EOF
chmod +x "$fake_clock"

readonly V3_CAMPAIGN_ID=trace-campaign
readonly V3_SESSION_ID=trace-session
readonly V3_NONCE=1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef

write_v3_final_run() {
    local path="$1" intent_hash="${2:-$v3_run_intent_hash}"
    {
        printf 'format_version\t2\n'
        for key in subject subject_identity_sha256 scenario scenario_plan_sha256 \
            workload_sha256 command_sha256 environment_sha256 font_sha256 \
            initial_grid_sha256 measured_duration_ms process_pid process_start_identity; do
            printf '%s\t%s\n' "$key" "$(metric "$v3_run_intent" "$key")"
        done
        printf 'run_intent_sha256\t%s\n' "$intent_hash"
        printf 'native_observation_sha256\t%s\n' "$V3_HASH_C"
        printf 'native_runtime_metadata_sha256\t%s\n' "$V3_HASH_D"
        printf 'native_failure_actions_sha256\t%s\n' "$V3_HASH_E"
        printf 'native_failure_action_enabled\tfalse\n'
        printf 'native_failure_request_count\t0\nnative_failure_result_count\t0\n'
        printf 'native_failure_resource_staged_count\t0\n'
        printf 'native_failure_resource_staged_bytes\t0\n'
        printf 'native_failure_resource_rolled_back_count\t0\n'
        printf 'native_failure_resource_rolled_back_bytes\t0\nstatus\tcomplete\n'
    } > "$path"
    chmod 0444 "$path"
}
v3_secret="$TEMP_ROOT/v3-secret"
printf '0123456789abcdef0123456789abcdef\n' > "$v3_secret"
chmod 0400 "$v3_secret"
v3_events="$TEMP_ROOT/v3-events.tsv"
printf 'sequence\tcontinuous_ns\tkind\n0\t1100000000\tstarted\n' > "$v3_events"
chmod 0400 "$v3_events"
v3_events_hash="$(shasum -a 256 "$v3_events" | awk '{ print $1 }')"
v3_ready="$TEMP_ROOT/v3-ready.tsv"
v3_events_device="$(stat -f '%d' "$v3_events")"
v3_events_inode="$(stat -f '%i' "$v3_events")"
v3_events_bytes="$(wc -c < "$v3_events" | tr -d '[:space:]')"
{
    printf 'format_version\t1\ncampaign_id\t%s\nsession_id\t%s\nnonce\t%s\n' \
        "$V3_CAMPAIGN_ID" "$V3_SESSION_ID" "$V3_NONCE"
    printf 'subject_identity_sha256\t%s\nproducer_pid\t12345\n' "$v3_subject_hash"
    printf 'producer_started_continuous_ns\t500000000\nproducer_session_id\t12345\n'
    printf 'producer_process_group\t12345\ntty_device\t1\ntty_inode\t2\ntty_rdev\t3\n'
    printf 'events_device\t%s\nevents_inode\t%s\nevents_prefix_bytes\t%s\n' \
        "$v3_events_device" "$v3_events_inode" "$v3_events_bytes"
    printf 'events_prefix_sha256\t%s\nmeasurement_ready_continuous_ns\t900000000\n' \
        "$v3_events_hash"
    printf 'measurement_ready_byte_count\t80\nauth_algorithm\thmac-sha256\n'
} > "$v3_ready"
python3 - "$v3_ready" "$v3_secret" <<'PY'
import hashlib, hmac, pathlib, struct, sys
path = pathlib.Path(sys.argv[1])
unsigned = path.read_bytes()
authenticated = b"spaceterm.performance.workload-ready/v1\0" + struct.pack(">Q", len(unsigned)) + unsigned
with path.open("ab") as destination:
    destination.write(b"ready_hmac_sha256\t" + hmac.new(
        pathlib.Path(sys.argv[2]).read_bytes(), authenticated, hashlib.sha256
    ).hexdigest().encode() + b"\n")
PY
chmod 0400 "$v3_ready"
v3_ready_hash="$(shasum -a 256 "$v3_ready" | awk '{ print $1 }')"

write_v3_workload() {
    local path="$1" start="${2:-1000000000}" end="${3:-2000000000}"
    local producer_sha256="${4:-$V3_HASH_B}"
    {
        printf 'format_version\t3\nscenario\tascii\ncampaign_id\t%s\n' "$V3_CAMPAIGN_ID"
        printf 'session_id\t%s\nnonce\t%s\nsubject_identity_sha256\t%s\n' \
            "$V3_SESSION_ID" "$V3_NONCE" "$v3_subject_hash"
        printf 'subject_process_pid\t%s\nsubject_process_start_identity\t%s\n' \
            "$v3_target_pid" "$v3_start_identity"
        printf 'producer_sha256\t%s\nproducer_pid\t12345\n' "$producer_sha256"
        printf 'producer_started_continuous_ns\t500000000\nproducer_session_id\t12345\n'
        printf 'producer_process_group\t12345\ntty_device\t1\ntty_inode\t2\ntty_rdev\t3\n'
        printf 'ready_receipt_sha256\t%s\n' "$v3_ready_hash"
        printf 'events_sha256\t%s\nauth_algorithm\thmac-sha256\n' "$v3_events_hash"
        printf 'seed_sha256\t%s\nseed_bytes\t80\nrequested_duration_ms\t1000\n' "$V3_HASH_B"
        printf 'warmup_ms\t0\nrequested_iterations\t0\nrequested_seed_rows\t0\n'
        printf 'emitted_bytes\t80\ninput_events\t0\nplan_start_continuous_ns\t%s\n' "$start"
        printf 'started_continuous_ns\t%s\n' "$start"
        printf 'ended_continuous_ns\t%s\nstatus\tcomplete\n' "$end"
    } > "$path"
    python3 - "$path" "$v3_events" "$v3_secret" <<'PY'
import hashlib, hmac, pathlib, struct, sys
metadata = pathlib.Path(sys.argv[1])
unsigned = metadata.read_bytes()
events = pathlib.Path(sys.argv[2]).read_bytes()
secret = pathlib.Path(sys.argv[3]).read_bytes()
authenticated = (b"spaceterm.performance.workload-auth/v1\0"
    + struct.pack(">Q", len(unsigned)) + unsigned
    + struct.pack(">Q", len(events)) + events)
with metadata.open("ab") as destination:
    destination.write(b"events_hmac_sha256\t" + hmac.new(
        secret, authenticated, hashlib.sha256).hexdigest().encode() + b"\n")
PY
    chmod 0444 "$path"
}

v3_log="$TEMP_ROOT/v3-xcrun.log"
v3_supplemental="$TEMP_ROOT/v3-supplemental.tsv"
printf 'format_version\t1\nstatus\tcomplete\n' > "$v3_supplemental"
chmod 0444 "$v3_supplemental"
run_v3_incomplete() {
    local name="$1" expected="$2" start="${3:-1000000000}" end="${4:-2000000000}"
    shift 4 || true
    local directory="$TEMP_ROOT/v3-$name" workload="$TEMP_ROOT/v3-$name-workload.tsv"
    local run_metadata="$TEMP_ROOT/v3-$name-run.tsv"
    local run_source="$TEMP_ROOT/v3-$name-run.pending"
    local provisional="$TEMP_ROOT/v3-$name-provisional.tsv"
    local recorder_error="$TEMP_ROOT/v3-$name-recorder.err"
    local late_publisher_pid=""
    local -a scenario_environment=("SPACETERM_TEST_TRACE_SCENARIO=$name")
    local -a recorder_arguments=(
        --subject-identity "$v3_subject" --run-intent "$v3_run_intent"
        --run-metadata "$run_metadata" --provisional-receipt "$provisional"
        --workload-metadata "$workload" --workload-events "$v3_events"
        --workload-ready-receipt "$v3_ready"
        --campaign-secret-file "$v3_secret" --campaign-id "$V3_CAMPAIGN_ID"
        --session-id "$V3_SESSION_ID" --nonce "$V3_NONCE"
        --scenario ascii --warmup-ms 0 --duration-ms 1000
    )
    local producer_sha256="$V3_HASH_B"
    [[ "$name" != producer-mismatch ]] || producer_sha256="$V3_HASH_A"
    write_v3_workload "$workload" "$start" "$end" "$producer_sha256"
    write_v3_final_run "$run_source"
    scenario_environment+=(
        "FAKE_XCRUN_PUBLISH_RUN_SOURCE=$run_source"
        "FAKE_XCRUN_PUBLISH_RUN_TARGET=$run_metadata"
    )
    case "$name" in
        invalid-hmac) scenario_environment+=("FAKE_XCRUN_MUTATE_HMAC_FILE=$workload") ;;
        mutated-run) scenario_environment+=("FAKE_XCRUN_MUTATE_FILE=$v3_run_intent") ;;
        cross-intent)
            chmod 0600 "$run_source"
            sed -i '' $'s/^run_intent_sha256\t.*/run_intent_sha256\taaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/' "$run_source"
            chmod 0444 "$run_source"
            ;;
        malformed-final)
            chmod 0600 "$run_source"
            printf 'unknown\tfield\n' >> "$run_source"
            chmod 0444 "$run_source"
            ;;
        replaced-final)
            local replacement="$TEMP_ROOT/v3-$name-run.replacement"
            write_v3_final_run "$replacement" "$V3_HASH_A"
            scenario_environment+=("FAKE_XCRUN_REPLACE_PUBLISHED_RUN_SOURCE=$replacement")
            ;;
        missing-final)
            scenario_environment+=(
                "FAKE_XCRUN_SKIP_RUN_PUBLISH=1"
                "SPACETERM_TEST_RUN_METADATA_WAIT_TENTHS=2"
            )
            ;;
        late-final)
            scenario_environment+=(
                "FAKE_XCRUN_SKIP_RUN_PUBLISH=1"
                "SPACETERM_TEST_RUN_METADATA_WAIT_TENTHS=2"
            )
            ( sleep 2; mv -- "$run_source" "$run_metadata" ) &
            late_publisher_pid=$!
            TARGET_PIDS+=("$late_publisher_pid")
            ;;
        mutated-plist-identifier)
            scenario_environment+=("FAKE_XCRUN_MUTATE_PLIST=$trace_app/Contents/Info.plist")
            ;;
        mutated-plist-marketing-version)
            scenario_environment+=(
                "FAKE_XCRUN_MUTATE_PLIST=$trace_app/Contents/Info.plist"
                "FAKE_XCRUN_MUTATE_PLIST_KEY=CFBundleShortVersionString"
            )
            ;;
        mutated-plist-build-version)
            scenario_environment+=(
                "FAKE_XCRUN_MUTATE_PLIST=$trace_app/Contents/Info.plist"
                "FAKE_XCRUN_MUTATE_PLIST_KEY=CFBundleVersion"
            )
            ;;
        mutated-plist-executable)
            scenario_environment+=(
                "FAKE_XCRUN_MUTATE_PLIST=$trace_app/Contents/Info.plist"
                "FAKE_XCRUN_MUTATE_PLIST_KEY=CFBundleExecutable"
                "FAKE_XCRUN_MUTATE_PLIST_VALUE=ChangedExecutable"
            )
            ;;
        mutated-secret)
            scenario_environment+=(
                "FAKE_XCRUN_REPLACE_FILE_SOURCE=$v3_replacement_secret"
                "FAKE_XCRUN_REPLACE_FILE_TARGET=$v3_secret"
            )
            ;;
        supplemental) recorder_arguments+=(--supplemental-evidence "$v3_supplemental") ;;
        mutated-supplemental)
            recorder_arguments+=(--supplemental-evidence "$v3_supplemental")
            scenario_environment+=("FAKE_XCRUN_MUTATE_FILE=$v3_supplemental")
            ;;
        pending-supplemental)
            recorder_arguments+=(--supplemental-evidence "$v3_pending_supplemental")
            scenario_environment+=(
                "FAKE_XCRUN_REPLACE_FILE_SOURCE=$v3_pending_supplemental_source"
                "FAKE_XCRUN_REPLACE_FILE_TARGET=$v3_pending_supplemental"
            )
            ;;
        mutated-executable)
            scenario_environment+=(
                "FAKE_XCRUN_REPLACE_EXECUTABLE_SOURCE=$v3_replacement_executable"
                "FAKE_XCRUN_REPLACE_EXECUTABLE_TARGET=$trace_executable"
            )
            ;;
    esac
    if env FAKE_XCRUN_LOG="$v3_log" FAKE_XCRUN_TARGET_PID="$v3_target_pid" \
        FAKE_XCRUN_SLEEP_SECONDS=1 FAKE_CLOCK_COUNTER="$TEMP_ROOT/v3-$name-clock" \
        FAKE_INSPECTOR_LIVE_CODE=1 \
        SPACETERM_CONTINUOUS_CLOCK="$fake_clock" SPACETERM_XCRUN="$fake_xcrun" \
        SPACETERM_PROCESS_INSPECTOR="$fake_process_inspector" \
        "${scenario_environment[@]}" "$@" \
        "$SCRIPT_DIRECTORY/record-release-performance-trace.sh" \
        "${recorder_arguments[@]}" \
        --output-directory "$directory" >/dev/null 2>"$recorder_error"; then
        sed -n '1,80p' "$recorder_error" >&2
        [[ ! -f "$directory/spaceterm-ascii-trace-metadata.tsv" ]] \
            || sed -n '1,40p' "$directory/spaceterm-ascii-trace-metadata.tsv" >&2
        fail "v3 trace recorder accepted incomplete $name evidence"
    fi
    local metadata="$directory/spaceterm-ascii-trace-metadata.tsv"
    if [[ ! -f "$metadata" ]]; then
        sed -n '1,80p' "$recorder_error" >&2
        fail "v3 trace recorder did not finalize $name metadata"
    fi
    assert_equal INCOMPLETE "$(metric "$metadata" capture_status)" "$name capture status"
    assert_equal "$expected" "$(metric "$metadata" incomplete_reason)" "$name reason"
    assert_equal 3 "$(metric "$metadata" format_version)" "$name v3 format"
    assert_equal incomplete "$(metric "$metadata" status)" "$name finalization status"
    [[ ! -e "$provisional" ]] \
        || fail "$name published a CAPTURED provisional receipt under test overrides"
    if [[ -n "$late_publisher_pid" ]]; then
        wait "$late_publisher_pid"
        forget_target_pid "$late_publisher_pid"
        assert_equal INCOMPLETE "$(metric "$metadata" capture_status)" \
            "$name remained incomplete after late evidence appeared"
    fi
}

run_v3_incomplete overrides test-overrides-active 1000000000 2000000000
v3_metadata="$TEMP_ROOT/v3-overrides/spaceterm-ascii-trace-metadata.tsv"
assert_equal "$v3_subject_hash" "$(metric "$v3_metadata" subject_identity_sha256)" \
    "trace subject binding"
assert_equal "$(shasum -a 256 "$TEMP_ROOT/v3-overrides-run.tsv" | awk '{print $1}')" \
    "$(metric "$v3_metadata" run_metadata_sha256)" "trace run binding"
assert_equal true "$(metric "$v3_metadata" trace_target_pid_verified)" "trace PID binding"
assert_equal 2 "$(metric "$v3_metadata" time_profiler_rows)" "time profiler rows"
assert_equal 1 "$(metric "$v3_metadata" allocations_rows)" "allocations rows"
assert_equal 0 "$(metric "$v3_metadata" hangs_rows)" "hang rows"
assert_equal 0.000000 "$(metric "$v3_metadata" maximum_main_thread_hang_ms)" \
    "maximum hang duration"
assert_equal 0000000000000000000000000000000000000000000000000000000000000000 \
    "$(metric "$v3_metadata" supplemental_evidence_sha256)" \
    "absent supplemental evidence binding"
assert_equal "$v3_ready_hash" \
    "$(metric "$v3_metadata" workload_ready_receipt_sha256)" \
    "workload ready receipt binding"
[[ "$(awk 'END { print NR }' "$v3_metadata")" == 25 ]] \
    || fail "trace v3 metadata contains unexpected records"
! grep -Fq PASS "$v3_metadata" || fail "trace evidence claimed a performance verdict"
grep -Fq $'\t--attach\t'"$v3_target_pid" "$v3_log" \
    || fail "trace recorder did not attach to the frozen PID"
grep -Fq $'\t--time-limit\t4s' "$v3_log" \
    || fail "trace recorder omitted its bounded capture envelope"

premature_run="$TEMP_ROOT/v3-premature-run.tsv"
write_v3_final_run "$premature_run"
premature_workload="$TEMP_ROOT/v3-premature-workload.tsv"
write_v3_workload "$premature_workload"
if env FAKE_XCRUN_LOG="$v3_log" FAKE_XCRUN_TARGET_PID="$v3_target_pid" \
    FAKE_CLOCK_COUNTER="$TEMP_ROOT/v3-premature-clock" FAKE_INSPECTOR_LIVE_CODE=1 \
    SPACETERM_CONTINUOUS_CLOCK="$fake_clock" SPACETERM_XCRUN="$fake_xcrun" \
    SPACETERM_PROCESS_INSPECTOR="$fake_process_inspector" \
    "$SCRIPT_DIRECTORY/record-release-performance-trace.sh" \
    --subject-identity "$v3_subject" --run-intent "$v3_run_intent" \
    --run-metadata "$premature_run" \
    --provisional-receipt "$TEMP_ROOT/v3-premature-provisional.tsv" \
    --workload-metadata "$premature_workload" --workload-events "$v3_events" \
    --workload-ready-receipt "$v3_ready" --campaign-secret-file "$v3_secret" \
    --campaign-id "$V3_CAMPAIGN_ID" --session-id "$V3_SESSION_ID" \
    --nonce "$V3_NONCE" --scenario ascii --warmup-ms 0 --duration-ms 1000 \
    --output-directory "$TEMP_ROOT/v3-premature" >/dev/null 2>&1; then
    fail "trace recorder accepted run metadata finalized before capture"
fi
[[ ! -e "$TEMP_ROOT/v3-premature/spaceterm-ascii-trace-metadata.tsv" ]] \
    || fail "trace recorder published metadata for a premature final run"
run_v3_incomplete supplemental test-overrides-active 1000000000 2000000000
assert_equal "$(shasum -a 256 "$v3_supplemental" | awk '{ print $1 }')" \
    "$(metric "$TEMP_ROOT/v3-supplemental/spaceterm-ascii-trace-metadata.tsv" \
        supplemental_evidence_sha256)" "supplemental evidence binding"
v3_pending_supplemental="$TEMP_ROOT/v3-pending-supplemental.tsv"
v3_pending_supplemental_source="$TEMP_ROOT/v3-pending-supplemental.source"
cp "$v3_supplemental" "$v3_pending_supplemental_source"
run_v3_incomplete pending-supplemental test-overrides-active 1000000000 2000000000
assert_equal "$(shasum -a 256 "$v3_pending_supplemental" | awk '{ print $1 }')" \
    "$(metric "$TEMP_ROOT/v3-pending-supplemental/spaceterm-ascii-trace-metadata.tsv" \
        supplemental_evidence_sha256)" "pending supplemental evidence binding"
v3_supplemental_backup="$TEMP_ROOT/v3-supplemental-backup.tsv"
cp "$v3_supplemental" "$v3_supplemental_backup"
run_v3_incomplete mutated-supplemental frozen-input-changed 1000000000 2000000000
chmod 0600 "$v3_supplemental"
cp "$v3_supplemental_backup" "$v3_supplemental"
chmod 0444 "$v3_supplemental"

notified_workload="$TEMP_ROOT/v3-notified-workload.tsv"
write_v3_workload "$notified_workload" 1000000000 2000000000
notified_run="$TEMP_ROOT/v3-notified-run.tsv"
notified_run_source="$TEMP_ROOT/v3-notified-run.pending"
write_v3_final_run "$notified_run_source"
capture_notification="$TEMP_ROOT/v3-capture-start.tsv"
notified_directory="$TEMP_ROOT/v3-notified"
if env FAKE_XCRUN_LOG="$v3_log" FAKE_XCRUN_TARGET_PID="$v3_target_pid" \
    FAKE_XCRUN_SLEEP_SECONDS=1 FAKE_CLOCK_COUNTER="$TEMP_ROOT/v3-notified-clock" \
    FAKE_INSPECTOR_LIVE_CODE=1 \
    FAKE_XCRUN_REQUIRE_NOTIFICATION="$capture_notification" \
    FAKE_XCRUN_PUBLISH_RUN_SOURCE="$notified_run_source" \
    FAKE_XCRUN_PUBLISH_RUN_TARGET="$notified_run" \
    SPACETERM_CONTINUOUS_CLOCK="$fake_clock" SPACETERM_XCRUN="$fake_xcrun" \
    SPACETERM_PROCESS_INSPECTOR="$fake_process_inspector" \
    "$SCRIPT_DIRECTORY/record-release-performance-trace.sh" \
    --subject-identity "$v3_subject" --run-intent "$v3_run_intent" \
    --run-metadata "$notified_run" \
    --provisional-receipt "$TEMP_ROOT/v3-notified-provisional.tsv" \
    --workload-metadata "$notified_workload" --workload-events "$v3_events" \
    --workload-ready-receipt "$v3_ready" \
    --campaign-secret-file "$v3_secret" --campaign-id "$V3_CAMPAIGN_ID" \
    --session-id "$V3_SESSION_ID" --nonce "$V3_NONCE" \
    --scenario ascii --warmup-ms 0 --duration-ms 1000 \
    --capture-start-notification "$capture_notification" \
    --output-directory "$notified_directory" >/dev/null 2>&1; then
    fail "test overrides produced CAPTURED evidence with capture notification"
fi
assert_equal test-overrides-active \
    "$(metric "$notified_directory/spaceterm-ascii-trace-metadata.tsv" incomplete_reason)" \
    "capture notification evidence reason"
assert_equal launched "$(metric "$capture_notification" status)" \
    "capture-start notification status"
[[ "$(stat -f '%Lp' "$capture_notification")" == 400 ]] \
    || fail "capture-start notification is not private and immutable"

run_v3_incomplete one-sample time-profile-samples-insufficient 1000000000 2000000000 \
    FAKE_XCRUN_ONE_SAMPLE=1
run_v3_incomplete wrong-profile-pid time-profile-row-target-mismatch 1000000000 2000000000 \
    FAKE_XCRUN_ROW_PID=999999
run_v3_incomplete wrong-allocations-pid allocations-target-binding-missing 1000000000 2000000000 \
    FAKE_XCRUN_ALLOCATIONS_PID=999999
run_v3_incomplete mixed-allocations-pid allocations-target-binding-missing 1000000000 2000000000 \
    FAKE_XCRUN_ALLOCATIONS_FOREIGN_PID=999999
run_v3_incomplete wrong-hangs-track hangs-target-binding-missing 1000000000 2000000000 \
    FAKE_XCRUN_HANGS_TARGET_PID=999999
run_v3_incomplete mixed-hangs-track hangs-target-binding-missing 1000000000 2000000000 \
    FAKE_XCRUN_HANGS_FOREIGN_PID=999999
run_v3_incomplete wrong-hang-row hangs-target-binding-missing 1000000000 2000000000 \
    FAKE_XCRUN_HANG_PID=999999
run_v3_incomplete schema-only time-profile-samples-insufficient 1000000000 2000000000 \
    FAKE_XCRUN_SCHEMA_ONLY=1
run_v3_incomplete empty-trace trace-bundle-is-empty 1000000000 2000000000 \
    FAKE_XCRUN_EMPTY_TRACE=1
run_v3_incomplete empty-allocations allocation-events-empty 1000000000 2000000000 \
    FAKE_XCRUN_EMPTY_ALLOCATIONS=1
run_v3_incomplete nan-hang invalid-numeric-trace-field 1000000000 2000000000 \
    FAKE_XCRUN_HANG_DURATION=NaN
run_v3_incomplete negative-hang invalid-numeric-trace-field 1000000000 2000000000 \
    FAKE_XCRUN_HANG_DURATION=-1
run_v3_incomplete short requested-duration-not-covered 1000000000 2000000000 \
    FAKE_XCRUN_TRACE_DURATION=0.5 FAKE_XCRUN_END_DATE=2026-08-12T00:00:00.500000Z
run_v3_incomplete missing-tables trace-table-export-failed 1000000000 2000000000 \
    FAKE_XCRUN_MISSING_TABLES=1
run_v3_incomplete duplicate-profile-table time-profile-table-is-not-exact 1000000000 2000000000 \
    FAKE_XCRUN_DUPLICATE_TIME_PROFILE=1
run_v3_incomplete duplicate-hangs-table hangs-table-is-not-exact 1000000000 2000000000 \
    FAKE_XCRUN_DUPLICATE_HANGS=1
run_v3_incomplete reused-trace trace-workload-interval-mismatch 3500000000 4500000000
run_v3_incomplete coherent-full-envelope test-overrides-active 3000000000 4000000000 \
    FAKE_XCRUN_TRACE_DURATION=4 FAKE_XCRUN_END_DATE=2026-08-12T00:00:04Z \
    FAKE_CLOCK_END_NS=5000000000 FAKE_CLOCK_END_EPOCH_NS=1786492804000000000 \
    FAKE_XCRUN_SLEEP_SECONDS=4 FAKE_XCRUN_FULL_ENVELOPE_SAMPLES=1
run_v3_incomplete cross-intent run-metadata-invalid 1000000000 2000000000
run_v3_incomplete malformed-final run-metadata-invalid 1000000000 2000000000
run_v3_incomplete replaced-final run-metadata-invalid 1000000000 2000000000
run_v3_incomplete missing-final run-metadata-invalid 1000000000 2000000000
run_v3_incomplete late-final run-metadata-invalid 1000000000 2000000000
run_v3_incomplete invalid-hmac workload-metadata-invalid 1000000000 2000000000
run_v3_incomplete producer-mismatch workload-metadata-invalid 1000000000 2000000000
run_v3_incomplete short-workload workload-metadata-invalid 1000000000 1800000000
run_v3_incomplete long-workload workload-metadata-invalid 1000000000 4100000000
run_v3_incomplete producer-after-measurement workload-metadata-invalid 400000000 1400000000
run_v3_incomplete clock-drift trace-clock-correlation-invalid 1000000000 2000000000 \
    FAKE_CLOCK_END_EPOCH_NS=1786492801200000000
run_v3_incomplete wide-clock-anchor trace-clock-correlation-invalid 1000000000 2000000000 \
    FAKE_CLOCK_START_WIDTH_NS=10000001
v3_generation_counter="$TEMP_ROOT/v3-generation-counter"
run_v3_incomplete changed-generation target-identity-changed 1000000000 2000000000 \
    FAKE_INSPECTOR_COUNTER="$v3_generation_counter" FAKE_INSPECTOR_CHANGE_AFTER=2

expect_v3_input_rejected() {
    local name="$1" subject="$2"
    local workload="$TEMP_ROOT/v3-input-$name-workload.tsv"
    write_v3_workload "$workload"
    if FAKE_XCRUN_LOG="$v3_log" FAKE_XCRUN_TARGET_PID="$v3_target_pid" \
        FAKE_CLOCK_COUNTER="$TEMP_ROOT/v3-input-$name-clock" \
        FAKE_INSPECTOR_LIVE_CODE=1 \
        SPACETERM_CONTINUOUS_CLOCK="$fake_clock" SPACETERM_XCRUN="$fake_xcrun" \
        SPACETERM_PROCESS_INSPECTOR="$fake_process_inspector" \
        "$SCRIPT_DIRECTORY/record-release-performance-trace.sh" \
        --subject-identity "$subject" --run-intent "$v3_run_intent" \
        --run-metadata "$TEMP_ROOT/v3-input-$name-run.tsv" \
        --provisional-receipt "$TEMP_ROOT/v3-input-$name-provisional.tsv" \
        --workload-metadata "$workload" --workload-events "$v3_events" \
        --workload-ready-receipt "$v3_ready" \
        --campaign-secret-file "$v3_secret" --campaign-id "$V3_CAMPAIGN_ID" \
        --session-id "$V3_SESSION_ID" --nonce "$V3_NONCE" \
        --scenario ascii --warmup-ms 0 --duration-ms 1000 \
        --output-directory "$TEMP_ROOT/v3-input-$name" >/dev/null 2>&1; then
        fail "v3 trace recorder accepted $name identity evidence"
    fi
}

v3_writable="$TEMP_ROOT/v3-writable-subject.tsv"
cp "$v3_subject" "$v3_writable"; chmod 0644 "$v3_writable"
expect_v3_input_rejected writable "$v3_writable"
v3_symlink="$TEMP_ROOT/v3-symlink-subject.tsv"
ln -s "$v3_subject" "$v3_symlink"
expect_v3_input_rejected symlink "$v3_symlink"
chmod 0600 "$v3_secret"
expect_v3_input_rejected writable-secret "$v3_subject"
chmod 0400 "$v3_secret"
for mutation in duplicate unknown malformed downgrade; do
    v3_mutated="$TEMP_ROOT/v3-$mutation-subject.tsv"
    cp "$v3_subject" "$v3_mutated"; chmod 0644 "$v3_mutated"
    case "$mutation" in
        duplicate) printf 'subject\tspaceterm\n' >> "$v3_mutated" ;;
        unknown) printf 'invented\tvalue\n' >> "$v3_mutated" ;;
        malformed) printf 'malformed\n' >> "$v3_mutated" ;;
        downgrade) sed -i '' $'s/^format_version\t1$/format_version\t0/' "$v3_mutated" ;;
    esac
    chmod 0444 "$v3_mutated"
    expect_v3_input_rejected "$mutation" "$v3_mutated"
done

v3_run_backup="$TEMP_ROOT/v3-run-intent-backup.tsv"
cp "$v3_run_intent" "$v3_run_backup"
run_v3_incomplete mutated-run frozen-input-changed 1000000000 2000000000
chmod 0600 "$v3_run_intent"
cp "$v3_run_backup" "$v3_run_intent"
chmod 0444 "$v3_run_intent"

# Genuine live guest proof: the real inspector rejects forged code identity
# and PID generation even though the static copied bundle remains valid.
"$SCRIPT_DIRECTORY/inspect-release-performance-process.py" --pid "$v3_target_pid" \
    --expected-executable "$trace_executable" --expected-sha256 "$v3_hash" \
    --expected-device "$v3_device" --expected-inode "$v3_inode" \
    --expected-start-identity "$v3_start_identity" \
    --expected-signing-identifier "$v3_identifier" --expected-team-identifier "$v3_team" \
    --expected-cdhash "$v3_cdhash" | grep -Fxq $'live_code_identity_verified\ttrue' \
    || fail "real inspector did not verify a signed dynamic guest"
if "$SCRIPT_DIRECTORY/inspect-release-performance-process.py" --pid "$v3_target_pid" \
    --expected-executable "$trace_executable" --expected-sha256 "$v3_hash" \
    --expected-device "$v3_device" --expected-inode "$v3_inode" \
    --expected-start-identity "$v3_start_identity" \
    --expected-signing-identifier "$v3_identifier" --expected-team-identifier "$v3_team" \
    --expected-cdhash 0000000000000000000000000000000000000000 >/dev/null 2>&1; then
    fail "real inspector accepted a static/live CDHash mismatch"
fi
if "$SCRIPT_DIRECTORY/inspect-release-performance-process.py" --pid "$v3_target_pid" \
    --expected-executable "$trace_executable" --expected-sha256 "$v3_hash" \
    --expected-device "$v3_device" --expected-inode "$v3_inode" \
    --expected-start-identity 'Mon Jan 1 00:00:00 2001' \
    --expected-signing-identifier "$v3_identifier" --expected-team-identifier "$v3_team" \
    --expected-cdhash "$v3_cdhash" >/dev/null 2>&1; then
    fail "real inspector accepted a reused PID generation"
fi

# Package identity is rechecked after capture, including the plist, bundle
# signature, executable vnode, and executable content hash.
v3_plist_backup="$TEMP_ROOT/v3-info-backup.plist"
cp "$trace_app/Contents/Info.plist" "$v3_plist_backup"
for plist_mutation in identifier marketing-version build-version executable; do
    run_v3_incomplete "mutated-plist-$plist_mutation" frozen-input-changed \
        1000000000 2000000000
    cp "$v3_plist_backup" "$trace_app/Contents/Info.plist"
    codesign --verify --strict "$trace_app" >/dev/null 2>&1 \
        || fail "restored trace fixture bundle did not retain its frozen signature"
done

v3_secret_backup="$TEMP_ROOT/v3-secret-backup"
cp "$v3_secret" "$v3_secret_backup"
v3_replacement_secret="$TEMP_ROOT/v3-replacement-secret"
printf 'replacement-secret-replacement-secret\n' > "$v3_replacement_secret"
chmod 0400 "$v3_replacement_secret"
run_v3_incomplete mutated-secret frozen-input-changed 1000000000 2000000000
chmod 0600 "$v3_secret"
cp "$v3_secret_backup" "$v3_secret"
chmod 0400 "$v3_secret"

v3_replacement_executable="$TEMP_ROOT/replacement-SpaceTerm"
cp /bin/date "$v3_replacement_executable"
chmod 0555 "$v3_replacement_executable"
codesign --force --sign - --identifier io.github.sadiksaifi.spaceterm \
    "$v3_replacement_executable" >/dev/null 2>&1
run_v3_incomplete mutated-executable frozen-input-changed 1000000000 2000000000

echo "release performance tooling tests passed"
