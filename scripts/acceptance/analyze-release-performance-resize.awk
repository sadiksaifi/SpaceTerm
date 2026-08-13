BEGIN {
    FS = "\t"
    required_interval_ms = 10000
    cadence_tolerance_ms = 1000
    required_duration_ms = 300000
    required_samples = 31
    required_resize_cycles = 300
    sample_count = 0
}

function fail_not_run(reason) {
    if (not_run_reason == "") {
        not_run_reason = reason
        print "error: resize RSS evidence is not runnable: " reason > "/dev/stderr"
    }
    invalid = 1
    if (in_end) {
        print "format_version\t3"
        print "result\tNOT-RUN"
        print "reason\t" not_run_reason
    }
    exit 2
}

function abs(value) {
    return value < 0 ? -value : value
}

function remember_metadata(key, value) {
    if (seen_metadata[key]++) {
        fail_not_run("duplicate-metadata-" key)
    }
    metadata[key] = value
}

function median_prefix(count,    i, j, value, middle) {
    for (i = 1; i <= count; i += 1) {
        ordered[i] = rss[i]
    }
    for (i = 2; i <= count; i += 1) {
        value = ordered[i]
        j = i - 1
        while (j >= 1 && ordered[j] > value) {
            ordered[j + 1] = ordered[j]
            j -= 1
        }
        ordered[j + 1] = value
    }
    middle = int(count / 2)
    if (count % 2 == 1) {
        return ordered[middle + 1]
    }
    return (ordered[middle] + ordered[middle + 1]) / 2
}

NR == 1 {
    if ($0 != "elapsed_ms\tcontinuous_ns\trss_kib\tworkload_bytes\tresize_count") {
        fail_not_run("unexpected-header")
    }
    next
}

/^#/ {
    if (NF != 2 || substr($1, 1, 2) != "# ") {
        fail_not_run("invalid-metadata")
    }
    key = substr($1, 3)
    if (key != "format_version" \
        && key != "sample_interval_ms" \
        && key != "scenario" \
        && key != "requested_duration_ms" \
        && key != "subject_identity_sha256" \
        && key != "workload_events_sha256" \
        && key != "workload_metadata_sha256" \
        && key != "ready_receipt_sha256" \
        && key != "plan_start_continuous_ns" \
        && key != "measurement_start_continuous_ns" \
        && key != "plan_start_gate_sha256" \
        && key != "workload_authentication" \
        && key != "progress_interval_ms" \
        && key != "maximum_progress_age_ms" \
        && key != "driver_events_sha256" \
        && key != "distinct_geometry_count" \
        && key != "geometry_change_count" \
        && key != "completed_resize_cycles" \
        && key != "geometry_correlated" \
        && key != "status") {
        fail_not_run("unknown-metadata-" key)
    }
    remember_metadata(key, $2)
    next
}

{
    if (seen_metadata["status"]) {
        fail_not_run("sample-after-status")
    }
    if (NF != 5 \
        || $1 !~ /^[0-9]+$/ \
        || $2 !~ /^[0-9]+$/ \
        || $3 !~ /^[1-9][0-9]*$/ \
        || $4 !~ /^[0-9]+$/ \
        || $5 !~ /^[0-9]+$/) {
        fail_not_run("invalid-sample")
    }

    sample_count += 1
    elapsed[sample_count] = $1 + 0
    continuous[sample_count] = $2 + 0
    rss[sample_count] = $3 + 0
    bytes[sample_count] = $4 + 0
    resizes[sample_count] = $5 + 0

    if (sample_count > 1) {
        elapsed_delta = elapsed[sample_count] - elapsed[sample_count - 1]
        continuous_delta = continuous[sample_count] - continuous[sample_count - 1]
        if (elapsed_delta <= 0 || continuous_delta <= 0) {
            fail_not_run("non-monotonic-samples")
        }
        if (abs(elapsed_delta - required_interval_ms) > cadence_tolerance_ms) {
            fail_not_run("invalid-elapsed-cadence")
        }
        expected_continuous_delta = elapsed_delta * 1000000
        if (abs(continuous_delta - expected_continuous_delta) \
            > cadence_tolerance_ms * 1000000) {
            fail_not_run("invalid-continuous-cadence")
        }
        if (bytes[sample_count] < bytes[sample_count - 1]) {
            fail_not_run("workload-bytes-regressed")
        }
        if (resizes[sample_count] < resizes[sample_count - 1]) {
            fail_not_run("resize-count-regressed")
        }
    }
}

END {
    in_end = 1
    if (invalid) {
        print "format_version\t3"
        print "result\tNOT-RUN"
        print "reason\t" not_run_reason
        exit 2
    }
    if (metadata["format_version"] != "4") {
        fail_not_run("unsupported-format-version")
    }
    if (metadata["scenario"] != "resize") {
        fail_not_run("invalid-scenario")
    }
    if (metadata["sample_interval_ms"] !~ /^[0-9]+$/ \
        || metadata["sample_interval_ms"] + 0 != required_interval_ms) {
        fail_not_run("sample-interval-is-not-10-seconds")
    }
    if (metadata["requested_duration_ms"] !~ /^[0-9]+$/ \
        || metadata["requested_duration_ms"] + 0 != required_duration_ms) {
        fail_not_run("requested-duration-is-not-five-minutes")
    }
    if (metadata["workload_authentication"] != "hmac-sha256" \
        || metadata["progress_interval_ms"] != "1000" \
        || metadata["maximum_progress_age_ms"] != "2000") {
        fail_not_run("workload-progress-authentication-invalid")
    }
    if (metadata["plan_start_continuous_ns"] !~ /^[1-9][0-9]*$/ \
        || metadata["measurement_start_continuous_ns"] \
            != metadata["plan_start_continuous_ns"]) {
        fail_not_run("measurement-boundary-invalid")
    }
    for (required_hash_index = 1; required_hash_index <= 6; required_hash_index += 1) {
        required_hash_key = required_hash_index == 1 ? "subject_identity_sha256" \
            : required_hash_index == 2 ? "workload_events_sha256" \
            : required_hash_index == 3 ? "workload_metadata_sha256" \
            : required_hash_index == 4 ? "ready_receipt_sha256" \
            : required_hash_index == 5 ? "plan_start_gate_sha256" \
            : "driver_events_sha256"
        if (length(metadata[required_hash_key]) != 64 \
            || metadata[required_hash_key] !~ /^[0-9a-f]+$/) {
            fail_not_run("invalid-" required_hash_key)
        }
    }
    if (metadata["distinct_geometry_count"] !~ /^[0-9]+$/ \
        || metadata["distinct_geometry_count"] + 0 < 3) {
        fail_not_run("geometry-has-no-meaningful-variance")
    }
    if (metadata["geometry_change_count"] !~ /^[0-9]+$/ \
        || metadata["geometry_change_count"] + 0 < required_resize_cycles) {
        fail_not_run("geometry-change-count-is-too-small")
    }
    if (metadata["completed_resize_cycles"] !~ /^[0-9]+$/ \
        || metadata["completed_resize_cycles"] + 0 < required_resize_cycles) {
        fail_not_run("completed-resize-count-is-too-small")
    }
    if (metadata["geometry_correlated"] != "true") {
        fail_not_run("geometry-is-not-correlated")
    }
    if (metadata["status"] != "complete") {
        fail_not_run("capture-status-is-not-complete")
    }
    if (sample_count < required_samples) {
        fail_not_run("insufficient-samples")
    }
    if (elapsed[1] > cadence_tolerance_ms \
        || elapsed[sample_count] < required_duration_ms \
        || elapsed[sample_count] > required_duration_ms + cadence_tolerance_ms) {
        fail_not_run("capture-does-not-cover-five-minutes")
    }
    if (bytes[sample_count] <= bytes[1]) {
        fail_not_run("workload-byte-count-has-no-growth")
    }
    resize_delta = resizes[sample_count] - resizes[1]
    if (resizes[sample_count] < required_resize_cycles) {
        fail_not_run("sampled-resize-count-is-too-small")
    }
    if (resizes[sample_count] > metadata["completed_resize_cycles"] + 0) {
        fail_not_run("sampled-resize-count-exceeds-completed-events")
    }

    sum_x = 0
    sum_y = 0
    for (i = 1; i <= sample_count; i += 1) {
        sum_x += resizes[i]
        sum_y += rss[i]
    }
    mean_x = sum_x / sample_count
    mean_y = sum_y / sample_count
    sxx = 0
    sxy = 0
    for (i = 1; i <= sample_count; i += 1) {
        x_delta = resizes[i] - mean_x
        y_delta = rss[i] - mean_y
        sxx += x_delta * x_delta
        sxy += x_delta * y_delta
    }
    if (sxx <= 0) {
        fail_not_run("resize-count-has-no-variance")
    }
    slope = sxy / sxx
    intercept = mean_y - slope * mean_x
    residual_sum_squares = 0
    for (i = 1; i <= sample_count; i += 1) {
        residual = rss[i] - (intercept + slope * resizes[i])
        residual_sum_squares += residual * residual
    }
    degrees_of_freedom = sample_count - 2
    if (degrees_of_freedom <= 0) {
        fail_not_run("insufficient-regression-degrees-of-freedom")
    }
    slope_standard_error = sqrt((residual_sum_squares / degrees_of_freedom) / sxx)
    if (slope_standard_error != slope_standard_error) {
        fail_not_run("invalid-regression-precision")
    }
    lower_slope = slope - 1.96 * slope_standard_error
    upper_slope = slope + 1.96 * slope_standard_error

    baseline_count = int(sample_count / 5)
    if (baseline_count < 3) {
        baseline_count = 3
    }
    baseline_median = median_prefix(baseline_count)
    tolerance = baseline_median * 0.10
    if (tolerance < 65536) {
        tolerance = 65536
    }
    upper_predicted_growth = upper_slope * resize_delta
    if (upper_predicted_growth < 0) {
        upper_predicted_growth = 0
    }
    lower_bound_allows_plateau = lower_slope <= 0
    upper_growth_is_bounded = upper_predicted_growth <= tolerance
    point_predicted_growth = slope * resize_delta
    if (point_predicted_growth < 0) {
        point_predicted_growth = 0
    }
    if (lower_slope > 0 || point_predicted_growth > tolerance) {
        result = "FAIL"
    } else if (!upper_growth_is_bounded) {
        print "format_version\t3"
        print "result\tNOT-RUN"
        print "reason\tresize-rss-regression-is-insufficiently-precise"
        exit 2
    } else {
        result = "PASS"
    }

    print "format_version\t3"
    print "sample_count\t" sample_count
    print "sample_interval_ms\t" required_interval_ms
    print "requested_duration_ms\t" required_duration_ms
    print "initial_resize_count\t" resizes[1]
    print "final_resize_count\t" resizes[sample_count]
    print "resize_count_delta\t" resize_delta
    print "baseline_sample_count\t" baseline_count
    printf "baseline_median_rss_kib\t%.3f\n", baseline_median
    printf "rss_kib_per_resize_slope\t%.9f\n", slope
    printf "slope_standard_error\t%.9f\n", slope_standard_error
    printf "slope_95_lower\t%.9f\n", lower_slope
    printf "slope_95_upper\t%.9f\n", upper_slope
    printf "upper_predicted_growth_kib\t%.3f\n", upper_predicted_growth
    printf "point_predicted_growth_kib\t%.3f\n", point_predicted_growth
    printf "growth_tolerance_kib\t%.3f\n", tolerance
    print "lower_bound_allows_plateau\t" \
        (lower_bound_allows_plateau ? "true" : "false")
    print "upper_growth_is_bounded\t" \
        (upper_growth_is_bounded ? "true" : "false")
    print "result\t" result
    exit result == "PASS" ? 0 : 1
}
