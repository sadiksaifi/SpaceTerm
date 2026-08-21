#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C

TEST_SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly TEST_SCRIPT_DIR
export SPACETERM_ACCEPTANCE_IDENTITY_LIBRARY_ONLY=1
# shellcheck source=scripts/acceptance-identity.sh
source "$TEST_SCRIPT_DIR/acceptance-identity.sh"

TEMP_TEST_DIR="$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-acceptance-identity-test.XXXXXX")"
readonly TEMP_TEST_DIR
trap 'rm -rf -- "$TEMP_TEST_DIR"' EXIT INT TERM

fail() {
    echo "test failure: $*" >&2
    exit 1
}

assert_eq() {
    [[ "$1" == "$2" ]] || fail "expected [$2], observed [$1]"
}

test_value_encoding_round_trips() {
    local value=$'100%\ttab\r\nline'
    assert_eq "$(decode_value "$(encode_value "$value")")" "$value"
}

test_bundle_tree_hash_is_stable_and_content_sensitive() {
    local bundle="$TEMP_TEST_DIR/Test App.app"
    mkdir -p -- "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
    printf 'binary-one\n' > "$bundle/Contents/MacOS/Test"
    chmod 0755 "$bundle/Contents/MacOS/Test"
    printf 'resource\n' > "$bundle/Contents/Resources/value.txt"
    ln -s value.txt "$bundle/Contents/Resources/link"
    local first second changed
    first="$(bundle_tree_sha256 "$bundle")"
    second="$(bundle_tree_sha256 "$bundle")"
    assert_eq "$second" "$first"
    printf 'resource-two\n' > "$bundle/Contents/Resources/value.txt"
    changed="$(bundle_tree_sha256 "$bundle")"
    [[ "$changed" != "$first" ]] || fail "bundle tree hash ignored file content"

    mkdir -p -- "$bundle/Contents/Versions/A" "$bundle/Contents/Versions/B"
    printf 'version-a\n' > "$bundle/Contents/Versions/A/value"
    printf 'version-b\n' > "$bundle/Contents/Versions/B/value"
    ln -s A "$bundle/Contents/Versions/Current"
    first="$(bundle_tree_sha256 "$bundle")"
    unlink "$bundle/Contents/Versions/Current"
    ln -s B "$bundle/Contents/Versions/Current"
    changed="$(bundle_tree_sha256 "$bundle")"
    [[ "$changed" != "$first" ]] || fail "bundle tree hash ignored a directory symlink target"
}

test_display_summary_excludes_serial_and_computes_scale() {
    local plist="$TEMP_TEST_DIR/displays.plist"
    local summary="$TEMP_TEST_DIR/displays.tsv"
    printf '%s\n' \
        '<?xml version="1.0" encoding="UTF-8"?>' \
        '<plist version="1.0"><dict><key>SPDisplaysDataType</key><array><dict>' \
        '<key>spdisplays_ndrvs</key><array><dict>' \
        '<key>_name</key><string>Test Display</string>' \
        '<key>_spdisplays_pixels</key><string>3024 x 1964</string>' \
        '<key>_spdisplays_resolution</key><string>1512 x 982 @ 120.00Hz</string>' \
        '<key>_spdisplays_display-serial-number</key><string>private-serial</string>' \
        '<key>spdisplays_main</key><string>spdisplays_yes</string>' \
        '<key>spdisplays_online</key><string>spdisplays_yes</string>' \
        '</dict></array></dict></array></dict></plist>' > "$plist"
    assert_eq "$(summarize_displays "$plist" "$summary")" "1"
    grep -Fq $'0\tTest Display\t3024 x 1964\t1512 x 982 @ 120.00Hz\t2' "$summary" \
        || fail "display summary omitted required geometry"
    ! grep -Fq "private-serial" "$summary" \
        || fail "display summary retained a serial number"
}

test_manifest_syntax_rejects_duplicate_keys() {
    local manifest="$TEMP_TEST_DIR/duplicate.tsv"
    printf 'schema\tone\nschema\ttwo\n' > "$manifest"
    if (validate_manifest_syntax "$manifest") >/dev/null 2>&1; then
        fail "manifest syntax accepted duplicate keys"
    fi
}

test_font_matching_requires_an_exact_family() {
    local report="$TEMP_TEST_DIR/fonts.txt"
    printf '      Family: JetBrainsMono Nerd Font Mono\n' > "$report"
    font_available "$report" "JetBrainsMono Nerd Font Mono" \
        || fail "exact font family was not found"
    if font_available "$report" "JetBrainsMono Nerd Font"; then
        fail "font family matching accepted a prefix"
    fi
}

test_mounted_dmg_origin_rejects_a_dist_app_claim() {
    local manifest="$TEMP_TEST_DIR/origin.tsv"
    write_record "$manifest" "run.origin" "mounted-dmg"
    write_record "$manifest" "package.app.path" "/tmp/dist/SpaceTerm.app"
    write_record "$manifest" "package.app.executable.path" "/tmp/dist/SpaceTerm.app/Contents/MacOS/SpaceTerm"
    if (validate_origin_claim "$manifest") >/dev/null 2>&1; then
        fail "mounted-DMG origin accepted a dist application path"
    fi

    : > "$manifest"
    write_record "$manifest" "run.origin" "mounted-dmg"
    write_record "$manifest" "package.app.path" "$MOUNTED_DMG_APP_PATH"
    write_record "$manifest" "package.app.executable.path" \
        "$MOUNTED_DMG_APP_PATH/Contents/MacOS/SpaceTerm"
    validate_origin_claim "$manifest"
}

test_mounted_dmg_app_cannot_escape_through_a_symlink() {
    local mount="$TEMP_TEST_DIR/dmg-mount"
    local outside="$TEMP_TEST_DIR/outside/SpaceTerm.app"
    mkdir -p -- "$mount" "$outside"
    ln -s "$outside" "$mount/SpaceTerm.app"
    MOUNT_POINT="$mount"
    if (validate_mounted_app "$mount/SpaceTerm.app") >/dev/null 2>&1; then
        fail "mounted-DMG validation followed a top-level application symlink"
    fi
    unlink "$mount/SpaceTerm.app"
    mkdir -p -- "$mount/SpaceTerm.app"
    validate_mounted_app "$mount/SpaceTerm.app"
    MOUNT_POINT=""
}

test_apfs_attach_structure_binds_the_canonical_mounted_entity() {
    local mount="$TEMP_TEST_DIR/recorded-apfs-mount"
    local canonical_mount recorded_mount plist duplicate
    mkdir -p -- "$mount"
    canonical_mount="$(cd -- "$mount" && pwd -P)"
    recorded_mount="$mount/../recorded-apfs-mount"
    plist="$TEMP_TEST_DIR/recorded-apfs-attach.plist"
    printf '%s\n' \
        '<?xml version="1.0" encoding="UTF-8"?>' \
        '<plist version="1.0"><dict><key>system-entities</key><array>' \
        '<dict><key>dev-entry</key><string>/dev/disk13</string></dict>' \
        '<dict><key>dev-entry</key><string>/dev/disk13s1</string></dict>' \
        '<dict><key>dev-entry</key><string>/dev/disk14</string></dict>' \
        "<dict><key>dev-entry</key><string>/dev/disk14s1</string><key>mount-point</key><string>$recorded_mount</string></dict>" \
        '</array></dict></plist>' > "$plist"

    assert_eq "$(attached_mount_device "$plist" "$canonical_mount")" "/dev/disk14s1"

    duplicate="$TEMP_TEST_DIR/recorded-apfs-duplicate.plist"
    cp -- "$plist" "$duplicate"
    /usr/libexec/PlistBuddy -c \
        "Add :system-entities:4 dict" "$duplicate"
    /usr/libexec/PlistBuddy -c \
        "Add :system-entities:4:dev-entry string /dev/disk15s1" "$duplicate"
    /usr/libexec/PlistBuddy -c \
        "Add :system-entities:4:mount-point string $recorded_mount" "$duplicate"
    if attached_mount_device "$duplicate" "$canonical_mount" >/dev/null 2>&1; then
        fail "APFS attach parser accepted duplicate mounted entities"
    fi
}

write_final_readiness_manifest() {
    local manifest="$1"
    local origin="$2"
    local source="$3"
    local rows="${4:-24}"
    : > "$manifest"
    write_record "$manifest" "run.origin" "$origin"
    write_record "$manifest" "native.observation.source" "$source"
    write_record "$manifest" "font.selected.source" "production-app-observation"
    write_record "$manifest" "font.selected.family" "JetBrainsMono Nerd Font"
    write_record "$manifest" "font.jetbrainsmono-nerd-font.available" "true"
    write_record "$manifest" "host.initial_grid.rows" "$rows"
    write_record "$manifest" "host.initial_grid.columns" "80"
    write_record "$manifest" "host.initial_grid.logical_width" "800.5"
    write_record "$manifest" "host.initial_grid.logical_height" "480"
    write_record "$manifest" "host.initial_grid.backing_pixel_width" "1601"
    write_record "$manifest" "host.initial_grid.backing_pixel_height" "960"
}

test_final_readiness_requires_mounted_origin_and_native_geometry() {
    local manifest="$TEMP_TEST_DIR/font-readiness.tsv"
    write_final_readiness_manifest "$manifest" "app-bundle" "production-app"
    if (validate_final_readiness "$manifest") >/dev/null 2>&1; then
        fail "final readiness accepted a non-mounted application origin"
    fi

    write_final_readiness_manifest "$manifest" "mounted-dmg" "unobserved"
    if (validate_final_readiness "$manifest") >/dev/null 2>&1; then
        fail "final readiness accepted missing production-app launch evidence"
    fi

    write_final_readiness_manifest "$manifest" "mounted-dmg" "production-app" "0"
    if (validate_final_readiness "$manifest") >/dev/null 2>&1; then
        fail "final readiness accepted missing production-app rows"
    fi

    write_final_readiness_manifest "$manifest" "mounted-dmg" "production-app"
    validate_final_readiness "$manifest"
}

write_native_observation_fixture() {
    local observation="$1"
    local app_sha256="$2"
    local font="${3:-JetBrainsMono Nerd Font}"
    : > "$observation"
    write_record "$observation" "schema" "$NATIVE_OBSERVATION_SCHEMA"
    write_record "$observation" "observation.source" "production-app"
    write_record "$observation" "launch.nonce" \
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    write_record "$observation" "run.id" "i43-proof"
    write_record "$observation" "package.app.sha256" "$app_sha256"
    write_record "$observation" "runtime.schema" "spaceterm.acceptance.runtime-stream/v1"
    write_record "$observation" "runtime.sample_interval_ms" "1000"
    write_record "$observation" "runtime.transition_capacity" "64"
    write_record "$observation" "failure.action.schema" "$FAILURE_ACTION_SCHEMA"
    write_record "$observation" "failure.action.enabled" "true"
    write_record "$observation" "process.pid" "1234"
    write_record "$observation" "process.pidversion" "7"
    write_record "$observation" "process.executable.path" \
        "/private/tmp/mount/SpaceTerm.app/Contents/MacOS/SpaceTerm"
    write_record "$observation" "process.executable.device" "17"
    write_record "$observation" "process.executable.inode" "19"
    write_record "$observation" "process.executable.fsid" "23:29"
    write_record "$observation" "process.signature.cdhash" "ABCDEF0123456789"
    write_record "$observation" "process.signature.identifier" "dev.sadiksaifi.spaceterm"
    write_record "$observation" "process.signature.team_identifier" ""
    write_record "$observation" "terminal_font_selected" "$font"
    write_record "$observation" "initial_grid.rows" "24"
    write_record "$observation" "initial_grid.columns" "80"
    write_record "$observation" "initial_grid.logical_width" "800.5"
    write_record "$observation" "initial_grid.logical_height" "480"
    write_record "$observation" "initial_grid.backing_pixel_width" "1601"
    write_record "$observation" "initial_grid.backing_pixel_height" "960"
    write_runtime_observation_fixture "$observation" "$app_sha256"
    local parent metadata failure_actions
    parent="$(dirname -- "$observation")"
    metadata="$parent/runtime-metadata.tsv"
    failure_actions="$parent/failure-actions.tsv"
    write_record "$observation" "provisional.observation.sha256" \
        "$(provisional_native_observation_sha256 "$observation")"
    write_record "$observation" "runtime.metadata.schema" \
        "$RUNTIME_OBSERVATION_METADATA_SCHEMA"
    write_record "$observation" "runtime.metadata.path" "runtime-metadata.tsv"
    write_record "$observation" "runtime.metadata.sha256" "$(sha256_file "$metadata")"
    write_record "$observation" "failure.result.schema" "$FAILURE_ACTION_RESULT_SCHEMA"
    write_record "$observation" "failure.actions.path" "failure-actions.tsv"
    write_record "$observation" "failure.actions.sha256" "$(sha256_file "$failure_actions")"
    write_record "$observation" "failure.request_count" "1"
    write_record "$observation" "failure.result_count" "4"
    write_record "$observation" "observation.complete" "true"
}

append_failure_row() {
    if (( $# == 17 )); then
        set -- "$@" 0 0 0 0
    fi
    (( $# == 21 )) || fail "failure row fixture has an unexpected field count"
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$@"
}

append_failure_group() {
    local output="$1"
    local case_id="$2"
    local request_id="$3"
    local sequence="$4"
    local pane_id="1"
    local failure_class recoverability operation pending
    append_failure_row \
        "$request_id" "$sequence" "$case_id" armed accepted "$pane_id" running \
        none none none 0 5 5 5 none 1 1 >> "$output"
    case "$case_id" in
        presentation-invalid-scale)
            failure_class=presentation
            recoverability=recoverable
            operation=update-backing-scale
            pending=presentation
            ;;
        presentation-glyph)
            failure_class=presentation
            recoverability=recoverable
            operation=paint-terminal-presentation
            pending=presentation
            ;;
        renderer-image-preflight)
            failure_class=resource
            recoverability=recoverable
            operation=paint-terminal-graphics
            pending=renderer-resources
            ;;
        renderer-resource-before-sync|renderer-resource-after-staging)
            failure_class=resource
            recoverability=recoverable
            operation=prepare-terminal-graphics
            pending=renderer-resources
            ;;
        pasteboard-write)
            append_failure_row \
                "$request_id" "$sequence" "$case_id" injected failed-state "$pane_id" \
                failed platform recoverable write-selection-pasteboard 1 5 5 5 copy-selection 1 1 \
                >> "$output"
            append_failure_row \
                "$request_id" "$sequence" "$case_id" retry-requested accepted "$pane_id" \
                failed platform recoverable write-selection-pasteboard 2 6 6 6 copy-selection 1 1 \
                >> "$output"
            append_failure_row \
                "$request_id" "$sequence" "$case_id" completed recovered "$pane_id" \
                running none none none 3 7 7 7 none 1 1 >> "$output"
            return
            ;;
        pty-fatal)
            append_failure_row \
                "$request_id" "$sequence" "$case_id" injected failed-state "$pane_id" \
                failed pty fatal read-shell-output 1 5 5 5 none 0 1 >> "$output"
            append_failure_row \
                "$request_id" "$sequence" "$case_id" completed closed "$pane_id" \
                failed pty fatal read-shell-output 2 5 5 5 none 0 0 >> "$output"
            return
            ;;
        emulator-fatal)
            append_failure_row \
                "$request_id" "$sequence" "$case_id" injected failed-state "$pane_id" \
                failed emulator fatal session-runtime 1 5 5 5 none 0 1 >> "$output"
            append_failure_row \
                "$request_id" "$sequence" "$case_id" completed closed "$pane_id" \
                failed emulator fatal session-runtime 2 5 5 5 none 0 0 >> "$output"
            return
            ;;
        normal-exit-control)
            append_failure_row \
                "$request_id" "$sequence" "$case_id" completed exited "$pane_id" \
                exited none none none 1 5 5 5 none 0 1 >> "$output"
            return
            ;;
        *) fail "unsupported failure fixture case: $case_id" ;;
    esac
    if [[ "$case_id" == "renderer-resource-after-staging" ]]; then
        {
            append_failure_row \
                "$request_id" "$sequence" "$case_id" injected failed-state "$pane_id" \
                failed "$failure_class" "$recoverability" "$operation" 1 5 5 5 "$pending" 1 1 \
                2 2048 2 2048
            append_failure_row \
                "$request_id" "$sequence" "$case_id" retry-requested accepted "$pane_id" \
                failed "$failure_class" "$recoverability" "$operation" 2 5 5 5 "$pending" 1 1 \
                2 2048 2 2048
            append_failure_row \
                "$request_id" "$sequence" "$case_id" completed recovered "$pane_id" \
                running none none none 3 5 5 5 none 1 1 2 2048 2 2048
        } >> "$output"
        return
    fi
    {
        append_failure_row \
            "$request_id" "$sequence" "$case_id" injected failed-state "$pane_id" \
            failed "$failure_class" "$recoverability" "$operation" 1 5 5 5 "$pending" 1 1
        append_failure_row \
            "$request_id" "$sequence" "$case_id" retry-requested accepted "$pane_id" \
            failed "$failure_class" "$recoverability" "$operation" 2 5 5 5 "$pending" 1 1
        append_failure_row \
            "$request_id" "$sequence" "$case_id" completed recovered "$pane_id" \
            running none none none 3 5 5 5 none 1 1
    } >> "$output"
}

write_runtime_observation_fixture() {
    local observation="$1"
    local app_sha256="$2"
    local parent samples events failure_actions metadata
    parent="$(dirname -- "$observation")"
    samples="$parent/runtime-samples.tsv"
    events="$parent/runtime-events.tsv"
    failure_actions="$parent/failure-actions.tsv"
    metadata="$parent/runtime-metadata.tsv"
    printf '%s\n' "$RUNTIME_SAMPLES_HEADER" > "$samples"
    printf '%s\n' $'0\t1\t1\t1\t1\t0\t0\t1\t1\t1\t1\t1\t1\t1\t1\t1\t0\t0\t1\t1\t0\t24\t24\t0\t0\t0\t0\t0\t0\t24\t80\t1600\t960\t0\texited\t0' >> "$samples"
    printf '%s\n' "$RUNTIME_EVENTS_HEADER" > "$events"
    printf '%s\n' $'0\t1\tsession-exited\t1\t1\t0' >> "$events"
    printf '%s\n' "$FAILURE_ACTIONS_HEADER" > "$failure_actions"
    append_failure_group \
        "$failure_actions" presentation-invalid-scale \
        aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 0
    : > "$metadata"
    write_record "$metadata" "schema" "$RUNTIME_OBSERVATION_METADATA_SCHEMA"
    write_record "$metadata" "observation.source" "production-app"
    write_record "$metadata" "run.id" "i43-proof"
    write_record "$metadata" "package.app.sha256" "$app_sha256"
    write_record "$metadata" "process.pid" "1234"
    write_record "$metadata" "runtime.samples.path" "runtime-samples.tsv"
    write_record "$metadata" "runtime.samples.sha256" "$(sha256_file "$samples")"
    write_record "$metadata" "runtime.events.path" "runtime-events.tsv"
    write_record "$metadata" "runtime.events.sha256" "$(sha256_file "$events")"
    write_record "$metadata" "failure.action.schema" "$FAILURE_ACTION_SCHEMA"
    write_record "$metadata" "failure.action.enabled" "true"
    write_record "$metadata" "failure.result.schema" "$FAILURE_ACTION_RESULT_SCHEMA"
    write_record "$metadata" "failure.actions.path" "failure-actions.tsv"
    write_record "$metadata" "failure.actions.sha256" "$(sha256_file "$failure_actions")"
    write_record "$metadata" "failure.request_count" "1"
    write_record "$metadata" "failure.result_count" "4"
    write_record "$metadata" "observer.started_continuous_ns" "1"
    write_record "$metadata" "observer.ended_continuous_ns" "1"
    write_record "$metadata" "observer.sample_interval_ms" "1000"
    write_record "$metadata" "observer.transition_capacity" "64"
    write_record "$metadata" "observer.sample_count" "1"
    write_record "$metadata" "observer.event_count" "1"
    write_record "$metadata" "observer.status" "complete"
    write_record "$metadata" "observation.complete" "true"
}

replace_record_value() {
    local manifest="$1"
    local key="$2"
    local value="$3"
    local encoded
    encoded="$(encode_value "$value")"
    sed -i '' "s|^${key}"$'\t'".*$|${key}"$'\t'"${encoded}|" "$manifest"
}

refresh_runtime_closure() {
    local observation="$1"
    local request_count="$2"
    local parent metadata failure_actions result_count
    parent="$(dirname -- "$observation")"
    metadata="$parent/runtime-metadata.tsv"
    failure_actions="$parent/failure-actions.tsv"
    result_count="$(( $(awk 'END { print NR }' "$failure_actions") - 1 ))"
    replace_record_value \
        "$metadata" failure.actions.sha256 "$(sha256_file "$failure_actions")"
    replace_record_value "$metadata" failure.request_count "$request_count"
    replace_record_value "$metadata" failure.result_count "$result_count"
    replace_record_value \
        "$observation" failure.actions.sha256 "$(sha256_file "$failure_actions")"
    replace_record_value "$observation" failure.request_count "$request_count"
    replace_record_value "$observation" failure.result_count "$result_count"
    replace_record_value \
        "$observation" runtime.metadata.sha256 "$(sha256_file "$metadata")"
}

replace_failure_groups() {
    local observation="$1"
    shift
    local failure_actions sequence case_id request_id
    failure_actions="$(dirname -- "$observation")/failure-actions.tsv"
    printf '%s\n' "$FAILURE_ACTIONS_HEADER" > "$failure_actions"
    sequence=0
    for case_id in "$@"; do
        printf -v request_id '%064x' "$((sequence + 1))"
        append_failure_group "$failure_actions" "$case_id" "$request_id" "$sequence"
        sequence="$((sequence + 1))"
    done
    refresh_runtime_closure "$observation" "$sequence"
}

assert_runtime_rejected() {
    local observation="$1"
    local message="$2"
    if (validate_runtime_observation "$observation") >/dev/null 2>&1; then
        fail "$message"
    fi
}

test_native_observation_is_hashed_bound_and_font_consistent() {
    local run_dir="$TEMP_TEST_DIR/native-run"
    local observation="$run_dir/identity/native-observation.tsv"
    local manifest="$run_dir/run-identity.tsv"
    local font_report="$TEMP_TEST_DIR/native-fonts.txt"
    local font_manifest="$TEMP_TEST_DIR/native-font-manifest.tsv"
    local app_sha256="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    mkdir -p -- "$run_dir/identity"
    write_native_observation_fixture "$observation" "$app_sha256"
    validate_native_observation "$observation" "$app_sha256"
    validate_runtime_observation "$observation"
    sed -i '' \
        's/spaceterm.acceptance.failure-action\/v1/spaceterm.acceptance.failure-action\/forged/' \
        "$observation"
    if (validate_native_observation "$observation" "$app_sha256") >/dev/null 2>&1; then
        fail "native observation accepted a forged failure action schema"
    fi
    write_native_observation_fixture "$observation" "$app_sha256"
    printf '%s\n' \
        $'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\t0\tpresentation-invalid-scale\tarmed\taccepted\t1\trunning\tnone\tnone\tnone\t0\t0\t0\tunavailable\tnone\t1\t1' \
        >> "$run_dir/identity/failure-actions.tsv"
    if (validate_runtime_observation "$observation") >/dev/null 2>&1; then
        fail "runtime observation accepted a changed failure action artifact"
    fi
    write_native_observation_fixture "$observation" "$app_sha256"
    sed -i '' \
        's/spaceterm.acceptance.failure-action-result\/v2/spaceterm.acceptance.failure-action-result\/forged/' \
        "$run_dir/identity/runtime-metadata.tsv"
    refresh_runtime_closure "$observation" 1
    if (validate_runtime_observation "$observation") >/dev/null 2>&1; then
        fail "runtime observation accepted a forged failure result schema"
    fi
    write_native_observation_fixture "$observation" "$app_sha256"
    sed -i '' 's/observer.status\tcomplete/observer.status\tnot-run/' \
        "$run_dir/identity/runtime-metadata.tsv"
    refresh_runtime_closure "$observation" 1
    if (validate_runtime_observation "$observation") >/dev/null 2>&1; then
        fail "runtime observation accepted NOT-RUN metadata as complete"
    fi
    write_native_observation_fixture "$observation" "$app_sha256"
    sed -i '' 's/observation.complete\ttrue/observation.complete\tfalse/' "$observation"
    if (validate_native_observation "$observation" "$app_sha256") >/dev/null 2>&1; then
        fail "native observation accepted an incomplete authenticated envelope"
    fi
    write_native_observation_fixture "$observation" "$app_sha256"
    : > "$manifest"
    write_native_observation_identity "$manifest" "$observation"
    write_record "$manifest" "package.app.sha256" "$app_sha256"
    write_record "$manifest" "run.id" "i43-proof"
    write_record "$manifest" "package.app.signature.cdhash" "ABCDEF0123456789"
    write_record "$manifest" "package.app.signature.identifier" "dev.sadiksaifi.spaceterm"
    write_record "$manifest" "package.app.signature.team_identifier" "not-set"
    write_record "$manifest" "font.selected.source" "production-app-observation"
    write_record "$manifest" "font.selected.family" "JetBrainsMono Nerd Font"
    verify_native_observation_identity "$manifest" "$run_dir"
    replace_record_value "$manifest" native.failure.result_count 3
    if (verify_native_observation_identity "$manifest" "$run_dir") >/dev/null 2>&1; then
        fail "run identity accepted a native failure count outside its immutable closure"
    fi
    replace_record_value "$manifest" native.failure.result_count 4
    sed -i '' 's/run.id\ti43-proof/run.id\tforged-run/' "$manifest"
    if (verify_native_observation_identity "$manifest" "$run_dir") >/dev/null 2>&1; then
        fail "native observation accepted a manifest with a different run ID"
    fi
    sed -i '' 's/run.id\tforged-run/run.id\ti43-proof/' "$manifest"
    write_record "$observation" "tampered" "true"
    if (verify_native_observation_identity "$manifest" "$run_dir") >/dev/null 2>&1; then
        fail "native observation verification accepted a changed artifact"
    fi

    write_native_observation_fixture "$observation" "$app_sha256"
    printf '      Family: JetBrainsMono Nerd Font\n' > "$font_report"
    collect_fonts "$font_manifest" "$observation" "$font_report"
    assert_eq "$(manifest_value "$font_manifest" font.selected.source)" \
        "production-app-observation"
    write_native_observation_fixture "$observation" "$app_sha256" "Uninstalled Font"
    if (collect_fonts "$font_manifest" "$observation" "$font_report") >/dev/null 2>&1; then
        fail "font collection accepted an uninstalled arbitrary observation value"
    fi
}

test_failure_action_groups_are_revalidated_offline() {
    local run_dir="$TEMP_TEST_DIR/failure-groups"
    local observation="$run_dir/native-observation.tsv"
    local failure_actions="$run_dir/failure-actions.tsv"
    local app_sha256="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    mkdir -p -- "$run_dir"

    write_native_observation_fixture "$observation" "$app_sha256"
    replace_failure_groups \
        "$observation" presentation-invalid-scale presentation-glyph \
        renderer-image-preflight renderer-resource-before-sync \
        renderer-resource-after-staging pasteboard-write pty-fatal emulator-fatal \
        normal-exit-control
    validate_runtime_observation "$observation"

    write_native_observation_fixture "$observation" "$app_sha256"
    sed -i '' $'s/\t0\tpresentation-invalid-scale\t/\t1\tpresentation-invalid-scale\t/g' \
        "$failure_actions"
    refresh_runtime_closure "$observation" 1
    assert_runtime_rejected "$observation" \
        "offline validation accepted a failure sequence that did not start at zero"

    write_native_observation_fixture "$observation" "$app_sha256"
    sed -i '' '3s/^aaaaaaaa/bbbbbbbb/' "$failure_actions"
    refresh_runtime_closure "$observation" 1
    assert_runtime_rejected "$observation" \
        "offline validation accepted a request ID change inside a group"

    write_native_observation_fixture "$observation" "$app_sha256"
    sed -i '' '3s/'$'\t1\tfailed\t''/'$'\t2\tfailed\t''/' "$failure_actions"
    refresh_runtime_closure "$observation" 1
    assert_runtime_rejected "$observation" \
        "offline validation accepted a pane change inside a group"

    write_native_observation_fixture "$observation" "$app_sha256"
    sed -i '' '2s/'$'\t1\trunning\t''/'$'\t9007199254740992\trunning\t''/' \
        "$failure_actions"
    sed -i '' '3,5s/'$'\t1\t''/'$'\t9007199254740993\t''/' \
        "$failure_actions"
    refresh_runtime_closure "$observation" 1
    assert_runtime_rejected "$observation" \
        "offline validation accepted a precision-hidden Pane change"

    write_native_observation_fixture "$observation" "$app_sha256"
    sed -i '' '4s/'$'\t2\t5\t5\t5\t''/'$'\t0\t5\t5\t5\t''/' "$failure_actions"
    refresh_runtime_closure "$observation" 1
    assert_runtime_rejected "$observation" \
        "offline validation accepted a decreasing state revision"

    write_native_observation_fixture "$observation" "$app_sha256"
    sed -i '' '4s/'$'\t5\t5\t5\tpresentation\t''/'$'\t6\t5\t5\tpresentation\t''/' \
        "$failure_actions"
    refresh_runtime_closure "$observation" 1
    assert_runtime_rejected "$observation" \
        "offline validation accepted render generation drift during retry"

    write_native_observation_fixture "$observation" "$app_sha256"
    replace_failure_groups "$observation" renderer-resource-after-staging
    sed -i '' '4s/'$'\t2\t2048\t2\t2048$''/'$'\t2\t2048\t1\t2048''/' \
        "$failure_actions"
    refresh_runtime_closure "$observation" 1
    assert_runtime_rejected "$observation" \
        "offline validation accepted incomplete staged-resource rollback proof"

    write_native_observation_fixture "$observation" "$app_sha256"
    sed -i '' '3s/'$'\t0\t0\t0\t0$''/'$'\t1\t64\t1\t64''/' "$failure_actions"
    refresh_runtime_closure "$observation" 1
    assert_runtime_rejected "$observation" \
        "offline validation accepted resource metrics for an unrelated failure case"

    write_native_observation_fixture "$observation" "$app_sha256"
    sed -i '' '$d' "$failure_actions"
    refresh_runtime_closure "$observation" 1
    assert_runtime_rejected "$observation" \
        "offline validation accepted an incomplete final failure group"

    write_native_observation_fixture "$observation" "$app_sha256"
    replace_failure_groups "$observation" pasteboard-write
    validate_runtime_observation "$observation"
    sed -i '' '4s/'$'\t6\t6\t6\tcopy-selection\t''/'$'\t4\t6\t6\tcopy-selection\t''/' \
        "$failure_actions"
    refresh_runtime_closure "$observation" 1
    assert_runtime_rejected "$observation" \
        "offline validation accepted a pasteboard generation regression"

    write_native_observation_fixture "$observation" "$app_sha256"
    replace_failure_groups "$observation" pasteboard-write
    sed -i '' '5s/'$'\t7\t7\t7\tnone\t''/'$'\t5\t5\t5\tnone\t''/' \
        "$failure_actions"
    refresh_runtime_closure "$observation" 1
    assert_runtime_rejected "$observation" \
        "offline validation accepted pasteboard completion older than its retry"

    write_native_observation_fixture "$observation" "$app_sha256"
    replace_failure_groups "$observation" pty-fatal
    sed -i '' \
        '3s/'$'\tnone\t0\t1\t0\t0\t0\t0$''/'$'\tnone\t1\t1\t0\t0\t0\t0''/' \
        "$failure_actions"
    refresh_runtime_closure "$observation" 1
    assert_runtime_rejected "$observation" \
        "offline validation accepted terminal input after a fatal injection"

    write_native_observation_fixture "$observation" "$app_sha256"
    replace_failure_groups "$observation" normal-exit-control
    sed -i '' '3s/'$'\tcompleted\texited\t''/'$'\tinjected\tfailed-state\t''/' \
        "$failure_actions"
    refresh_runtime_closure "$observation" 1
    assert_runtime_rejected "$observation" \
        "offline validation accepted an injected phase for normal exit"

    write_native_observation_fixture "$observation" "$app_sha256"
    replace_failure_groups "$observation" presentation-invalid-scale presentation-invalid-scale
    sed -i '' '6,9s/'$'\t1\tpresentation-invalid-scale\t''/'$'\t2\tpresentation-invalid-scale\t''/' \
        "$failure_actions"
    refresh_runtime_closure "$observation" 2
    assert_runtime_rejected "$observation" \
        "offline validation accepted a gap in the global failure sequence"

    write_native_observation_fixture "$observation" "$app_sha256"
    replace_failure_groups "$observation" presentation-invalid-scale presentation-invalid-scale
    sed -i '' '6,9s/^0000000000000000000000000000000000000000000000000000000000000002/0000000000000000000000000000000000000000000000000000000000000001/' \
        "$failure_actions"
    refresh_runtime_closure "$observation" 2
    assert_runtime_rejected "$observation" \
        "offline validation accepted a reused request ID"

    write_native_observation_fixture "$observation" "$app_sha256"
    replace_failure_groups "$observation"
    replace_record_value "$observation" failure.action.enabled false
    replace_record_value "$run_dir/runtime-metadata.tsv" failure.action.enabled false
    replace_record_value \
        "$observation" runtime.metadata.sha256 \
        "$(sha256_file "$run_dir/runtime-metadata.tsv")"
    validate_runtime_observation "$observation"
}

test_private_dmg_stage_survives_source_path_replacement() {
    local source="$TEMP_TEST_DIR/source.dmg"
    local stage_root="$TEMP_TEST_DIR/dmg-stage"
    printf 'immutable staged bytes\n' > "$source"
    hdiutil() { return 0; }
    stage_dmg "$source" "$stage_root"
    unlink "$source"
    printf 'replacement bytes\n' > "$source"
    verify_staged_dmg
    [[ "$(sha256_file "$source")" != "$STAGED_DMG_SHA256" ]] \
        || fail "DMG source-path replacement changed the retained staged vnode"
    release_staged_dmg
    rmdir "$stage_root"
    unset -f hdiutil
}

test_collection_rejects_caller_provided_observation() {
    if (collect_run --native-observation "$TEMP_TEST_DIR/handwritten.tsv") >/dev/null 2>&1; then
        fail "collector accepted a caller-provided native observation"
    fi
}

test_failure_control_is_mounted_dmg_only_and_absolute() {
    local run_dir="$TEMP_TEST_DIR/failure-control-run"
    if (collect_run \
        --run-dir "$run_dir" \
        --origin app-bundle \
        --failure-control "$TEMP_TEST_DIR/failure-control.fifo") >/dev/null 2>&1; then
        fail "collector accepted failure control for a non-mounted origin"
    fi
    if (collect_run \
        --run-dir "$run_dir" \
        --origin mounted-dmg \
        --failure-control relative-control.fifo) >/dev/null 2>&1; then
        fail "collector accepted a relative failure control path"
    fi
    if (collect_run \
        --run-dir "$run_dir" \
        --origin mounted-dmg \
        --failure-control) >/dev/null 2>&1; then
        fail "collector accepted a missing failure control path"
    fi
}

test_failure_action_driver_is_fixed_and_content_free() {
    local control_dir="$TEMP_TEST_DIR/failure-driver"
    local control="$control_dir/control.fifo"
    local status="$control.status"
    local received="$control_dir/received"
    local reader_pid
    mkdir -m 0700 -- "$control_dir"
    mkfifo -m 0600 -- "$control"
    mkfifo -m 0600 -- "$status"
    (
        exec 4> "$status"
        IFS= read -r value < "$control"
        printf '%s\n' "$value" > "$received"
        token="${value#*$'\t'}"
        printf 'accepted\t%s\ncompleted\t%s\n' "$token" "$token" >&4
    ) &
    reader_pid=$!
    "$TEST_SCRIPT_DIR/acceptance/failure-action-driver.sh" \
        --control "$control" --case presentation-invalid-scale
    wait "$reader_pid"
    assert_eq "$(cut -f 1 < "$received")" "presentation-invalid-scale"
    (
        exec 4> "$status"
        IFS= read -r _ < "$control"
        printf 'invalid\n' >&4
    ) &
    reader_pid=$!
    if "$TEST_SCRIPT_DIR/acceptance/failure-action-driver.sh" \
        --control "$control" --case pasteboard-write >/dev/null 2>&1; then
        fail "failure driver accepted a malformed verifier status"
    fi
    wait "$reader_pid"
    if "$TEST_SCRIPT_DIR/acceptance/failure-action-driver.sh" \
        --control "$control" --case $'pasteboard-write\tclipboard-canary' \
        >/dev/null 2>&1; then
        fail "failure driver accepted content outside the fixed case allowlist"
    fi
    chmod 0666 "$control"
    if "$TEST_SCRIPT_DIR/acceptance/failure-action-driver.sh" \
        --control "$control" --case pasteboard-write >/dev/null 2>&1; then
        fail "failure driver accepted a non-private control FIFO"
    fi
}

test_native_launcher_has_required_authenticated_bindings() {
    local launcher="$TEST_SCRIPT_DIR/acceptance-launch-verifier.m"
    for required in \
        'createsNewApplicationInstance = YES' \
        'allowsRunningApplicationSubstitution = NO' \
        'LOCAL_PEERTOKEN' \
        'audit_token_to_pidversion' \
        'proc_pidpath_audittoken' \
        'PROC_PIDREGIONPATHINFO' \
        'kSecGuestAttributeAudit' \
        'SecCodeCheckValidityWithErrors' \
        'kSecCodeInfoUnique' \
        'MNT_RDONLY' \
        'spaceterm.acceptance.native-launch-challenge/v5' \
        'spaceterm.acceptance.failure-action/v1' \
        'spaceterm.acceptance.failure-action-result/v2' \
        'spaceterm.acceptance.ax-subject/v1' \
        'native-observation-live.tsv' \
        'ax-subject.tsv' \
        'process.start.tv-sec' \
        'launch.observation.sha256' \
        'O_NOFOLLOW' \
        'RENAME_EXCL'; do
        grep -Fq "$required" "$launcher" \
            || fail "native launcher omitted required binding: $required"
    done
    ! grep -Fq 'SPACETERM_ACCEPTANCE_NONCE' "$launcher" \
        || fail "native launcher exposes the challenge nonce through the environment"
    ! grep -Fq 'NSProcessInfo.processInfo.environment mutableCopy' "$launcher" \
        || fail "native launcher forwards the harness environment into the Shell Process"
    xcrun clang -fobjc-arc -fblocks -Wall -Wextra -Werror \
        -mmacosx-version-min=11.0 -fsyntax-only "$launcher"
    local helper="$TEMP_TEST_DIR/acceptance-launch-verifier-tests"
    xcrun clang -fobjc-arc -fblocks -Wall -Wextra -Werror \
        -mmacosx-version-min=11.0 -framework AppKit -framework Foundation \
        -framework Security -framework CoreFoundation -lbsm \
        "$launcher" -o "$helper"
    "$helper" --self-test
}

test_replay_must_match_recorded_runtime_facts() {
    local recorded="$TEMP_TEST_DIR/recorded-runtime.tsv"
    local replayed="$TEMP_TEST_DIR/replayed-runtime.tsv"
    local key value
    for key in \
        run.id package.app.sha256 runtime.schema runtime.sample_interval_ms \
        runtime.transition_capacity failure.action.schema failure.action.enabled \
        process.signature.cdhash process.signature.identifier \
        process.signature.team_identifier terminal_font_selected \
        initial_grid.rows initial_grid.columns \
        initial_grid.logical_width initial_grid.logical_height \
        initial_grid.backing_pixel_width initial_grid.backing_pixel_height; do
        case "$key" in
            run.id) value="i43-proof" ;;
            package.app.sha256) value="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" ;;
            runtime.schema) value="spaceterm.acceptance.runtime-stream/v1" ;;
            runtime.sample_interval_ms) value="1000" ;;
            runtime.transition_capacity) value="64" ;;
            failure.action.schema) value="$FAILURE_ACTION_SCHEMA" ;;
            failure.action.enabled) value="false" ;;
            process.signature.cdhash) value="ABCDEF0123456789" ;;
            process.signature.identifier) value="dev.sadiksaifi.spaceterm" ;;
            process.signature.team_identifier) value="" ;;
            terminal_font_selected) value="JetBrainsMono Nerd Font" ;;
            initial_grid.rows) value="24" ;;
            initial_grid.columns) value="80" ;;
            initial_grid.logical_width) value="800" ;;
            initial_grid.logical_height) value="480" ;;
            initial_grid.backing_pixel_width) value="1600" ;;
            initial_grid.backing_pixel_height) value="960" ;;
        esac
        write_record "$recorded" "$key" "$value"
        write_record "$replayed" "$key" "$value"
    done
    compare_runtime_observations "$recorded" "$replayed"
    sed -i '' \
        's/spaceterm.acceptance.failure-action\/v1/spaceterm.acceptance.failure-action\/forged/' \
        "$replayed"
    if (compare_runtime_observations "$recorded" "$replayed") >/dev/null 2>&1; then
        fail "fresh production-app replay accepted a changed failure action schema"
    fi
    sed -i '' \
        's/spaceterm.acceptance.failure-action\/forged/spaceterm.acceptance.failure-action\/v1/' \
        "$replayed"
    sed -i '' 's/initial_grid.rows\t24/initial_grid.rows\t25/' "$replayed"
    if (compare_runtime_observations "$recorded" "$replayed") >/dev/null 2>&1; then
        fail "fresh production-app replay accepted invented runtime geometry"
    fi
}

test_workspace_cleanliness_is_an_initial_invariant() {
    local run_dir="$TEMP_TEST_DIR/run"
    local manifest="$TEMP_TEST_DIR/workspace.tsv"
    mkdir -p -- "$run_dir/workspace"
    write_record "$manifest" "run.workspace_root" "$run_dir/workspace"
    write_record "$manifest" "run.workspace.initially_empty" "true"
    printf 'created during acceptance\n' > "$run_dir/workspace/example.txt"
    validate_workspace_identity "$manifest" "$run_dir"
}

test_public_manifest_redacts_account_paths() {
    local private_manifest="$TEMP_TEST_DIR/private.tsv"
    local public_manifest="$TEMP_TEST_DIR/public.tsv"
    local run_dir="/Users/collector/acceptance/run"
    write_record "$private_manifest" "schema" "$ACCEPTANCE_IDENTITY_SCHEMA"
    write_record "$private_manifest" "run.workspace_root" "$run_dir/workspace"
    write_record "$private_manifest" "package.app.path" "/Users/collector/dist/SpaceTerm.app"
    write_record "$private_manifest" "package.app.executable.path" \
        "/Users/collector/dist/SpaceTerm.app/Contents/MacOS/SpaceTerm"
    write_record "$private_manifest" "package.dmg.path" "/Volumes/builds/SpaceTerm.dmg"
    write_record "$private_manifest" "executable.claude-code.path" \
        "/Volumes/External/another-account/bin/claude"
    write_record "$private_manifest" "executable.bash.path" "/usr/bin/bash"
    write_record "$private_manifest" "executable.zsh.path" "/opt/homebrew/bin/zsh"
    write_record "$private_manifest" "executable.missing.path" ""
    write_record "$private_manifest" "executable.claude-code.version" \
        "Claude from /srv/accounts/another-account/lib/claude"
    write_record "$private_manifest" "executable.bash.version" \
        "bash at /Users/alice/My Tools/[private]/bash"
    write_record "$private_manifest" "documentation.url" "https://example.com/acceptance"
    write_public_manifest "$private_manifest" "$public_manifest" "$run_dir"
    HOME="/Users/different-verifier" \
        verify_public_manifest "$private_manifest" "$public_manifest" "$run_dir"
    (unset HOME; verify_public_manifest "$private_manifest" "$public_manifest" "$run_dir")
    grep -Fq "$PUBLIC_RUN_DIR_TOKEN/workspace" "$public_manifest" \
        || fail "public manifest did not redact the Workspace path"
    grep -Fq $'executable.claude-code.path\t'$PUBLIC_EXECUTABLE_TOKEN "$public_manifest" \
        || fail "public manifest did not fully redact an executable path"
    grep -Fq $'executable.bash.path\t'$PUBLIC_EXECUTABLE_TOKEN "$public_manifest" \
        || fail "public manifest retained a system executable path"
    grep -Fq $'executable.missing.path\t' "$public_manifest" \
        || fail "public manifest changed an empty executable path"
    grep -Fq "$PUBLIC_LOCAL_PATH_VALUE_TOKEN" "$public_manifest" \
        || fail "public manifest did not redact a non-HOME embedded path"
    grep -Fq "https://example.com/acceptance" "$public_manifest" \
        || fail "public manifest confused an HTTPS URL with a local path"
    ! grep -Eq '/Users/|/Volumes/|/srv/accounts/' "$public_manifest" \
        || fail "public manifest retained an account-specific path"

    printf 'tampered.record\tvalue\n' >> "$public_manifest"
    if (verify_public_manifest "$private_manifest" "$public_manifest" "$run_dir") \
        >/dev/null 2>&1; then
        fail "public manifest verification accepted a noncanonical added record"
    fi

    write_public_manifest "$private_manifest" "$public_manifest" "$run_dir"
    sed -i '' 's/DMG_ARTIFACT/ALTERED_ARTIFACT/' "$public_manifest"
    if (verify_public_manifest "$private_manifest" "$public_manifest" "$run_dir") \
        >/dev/null 2>&1; then
        fail "public manifest verification accepted an altered projection record"
    fi
}

test_native_launch_lifecycle_arguments_are_bash_32_nounset_safe() (
    validate_native_observation() { :; }
    validate_runtime_observation() { :; }
    xcrun() {
        local previous="" argument output=""
        for argument in "$@"; do
            if [[ "$previous" == "-o" ]]; then
                output="$argument"
                break
            fi
            previous="$argument"
        done
        [[ -n "$output" ]] || return 1
        cat > "$output" <<'SH'
#!/bin/bash
set -euo pipefail
: > "${SPACETERM_TEST_LAUNCH_OBSERVATION:?}"
printf '%s\n' "$@" > "${SPACETERM_TEST_LAUNCH_ARGUMENTS:?}"
SH
        chmod 0700 "$output"
    }

    local app="$TEMP_TEST_DIR/lifecycle-app/SpaceTerm.app"
    mkdir -p -- "$app/Contents/MacOS"
    local launch_root="$TEMP_TEST_DIR/no-lifecycle-launch"
    export SPACETERM_TEST_LAUNCH_OBSERVATION="$launch_root/native-observation.tsv"
    export SPACETERM_TEST_LAUNCH_ARGUMENTS="$TEMP_TEST_DIR/no-lifecycle-arguments.txt"
    collect_native_launch_observation \
        "$app" "$(printf 'a%.0s' {1..64})" "i43-no-lifecycle" \
        "$SPACETERM_TEST_LAUNCH_OBSERVATION" "$launch_root" \
        "abcdef" "io.github.sadiksaifi.spaceterm" "" "replay" "none" "none"
    if grep -Fxq -- "--external-lifecycle" "$SPACETERM_TEST_LAUNCH_ARGUMENTS"; then
        fail "native launch emitted external lifecycle arguments for the ordinary path"
    fi

    launch_root="$TEMP_TEST_DIR/external-lifecycle-launch"
    export SPACETERM_TEST_LAUNCH_OBSERVATION="$launch_root/native-observation.tsv"
    export SPACETERM_TEST_LAUNCH_ARGUMENTS="$TEMP_TEST_DIR/external-lifecycle-arguments.txt"
    collect_native_launch_observation \
        "$app" "$(printf 'b%.0s' {1..64})" "i43-external-lifecycle" \
        "$SPACETERM_TEST_LAUNCH_OBSERVATION" "$launch_root" \
        "abcdef" "io.github.sadiksaifi.spaceterm" "" "replay" "none" \
        "$TEMP_TEST_DIR/quit-control.fifo"
    awk '
        $0 == "--external-lifecycle" { count += 1; getline; if ($0 != "true") exit 1 }
        END { exit count != 1 }
    ' "$SPACETERM_TEST_LAUNCH_ARGUMENTS" \
        || fail "native launch did not emit the exact external lifecycle argument pair"
)

test_value_encoding_round_trips
test_bundle_tree_hash_is_stable_and_content_sensitive
test_display_summary_excludes_serial_and_computes_scale
test_manifest_syntax_rejects_duplicate_keys
test_font_matching_requires_an_exact_family
test_mounted_dmg_origin_rejects_a_dist_app_claim
test_mounted_dmg_app_cannot_escape_through_a_symlink
test_apfs_attach_structure_binds_the_canonical_mounted_entity
test_final_readiness_requires_mounted_origin_and_native_geometry
test_native_observation_is_hashed_bound_and_font_consistent
test_failure_action_groups_are_revalidated_offline
test_private_dmg_stage_survives_source_path_replacement
test_collection_rejects_caller_provided_observation
test_failure_control_is_mounted_dmg_only_and_absolute
test_failure_action_driver_is_fixed_and_content_free
test_native_launcher_has_required_authenticated_bindings
test_replay_must_match_recorded_runtime_facts
test_workspace_cleanliness_is_an_initial_invariant
test_public_manifest_redacts_account_paths
test_native_launch_lifecycle_arguments_are_bash_32_nounset_safe
echo "acceptance identity tests passed"
