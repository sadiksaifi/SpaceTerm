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
        printf 'process_start_identity\tWed Aug 12 00:00:00 2026\n'
        printf 'identity_status\tfrozen\n'
    } > "$path"
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
    awk -F '\t' -v base="$BASE_CONTINUOUS_NS" 'BEGIN {
            OFS = "\t"
            print "sequence", "continuous_ns", "kind", "event_id", \
                "byte_count", "rows", "columns", "pixel_width", \
                "pixel_height", "status"
            sequence = 0
            print sequence++, base, "started", "none", \
                0, 40, 100, 1000, 800, "ok"
            print sequence++, base + 1, "geometry", "none", \
                0, 40, 100, 1000, 800, "ok"
            print sequence++, base + 2, "seed-complete", "none", \
                1024, 40, 100, 1000, 800, "ok"
        }
        NR > 1 && $4 == "input" {
            print sequence++, $2 + 50000000, "input-read", $3, \
                0, 40, 100, 1000, 800, "ok"
            print sequence++, $2 + 75000000, "input-ack-written", $3, \
                64, 40, 100, 1000, 800, "ok"
        }
        END {
            print sequence++, base + 660000000000, "producer-end", "none", \
                123456, 40, 100, 1000, 800, "success"
        }
    ' "$driver" > "$output"
}

write_workload_metadata() {
    local workload_binary="$1"
    local output="$2"
    {
        printf 'format_version\t2\n'
        printf 'scenario\tascii\n'
        printf 'producer_sha256\t%s\n' "$(sha256 "$workload_binary")"
        printf 'seed_sha256\t%s\n' "$HASH_B"
        printf 'seed_bytes\t1024\n'
        printf 'requested_duration_ms\t600000\n'
        printf 'warmup_ms\t60000\n'
        printf 'requested_iterations\t0\n'
        printf 'requested_seed_rows\t0\n'
        printf 'emitted_bytes\t123456\n'
        printf 'input_events\t20\n'
        printf 'started_continuous_ns\t%d\n' "$((BASE_CONTINUOUS_NS + 60000000000))"
        printf 'ended_continuous_ns\t%d\n' "$((BASE_CONTINUOUS_NS + 660000000000))"
        printf 'status\tcomplete\n'
    } > "$output"
}

write_sustained_rss() {
    local subject_identity="$1"
    local workload_events="$2"
    local driver_events="$3"
    local output="$4"
    local final_shift_kib="${5:-0}"
    {
        printf 'elapsed_ms\tcontinuous_ns\trss_kib\tworkload_bytes\tresize_count\n'
        printf '# format_version\t3\n'
        printf '# scenario\tascii\n'
        printf '# sample_interval_ms\t10000\n'
        printf '# requested_duration_ms\t600000\n'
        printf '# subject_identity_sha256\t%s\n' "$(sha256 "$subject_identity")"
        printf '# workload_events_sha256\t%s\n' "$(sha256 "$workload_events")"
        printf '# driver_events_sha256\t%s\n' "$(sha256 "$driver_events")"
        for ((index = 0; index <= 60; index += 1)); do
            rss_kib=$((100000 + index % 2 * 1000))
            (( index <= 30 )) || rss_kib=$((rss_kib + final_shift_kib))
            printf '%d\t%d\t%d\t%d\t0\n' \
                "$((index * 10000))" \
                "$((BASE_CONTINUOUS_NS + 60000000000 + index * 10000000000))" \
                "$rss_kib" "$((index * 1000000))"
        done
        printf '# status\tcomplete\n'
    } > "$output"
}

write_raw_rss() {
    local subject_identity="$1"
    local output="$2"
    local identity_hash="${3:-$(sha256 "$subject_identity")}"
    local continuous_delay_ns="${4:-0}"
    {
        printf 'elapsed_ms\tcontinuous_ns\trss_kib\n'
        printf '# format_version\t1\n'
        printf '# sample_interval_ms\t10000\n'
        printf '# requested_warmup_ms\t60000\n'
        printf '# requested_duration_ms\t600000\n'
        printf '# subject_identity_sha256\t%s\n' "$identity_hash"
        for ((index = 0; index <= 60; index += 1)); do
            printf '%d\t%d\t%d\n' "$((index * 10000))" \
                "$((BASE_CONTINUOUS_NS + 60000000000 + continuous_delay_ns \
                    + index * 10000000000))" \
                "$((100000 + index % 2 * 1000))"
        done
        printf '# status\tcomplete\n'
    } > "$output"
}

write_trace_metadata() {
    local subject_identity="$1"
    local output="$2"
    local target_verified="${3:-true}"
    {
        printf 'format_version\t3\n'
        printf 'capture_status\tCAPTURED\n'
        printf 'incomplete_reason\tnone\n'
        printf 'subject_identity_sha256\t%s\n' "$(sha256 "$subject_identity")"
        printf 'requested_duration_ms\t600000\n'
        printf 'actual_duration_ms\t600001\n'
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
        printf 'schema\tspaceterm.acceptance.native-launch-proof/v2\n'
        printf 'observation.source\tproduction-app\n'
        printf 'launch.nonce\tlaunch-nonce-43\n'
        printf 'run.id\trun-43-ascii\n'
        printf 'package.app.sha256\t%s\n' "$HASH_C"
        printf 'process.pid\t123\n'
        printf 'process.pidversion\t5\n'
        printf 'process.executable.path\t%s\n' \
            "$(awk -F '\t' '$1 == "executable_path" { print $2 }' "$identity")"
        printf 'process.executable.device\t1\n'
        printf 'process.executable.inode\t2\n'
        printf 'process.executable.fsid\t1\n'
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
    local drops="${5:-0}"
    local start end accepted
    start="$(awk -F '\t' '$3 == "input-read" { inputs += 1 } \
        END { print inputs + 0 }' "$workload_events")"
    accepted="$start"
    start="$((BASE_CONTINUOUS_NS + 60000000000))"
    end="$((BASE_CONTINUOUS_NS + 660000000000))"
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
        printf 'schema\tspaceterm.acceptance.runtime-observation-metadata/v1\n'
        printf 'observation.source\tproduction-app\n'
        printf 'run.id\trun-43-ascii\n'
        printf 'package.app.sha256\t%s\n' "$HASH_C"
        printf 'process.pid\t123\n'
        printf 'runtime.samples.path\truntime-samples.tsv\n'
        printf 'runtime.samples.sha256\t%s\n' "$(sha256 "$samples")"
        printf 'runtime.events.path\truntime-events.tsv\n'
        printf 'runtime.events.sha256\t%s\n' "$(sha256 "$events")"
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
    shift 8
    "$SCRIPT_DIRECTORY/analyze-release-performance-case.sh" \
        --subject "$subject" \
        --scenario ascii \
        --plan "$PLAN" \
        --plan-metadata "$PLAN_METADATA" \
        --pair-metadata "$PAIR_METADATA" \
        --subject-identity "$identity" \
        --run-metadata "$run_metadata" \
        --workload-metadata "$WORKLOAD_METADATA" \
        --workload-events "$workload_events" \
        --driver-events "$driver_events" \
        --rss-samples "$rss" \
        --trace-metadata "$trace" \
        --manual-artifacts "$manual" \
        --manual-screenshot "$MANUAL_SCREENSHOT" \
        --manual-video "$MANUAL_VIDEO" \
        "$@"
}

for command in awk bash chmod cp grep mktemp rm sed shasum; do
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
GHOSTTY_RSS="$TEMP_ROOT/ghostty-rss.tsv"
GHOSTTY_TRACE="$TEMP_ROOT/ghostty-trace.tsv"
MANUAL="$TEMP_ROOT/manual.tsv"
MANUAL_SCREENSHOT="$TEMP_ROOT/manual-screenshot.png"
MANUAL_VIDEO="$TEMP_ROOT/manual-video.mov"
write_driver_events "$PLAN" "$DRIVER_EVENTS"
write_workload_events "$DRIVER_EVENTS" "$WORKLOAD_EVENTS"
write_workload_metadata "$WORKLOAD_BINARY" "$WORKLOAD_METADATA"
write_sustained_rss "$GHOSTTY_IDENTITY" "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$GHOSTTY_RSS"
write_trace_metadata "$GHOSTTY_IDENTITY" "$GHOSTTY_TRACE"
printf 'bounded fake screenshot evidence\n' > "$MANUAL_SCREENSHOT"
printf 'bounded fake video evidence\n' > "$MANUAL_VIDEO"
write_manual_artifacts "$MANUAL"
readonly DRIVER_EVENTS WORKLOAD_EVENTS WORKLOAD_METADATA GHOSTTY_RSS GHOSTTY_TRACE MANUAL
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
    --driver-events "$DRIVER_EVENTS" \
    --output "$ASSEMBLED_RSS"
readonly RAW_RSS ASSEMBLED_RSS

# Normal scheduling delay changes actual continuous time, never the exact
# scheduled elapsed cadence or final requested-duration boundary.
DELAYED_RAW_RSS="$TEMP_ROOT/delayed-raw-rss.tsv"
DELAYED_ASSEMBLED_RSS="$TEMP_ROOT/delayed-assembled-rss.tsv"
write_raw_rss "$GHOSTTY_IDENTITY" "$DELAYED_RAW_RSS" \
    "$(sha256 "$GHOSTTY_IDENTITY")" 900000000
[[ "$(awk -F '\t' '$1 !~ /^#/ && $1 ~ /^[0-9]+$/ { last = $1 } END { print last }' \
    "$DELAYED_RAW_RSS")" == 600000 ]] \
    || fail "delayed raw RSS changed the scheduled final boundary"
"$SCRIPT_DIRECTORY/assemble-release-performance-rss-v3.sh" \
    --subject-identity "$GHOSTTY_IDENTITY" \
    --scenario ascii \
    --requested-warmup-ms 60000 \
    --requested-duration-ms 600000 \
    --raw-samples "$DELAYED_RAW_RSS" \
    --workload-events "$WORKLOAD_EVENTS" \
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
        --driver-events "$DRIVER_EVENTS" \
        --output "$TEMP_ROOT/truncated-assembled.tsv"

expect_result 0 PASS "valid Ghostty case with zero Hangs" \
    run_case ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$GHOSTTY_RSS" "$GHOSTTY_TRACE" "$MANUAL"

SPACETERM_RSS="$TEMP_ROOT/spaceterm-rss.tsv"
SPACETERM_TRACE="$TEMP_ROOT/spaceterm-trace.tsv"
NATIVE_LAUNCH="$TEMP_ROOT/native-launch.tsv"
RUNTIME_SAMPLES="$TEMP_ROOT/runtime-samples.tsv"
RUNTIME_EVENTS="$TEMP_ROOT/runtime-events.tsv"
RUNTIME_METADATA="$TEMP_ROOT/runtime-metadata.tsv"
write_sustained_rss "$SPACETERM_IDENTITY" "$WORKLOAD_EVENTS" \
    "$DRIVER_EVENTS" "$SPACETERM_RSS"
write_trace_metadata "$SPACETERM_IDENTITY" "$SPACETERM_TRACE"
write_native_launch "$SPACETERM_IDENTITY" "$NATIVE_LAUNCH"
write_runtime_observation "$WORKLOAD_EVENTS" "$RUNTIME_SAMPLES" \
    "$RUNTIME_EVENTS" "$RUNTIME_METADATA"
readonly SPACETERM_RSS SPACETERM_TRACE NATIVE_LAUNCH
readonly RUNTIME_SAMPLES RUNTIME_EVENTS RUNTIME_METADATA

expect_result 0 PASS "valid authenticated SpaceTerm runtime case" \
    run_case spaceterm "$SPACETERM_IDENTITY" "$SPACETERM_RUN" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$SPACETERM_RSS" \
        "$SPACETERM_TRACE" "$MANUAL" \
        --runtime-samples "$RUNTIME_SAMPLES" \
        --runtime-events "$RUNTIME_EVENTS" \
        --runtime-metadata "$RUNTIME_METADATA" \
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
SLOW_RSS="$TEMP_ROOT/slow-rss.tsv"
write_sustained_rss "$GHOSTTY_IDENTITY" "$SLOW_WORKLOAD" "$DRIVER_EVENTS" "$SLOW_RSS"
expect_result 1 FAIL "input acknowledgement over 250 ms" \
    run_case ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$SLOW_WORKLOAD" "$DRIVER_EVENTS" "$SLOW_RSS" "$GHOSTTY_TRACE" "$MANUAL"

MISSING_END="$TEMP_ROOT/missing-producer-end.tsv"
sed '$d' "$WORKLOAD_EVENTS" > "$MISSING_END"
MISSING_END_RSS="$TEMP_ROOT/missing-end-rss.tsv"
write_sustained_rss "$GHOSTTY_IDENTITY" "$MISSING_END" "$DRIVER_EVENTS" "$MISSING_END_RSS"
expect_result 2 NOT-RUN "missing producer end" \
    run_case ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$MISSING_END" "$DRIVER_EVENTS" "$MISSING_END_RSS" "$GHOSTTY_TRACE" "$MANUAL"

BAD_DRIVER="$TEMP_ROOT/bad-driver.tsv"
awk -F '\t' 'BEGIN { OFS = "\t" } NR == 3 { $3 = "duplicate-event" } { print }' \
    "$DRIVER_EVENTS" > "$BAD_DRIVER"
BAD_DRIVER_RSS="$TEMP_ROOT/bad-driver-rss.tsv"
write_sustained_rss "$GHOSTTY_IDENTITY" "$WORKLOAD_EVENTS" "$BAD_DRIVER" "$BAD_DRIVER_RSS"
expect_result 2 NOT-RUN "driver/plan event mismatch" \
    run_case ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$WORKLOAD_EVENTS" "$BAD_DRIVER" "$BAD_DRIVER_RSS" "$GHOSTTY_TRACE" "$MANUAL"

NONMONOTONIC_DRIVER="$TEMP_ROOT/nonmonotonic-driver.tsv"
awk -F '\t' 'BEGIN { OFS = "\t" } NR == 4 { $2 = 1 } { print }' \
    "$DRIVER_EVENTS" > "$NONMONOTONIC_DRIVER"
NONMONOTONIC_RSS="$TEMP_ROOT/nonmonotonic-driver-rss.tsv"
write_sustained_rss "$GHOSTTY_IDENTITY" "$WORKLOAD_EVENTS" \
    "$NONMONOTONIC_DRIVER" "$NONMONOTONIC_RSS"
expect_result 2 NOT-RUN "nonmonotonic driver event time" \
    run_case ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$WORKLOAD_EVENTS" "$NONMONOTONIC_DRIVER" "$NONMONOTONIC_RSS" \
        "$GHOSTTY_TRACE" "$MANUAL"

BAD_TRACE="$TEMP_ROOT/bad-trace.tsv"
write_trace_metadata "$GHOSTTY_IDENTITY" "$BAD_TRACE" false
expect_result 2 NOT-RUN "trace without exact target binding" \
    run_case ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$GHOSTTY_RSS" "$BAD_TRACE" "$MANUAL"

TRACE_WITHOUT_DURATION="$TEMP_ROOT/trace-without-duration.tsv"
sed '/^actual_duration_ms\t/d' "$GHOSTTY_TRACE" > "$TRACE_WITHOUT_DURATION"
expect_result 2 NOT-RUN "trace schema without duration" \
    run_case ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$GHOSTTY_RSS" \
        "$TRACE_WITHOUT_DURATION" "$MANUAL"

UNREVIEWED="$TEMP_ROOT/unreviewed.tsv"
write_manual_artifacts "$UNREVIEWED" NOT-REVIEWED
expect_result 2 NOT-RUN "automated success without manual artifacts" \
    run_case ghostty "$GHOSTTY_IDENTITY" "$GHOSTTY_RUN" \
        "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" "$GHOSTTY_RSS" "$GHOSTTY_TRACE" "$UNREVIEWED"

SHIFTED_RSS="$TEMP_ROOT/shifted-rss.tsv"
write_sustained_rss "$GHOSTTY_IDENTITY" "$WORKLOAD_EVENTS" "$DRIVER_EVENTS" \
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
    printf '# format_version\t3\n# scenario\tresize\n# sample_interval_ms\t10000\n'
    printf '# requested_duration_ms\t300000\n'
    printf '# subject_identity_sha256\t%s\n' "$HASH_A"
    printf '# workload_events_sha256\t%s\n' "$HASH_B"
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
