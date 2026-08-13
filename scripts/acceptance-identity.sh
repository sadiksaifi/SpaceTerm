#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C

readonly ACCEPTANCE_IDENTITY_SCHEMA="spaceterm.acceptance.run-identity/v1"
readonly ACCEPTANCE_PUBLIC_IDENTITY_SCHEMA="spaceterm.acceptance.run-identity-public/v1"
readonly NATIVE_OBSERVATION_SCHEMA="spaceterm.acceptance.native-launch-proof/v3"
readonly RUNTIME_OBSERVATION_METADATA_SCHEMA="spaceterm.acceptance.runtime-observation-metadata/v1"
readonly RUNTIME_SAMPLES_HEADER=$'sequence\tcontinuous_ns\tworker_generation\tscreens_published\tscreens_enqueued\tscreens_superseded\tevent_queue_length\tevent_queue_high_water\tui_dispatches\tui_screen_events\tui_drain_high_water\tui_latest_generation\trender_latest_generation\tnext_frame_generation\tnext_frame_count\tpresentable\tminimized\toccluded\tworkspace_visible\tpane_visible\tlive_resize\tviewport_total_rows\tviewport_visible_rows\tviewport_offset_rows\tselection_present\tresize_requests\tresize_notifications\tresize_applied\tresize_coalesced\tpty_rows\tpty_columns\tpty_pixel_width\tpty_pixel_height\tterminal_inputs_accepted\tlifecycle\tobserver_drops'
readonly RUNTIME_EVENTS_HEADER=$'sequence\tcontinuous_ns\tkind\tgeneration\taux0\taux1'
readonly APP_NAME="SpaceTerm"
readonly MOUNTED_DMG_APP_PATH="dmg:/SpaceTerm.app"
readonly PUBLIC_APP_BUNDLE_TOKEN="\$APP_BUNDLE"
readonly PUBLIC_DMG_ARTIFACT_TOKEN="\$DMG_ARTIFACT"
readonly PUBLIC_EXECUTABLE_TOKEN="\$EXECUTABLE"
readonly PUBLIC_LOCAL_PATH_VALUE_TOKEN="\$REDACTED_LOCAL_PATH_VALUE"
readonly PUBLIC_RUN_DIR_TOKEN="\$RUN_DIR"

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIR
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
readonly REPO_ROOT

readonly EXECUTABLE_IDS=$'bash\nzsh\nvim\nneovim\ntmux\nless\nfzf\nbtop\nyazi\nclaude-code\npi-coding-agent'
readonly FONT_CANDIDATES="JetBrainsMono Nerd Font|JetBrainsMono Nerd Font Mono|JetBrains Mono|Menlo"

TEMP_RUN_DIR=""
TEMP_MOUNT_ROOT=""
MOUNT_POINT=""
MOUNT_DEVICE=""
DMG_MOUNTED=0
OBSERVATION_HELPER_PID=""
STAGED_DMG_PATH=""
STAGED_DMG_DEVICE=""
STAGED_DMG_INODE=""
STAGED_DMG_SHA256=""
STAGED_DMG_FD_OPEN=0

usage() {
    cat <<EOF
Usage:
  $(basename -- "$0") collect --run-dir PATH --origin ORIGIN [OPTIONS]
  $(basename -- "$0") verify --run-dir PATH [--final]

Create or verify a versioned acceptance-run identity directory.

Collect options:
  --run-dir PATH           New directory to create. It must not already exist.
  --origin ORIGIN          app-bundle, mounted-dmg, or source-build.
  --app PATH               Packaged SpaceTerm.app (default: dist/SpaceTerm.app).
  --dmg PATH               Packaged SpaceTerm.dmg (default: dist/SpaceTerm.dmg).
  --run-id ID              Stable run label (default: run directory basename).
  --executable ID=PATH     Override discovery for one matrix executable; repeatable.
  -h, --help               Show this help.

The private manifest is canonical tab-separated data. Values percent-encode percent signs,
tabs, carriage returns, and newlines. The clean Workspace root is RUN_DIR/workspace. A
publishable projection with account-specific paths redacted is written beside the private
manifest as public-run-identity.tsv.

For mounted-dmg collection, a native harness helper launches the exact mounted bundle through
LaunchServices with substitution disabled. It authenticates the production app's private Unix
socket peer, live code signature, mounted executable, and content-free runtime observation. The
same mounted process remains open for the acceptance campaign until you quit SpaceTerm. No
caller-provided observation is accepted. Persisted evidence is owner-writable, not a cryptographic
attestation; --final always performs a fresh authenticated replay. Other origins are non-final
identity checks only.
EOF
}

die() {
    echo "error: $*" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

cleanup() {
    local exit_status=$?
    if [[ -n "$OBSERVATION_HELPER_PID" ]] && kill -0 "$OBSERVATION_HELPER_PID" 2>/dev/null; then
        kill -TERM "$OBSERVATION_HELPER_PID" 2>/dev/null || true
        wait "$OBSERVATION_HELPER_PID" 2>/dev/null || true
    fi
    OBSERVATION_HELPER_PID=""
    if (( DMG_MOUNTED )) && [[ -z "$MOUNT_DEVICE" ]]; then
        echo "warning: mounted DMG device identity is unknown; mount preserved: $MOUNT_POINT" >&2
    fi
    if (( DMG_MOUNTED )) && [[ -n "$MOUNT_DEVICE" ]]; then
        if hdiutil detach "$MOUNT_DEVICE" >/dev/null 2>&1; then
            DMG_MOUNTED=0
            MOUNT_POINT=""
            MOUNT_DEVICE=""
        else
            echo "warning: failed to detach DMG during cleanup; evidence preserved: $MOUNT_DEVICE" >&2
        fi
    fi
    if (( STAGED_DMG_FD_OPEN )); then
        exec 9<&-
        STAGED_DMG_FD_OPEN=0
    fi
    if (( ! DMG_MOUNTED )) && [[ -n "$TEMP_MOUNT_ROOT" && -d "$TEMP_MOUNT_ROOT" ]]; then
        rm -rf -- "$TEMP_MOUNT_ROOT"
    fi
    if (( ! DMG_MOUNTED )) && [[ -n "$TEMP_RUN_DIR" && -d "$TEMP_RUN_DIR" ]]; then
        rm -rf -- "$TEMP_RUN_DIR"
    fi
    return "$exit_status"
}

stage_dmg() {
    local source="$1"
    local stage_root="$2"
    local staged="$stage_root/staged-package.dmg"
    [[ ! -e "$staged" && ! -L "$staged" ]] || die "staged DMG path already exists: $staged"
    mkdir -p -- "$stage_root"
    if ! ln -- "$source" "$staged" 2>/dev/null; then
        cp -p -- "$source" "$staged"
    fi
    [[ -f "$staged" && ! -L "$staged" ]] || die "private DMG staging failed"
    exec 9< "$staged"
    STAGED_DMG_FD_OPEN=1
    STAGED_DMG_PATH="$staged"
    STAGED_DMG_DEVICE="$(stat -f '%d' "$staged")"
    STAGED_DMG_INODE="$(stat -f '%i' "$staged")"
    [[ "$(stat -f '%i' /dev/fd/9)" == "$STAGED_DMG_INODE" ]] \
        || die "retained DMG descriptor does not match private staged vnode"
    STAGED_DMG_SHA256="$(sha256_file "$staged")"
    hdiutil verify "$staged" >/dev/null || die "staged DMG checksum verification failed"
}

verify_staged_dmg() {
    [[ -n "$STAGED_DMG_PATH" && -f "$STAGED_DMG_PATH" && ! -L "$STAGED_DMG_PATH" && \
        "$(stat -f '%d' "$STAGED_DMG_PATH")" == "$STAGED_DMG_DEVICE" && \
        "$(stat -f '%i' "$STAGED_DMG_PATH")" == "$STAGED_DMG_INODE" && \
        "$(stat -f '%i' /dev/fd/9)" == "$STAGED_DMG_INODE" ]] \
        || die "private staged DMG vnode changed during acceptance"
    [[ "$(sha256_file "$STAGED_DMG_PATH")" == "$STAGED_DMG_SHA256" ]] \
        || die "private staged DMG content changed during acceptance"
}

release_staged_dmg() {
    verify_staged_dmg
    exec 9<&-
    STAGED_DMG_FD_OPEN=0
    unlink -- "$STAGED_DMG_PATH"
    STAGED_DMG_PATH=""
    STAGED_DMG_DEVICE=""
    STAGED_DMG_INODE=""
    STAGED_DMG_SHA256=""
}

mount_dmg() {
    local dmg="$1"
    local mount_point="$2"
    local attach_plist entity_index entity_mount entity_device
    [[ ! -e "$mount_point" ]] || die "DMG mount point already exists: $mount_point"
    mkdir -p -- "$mount_point"
    attach_plist="$(mktemp "${TMPDIR:-/tmp}/spaceterm-dmg-attach.XXXXXX")"
    hdiutil attach -plist -nobrowse -readonly -mountpoint "$mount_point" "$dmg" \
        > "$attach_plist" \
        || die "failed to mount DMG: $dmg"
    MOUNT_POINT="$mount_point"
    DMG_MOUNTED=1
    entity_index=0
    while (( entity_index < 64 )); do
        entity_mount="$(/usr/libexec/PlistBuddy \
            -c "Print :system-entities:$entity_index:mount-point" "$attach_plist" 2>/dev/null \
            || true)"
        entity_device="$(/usr/libexec/PlistBuddy \
            -c "Print :system-entities:$entity_index:dev-entry" "$attach_plist" 2>/dev/null \
            || true)"
        if [[ "$entity_mount" == "$mount_point" && "$entity_device" == /dev/disk* ]]; then
            MOUNT_DEVICE="$entity_device"
            break
        fi
        [[ -n "$entity_mount$entity_device" ]] || break
        entity_index=$((entity_index + 1))
    done
    rm -f -- "$attach_plist"
    [[ -n "$MOUNT_DEVICE" ]] || die "could not bind DMG mount to its attached device"
}

detach_dmg() {
    if (( DMG_MOUNTED )); then
        [[ -n "$MOUNT_DEVICE" ]] || die "mounted DMG has no bound device identity"
        hdiutil detach "$MOUNT_DEVICE" >/dev/null \
            || die "failed to detach DMG device: $MOUNT_DEVICE"
        DMG_MOUNTED=0
        MOUNT_POINT=""
        MOUNT_DEVICE=""
    fi
}

encode_value() {
    local value="$1"
    value="${value//%/%25}"
    value="${value//$'\t'/%09}"
    value="${value//$'\r'/%0D}"
    value="${value//$'\n'/%0A}"
    printf '%s' "$value"
}

decode_value() {
    local value="$1"
    value="${value//%09/$'\t'}"
    value="${value//%0D/$'\r'}"
    value="${value//%0A/$'\n'}"
    value="${value//%25/%}"
    printf '%s' "$value"
}

write_record() {
    local manifest="$1"
    local key="$2"
    local value="$3"
    [[ "$key" =~ ^[a-z][a-z0-9_.-]*$ ]] || die "invalid manifest key: $key"
    printf '%s\t%s\n' "$key" "$(encode_value "$value")" >> "$manifest"
}

manifest_encoded_value() {
    local manifest="$1"
    local key="$2"
    awk -F '\t' -v wanted="$key" '
        $1 == wanted { print $2; found += 1 }
        END { if (found != 1) exit 1 }
    ' "$manifest"
}

manifest_value() {
    local encoded
    encoded="$(manifest_encoded_value "$1" "$2")" \
        || die "manifest key is missing or duplicated: $2"
    decode_value "$encoded"
}

sha256_file() {
    shasum -a 256 "$1" | awk '{ print $1 }'
}

absolute_existing_path() {
    local path="$1"
    local directory base
    directory="$(cd -- "$(dirname -- "$path")" && pwd -P)" \
        || die "path parent does not exist: $path"
    base="$(basename -- "$path")"
    printf '%s/%s' "$directory" "$base"
}

absolute_new_path() {
    local path="$1"
    local parent base
    parent="$(dirname -- "$path")"
    base="$(basename -- "$path")"
    [[ -n "$base" && "$base" != "." && "$base" != ".." ]] \
        || die "run directory must name a new child directory"
    mkdir -p -- "$parent"
    parent="$(cd -- "$parent" && pwd -P)"
    printf '%s/%s' "$parent" "$base"
}

# The application bundle is a directory, so its acceptance hash is a canonical tree hash. The
# stream includes every relative path, entry kind, permission mode, file digest, and symlink target.
bundle_tree_sha256() {
    local root="$1"
    [[ -d "$root" ]] || die "application bundle is missing: $root"
    (
        cd -- "$root"
        find . -mindepth 1 -print | sort | while IFS= read -r entry; do
            local mode
            mode="$(stat -f '%Lp' "$entry")"
            if [[ -L "$entry" ]]; then
                printf 'symlink\t%s\t%s\t%s\n' \
                    "$(encode_value "$entry")" \
                    "$mode" \
                    "$(encode_value "$(readlink "$entry")")"
            elif [[ -d "$entry" ]]; then
                printf 'directory\t%s\t%s\n' "$(encode_value "$entry")" "$mode"
            elif [[ -f "$entry" ]]; then
                printf 'file\t%s\t%s\t%s\n' \
                    "$(encode_value "$entry")" \
                    "$mode" \
                    "$(sha256_file "$entry")"
            else
                die "unsupported application bundle entry: $root/${entry#./}"
            fi
        done
    ) | shasum -a 256 | awk '{ print $1 }'
}

plist_value() {
    local plist="$1"
    local key="$2"
    /usr/libexec/PlistBuddy -c "Print :$key" "$plist" 2>/dev/null \
        || die "missing Info.plist key: $key"
}

signature_value() {
    local details="$1"
    local key="$2"
    local value
    value="$(printf '%s\n' "$details" | awk -F= -v wanted="$key" '$1 == wanted { print substr($0, index($0, "=") + 1); exit }')"
    printf '%s' "${value:-not-set}"
}

program_id_known() {
    local wanted="$1"
    local id
    for id in $EXECUTABLE_IDS; do
        [[ "$id" == "$wanted" ]] && return 0
    done
    return 1
}

default_program_command() {
    case "$1" in
        bash) printf 'bash' ;;
        zsh) printf 'zsh' ;;
        vim) printf 'vim' ;;
        neovim) printf 'nvim' ;;
        tmux) printf 'tmux' ;;
        less) printf 'less' ;;
        fzf) printf 'fzf' ;;
        btop) printf 'btop' ;;
        yazi) printf 'yazi' ;;
        claude-code) printf 'claude' ;;
        pi-coding-agent) printf 'pi' ;;
        *) return 1 ;;
    esac
}

program_override() {
    local wanted="$1"
    shift
    local override id
    for override in "$@"; do
        id="${override%%=*}"
        if [[ "$id" == "$wanted" ]]; then
            printf '%s' "${override#*=}"
            return 0
        fi
    done
    return 1
}

program_version() {
    local id="$1"
    local path="$2"
    case "$id" in
        tmux) "$path" -V </dev/null ;;
        *) "$path" --version </dev/null ;;
    esac
}

collect_program() {
    local manifest="$1"
    local id="$2"
    shift 2
    local command_name path version status digest override
    command_name="$(default_program_command "$id")"
    override="$(program_override "$id" "$@")" || override=""
    if [[ -n "$override" ]]; then
        [[ -x "$override" && ! -d "$override" ]] \
            || die "executable override is not an executable file: $id=$override"
        path="$(absolute_existing_path "$override")"
    else
        path="$(command -v "$command_name" 2>/dev/null || true)"
        if [[ -n "$path" && -e "$path" ]]; then
            path="$(absolute_existing_path "$path")"
        fi
    fi

    if [[ -z "$path" ]]; then
        status="missing"
        version=""
        digest=""
    else
        if version="$(program_version "$id" "$path" 2>&1)"; then
            status="available"
        else
            status="version-failed"
        fi
        version="$(printf '%s\n' "$version" | awk 'NF { print; exit }')"
        digest="$(sha256_file "$path")"
    fi

    write_record "$manifest" "executable.$id.status" "$status"
    write_record "$manifest" "executable.$id.path" "$path"
    write_record "$manifest" "executable.$id.sha256" "$digest"
    write_record "$manifest" "executable.$id.version" "$version"
}

font_available() {
    local font_report="$1"
    local family="$2"
    awk -F ': ' -v wanted="$family" '
        $1 ~ /^[[:space:]]*Family$/ && $2 == wanted { found = 1 }
        END { exit !found }
    ' "$font_report"
}

font_availability_key() {
    local family="$1"
    local candidate
    local old_ifs="$IFS"
    IFS='|'
    for candidate in $FONT_CANDIDATES; do
        if [[ "$candidate" == "$family" ]]; then
            IFS="$old_ifs"
            printf 'font.%s.available' \
                "$(printf '%s' "$candidate" | tr '[:upper:] ' '[:lower:]-')"
            return 0
        fi
    done
    IFS="$old_ifs"
    return 1
}

positive_number() {
    local value="$1"
    [[ "$value" =~ ^[0-9]+([.][0-9]+)?$ ]] || return 1
    awk -v value="$value" 'BEGIN { exit !(value > 0) }'
}

validate_native_observation() {
    local observation="$1"
    local app_sha256="$2"
    local key
    validate_manifest_syntax "$observation"
    for key in \
        schema observation.source launch.nonce run.id package.app.sha256 \
        runtime.schema runtime.sample_interval_ms runtime.transition_capacity \
        process.pid process.pidversion process.executable.path \
        process.executable.device process.executable.inode process.executable.fsid \
        process.signature.cdhash process.signature.identifier \
        process.signature.team_identifier terminal_font_selected \
        initial_grid.rows initial_grid.columns \
        initial_grid.logical_width initial_grid.logical_height \
        initial_grid.backing_pixel_width initial_grid.backing_pixel_height \
        observation.complete; do
        require_manifest_key "$observation" "$key"
    done
    [[ "$(awk 'END { print NR }' "$observation")" == "25" ]] \
        || die "native observation contains unexpected records"
    [[ "$(manifest_value "$observation" schema)" == "$NATIVE_OBSERVATION_SCHEMA" ]] \
        || die "unsupported native observation schema"
    [[ "$(manifest_value "$observation" observation.source)" == "production-app" ]] \
        || die "native observation was not emitted by the production app"
    [[ "$(manifest_value "$observation" launch.nonce)" =~ ^[0-9a-f]{64}$ ]] \
        || die "native observation nonce is invalid"
    [[ "$(manifest_value "$observation" run.id)" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,79}$ ]] \
        || die "native observation run ID is invalid"
    [[ "$(manifest_value "$observation" package.app.sha256)" == "$app_sha256" ]] \
        || die "native observation does not describe the exact application bundle"
    [[ "$(manifest_value "$observation" runtime.schema)" == \
        "spaceterm.acceptance.runtime-stream/v1" && \
        "$(manifest_value "$observation" runtime.sample_interval_ms)" == "1000" && \
        "$(manifest_value "$observation" runtime.transition_capacity)" == "64" ]] \
        || die "native observation runtime stream contract is invalid"
    for key in process.pid process.pidversion process.executable.device process.executable.inode; do
        [[ "$(manifest_value "$observation" "$key")" =~ ^[1-9][0-9]*$ ]] \
            || die "native observation $key must be a positive integer"
    done
    [[ "$(manifest_value "$observation" process.executable.path)" == /* ]] \
        || die "native observation executable path is not absolute"
    [[ "$(manifest_value "$observation" process.executable.fsid)" =~ ^-?[0-9]+:-?[0-9]+$ ]] \
        || die "native observation executable filesystem identity is invalid"
    [[ "$(manifest_value "$observation" process.signature.cdhash)" =~ ^[0-9A-Fa-f]+$ ]] \
        || die "native observation live CDHash is invalid"
    [[ -n "$(manifest_value "$observation" process.signature.identifier)" ]] \
        || die "native observation has no live signing identifier"
    [[ -n "$(manifest_value "$observation" terminal_font_selected)" ]] \
        || die "native observation has no selected font"
    for key in \
        initial_grid.rows initial_grid.columns \
        initial_grid.backing_pixel_width initial_grid.backing_pixel_height; do
        [[ "$(manifest_value "$observation" "$key")" =~ ^[1-9][0-9]*$ ]] \
            || die "native observation $key must be a positive integer"
    done
    for key in initial_grid.logical_width initial_grid.logical_height; do
        positive_number "$(manifest_value "$observation" "$key")" \
            || die "native observation $key must be a positive number"
    done
    [[ "$(manifest_value "$observation" observation.complete)" == "true" ]] \
        || die "native observation is incomplete"
}

validate_runtime_observation() {
    local native_observation="$1"
    local parent metadata samples events key
    parent="$(dirname -- "$native_observation")"
    metadata="$parent/runtime-metadata.tsv"
    samples="$parent/runtime-samples.tsv"
    events="$parent/runtime-events.tsv"
    for key in "$metadata" "$samples" "$events"; do
        [[ -f "$key" && ! -L "$key" ]] \
            || die "runtime observation artifact is missing or symlinked: $key"
    done
    validate_manifest_syntax "$metadata"
    for key in \
        schema observation.source run.id package.app.sha256 process.pid \
        runtime.samples.path runtime.samples.sha256 runtime.events.path runtime.events.sha256 \
        observer.started_continuous_ns observer.ended_continuous_ns \
        observer.sample_interval_ms observer.transition_capacity observer.sample_count \
        observer.event_count observer.status observation.complete; do
        require_manifest_key "$metadata" "$key"
    done
    [[ "$(awk 'END { print NR }' "$metadata")" == "17" ]] \
        || die "runtime observation metadata contains unexpected records"
    [[ "$(manifest_value "$metadata" schema)" == "$RUNTIME_OBSERVATION_METADATA_SCHEMA" && \
        "$(manifest_value "$metadata" observation.source)" == "production-app" && \
        "$(manifest_value "$metadata" run.id)" == \
            "$(manifest_value "$native_observation" run.id)" && \
        "$(manifest_value "$metadata" package.app.sha256)" == \
            "$(manifest_value "$native_observation" package.app.sha256)" && \
        "$(manifest_value "$metadata" process.pid)" == \
            "$(manifest_value "$native_observation" process.pid)" ]] \
        || die "runtime observation metadata is not bound to the native observation"
    [[ "$(manifest_value "$metadata" runtime.samples.path)" == "runtime-samples.tsv" && \
        "$(manifest_value "$metadata" runtime.events.path)" == "runtime-events.tsv" && \
        "$(manifest_value "$metadata" runtime.samples.sha256)" =~ ^[0-9a-f]{64}$ && \
        "$(manifest_value "$metadata" runtime.events.sha256)" =~ ^[0-9a-f]{64}$ && \
        "$(manifest_value "$metadata" runtime.samples.sha256)" == "$(sha256_file "$samples")" && \
        "$(manifest_value "$metadata" runtime.events.sha256)" == "$(sha256_file "$events")" ]] \
        || die "runtime observation artifact binding is invalid"
    [[ "$(manifest_value "$metadata" observer.started_continuous_ns)" =~ ^[1-9][0-9]*$ && \
        "$(manifest_value "$metadata" observer.ended_continuous_ns)" =~ ^[1-9][0-9]*$ && \
        "$(manifest_value "$metadata" observer.sample_count)" =~ ^[1-9][0-9]*$ && \
        "$(manifest_value "$metadata" observer.event_count)" =~ ^(0|[1-9][0-9]*)$ && \
        "$(manifest_value "$metadata" observer.sample_count)" -le 43201 && \
        "$(manifest_value "$metadata" observer.event_count)" -le 65536 ]] \
        || die "runtime observation metadata counters are invalid"
    [[ "$(manifest_value "$metadata" observer.sample_interval_ms)" == "1000" && \
        "$(manifest_value "$metadata" observer.transition_capacity)" == "64" && \
        "$(manifest_value "$metadata" observer.status)" == "complete" && \
        "$(manifest_value "$metadata" observation.complete)" == "true" ]] \
        || die "runtime observation is NOT-RUN"
    [[ "$(head -n 1 "$samples")" == "$RUNTIME_SAMPLES_HEADER" && \
        "$(head -n 1 "$events")" == "$RUNTIME_EVENTS_HEADER" ]] \
        || die "runtime observation header is invalid"
    [[ "$(wc -c < "$samples" | tr -d '[:space:]')" -le 33554432 && \
        "$(wc -c < "$events" | tr -d '[:space:]')" -le 16777216 ]] \
        || die "runtime observation artifact exceeds its bound"
    [[ "$(( $(awk 'END { print NR }' "$samples") - 1 ))" == \
        "$(manifest_value "$metadata" observer.sample_count)" && \
        "$(( $(awk 'END { print NR }' "$events") - 1 ))" == \
        "$(manifest_value "$metadata" observer.event_count)" ]] \
        || die "runtime observation row count is invalid"
}

collect_native_launch_observation() {
    local app="$1"
    local app_sha256="$2"
    local run_id="$3"
    local observation="$4"
    local launch_root="$5"
    local cdhash="$6"
    local identifier="$7"
    local team_identifier="$8"
    local mode="$9"
    local helper="$launch_root/identity/acceptance-launch-verifier"

    [[ ! -e "$observation" && ! -L "$observation" ]] \
        || die "native observation output must not already exist: $observation"
    mkdir -p -- "$launch_root/identity" "$launch_root/logs" "$launch_root/workspace"
    chmod 0700 "$launch_root/identity"
    xcrun clang -fobjc-arc -fblocks -Wall -Wextra -Werror \
        -mmacosx-version-min=11.0 -framework AppKit -framework Foundation \
        -framework Security -framework CoreFoundation -lbsm \
        "$SCRIPT_DIR/acceptance-launch-verifier.m" -o "$helper"
    chmod 0700 "$helper"
    if [[ "$mode" == "campaign" ]]; then
        echo "SpaceTerm will remain open from the exact read-only DMG mount." >&2
        echo "Run the acceptance campaign in that window, then quit SpaceTerm to finish collection." >&2
    fi
    "$helper" \
        --app "$app" \
        --executable "$app/Contents/MacOS/$APP_NAME" \
        --run-id "$run_id" \
        --app-sha256 "$app_sha256" \
        --cdhash "$cdhash" \
        --identifier "$identifier" \
        --team-identifier "$team_identifier" \
        --home "$launch_root/workspace" \
        --output "$observation" \
        --mode "$mode" \
        >"$launch_root/logs/native-launch.stdout" \
        2>"$launch_root/logs/native-launch.stderr" &
    OBSERVATION_HELPER_PID=$!
    if ! wait "$OBSERVATION_HELPER_PID"; then
        OBSERVATION_HELPER_PID=""
        die "authenticated LaunchServices observation failed; see $launch_root/logs/native-launch.stderr"
    fi
    OBSERVATION_HELPER_PID=""
    rm -f -- "$helper"
    validate_native_observation "$observation" "$app_sha256"
    validate_runtime_observation "$observation"
}

compare_runtime_observations() {
    local recorded="$1"
    local replayed="$2"
    local key
    for key in \
        run.id package.app.sha256 runtime.schema runtime.sample_interval_ms \
        runtime.transition_capacity process.signature.cdhash process.signature.identifier \
        process.signature.team_identifier terminal_font_selected \
        initial_grid.rows initial_grid.columns \
        initial_grid.logical_width initial_grid.logical_height \
        initial_grid.backing_pixel_width initial_grid.backing_pixel_height; do
        [[ "$(manifest_value "$recorded" "$key")" == "$(manifest_value "$replayed" "$key")" ]] \
            || die "fresh production-app launch changed runtime observation field: $key"
    done
}

collect_fonts() {
    local manifest="$1"
    local observation="$2"
    local font_report="$3"
    local candidate available preferred_available="" selected="" selection_source="unobserved"
    local old_ifs="$IFS"
    IFS='|'
    for candidate in $FONT_CANDIDATES; do
        if font_available "$font_report" "$candidate"; then
            available="true"
            [[ -n "$preferred_available" ]] || preferred_available="$candidate"
        else
            available="false"
        fi
        write_record "$manifest" \
            "font.$(printf '%s' "$candidate" | tr '[:upper:] ' '[:lower:]-').available" \
            "$available"
    done
    IFS="$old_ifs"

    if [[ -n "$observation" ]]; then
        selected="$(manifest_value "$observation" terminal_font_selected)"
        font_available "$font_report" "$selected" \
            || die "production app selected font is not installed on the acceptance host: $selected"
        font_availability_key "$selected" >/dev/null \
            || die "production app selected font is outside the acceptance font matrix: $selected"
        selection_source="production-app-observation"
    fi
    [[ -n "$preferred_available" ]] || preferred_available="Menlo"
    write_record "$manifest" "font.preferred_available.family" "$preferred_available"
    write_record "$manifest" "font.selected.family" "$selected"
    write_record "$manifest" "font.selected.source" "$selection_source"
}

write_native_observation_identity() {
    local manifest="$1"
    local observation="$2"
    local relative_path="identity/native-observation.tsv"
    local key value
    if [[ -z "$observation" ]]; then
        write_record "$manifest" "native.observation.path" ""
        write_record "$manifest" "native.observation.sha256" ""
        write_record "$manifest" "native.observation.source" "unobserved"
        for key in \
            rows columns logical_width logical_height \
            backing_pixel_width backing_pixel_height; do
            write_record "$manifest" "host.initial_grid.$key" ""
        done
        write_record "$manifest" "host.terminal_font_selected" ""
        return
    fi

    write_record "$manifest" "native.observation.path" "$relative_path"
    write_record "$manifest" "native.observation.sha256" "$(sha256_file "$observation")"
    write_record "$manifest" "native.observation.source" \
        "$(manifest_value "$observation" observation.source)"
    for key in \
        rows columns logical_width logical_height \
        backing_pixel_width backing_pixel_height; do
        value="$(manifest_value "$observation" "initial_grid.$key")"
        write_record "$manifest" "host.initial_grid.$key" "$value"
    done
    write_record "$manifest" "host.terminal_font_selected" \
        "$(manifest_value "$observation" terminal_font_selected)"
}

verify_native_observation_identity() {
    local manifest="$1"
    local run_dir="$2"
    local source path observation key
    source="$(manifest_value "$manifest" native.observation.source)"
    path="$(manifest_value "$manifest" native.observation.path)"
    if [[ "$source" == "unobserved" ]]; then
        [[ -z "$path" && -z "$(manifest_value "$manifest" native.observation.sha256)" ]] \
            || die "unobserved native identity unexpectedly references an artifact"
        [[ "$(manifest_value "$manifest" font.selected.source)" == "unobserved" && \
            -z "$(manifest_value "$manifest" font.selected.family)" && \
            -z "$(manifest_value "$manifest" host.terminal_font_selected)" ]] \
            || die "unobserved native identity contains a selected-font claim"
        for key in \
            rows columns logical_width logical_height backing_pixel_width backing_pixel_height; do
            [[ -z "$(manifest_value "$manifest" "host.initial_grid.$key")" ]] \
                || die "unobserved native identity contains an initial-grid claim"
        done
        return
    fi
    [[ "$source" == "production-app" && "$path" == "identity/native-observation.tsv" ]] \
        || die "native observation identity is invalid"
    [[ "$(manifest_value "$manifest" native.observation.sha256)" =~ ^[0-9a-f]{64}$ ]] \
        || die "native observation checksum is invalid"
    [[ "$(manifest_value "$manifest" font.selected.source)" == "production-app-observation" ]] \
        || die "selected-font source disagrees with the native observation"
    observation="$run_dir/$path"
    [[ -f "$observation" && ! -L "$observation" ]] \
        || die "native observation artifact is missing or symlinked"
    [[ "$(manifest_value "$manifest" native.observation.sha256)" == "$(sha256_file "$observation")" ]] \
        || die "native observation artifact changed since identity collection"
    validate_native_observation \
        "$observation" \
        "$(manifest_value "$manifest" package.app.sha256)"
    validate_runtime_observation "$observation"
    [[ "$(manifest_value "$observation" run.id)" == "$(manifest_value "$manifest" run.id)" ]] \
        || die "native observation run ID disagrees with the acceptance manifest"
    [[ "$(manifest_value "$observation" process.signature.cdhash | tr '[:lower:]' '[:upper:]')" == \
        "$(manifest_value "$manifest" package.app.signature.cdhash | tr '[:lower:]' '[:upper:]')" ]] \
        || die "live observation CDHash disagrees with the packaged application"
    [[ "$(manifest_value "$observation" process.signature.identifier)" == \
        "$(manifest_value "$manifest" package.app.signature.identifier)" ]] \
        || die "live observation signing identifier disagrees with the packaged application"
    local observed_team packaged_team
    observed_team="$(manifest_value "$observation" process.signature.team_identifier)"
    packaged_team="$(manifest_value "$manifest" package.app.signature.team_identifier)"
    [[ "$observed_team" == "$packaged_team" || \
        ( -z "$observed_team" && \
            ( "$packaged_team" == "not-set" || "$packaged_team" == "not set" ) ) ]] \
        || die "live observation team identifier disagrees with the packaged application"
    [[ "$(manifest_value "$manifest" font.selected.family)" == \
        "$(manifest_value "$observation" terminal_font_selected)" ]] \
        || die "selected font disagrees with the native observation"
    [[ "$(manifest_value "$manifest" host.terminal_font_selected)" == \
        "$(manifest_value "$observation" terminal_font_selected)" ]] \
        || die "host selected font disagrees with the native observation"
    for key in \
        rows columns logical_width logical_height backing_pixel_width backing_pixel_height; do
        [[ "$(manifest_value "$manifest" "host.initial_grid.$key")" == \
            "$(manifest_value "$observation" "initial_grid.$key")" ]] \
            || die "initial grid $key disagrees with the native observation"
    done
}

validate_origin_claim() {
    local manifest="$1"
    local origin app executable
    origin="$(manifest_value "$manifest" run.origin)"
    app="$(manifest_value "$manifest" package.app.path)"
    executable="$(manifest_value "$manifest" package.app.executable.path)"
    if [[ "$origin" == "mounted-dmg" ]]; then
        [[ "$app" == "$MOUNTED_DMG_APP_PATH" ]] \
            || die "mounted-DMG identity does not reference the app inside its exact DMG"
        [[ "$executable" == "$MOUNTED_DMG_APP_PATH/Contents/MacOS/$APP_NAME" ]] \
            || die "mounted-DMG executable identity is not inside its exact DMG app"
    fi
}

validate_mounted_app() {
    local app="$1"
    local mount_root
    [[ ! -L "$app" ]] \
        || die "mounted DMG $APP_NAME.app must not be a symlink"
    [[ -d "$app" ]] || die "mounted DMG does not contain $APP_NAME.app"
    mount_root="$(cd -- "$MOUNT_POINT" && pwd -P)"
    [[ "$(cd -- "$app/.." && pwd -P)/$(basename -- "$app")" == "$mount_root/$APP_NAME.app" ]] \
        || die "mounted DMG application escaped its dedicated mount point"
}

validate_final_readiness() {
    local manifest="$1"
    local key selected
    [[ "$(manifest_value "$manifest" run.origin)" == "mounted-dmg" ]] \
        || die "final acceptance identity requires the exact mounted-DMG packaged origin"
    [[ "$(manifest_value "$manifest" native.observation.source)" == "production-app" ]] \
        || die "final acceptance identity requires a harness-controlled production-app launch proof"
    [[ "$(manifest_value "$manifest" font.selected.source)" == "production-app-observation" ]] \
        || die "final acceptance identity requires the production app's runtime-selected font"
    [[ -n "$(manifest_value "$manifest" font.selected.family)" ]] \
        || die "final acceptance identity has no selected font"
    selected="$(manifest_value "$manifest" font.selected.family)"
    key="$(font_availability_key "$selected")" \
        || die "final acceptance selected font is outside the acceptance font matrix"
    [[ "$(manifest_value "$manifest" "$key")" == "true" ]] \
        || die "final acceptance selected font was not installed during collection"
    for key in \
        host.initial_grid.rows host.initial_grid.columns \
        host.initial_grid.backing_pixel_width host.initial_grid.backing_pixel_height; do
        [[ "$(manifest_value "$manifest" "$key")" =~ ^[1-9][0-9]*$ ]] \
            || die "final acceptance identity requires observed $key"
    done
    for key in host.initial_grid.logical_width host.initial_grid.logical_height; do
        positive_number "$(manifest_value "$manifest" "$key")" \
            || die "final acceptance identity requires observed $key"
    done
}

validate_workspace_identity() {
    local manifest="$1"
    local run_dir="$2"
    local workspace
    workspace="$(manifest_value "$manifest" run.workspace_root)"
    [[ "$workspace" == "$run_dir/workspace" && -d "$workspace" ]] \
        || die "manifest Workspace root does not match the run directory"
    [[ "$(manifest_value "$manifest" run.workspace.initially_empty)" == "true" ]] \
        || die "acceptance Workspace root was not initially clean"
}

contains_absolute_local_path() {
    local value="$1"
    awk -v value="$value" 'BEGIN {
        if (value ~ /file:\/+[^\/[:space:]]/ ||
            value ~ /(^|[^[:alnum:]_.\/-])\/[^\/[:space:]]/) exit 0
        exit 1
    }'
}

public_path_value() {
    local key="$1"
    local value="$2"
    case "$key" in
        run.workspace_root)
            printf '%s/workspace' "$PUBLIC_RUN_DIR_TOKEN"
            ;;
        package.app.path)
            if [[ "$value" == "$MOUNTED_DMG_APP_PATH" ]]; then
                printf '%s' "$value"
            else
                printf '%s' "$PUBLIC_APP_BUNDLE_TOKEN"
            fi
            ;;
        package.app.executable.path)
            if [[ "$value" == "$MOUNTED_DMG_APP_PATH/"* ]]; then
                printf '%s' "$value"
            else
                printf '%s/Contents/MacOS/%s' \
                    "$PUBLIC_APP_BUNDLE_TOKEN" \
                    "$(basename -- "$value")"
            fi
            ;;
        package.dmg.path)
            printf '%s' "$PUBLIC_DMG_ARTIFACT_TOKEN"
            ;;
        executable.*.path)
            if [[ -n "$value" ]]; then
                printf '%s' "$PUBLIC_EXECUTABLE_TOKEN"
            else
                printf ''
            fi
            ;;
        *)
            if contains_absolute_local_path "$value"; then
                printf '%s' "$PUBLIC_LOCAL_PATH_VALUE_TOKEN"
            else
                printf '%s' "$value"
            fi
            ;;
    esac
}

write_public_manifest() {
    local private_manifest="$1"
    local public_manifest="$2"
    local run_dir="$3"
    local key encoded value
    : > "$public_manifest"
    write_record "$public_manifest" "schema" "$ACCEPTANCE_PUBLIC_IDENTITY_SCHEMA"
    write_record "$public_manifest" "public.private_manifest_sha256" \
        "$(sha256_file "$private_manifest")"
    while IFS=$'\t' read -r key encoded; do
        [[ "$key" == "schema" ]] && continue
        value="$(decode_value "$encoded")"
        write_record "$public_manifest" "$key" "$(public_path_value "$key" "$value" "$run_dir")"
    done < "$private_manifest"
}

verify_public_manifest() {
    local private_manifest="$1"
    local public_manifest="$2"
    local run_dir="$3"
    local expected key encoded value
    validate_manifest_syntax "$public_manifest"
    expected="$(mktemp "${TMPDIR:-/tmp}/spaceterm-public-identity.XXXXXX")"
    write_public_manifest "$private_manifest" "$expected" "$run_dir"
    if ! cmp -s "$expected" "$public_manifest"; then
        rm -f -- "$expected"
        die "public manifest is not the deterministic redacted projection of the private manifest"
    fi
    rm -f -- "$expected"
    while IFS=$'\t' read -r key encoded; do
        value="$(decode_value "$encoded")"
        case "$key" in
            run.workspace_root)
                [[ "$value" == "$PUBLIC_RUN_DIR_TOKEN/workspace" ]] \
                    || die "public Workspace path is not redacted"
                ;;
            package.app.path)
                [[ "$value" == "$PUBLIC_APP_BUNDLE_TOKEN" || "$value" == "$MOUNTED_DMG_APP_PATH" ]] \
                    || die "public application path is not redacted"
                ;;
            package.app.executable.path)
                [[ "$value" == "$PUBLIC_APP_BUNDLE_TOKEN/Contents/MacOS/"* || \
                    "$value" == "$MOUNTED_DMG_APP_PATH/Contents/MacOS/"* ]] \
                    || die "public application executable path is not redacted"
                ;;
            package.dmg.path)
                [[ "$value" == "$PUBLIC_DMG_ARTIFACT_TOKEN" ]] \
                    || die "public DMG path is not redacted"
                ;;
            executable.*.path)
                [[ -z "$value" || "$value" == "$PUBLIC_EXECUTABLE_TOKEN" ]] \
                    || die "public executable path is not redacted: $key"
                ;;
            *)
                if contains_absolute_local_path "$value"; then
                    die "public manifest contains an absolute local path in $key"
                fi
                ;;
        esac
    done < "$public_manifest"
}

display_plist_value() {
    local plist="$1"
    local gpu_index="$2"
    local display_index="$3"
    local key="$4"
    /usr/libexec/PlistBuddy \
        -c "Print :SPDisplaysDataType:$gpu_index:spdisplays_ndrvs:$display_index:$key" \
        "$plist" 2>/dev/null
}

# Emit only acceptance-relevant display facts. In particular, do not retain display serial numbers.
summarize_displays() {
    local plist="$1"
    local output="$2"
    local gpu_index display_index name physical logical main online scale
    local count=0
    printf 'display\tname\tphysical_pixels\tlogical_resolution_refresh\tbacking_scale\tmain\tonline\n' > "$output"
    gpu_index=0
    while (( gpu_index < 32 )); do
        display_index=0
        while (( display_index < 32 )); do
            name="$(display_plist_value "$plist" "$gpu_index" "$display_index" _name || true)"
            if [[ -z "$name" ]]; then
                break
            fi
            physical="$(display_plist_value "$plist" "$gpu_index" "$display_index" _spdisplays_pixels || true)"
            logical="$(display_plist_value "$plist" "$gpu_index" "$display_index" _spdisplays_resolution || true)"
            main="$(display_plist_value "$plist" "$gpu_index" "$display_index" spdisplays_main || true)"
            online="$(display_plist_value "$plist" "$gpu_index" "$display_index" spdisplays_online || true)"
            scale="$(awk -v physical="$physical" -v logical="$logical" '
                BEGIN {
                    split(physical, p, " "); split(logical, l, " ");
                    if ((p[1] + 0) > 0 && (l[1] + 0) > 0) printf "%.4g", p[1] / l[1];
                    else printf "unknown";
                }
            ')"
            printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
                "$count" \
                "$(encode_value "$name")" \
                "$(encode_value "${physical:-unknown}")" \
                "$(encode_value "${logical:-unknown}")" \
                "$scale" \
                "${main:-unknown}" \
                "${online:-unknown}" >> "$output"
            count=$((count + 1))
            display_index=$((display_index + 1))
        done
        gpu_index=$((gpu_index + 1))
    done
    printf '%s' "$count"
}

validate_manifest_syntax() {
    local manifest="$1"
    [[ -f "$manifest" ]] || die "acceptance identity manifest is missing: $manifest"
    awk -F '\t' '
        NF != 2 || $1 !~ /^[a-z][a-z0-9_.-]*$/ { exit 1 }
        {
            value = $2
            gsub(/%[0-9A-F][0-9A-F]/, "", value)
            if (index(value, "%") != 0) exit 1
            if (seen[$1]++) exit 1
        }
    ' "$manifest" || die "manifest is malformed, contains an invalid escape, or has duplicate keys"
}

require_manifest_key() {
    manifest_encoded_value "$1" "$2" >/dev/null \
        || die "manifest key is missing or duplicated: $2"
}

validate_manifest_schema() {
    local manifest="$1"
    local key id
    validate_manifest_syntax "$manifest"
    for key in \
        schema run.id run.collected_at_utc run.origin run.workspace_root \
        run.workspace.initially_empty \
        repository.commit repository.cargo_lock_sha256 repository.clean \
        package.app.path package.app.marketing_version package.app.build_version \
        package.app.executable.path package.app.executable.architectures \
        package.app.executable.sha256 package.app.sha256 package.app.sha256_kind \
        package.app.signature.verified package.app.signature.kind \
        package.app.signature.identifier package.app.signature.team_identifier \
        package.app.signature.cdhash package.dmg.path package.dmg.sha256 \
        package.dmg.verified host.macos.product_version host.macos.build_version \
        host.machine.model host.machine.architecture host.cpu host.memory_bytes \
        host.display.count host.display.summary_path host.display.summary_sha256 \
        native.observation.path native.observation.sha256 native.observation.source \
        host.terminal_font_selected host.initial_grid.rows host.initial_grid.columns \
        host.initial_grid.logical_width host.initial_grid.logical_height \
        host.initial_grid.backing_pixel_width host.initial_grid.backing_pixel_height \
        font.jetbrainsmono-nerd-font.available \
        font.jetbrainsmono-nerd-font-mono.available \
        font.jetbrains-mono.available font.menlo.available \
        font.preferred_available.family \
        font.selected.family font.selected.source; do
        require_manifest_key "$manifest" "$key"
    done
    for id in $EXECUTABLE_IDS; do
        require_manifest_key "$manifest" "executable.$id.status"
        require_manifest_key "$manifest" "executable.$id.path"
        require_manifest_key "$manifest" "executable.$id.sha256"
        require_manifest_key "$manifest" "executable.$id.version"
    done

    [[ "$(manifest_value "$manifest" schema)" == "$ACCEPTANCE_IDENTITY_SCHEMA" ]] \
        || die "unsupported manifest schema"
    [[ "$(manifest_value "$manifest" repository.commit)" =~ ^[0-9a-f]{40}$ ]] \
        || die "manifest repository commit is invalid"
    [[ "$(manifest_value "$manifest" repository.cargo_lock_sha256)" =~ ^[0-9a-f]{64}$ ]] \
        || die "manifest Cargo.lock checksum is invalid"
    [[ "$(manifest_value "$manifest" package.app.sha256)" =~ ^[0-9a-f]{64}$ ]] \
        || die "manifest application hash is invalid"
    [[ "$(manifest_value "$manifest" package.dmg.sha256)" =~ ^[0-9a-f]{64}$ ]] \
        || die "manifest DMG hash is invalid"
    [[ "$(manifest_value "$manifest" package.app.sha256_kind)" == "bundle-tree-v1" ]] \
        || die "manifest application hash kind is invalid"
    [[ "$(manifest_value "$manifest" host.display.summary_path)" == "identity/displays.tsv" ]] \
        || die "manifest display summary path is invalid"
    [[ "$(manifest_value "$manifest" repository.clean)" == "true" ]] \
        || die "acceptance source repository was not clean"
    [[ "$(manifest_value "$manifest" package.app.signature.verified)" == "true" ]] \
        || die "application signature was not verified"
    [[ "$(manifest_value "$manifest" package.dmg.verified)" == "true" ]] \
        || die "DMG checksum was not verified"
    case "$(manifest_value "$manifest" run.origin)" in
        app-bundle|mounted-dmg|source-build) ;;
        *) die "manifest run origin is invalid" ;;
    esac
    case "$(manifest_value "$manifest" native.observation.source)" in
        production-app|unobserved) ;;
        *) die "manifest native-observation source is invalid" ;;
    esac
    case "$(manifest_value "$manifest" font.selected.source)" in
        production-app-observation|unobserved) ;;
        *) die "manifest selected-font source is invalid" ;;
    esac
    validate_origin_claim "$manifest"
}

verify_app_signature() {
    local manifest="$1"
    local app="$2"
    local details
    codesign --verify --strict --verbose=2 "$app" >/dev/null 2>&1 \
        || die "application signature verification failed: $app"
    details="$(codesign --display --verbose=4 "$app" 2>&1)" \
        || die "application signature metadata could not be read: $app"
    [[ "$(manifest_value "$manifest" package.app.signature.kind)" == "$(signature_value "$details" Signature)" ]] \
        || die "application signature kind changed since identity collection"
    [[ "$(manifest_value "$manifest" package.app.signature.identifier)" == "$(signature_value "$details" Identifier)" ]] \
        || die "application signature identifier changed since identity collection"
    [[ "$(manifest_value "$manifest" package.app.signature.team_identifier)" == "$(signature_value "$details" TeamIdentifier)" ]] \
        || die "application signature team identifier changed since identity collection"
    [[ "$(manifest_value "$manifest" package.app.signature.cdhash)" == "$(signature_value "$details" CDHash)" ]] \
        || die "application signature CDHash changed since identity collection"
}

verify_run() {
    local run_dir="$1"
    local require_final="${2:-0}"
    local manifest public_manifest app dmg display_summary origin
    [[ "$(uname -s)" == "Darwin" ]] || die "acceptance identity verification requires macOS"
    local command_name
    for command_name in chmod cmp codesign cp find git hdiutil ln readlink rmdir shasum stat \
        unlink xcrun; do
        require_command "$command_name"
    done
    run_dir="$(absolute_existing_path "$run_dir")"
    manifest="$run_dir/run-identity.tsv"
    public_manifest="$run_dir/public-run-identity.tsv"
    validate_manifest_schema "$manifest"
    validate_workspace_identity "$manifest" "$run_dir"
    verify_public_manifest "$manifest" "$public_manifest" "$run_dir"
    verify_native_observation_identity "$manifest" "$run_dir"

    [[ "$(manifest_value "$manifest" repository.commit)" == "$(git -C "$REPO_ROOT" rev-parse HEAD)" ]] \
        || die "manifest commit does not match the current checkout"
    [[ "$(manifest_value "$manifest" repository.cargo_lock_sha256)" == "$(sha256_file "$REPO_ROOT/Cargo.lock")" ]] \
        || die "Cargo.lock has changed since identity collection"

    dmg="$(manifest_value "$manifest" package.dmg.path)"
    [[ -f "$dmg" ]] || die "disk image is missing: $dmg"
    origin="$(manifest_value "$manifest" run.origin)"
    if [[ "$origin" == "mounted-dmg" ]]; then
        TEMP_MOUNT_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-acceptance-verify.XXXXXX")"
        trap cleanup EXIT
        trap 'exit 130' INT
        trap 'exit 143' TERM
        stage_dmg "$dmg" "$TEMP_MOUNT_ROOT/dmg-stage"
        [[ "$(manifest_value "$manifest" package.dmg.sha256)" == "$STAGED_DMG_SHA256" ]] \
            || die "DMG changed since identity collection"
        mount_dmg "$STAGED_DMG_PATH" "$TEMP_MOUNT_ROOT/mount"
        app="$MOUNT_POINT/$APP_NAME.app"
        validate_mounted_app "$app"
    else
        app="$(manifest_value "$manifest" package.app.path)"
    fi
    [[ "$(manifest_value "$manifest" package.app.sha256)" == "$(bundle_tree_sha256 "$app")" ]] \
        || die "application bundle changed since identity collection"
    verify_app_signature "$manifest" "$app"
    if [[ "$origin" != "mounted-dmg" ]]; then
        [[ "$(manifest_value "$manifest" package.dmg.sha256)" == "$(sha256_file "$dmg")" ]] \
            || die "DMG changed since identity collection"
        hdiutil verify "$dmg" >/dev/null || die "DMG checksum verification failed: $dmg"
    fi
    if (( require_final )); then
        validate_final_readiness "$manifest"
        local replay_root="$TEMP_MOUNT_ROOT/replay"
        local replay_observation="$replay_root/identity/native-observation.tsv"
        collect_native_launch_observation \
            "$app" \
            "$(manifest_value "$manifest" package.app.sha256)" \
            "$(manifest_value "$manifest" run.id)" \
            "$replay_observation" \
            "$replay_root" \
            "$(manifest_value "$manifest" package.app.signature.cdhash)" \
            "$(manifest_value "$manifest" package.app.signature.identifier)" \
            "$(manifest_value "$manifest" package.app.signature.team_identifier)" \
            replay
        compare_runtime_observations \
            "$run_dir/$(manifest_value "$manifest" native.observation.path)" \
            "$replay_observation"
    fi

    display_summary="$run_dir/$(manifest_value "$manifest" host.display.summary_path)"
    [[ "$(manifest_value "$manifest" host.display.summary_sha256)" == "$(sha256_file "$display_summary")" ]] \
        || die "display summary changed since identity collection"
    if (( DMG_MOUNTED )); then
        detach_dmg
    fi
    if (( STAGED_DMG_FD_OPEN )); then
        release_staged_dmg
        rmdir -- "$TEMP_MOUNT_ROOT/dmg-stage"
    fi
    if [[ -n "$TEMP_MOUNT_ROOT" ]]; then
        rm -rf -- "$TEMP_MOUNT_ROOT"
        TEMP_MOUNT_ROOT=""
    fi
    trap - EXIT INT TERM
    echo "Verified acceptance identity: $manifest"
}

collect_run() {
    local run_dir=""
    local origin=""
    local app_path="$REPO_ROOT/dist/$APP_NAME.app"
    local dmg_path="$REPO_ROOT/dist/$APP_NAME.dmg"
    local run_id=""
    local -a overrides=()
    local override_count=0
    while (( $# > 0 )); do
        case "$1" in
            --run-dir)
                (( $# >= 2 )) || die "--run-dir requires a path"
                run_dir="$2"
                shift
                ;;
            --origin)
                (( $# >= 2 )) || die "--origin requires a value"
                origin="$2"
                shift
                ;;
            --app)
                (( $# >= 2 )) || die "--app requires a path"
                app_path="$2"
                shift
                ;;
            --dmg)
                (( $# >= 2 )) || die "--dmg requires a path"
                dmg_path="$2"
                shift
                ;;
            --run-id)
                (( $# >= 2 )) || die "--run-id requires a value"
                run_id="$2"
                shift
                ;;
            --executable)
                (( $# >= 2 )) || die "--executable requires ID=PATH"
                [[ "$2" == *=* ]] || die "--executable requires ID=PATH"
                program_id_known "${2%%=*}" || die "unknown executable matrix ID: ${2%%=*}"
                overrides+=("$2")
                override_count=$((override_count + 1))
                shift
                ;;
            -h|--help)
                usage
                exit 0
                ;;
            *) die "unknown collect argument: $1" ;;
        esac
        shift
    done

    [[ -n "$run_dir" ]] || die "--run-dir is required"
    case "$origin" in
        app-bundle|mounted-dmg|source-build) ;;
        *) die "--origin must be app-bundle, mounted-dmg, or source-build" ;;
    esac
    run_dir="$(absolute_new_path "$run_dir")"
    [[ ! -e "$run_dir" && ! -L "$run_dir" ]] \
        || die "run directory already exists or is a symlink: $run_dir"
    [[ -n "$run_id" ]] || run_id="$(basename -- "$run_dir")"
    [[ "$run_id" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,79}$ ]] \
        || die "run ID must be 1-80 ASCII letters, digits, dots, underscores, or hyphens"

    [[ "$(uname -s)" == "Darwin" ]] || die "acceptance identity collection requires macOS"
    for command_name in chmod cmp codesign cp file find git hdiutil lipo ln plutil readlink rmdir \
        shasum stat sw_vers sysctl system_profiler unlink xcrun; do
        require_command "$command_name"
    done
    [[ -x /usr/libexec/PlistBuddy ]] || die "required command not found: /usr/libexec/PlistBuddy"
    [[ -z "$(git -C "$REPO_ROOT" status --porcelain --untracked-files=normal)" ]] \
        || die "source repository must be clean before identity collection"
    [[ -f "$REPO_ROOT/Cargo.lock" ]] || die "Cargo.lock is missing"

    dmg_path="$(absolute_existing_path "$dmg_path")"
    [[ -f "$dmg_path" ]] || die "disk image is missing: $dmg_path"

    TEMP_RUN_DIR="$(mktemp -d "$(dirname -- "$run_dir")/.acceptance-identity.XXXXXX")"
    chmod 0700 "$TEMP_RUN_DIR"
    trap cleanup EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    mkdir -p -- \
        "$TEMP_RUN_DIR/artifacts" \
        "$TEMP_RUN_DIR/identity" \
        "$TEMP_RUN_DIR/logs" \
        "$TEMP_RUN_DIR/screenshots" \
        "$TEMP_RUN_DIR/traces" \
        "$TEMP_RUN_DIR/workspace"
    stage_dmg "$dmg_path" "$TEMP_RUN_DIR/identity/dmg-stage"

    local recorded_app_path
    if [[ "$origin" == "mounted-dmg" ]]; then
        mount_dmg "$STAGED_DMG_PATH" "$TEMP_RUN_DIR/identity/dmg-mount"
        app_path="$MOUNT_POINT/$APP_NAME.app"
        recorded_app_path="$MOUNTED_DMG_APP_PATH"
        validate_mounted_app "$app_path"
    else
        app_path="$(absolute_existing_path "$app_path")"
        recorded_app_path="$app_path"
        [[ -d "$app_path" ]] || die "application bundle is missing: $app_path"
    fi
    local plist="$app_path/Contents/Info.plist"
    [[ -f "$plist" ]] || die "application Info.plist is missing: $plist"
    local executable_name executable_path
    executable_name="$(plist_value "$plist" CFBundleExecutable)"
    [[ "$executable_name" == "$APP_NAME" ]] \
        || die "packaged executable name is not $APP_NAME: $executable_name"
    executable_path="$app_path/Contents/MacOS/$executable_name"
    [[ -f "$executable_path" && ! -L "$executable_path" && -x "$executable_path" ]] \
        || die "packaged executable is missing, symlinked, or not executable: $executable_path"
    file "$executable_path" | grep -Fq "Mach-O" \
        || die "packaged executable is not Mach-O: $executable_path"

    codesign --verify --strict --verbose=2 "$app_path" >/dev/null 2>&1 \
        || die "application signature verification failed: $app_path"
    local signature_details
    signature_details="$(codesign --display --verbose=4 "$app_path" 2>&1)" \
        || die "application signature metadata could not be read"
    local manifest="$TEMP_RUN_DIR/run-identity.tsv"
    : > "$manifest"
    local app_sha256
    app_sha256="$(bundle_tree_sha256 "$app_path")"
    local collected_observation=""
    if [[ "$origin" == "mounted-dmg" ]]; then
        collected_observation="$TEMP_RUN_DIR/identity/native-observation.tsv"
        collect_native_launch_observation \
            "$app_path" "$app_sha256" "$run_id" "$collected_observation" "$TEMP_RUN_DIR" \
            "$(signature_value "$signature_details" CDHash)" \
            "$(signature_value "$signature_details" Identifier)" \
            "$(signature_value "$signature_details" TeamIdentifier)" \
            campaign
    fi

    local display_json="$TEMP_RUN_DIR/identity/displays.plist"
    local display_summary="$TEMP_RUN_DIR/identity/displays.tsv"
    system_profiler SPDisplaysDataType -json -detailLevel full > "$display_json"
    plutil -convert xml1 "$display_json"
    local display_count
    display_count="$(summarize_displays "$display_json" "$display_summary")"
    rm -f -- "$display_json"

    local font_report="$TEMP_RUN_DIR/fonts-report.txt"
    system_profiler SPFontsDataType -detailLevel mini > "$font_report"

    write_record "$manifest" "schema" "$ACCEPTANCE_IDENTITY_SCHEMA"
    write_record "$manifest" "run.id" "$run_id"
    write_record "$manifest" "run.collected_at_utc" "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    write_record "$manifest" "run.origin" "$origin"
    write_record "$manifest" "run.workspace_root" "$run_dir/workspace"
    write_record "$manifest" "run.workspace.initially_empty" "true"
    write_record "$manifest" "repository.commit" "$(git -C "$REPO_ROOT" rev-parse HEAD)"
    write_record "$manifest" "repository.cargo_lock_sha256" "$(sha256_file "$REPO_ROOT/Cargo.lock")"
    write_record "$manifest" "repository.clean" "true"
    write_record "$manifest" "package.app.path" "$recorded_app_path"
    write_record "$manifest" "package.app.marketing_version" "$(plist_value "$plist" CFBundleShortVersionString)"
    write_record "$manifest" "package.app.build_version" "$(plist_value "$plist" CFBundleVersion)"
    if [[ "$origin" == "mounted-dmg" ]]; then
        write_record "$manifest" "package.app.executable.path" \
            "$MOUNTED_DMG_APP_PATH/Contents/MacOS/$executable_name"
    else
        write_record "$manifest" "package.app.executable.path" "$executable_path"
    fi
    write_record "$manifest" "package.app.executable.architectures" "$(lipo -archs "$executable_path" | awk '{$1=$1; gsub(/ /, ","); print}')"
    write_record "$manifest" "package.app.executable.sha256" "$(sha256_file "$executable_path")"
    write_record "$manifest" "package.app.sha256" "$app_sha256"
    write_record "$manifest" "package.app.sha256_kind" "bundle-tree-v1"
    write_record "$manifest" "package.app.signature.verified" "true"
    write_record "$manifest" "package.app.signature.kind" "$(signature_value "$signature_details" Signature)"
    write_record "$manifest" "package.app.signature.identifier" "$(signature_value "$signature_details" Identifier)"
    write_record "$manifest" "package.app.signature.team_identifier" "$(signature_value "$signature_details" TeamIdentifier)"
    write_record "$manifest" "package.app.signature.cdhash" "$(signature_value "$signature_details" CDHash)"
    write_record "$manifest" "package.dmg.path" "$dmg_path"
    verify_staged_dmg
    write_record "$manifest" "package.dmg.sha256" "$STAGED_DMG_SHA256"
    write_record "$manifest" "package.dmg.verified" "true"
    write_record "$manifest" "host.macos.product_version" "$(sw_vers -productVersion)"
    write_record "$manifest" "host.macos.build_version" "$(sw_vers -buildVersion)"
    write_record "$manifest" "host.machine.model" "$(sysctl -n hw.model)"
    write_record "$manifest" "host.machine.architecture" "$(uname -m)"
    write_record "$manifest" "host.cpu" "$(sysctl -n machdep.cpu.brand_string)"
    write_record "$manifest" "host.memory_bytes" "$(sysctl -n hw.memsize)"
    write_record "$manifest" "host.display.count" "$display_count"
    write_record "$manifest" "host.display.summary_path" "identity/displays.tsv"
    write_record "$manifest" "host.display.summary_sha256" "$(sha256_file "$display_summary")"
    collect_fonts "$manifest" "$collected_observation" "$font_report"
    write_native_observation_identity "$manifest" "$collected_observation"
    rm -f -- "$font_report"

    local id
    for id in $EXECUTABLE_IDS; do
        if (( override_count > 0 )); then
            collect_program "$manifest" "$id" "${overrides[@]}"
        else
            collect_program "$manifest" "$id"
        fi
    done

    validate_manifest_schema "$manifest"
    [[ "$display_count" =~ ^[1-9][0-9]*$ ]] || die "no active display was discovered"
    write_public_manifest "$manifest" "$TEMP_RUN_DIR/public-run-identity.tsv" "$run_dir"
    verify_public_manifest "$manifest" "$TEMP_RUN_DIR/public-run-identity.tsv" "$run_dir"
    if (( DMG_MOUNTED )); then
        local finished_mount_point="$MOUNT_POINT"
        detach_dmg
        rmdir -- "$finished_mount_point"
        MOUNT_POINT=""
    fi
    release_staged_dmg
    rmdir -- "$TEMP_RUN_DIR/identity/dmg-stage"
    mv -- "$TEMP_RUN_DIR" "$run_dir"
    TEMP_RUN_DIR=""
    trap - EXIT INT TERM
    verify_run "$run_dir"
    echo "Created acceptance identity: $run_dir/run-identity.tsv"
}

main() {
    (( $# > 0 )) || {
        usage >&2
        exit 1
    }
    local command_name="$1"
    shift
    case "$command_name" in
        collect) collect_run "$@" ;;
        verify)
            [[ ( $# == 2 || $# == 3 ) && "$1" == "--run-dir" ]] \
                || die "verify requires --run-dir PATH [--final]"
            local require_final=0
            if (( $# == 3 )); then
                [[ "$3" == "--final" ]] || die "unknown verify argument: $3"
                require_final=1
            fi
            verify_run "$2" "$require_final"
            ;;
        -h|--help) usage ;;
        *) die "unknown command: $command_name" ;;
    esac
}

if [[ "${SPACETERM_ACCEPTANCE_IDENTITY_LIBRARY_ONLY:-0}" != "1" ]]; then
    main "$@"
fi
