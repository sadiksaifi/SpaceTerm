BEGIN {
    FS = "\t"
    window_seconds = 300
    required_interval_seconds = 10
    cadence_tolerance_seconds = 1
    growth_noise_kib = 1024
    minimum_duration_seconds = window_seconds * 2
    sample_count = 0
    status_count = 0
}

function not_run(reason) {
    if (not_run_reason == "") {
        not_run_reason = reason
        print "error: RSS evidence is not runnable: " reason > "/dev/stderr"
    }
    invalid = 1
    exit 2
}

function absolute(value) {
    return value < 0 ? -value : value
}

function emit_not_run(reason) {
    if (not_run_reason == "") {
        not_run_reason = reason
        print "error: RSS evidence is not runnable: " reason > "/dev/stderr"
    }
    print "format_version\t2"
    print "result\tNOT-RUN"
    print "reason\t" not_run_reason
    exit 2
}

function emit_unattested() {
    print "format_version\t2"
    print "result\tNOT-RUN"
    print "reason\tapp-owned-ingestion-attestation-unavailable"
    exit 2
}

NR == 1 {
    if ($0 != "elapsed_seconds\tepoch_seconds\trss_kib") {
        not_run("unexpected-header")
    }
    next
}

/^#/ {
    if (NF != 2) {
        not_run("invalid-metadata")
    }
    key = substr($1, 3)
    if (key == "format_version") {
        if (format_version != "") {
            not_run("duplicate-format-version")
        }
        format_version = $2
    } else if (key == "sample_interval_seconds") {
        if (sample_interval_seconds != "") {
            not_run("duplicate-sample-interval")
        }
        sample_interval_seconds = $2
    } else if (key == "requested_duration_seconds") {
        if (requested_duration_seconds != "") {
            not_run("duplicate-requested-duration")
        }
        requested_duration_seconds = $2
    } else if (key == "started_epoch_seconds") {
        if (started_epoch_seconds != "") {
            not_run("duplicate-start-time")
        }
        started_epoch_seconds = $2
    } else if (key == "workload_emitted_bytes") {
        if (workload_emitted_bytes != "") {
            not_run("duplicate-workload-emitted-bytes")
        }
        workload_emitted_bytes = $2
    } else if (key == "workload_metrics_sha256") {
        workload_metrics_sha256 = $2
    } else if (key == "output_receipt_sha256") {
        output_receipt_sha256 = $2
    } else if (key == "campaign_id") {
        campaign_id = $2
    } else if (key == "scenario") {
        scenario = $2
    } else if (key == "session_id") {
        session_id = $2
    } else if (key == "process_identity_sha256") {
        process_identity_sha256 = $2
    } else if (key == "sampler_tool_sha256") {
        sampler_tool_sha256 = $2
    } else if (key == "workload_tool_sha256") {
        workload_tool_sha256 = $2
    } else if (key == "analyzer_tool_sha256") {
        analyzer_tool_sha256 = $2
    } else if (key == "process_inspector_tool_sha256") {
        process_inspector_tool_sha256 = $2
    } else if (key == "status") {
        status_count += 1
        status = $2
    }
    next
}

{
    if (status_count > 0) {
        not_run("sample-after-status")
    }
    if (NF != 3 || $1 !~ /^[0-9]+$/ || $2 !~ /^[0-9]+$/ || $3 !~ /^[0-9]+$/) {
        not_run("invalid-sample")
    }
    sample_count += 1
    elapsed[sample_count] = $1 + 0
    epoch[sample_count] = $2 + 0
    rss[sample_count] = $3 + 0
    sum_elapsed += elapsed[sample_count]
    sum_rss += rss[sample_count]
    sum_elapsed_rss += elapsed[sample_count] * rss[sample_count]
    sum_elapsed_squared += elapsed[sample_count] * elapsed[sample_count]

    if (sample_count > 1) {
        elapsed_delta = elapsed[sample_count] - elapsed[sample_count - 1]
        epoch_delta = epoch[sample_count] - epoch[sample_count - 1]
        if (elapsed_delta <= 0 || epoch_delta <= 0) {
            not_run("non-monotonic-samples")
        }
        if (absolute(elapsed_delta - required_interval_seconds) > cadence_tolerance_seconds) {
            not_run("invalid-sample-cadence")
        }
        if (absolute(epoch_delta - required_interval_seconds) > cadence_tolerance_seconds) {
            not_run("invalid-epoch-cadence")
        }
    }
}

END {
    if (invalid) {
        emit_not_run(not_run_reason)
    }
    if (format_version != "2") {
        emit_not_run("unsupported-format-version")
    }
    if (sample_interval_seconds !~ /^[0-9]+$/ \
        || sample_interval_seconds + 0 != required_interval_seconds) {
        emit_not_run("sample-interval-is-not-10-seconds")
    }
    if (requested_duration_seconds !~ /^[0-9]+$/ \
        || requested_duration_seconds + 0 < minimum_duration_seconds) {
        emit_not_run("requested-duration-does-not-cover-two-windows")
    }
    if (started_epoch_seconds !~ /^[0-9]+$/) {
        emit_not_run("invalid-start-time")
    }
    if (workload_emitted_bytes !~ /^[1-9][0-9]*$/) {
        emit_not_run("invalid-workload-emitted-bytes")
    }
    if (workload_metrics_sha256 !~ /^[0-9a-f]{64}$/ \
        || output_receipt_sha256 !~ /^[0-9a-f]{64}$/ \
        || process_identity_sha256 !~ /^[0-9a-f]{64}$/ \
        || sampler_tool_sha256 !~ /^[0-9a-f]{64}$/ \
        || workload_tool_sha256 !~ /^[0-9a-f]{64}$/ \
        || analyzer_tool_sha256 !~ /^[0-9a-f]{64}$/ \
        || process_inspector_tool_sha256 !~ /^[0-9a-f]{64}$/ \
        || campaign_id !~ /^[A-Za-z0-9][A-Za-z0-9._-]*$/ \
        || scenario !~ /^[A-Za-z0-9][A-Za-z0-9._-]*$/ \
        || session_id !~ /^[A-Za-z0-9][A-Za-z0-9._-]*$/) {
        emit_not_run("capture-bindings-are-incomplete")
    }
    if (status_count != 1 || status != "complete") {
        emit_not_run("capture-status-is-not-complete")
    }
    if (sample_count < 4) {
        emit_not_run("insufficient-samples")
    }
    if (elapsed[1] > cadence_tolerance_seconds) {
        emit_not_run("capture-does-not-start-at-zero")
    }

    requested_duration = requested_duration_seconds + 0
    if (requested_duration % required_interval_seconds != 0) {
        emit_not_run("requested-duration-is-not-cadence-aligned")
    }
    final_elapsed = elapsed[sample_count]
    if (final_elapsed < requested_duration \
        || final_elapsed > requested_duration + cadence_tolerance_seconds) {
        emit_not_run("capture-does-not-cover-requested-duration")
    }
    expected_sample_count = int(requested_duration / required_interval_seconds) + 1
    if (sample_count != expected_sample_count) {
        emit_not_run("unexpected-sample-count")
    }
    if (absolute(epoch[1] - started_epoch_seconds) > cadence_tolerance_seconds \
        || absolute((epoch[sample_count] - epoch[1]) - requested_duration) \
            > cadence_tolerance_seconds) {
        emit_not_run("epoch-coverage-does-not-match-request")
    }
    for (sample_index = 1; sample_index <= sample_count; sample_index += 1) {
        if (absolute((epoch[sample_index] - epoch[1]) - elapsed[sample_index]) \
            > cadence_tolerance_seconds) {
            emit_not_run("elapsed-and-epoch-clocks-diverge")
        }
    }

    first_count = 0
    final_count = 0
    final_window_start = requested_duration - window_seconds
    for (sample_index = 1; sample_index <= sample_count; sample_index += 1) {
        if (elapsed[sample_index] < window_seconds) {
            if (first_count == 0 || rss[sample_index] < first_min) {
                first_min = rss[sample_index]
            }
            if (first_count == 0 || rss[sample_index] > first_max) {
                first_max = rss[sample_index]
            }
            first_sum += rss[sample_index]
            first_count += 1
        }
        if (elapsed[sample_index] >= final_window_start \
            && elapsed[sample_index] < requested_duration) {
            if (final_count == 0 || rss[sample_index] < final_min) {
                final_min = rss[sample_index]
            }
            if (final_count == 0 || rss[sample_index] > final_max) {
                final_max = rss[sample_index]
            }
            final_sum += rss[sample_index]
            final_count += 1
        }
        if (sample_index == 1 || rss[sample_index] > global_max) {
            global_max = rss[sample_index]
        }
    }
    if (first_count < 2 || final_count < 2) {
        emit_not_run("analysis-windows-are-not-covered")
    }

    first_range = first_max - first_min
    final_range = final_max - final_min
    range_change = absolute(final_range - first_range)
    ten_percent = int(first_range * 0.10 + 0.999999)
    tolerance = ten_percent > 65536 ? ten_percent : 65536
    first_mean = first_sum / first_count
    final_mean = final_sum / final_count
    level_growth = final_mean - first_mean
    regression_denominator = sample_count * sum_elapsed_squared \
        - sum_elapsed * sum_elapsed
    if (regression_denominator <= 0) {
        emit_not_run("invalid-trend-basis")
    }
    rss_slope_per_second = (sample_count * sum_elapsed_rss \
        - sum_elapsed * sum_rss) / regression_denominator
    trend_growth = rss_slope_per_second * requested_duration
    endpoint_growth = rss[sample_count] - rss[1]
    maximum_growth = level_growth
    if (trend_growth > maximum_growth) {
        maximum_growth = trend_growth
    }
    if (endpoint_growth > maximum_growth) {
        maximum_growth = endpoint_growth
    }
    if (maximum_growth < 0) {
        maximum_growth = 0
    }
    rss_growth_per_gib = trend_growth * 1073741824 \
        / (workload_emitted_bytes + 0)
    byte_normalized_growth_limit_kib_per_gib = 0
    range_plateau = range_change <= tolerance
    high_water_growth = global_max - first_max
    no_growth = maximum_growth <= growth_noise_kib \
        && high_water_growth <= growth_noise_kib
    byte_normalized_no_growth = rss_growth_per_gib \
        <= byte_normalized_growth_limit_kib_per_gib
    result = range_plateau && no_growth && byte_normalized_no_growth \
        ? "PASS" : "FAIL"

    # This generic workload layer does not yet include a packaged-app producer
    # for the terminal-ingestion receipt. Never convert operator-authored
    # metrics into acceptance until that independent attestation seam lands.
    if (result == "PASS") {
        emit_unattested()
    }

    print "format_version\t2"
    print "sample_count\t" sample_count
    print "sample_interval_seconds\t" required_interval_seconds
    print "cadence_tolerance_seconds\t" cadence_tolerance_seconds
    print "growth_noise_kib\t" growth_noise_kib
    print "requested_duration_seconds\t" requested_duration
    print "workload_emitted_bytes\t" workload_emitted_bytes
    print "window_seconds\t" window_seconds
    print "first_sample_count\t" first_count
    print "first_min_rss_kib\t" first_min
    print "first_max_rss_kib\t" first_max
    print "first_range_rss_kib\t" first_range
    print "final_sample_count\t" final_count
    print "final_min_rss_kib\t" final_min
    print "final_max_rss_kib\t" final_max
    print "final_range_rss_kib\t" final_range
    print "range_change_rss_kib\t" range_change
    print "tolerance_rss_kib\t" tolerance
    printf "first_mean_rss_kib\t%.3f\n", first_mean
    printf "final_mean_rss_kib\t%.3f\n", final_mean
    printf "level_growth_rss_kib\t%.3f\n", level_growth
    printf "trend_rss_kib_per_second\t%.6f\n", rss_slope_per_second
    printf "trend_growth_rss_kib\t%.3f\n", trend_growth
    printf "endpoint_growth_rss_kib\t%.3f\n", endpoint_growth
    printf "maximum_growth_rss_kib\t%.3f\n", maximum_growth
    print "global_max_rss_kib\t" global_max
    printf "high_water_growth_rss_kib\t%.3f\n", high_water_growth
    printf "trend_growth_rss_kib_per_gib_emitted\t%.3f\n", rss_growth_per_gib
    printf "byte_normalized_growth_limit_kib_per_gib\t%.3f\n", \
        byte_normalized_growth_limit_kib_per_gib
    print "range_plateau\t" (range_plateau ? "true" : "false")
    print "no_growth_with_bytes\t" (no_growth ? "true" : "false")
    print "byte_normalized_no_growth\t" \
        (byte_normalized_no_growth ? "true" : "false")
    print "result\t" result

    exit result == "PASS" ? 0 : 1
}
