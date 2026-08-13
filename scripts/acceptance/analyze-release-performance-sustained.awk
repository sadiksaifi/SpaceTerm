BEGIN {
    FS = "\t"
    window_ms = 300000
    required_duration_ms = 600000
    required_interval_ms = 10000
    cadence_tolerance_ms = 1000
    required_samples = 61
}

function stop_not_run(reason) {
    if (not_run_reason == "") {
        not_run_reason = reason
        print "error: sustained RSS evidence is not runnable: " reason > "/dev/stderr"
    }
    invalid = 1
    if (in_end) {
        print "format_version\t3"
        print "result\tNOT-RUN"
        print "reason\t" reason
    }
    exit 2
}

function abs(value) { return value < 0 ? -value : value }

function metadata_value(key, value) {
    if (seen_metadata[key]++) stop_not_run("duplicate-metadata-" key)
    metadata[key] = value
}

NR == 1 {
    if ($0 != "elapsed_ms\tcontinuous_ns\trss_kib\tworkload_bytes\tresize_count") {
        stop_not_run("unexpected-header")
    }
    next
}

/^#/ {
    if (NF != 2 || substr($1, 1, 2) != "# ") stop_not_run("invalid-metadata")
    key = substr($1, 3)
    if (key != "format_version" && key != "scenario" \
        && key != "sample_interval_ms" && key != "requested_duration_ms" \
        && key != "subject_identity_sha256" && key != "workload_events_sha256" \
        && key != "workload_metadata_sha256" \
        && key != "ready_receipt_sha256" \
        && key != "plan_start_continuous_ns" \
        && key != "measurement_start_continuous_ns" \
        && key != "plan_start_gate_sha256" \
        && key != "workload_authentication" \
        && key != "progress_interval_ms" \
        && key != "maximum_progress_age_ms" \
        && key != "driver_events_sha256" && key != "status") {
        stop_not_run("unknown-metadata-" key)
    }
    metadata_value(key, $2)
    next
}

{
    if (seen_metadata["status"]) stop_not_run("sample-after-status")
    if (NF != 5 || $1 !~ /^[0-9]+$/ || $2 !~ /^[0-9]+$/ \
        || $3 !~ /^[1-9][0-9]*$/ || $4 !~ /^[0-9]+$/ || $5 !~ /^[0-9]+$/) {
        stop_not_run("invalid-sample")
    }
    n += 1
    elapsed[n] = $1 + 0
    continuous[n] = $2 + 0
    rss[n] = $3 + 0
    bytes[n] = $4 + 0
    resize[n] = $5 + 0
    if (resize[n] != 0) stop_not_run("unexpected-resize-count")
    if (n > 1) {
        elapsed_delta = elapsed[n] - elapsed[n - 1]
        continuous_delta = continuous[n] - continuous[n - 1]
        if (elapsed_delta <= 0 || continuous_delta <= 0) stop_not_run("non-monotonic-samples")
        if (abs(elapsed_delta - required_interval_ms) > cadence_tolerance_ms) {
            stop_not_run("invalid-elapsed-cadence")
        }
        if (abs(continuous_delta - elapsed_delta * 1000000) \
            > cadence_tolerance_ms * 1000000) {
            stop_not_run("invalid-continuous-cadence")
        }
        if (bytes[n] < bytes[n - 1]) stop_not_run("workload-bytes-regressed")
    }
    sum_t += elapsed[n]
    sum_y += rss[n]
    sum_tt += elapsed[n] * elapsed[n]
    sum_ty += elapsed[n] * rss[n]
    sum_b += bytes[n]
    sum_bb += bytes[n] * bytes[n]
    sum_by += bytes[n] * rss[n]
}

END {
    in_end = 1
    if (invalid) {
        print "format_version\t3"
        print "result\tNOT-RUN"
        print "reason\t" not_run_reason
        exit 2
    }
    if (metadata["format_version"] != "4") stop_not_run("unsupported-format-version")
    if (!(metadata["scenario"] == "ascii" || metadata["scenario"] == "unicode-styles" \
        || metadata["scenario"] == "scrolled" || metadata["scenario"] == "hidden-occluded")) {
        stop_not_run("invalid-scenario")
    }
    if (metadata["sample_interval_ms"] + 0 != required_interval_ms \
        || metadata["sample_interval_ms"] !~ /^[0-9]+$/) {
        stop_not_run("sample-interval-is-not-10-seconds")
    }
    if (metadata["requested_duration_ms"] + 0 != required_duration_ms \
        || metadata["requested_duration_ms"] !~ /^[0-9]+$/) {
        stop_not_run("requested-duration-is-not-ten-minutes")
    }
    if (metadata["workload_authentication"] != "hmac-sha256" \
        || metadata["progress_interval_ms"] != "1000" \
        || metadata["maximum_progress_age_ms"] != "2000") {
        stop_not_run("workload-progress-authentication-invalid")
    }
    if (metadata["plan_start_continuous_ns"] !~ /^[1-9][0-9]*$/ \
        || metadata["measurement_start_continuous_ns"] !~ /^[1-9][0-9]*$/ \
        || metadata["measurement_start_continuous_ns"] + 0 \
            != metadata["plan_start_continuous_ns"] + 60000000000) {
        stop_not_run("measurement-boundary-invalid")
    }
    for (hash_index = 1; hash_index <= 6; hash_index += 1) {
        hash_key = hash_index == 1 ? "subject_identity_sha256" \
            : hash_index == 2 ? "workload_events_sha256" \
            : hash_index == 3 ? "workload_metadata_sha256" \
            : hash_index == 4 ? "ready_receipt_sha256" \
            : hash_index == 5 ? "plan_start_gate_sha256" : "driver_events_sha256"
        if (length(metadata[hash_key]) != 64 || metadata[hash_key] !~ /^[0-9a-f]+$/) {
            stop_not_run("invalid-" hash_key)
        }
    }
    if (metadata["status"] != "complete") stop_not_run("capture-status-is-not-complete")
    if (n < required_samples) stop_not_run("insufficient-samples")
    if (elapsed[1] > cadence_tolerance_ms || elapsed[n] < required_duration_ms \
        || elapsed[n] > required_duration_ms + cadence_tolerance_ms) {
        stop_not_run("capture-does-not-cover-ten-minutes")
    }
    if (bytes[n] <= bytes[1]) stop_not_run("workload-byte-count-has-no-growth")

    first_count = 0
    final_count = 0
    for (i = 1; i <= n; i += 1) {
        if (elapsed[i] < window_ms) {
            if (!first_count || rss[i] < first_min) first_min = rss[i]
            if (!first_count || rss[i] > first_max) first_max = rss[i]
            first_sum += rss[i]
            first_count += 1
        }
        if (elapsed[i] > required_duration_ms - window_ms \
            && elapsed[i] <= required_duration_ms + cadence_tolerance_ms) {
            if (!final_count || rss[i] < final_min) final_min = rss[i]
            if (!final_count || rss[i] > final_max) final_max = rss[i]
            final_sum += rss[i]
            final_count += 1
        }
    }
    if (first_count < 30 || final_count < 30) stop_not_run("analysis-window-incomplete")
    first_range = first_max - first_min
    final_range = final_max - final_min
    range_change = abs(final_range - first_range)
    tolerance = first_range * 0.10
    if (tolerance < 65536) tolerance = 65536
    first_mean = first_sum / first_count
    final_mean = final_sum / final_count
    level_growth = final_mean - first_mean

    time_denominator = n * sum_tt - sum_t * sum_t
    byte_denominator = n * sum_bb - sum_b * sum_b
    if (time_denominator <= 0 || byte_denominator <= 0) stop_not_run("invalid-growth-basis")
    time_slope = (n * sum_ty - sum_t * sum_y) / time_denominator
    time_growth = time_slope * required_duration_ms
    byte_slope = (n * sum_by - sum_b * sum_y) / byte_denominator
    byte_growth_per_gib = byte_slope * 1073741824
    endpoint_growth = rss[n] - rss[1]
    maximum_growth = level_growth
    if (time_growth > maximum_growth) maximum_growth = time_growth
    if (endpoint_growth > maximum_growth) maximum_growth = endpoint_growth
    if (maximum_growth < 0) maximum_growth = 0

    range_plateau = range_change <= tolerance
    level_and_trend_bounded = maximum_growth <= tolerance
    byte_growth_bounded = byte_growth_per_gib <= tolerance
    result = range_plateau && level_and_trend_bounded && byte_growth_bounded \
        ? "PASS" : "FAIL"

    print "format_version\t3"
    print "sample_count\t" n
    print "first_window_sample_count\t" first_count
    print "final_window_sample_count\t" final_count
    print "first_range_rss_kib\t" first_range
    print "final_range_rss_kib\t" final_range
    print "range_change_rss_kib\t" range_change
    printf "first_mean_rss_kib\t%.3f\n", first_mean
    printf "final_mean_rss_kib\t%.3f\n", final_mean
    printf "level_growth_rss_kib\t%.3f\n", level_growth
    printf "time_growth_rss_kib\t%.3f\n", time_growth
    printf "endpoint_growth_rss_kib\t%.3f\n", endpoint_growth
    printf "byte_growth_rss_kib_per_gib\t%.3f\n", byte_growth_per_gib
    printf "growth_tolerance_rss_kib\t%.3f\n", tolerance
    print "range_plateau\t" (range_plateau ? "true" : "false")
    print "level_and_trend_bounded\t" (level_and_trend_bounded ? "true" : "false")
    print "byte_growth_bounded\t" (byte_growth_bounded ? "true" : "false")
    print "result\t" result
    exit result == "PASS" ? 0 : 1
}
