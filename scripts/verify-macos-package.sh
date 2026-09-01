#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C

readonly APP_NAME="SpaceTerm"
readonly BUNDLE_IDENTIFIER="io.github.sadiksaifi.spaceterm"
readonly MINIMUM_MACOS_VERSION="11.0"
readonly ASKPASS_HELPER_MODE="broker-v1"
readonly ASKPASS_HELPER_TIMEOUT_SECONDS=5

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIR
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
readonly REPO_ROOT

APP_PATH="$REPO_ROOT/dist/$APP_NAME.app"
DMG_PATH="$REPO_ROOT/dist/$APP_NAME.dmg"
REQUIRE_UNIVERSAL=0
NATIVE_ARCHITECTURE=""
TEMP_ROOT=""
DMG_MOUNTED=0
MOUNT_POINT=""
EXPECTED_MARKETING_VERSION=""
EXPECTED_BUILD_NUMBER=""
HELPER_PID=""
HELPER_WATCHDOG_PID=""

usage() {
    cat <<EOF
Usage: $(basename -- "$0") [--app PATH] [--dmg PATH] [--universal]

Verify the SpaceTerm application bundle and installer disk image.

  --app PATH   Application bundle to verify (default: dist/SpaceTerm.app).
  --dmg PATH   Disk image to verify (default: dist/SpaceTerm.dmg).
  --universal  Require arm64 and x86_64 slices in both app executables.
  -h, --help   Show this help.
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
    if [[ -n "$HELPER_WATCHDOG_PID" ]]; then
        kill -KILL "$HELPER_WATCHDOG_PID" >/dev/null 2>&1 || true
        wait "$HELPER_WATCHDOG_PID" 2>/dev/null || true
    fi
    if [[ -n "$HELPER_PID" ]]; then
        kill -KILL "$HELPER_PID" >/dev/null 2>&1 || true
        wait "$HELPER_PID" 2>/dev/null || true
    fi
    if (( DMG_MOUNTED )) && [[ -n "$MOUNT_POINT" ]]; then
        hdiutil detach "$MOUNT_POINT" -force >/dev/null 2>&1 || true
    fi
    if [[ -n "$TEMP_ROOT" && -d "$TEMP_ROOT" ]]; then
        rm -rf -- "$TEMP_ROOT"
    fi
    return "$exit_status"
}

plist_value() {
    local plist="$1"
    local key="$2"
    /usr/libexec/PlistBuddy -c "Print :$key" "$plist" 2>/dev/null \
        || die "missing Info.plist key: $key"
}

verify_askpass_helper_mode() {
    local executable="$1"
    local label="$2"
    local stdout_path="$TEMP_ROOT/$label.askpass.stdout"
    local stderr_path="$TEMP_ROOT/$label.askpass.stderr"
    local timeout_path="$TEMP_ROOT/$label.askpass.timeout"
    local helper_exit helper_pid

    : > "$stdout_path"
    : > "$stderr_path"
    /usr/bin/env -i \
        SPACETERM_SSH_ASKPASS_MODE="$ASKPASS_HELPER_MODE" \
        "$executable" "SpaceTerm package verifier prompt" \
        >"$stdout_path" 2>"$stderr_path" &
    HELPER_PID=$!
    helper_pid="$HELPER_PID"

    (
        sleep "$ASKPASS_HELPER_TIMEOUT_SECONDS"
        if kill -0 "$helper_pid" >/dev/null 2>&1; then
            : > "$timeout_path"
            kill -TERM "$helper_pid" >/dev/null 2>&1 || true
            sleep 1
            kill -KILL "$helper_pid" >/dev/null 2>&1 || true
        fi
    ) &
    HELPER_WATCHDOG_PID=$!

    if wait "$helper_pid"; then
        helper_exit=0
    else
        helper_exit=$?
    fi
    HELPER_PID=""
    kill -TERM "$HELPER_WATCHDOG_PID" >/dev/null 2>&1 || true
    wait "$HELPER_WATCHDOG_PID" 2>/dev/null || true
    HELPER_WATCHDOG_PID=""

    [[ ! -e "$timeout_path" ]] \
        || die "$label AskPass helper mode exceeded its ${ASKPASS_HELPER_TIMEOUT_SECONDS}s deadline"
    ! kill -0 "$helper_pid" >/dev/null 2>&1 \
        || die "$label AskPass helper process remained alive after its bounded invocation"
    [[ "$helper_exit" == "2" ]] \
        || die "$label AskPass helper mode must reject missing transport inputs with exit 2, got: $helper_exit"
    [[ ! -s "$stdout_path" ]] \
        || die "$label AskPass helper mode wrote unexpected standard output"
    [[ ! -s "$stderr_path" ]] \
        || die "$label AskPass helper mode wrote unexpected standard error"
}

verify_app_bundle() {
    local app="$1"
    local label="$2"
    local plist="$app/Contents/Info.plist"
    local executable_name icon_file icon_name executable icon_path asset_catalog package_type
    local bundle_identifier
    local bundle_name display_name marketing_version build_number minimum_macos_version
    local extracted_iconset="$TEMP_ROOT/$label.iconset"
    local asset_info="$TEMP_ROOT/$label.assets.json"
    local executable_architectures executable_description signature_details
    local shell_integration="$app/Contents/Resources/shell-integration"
    local terminfo="$app/Contents/Resources/terminfo"
    local terminfo_description

    [[ -d "$app" ]] || die "$label app bundle is missing: $app"
    [[ -f "$plist" ]] || die "$label Info.plist is missing: $plist"
    plutil -lint "$plist" >/dev/null || die "$label Info.plist is invalid: $plist"

    executable_name="$(plist_value "$plist" CFBundleExecutable)"
    icon_file="$(plist_value "$plist" CFBundleIconFile)"
    icon_name="$(plist_value "$plist" CFBundleIconName)"
    package_type="$(plist_value "$plist" CFBundlePackageType)"
    bundle_identifier="$(plist_value "$plist" CFBundleIdentifier)"
    bundle_name="$(plist_value "$plist" CFBundleName)"
    display_name="$(plist_value "$plist" CFBundleDisplayName)"
    marketing_version="$(plist_value "$plist" CFBundleShortVersionString)"
    build_number="$(plist_value "$plist" CFBundleVersion)"
    minimum_macos_version="$(plist_value "$plist" LSMinimumSystemVersion)"
    [[ "$executable_name" == "$APP_NAME" ]] \
        || die "$label CFBundleExecutable must be $APP_NAME, got: $executable_name"
    [[ "$icon_file" == "$APP_NAME" ]] \
        || die "$label CFBundleIconFile must be $APP_NAME, got: $icon_file"
    [[ "$icon_name" == "$APP_NAME" ]] \
        || die "$label CFBundleIconName must be $APP_NAME, got: $icon_name"
    [[ "$package_type" == "APPL" ]] \
        || die "$label CFBundlePackageType must be APPL, got: $package_type"
    [[ "$bundle_identifier" == "$BUNDLE_IDENTIFIER" ]] \
        || die "$label CFBundleIdentifier must be $BUNDLE_IDENTIFIER, got: $bundle_identifier"
    [[ "$bundle_name" == "$APP_NAME" ]] \
        || die "$label CFBundleName must be $APP_NAME, got: $bundle_name"
    [[ "$display_name" == "$APP_NAME" ]] \
        || die "$label CFBundleDisplayName must be $APP_NAME, got: $display_name"
    [[ "$minimum_macos_version" == "$MINIMUM_MACOS_VERSION" ]] \
        || die "$label LSMinimumSystemVersion must be $MINIMUM_MACOS_VERSION, got: $minimum_macos_version"
    [[ "$marketing_version" =~ ^[0-9]+([.][0-9]+){2}$ ]] \
        || die "$label CFBundleShortVersionString is invalid: $marketing_version"
    [[ "$build_number" =~ ^[0-9]+([.][0-9]+){0,2}$ ]] \
        || die "$label CFBundleVersion is invalid: $build_number"
    if [[ -z "$EXPECTED_MARKETING_VERSION" ]]; then
        EXPECTED_MARKETING_VERSION="$marketing_version"
        EXPECTED_BUILD_NUMBER="$build_number"
    else
        [[ "$marketing_version" == "$EXPECTED_MARKETING_VERSION" ]] \
            || die "$label marketing version differs from the packaged app"
        [[ "$build_number" == "$EXPECTED_BUILD_NUMBER" ]] \
            || die "$label build number differs from the packaged app"
    fi

    executable="$app/Contents/MacOS/$executable_name"
    icon_path="$app/Contents/Resources/$APP_NAME.icns"
    asset_catalog="$app/Contents/Resources/Assets.car"
    [[ -x "$executable" ]] || die "$label executable is missing or not executable: $executable"
    executable_description="$(file "$executable")"
    grep -Fq "Mach-O" <<<"$executable_description" \
        || die "$label executable is not a Mach-O binary: $executable"
    executable_architectures="$(lipo -archs "$executable")" \
        || die "$label executable architecture could not be read: $executable"
    if (( REQUIRE_UNIVERSAL )); then
        lipo -verify_arch arm64 x86_64 "$executable" \
            || die "$label executable is not universal arm64 + x86_64"
    else
        [[ "$executable_architectures" == "$NATIVE_ARCHITECTURE" ]] \
            || die "$label executable must contain only $NATIVE_ARCHITECTURE, got: $executable_architectures"
    fi

    [[ -f "$icon_path" ]] || die "$label app icon is missing: $icon_path"
    [[ -f "$asset_catalog" ]] \
        || die "$label layered icon asset catalog is missing: $asset_catalog"
    assetutil --info "$asset_catalog" >"$asset_info" \
        || die "$label layered icon asset catalog is invalid: $asset_catalog"
    grep -Eq '"Name"[[:space:]]*:[[:space:]]*"SpaceTerm"' "$asset_info" \
        || die "$label layered icon asset catalog does not contain SpaceTerm"
    grep -Eq '"AssetType"[[:space:]]*:[[:space:]]*"Icon Image"' "$asset_info" \
        || die "$label layered icon asset catalog contains no icon image"
    grep -Eq '"PixelWidth"[[:space:]]*:[[:space:]]*1024' "$asset_info" \
        || die "$label layered icon asset catalog contains no 1024-pixel representation"
    iconutil --convert iconset --output "$extracted_iconset" "$icon_path" >/dev/null
    [[ -f "$extracted_iconset/icon_16x16.png" ]] \
        || die "$label app icon does not contain a 16x16 representation"
    [[ -f "$extracted_iconset/icon_128x128@2x.png" ]] \
        || die "$label app icon does not contain a 256x256 legacy representation"

    [[ "$(tr -d '[:space:]' < "$shell_integration/VERSION")" == "1" ]] \
        || die "$label shell integration version is missing or unsupported"
    for resource in \
        bash/spaceterm.bash \
        elvish/lib/spaceterm-integration.elv \
        fish/vendor_conf.d/spaceterm-shell-integration.fish \
        nushell/vendor/autoload/spaceterm.nu \
        zsh/.zshenv \
        zsh/spaceterm-integration; do
        [[ -f "$shell_integration/$resource" ]] \
            || die "$label shell integration resource is missing: $resource"
    done

    terminfo_description="$(TERMINFO="$terminfo" infocmp -x -1 xterm-spaceterm)" \
        || die "$label xterm-spaceterm entry is not discoverable"
    grep -Fq "xterm-spaceterm|SpaceTerm truthful xterm-compatible terminal" \
        <<<"$terminfo_description" \
        || die "$label terminfo entry has the wrong identity"
    for capability in "colors#256" "RGB#8" "Tc" "Ms=" "Ss=" "Se="; do
        grep -Fq "$capability" <<<"$terminfo_description" \
            || die "$label terminfo entry is missing verified capability: $capability"
    done
    ! grep -Fiq "ghostty" <<<"$terminfo_description" \
        || die "$label terminfo must not identify as Ghostty"

    codesign --verify --strict --verbose=2 "$app" >/dev/null 2>&1 \
        || die "$label app signature verification failed: $app"
    signature_details="$(codesign --display --verbose=4 "$app" 2>&1)" \
        || die "$label app signature metadata could not be read: $app"
    grep -Fq "Signature=adhoc" <<<"$signature_details" \
        || die "$label app is not ad-hoc signed: $app"
    verify_askpass_helper_mode "$executable" "$label"
}

while (( $# > 0 )); do
    case "$1" in
        --app)
            (( $# >= 2 )) || die "--app requires a path"
            APP_PATH="$2"
            shift
            ;;
        --dmg)
            (( $# >= 2 )) || die "--dmg requires a path"
            DMG_PATH="$2"
            shift
            ;;
        --universal)
            REQUIRE_UNIVERSAL=1
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            die "unknown argument: $1"
            ;;
    esac
    shift
done

[[ "$(uname -s)" == "Darwin" ]] || die "macOS package verification must run on macOS"
case "$(uname -m)" in
    arm64|x86_64) NATIVE_ARCHITECTURE="$(uname -m)" ;;
    *) die "unsupported native macOS architecture: $(uname -m)" ;;
esac
readonly NATIVE_ARCHITECTURE
require_command codesign
require_command assetutil
require_command file
require_command hdiutil
require_command iconutil
require_command infocmp
require_command lipo
require_command plutil
[[ -x /usr/libexec/PlistBuddy ]] || die "required command not found: /usr/libexec/PlistBuddy"
[[ -f "$DMG_PATH" ]] || die "disk image is missing: $DMG_PATH"

TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-verify.XXXXXX")"
readonly TEMP_ROOT
MOUNT_POINT="$TEMP_ROOT/mount"
readonly MOUNT_POINT
mkdir -p -- "$MOUNT_POINT"
trap cleanup EXIT INT TERM

verify_app_bundle "$APP_PATH" "dist"
hdiutil verify "$DMG_PATH" >/dev/null || die "disk image checksum verification failed: $DMG_PATH"

echo "Mounting $DMG_PATH for verification"
hdiutil attach -nobrowse -readonly -mountpoint "$MOUNT_POINT" "$DMG_PATH" >/dev/null
DMG_MOUNTED=1

[[ -L "$MOUNT_POINT/Applications" ]] \
    || die "disk image does not contain an Applications symlink"
[[ "$(readlink "$MOUNT_POINT/Applications")" == "/Applications" ]] \
    || die "disk image Applications symlink does not target /Applications"
verify_app_bundle "$MOUNT_POINT/$APP_NAME.app" "dmg"

hdiutil detach "$MOUNT_POINT" >/dev/null
DMG_MOUNTED=0

echo "Verified: $APP_PATH"
echo "Verified: $DMG_PATH"
