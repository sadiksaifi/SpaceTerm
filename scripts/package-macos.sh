#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C

readonly APP_NAME="SpaceTerm"
readonly BINARY_NAME="spaceterm"
readonly PACKAGER_VERSION="0.11.8"
readonly MINIMUM_XCODE_MAJOR="26"

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIR
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
readonly REPO_ROOT
readonly DIST_DIR="$REPO_ROOT/dist"
readonly OUTPUT_APP="$DIST_DIR/$APP_NAME.app"
readonly OUTPUT_DMG="$DIST_DIR/$APP_NAME.dmg"
readonly INFO_PLIST_SOURCE="$REPO_ROOT/packaging/macos/Info.plist"
readonly ICON_SOURCE="$REPO_ROOT/assets/macos/$APP_NAME.icon"
readonly TERMINFO_SOURCE="$REPO_ROOT/assets/terminfo/xterm-spaceterm.terminfo"
readonly THIRD_PARTY_NOTICES_SOURCE="$REPO_ROOT/assets/THIRD-PARTY-NOTICES.txt"
readonly BUILD_TARGET_DIR="$REPO_ROOT/target"
readonly PACKAGE_STAGE_DIR="$BUILD_TARGET_DIR/package-macos"
readonly STAGED_INFO_PLIST="$PACKAGE_STAGE_DIR/Info.plist"
readonly ICON_PARTIAL_PLIST="$PACKAGE_STAGE_DIR/IconPartialInfo.plist"
readonly STAGED_ICON="$PACKAGE_STAGE_DIR/$APP_NAME.icns"
readonly STAGED_ASSET_CATALOG="$PACKAGE_STAGE_DIR/Assets.car"
readonly STAGED_TERMINFO="$PACKAGE_STAGE_DIR/terminfo"

RELEASE_TAG=""
TEMP_ROOT=""

usage() {
    cat <<EOF
Usage: $(basename -- "$0") [--release TAG]

Build and package SpaceTerm for macOS with cargo-packager.

  --release TAG
               Package an annotated arm64 release tag with in-app updates.
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
    local exit_status="$1"
    if [[ -n "$TEMP_ROOT" && -d "$TEMP_ROOT" ]]; then
        rm -rf -- "$TEMP_ROOT"
    fi
    return "$exit_status"
}

require_packager() {
    local version
    version="$(cargo packager --version 2>/dev/null)" \
        || die "cargo-packager is unavailable; run: mise install"
    [[ "$version" == "cargo-packager $PACKAGER_VERSION" ]] \
        || die "cargo-packager $PACKAGER_VERSION is required, got: $version"
}

require_xcode() {
    local xcode_version xcode_major
    xcode_version="$(xcodebuild -version | awk 'NR == 1 { print $2 }')"
    xcode_major="${xcode_version%%.*}"
    [[ "$xcode_major" =~ ^[0-9]+$ ]] \
        || die "could not determine the installed Xcode version"
    (( xcode_major >= MINIMUM_XCODE_MAJOR )) \
        || die "Xcode $MINIMUM_XCODE_MAJOR or newer is required to compile $APP_NAME.icon, got: $xcode_version"
    xcrun --find actool >/dev/null \
        || die "Xcode actool is required to compile $APP_NAME.icon"
    xcrun --find assetutil >/dev/null \
        || die "Xcode assetutil is required to verify $APP_NAME.icon"
}

build_native_binary() {
    local output="$1"
    local binary="$BUILD_TARGET_DIR/release/$BINARY_NAME"

    CARGO_TARGET_DIR="$BUILD_TARGET_DIR" cargo build --release --locked \
        --manifest-path "$REPO_ROOT/Cargo.toml"
    [[ -x "$binary" ]] || die "release binary was not produced: $binary"
    if [[ -e "$output" && "$binary" -ef "$output" ]]; then
        chmod 0755 "$binary"
    else
        install -m 0755 "$binary" "$output"
    fi
}

compile_icon() {
    echo "Compiling layered $APP_NAME.icon"
    xcrun actool "$ICON_SOURCE" \
        --compile "$PACKAGE_STAGE_DIR" \
        --platform macosx \
        --minimum-deployment-target 26.0 \
        --app-icon "$APP_NAME" \
        --output-partial-info-plist "$ICON_PARTIAL_PLIST" \
        --enable-on-demand-resources NO \
        --development-region en \
        --target-device mac \
        --bundle-identifier io.github.sadiksaifi.spaceterm >/dev/null
    [[ -f "$STAGED_ICON" ]] || die "actool did not produce: $STAGED_ICON"
    [[ -f "$STAGED_ASSET_CATALOG" ]] || die "actool did not produce: $STAGED_ASSET_CATALOG"
    [[ -f "$ICON_PARTIAL_PLIST" ]] || die "actool did not produce: $ICON_PARTIAL_PLIST"
}

prepare_info_plist() {
    local version="$1"
    local icon_file icon_name

    cp "$INFO_PLIST_SOURCE" "$STAGED_INFO_PLIST"
    icon_file="$(plutil -extract CFBundleIconFile raw -o - "$ICON_PARTIAL_PLIST")"
    icon_name="$(plutil -extract CFBundleIconName raw -o - "$ICON_PARTIAL_PLIST")"
    [[ "$icon_file" == "$APP_NAME" && "$icon_name" == "$APP_NAME" ]] \
        || die "actool emitted unexpected icon metadata: file=$icon_file name=$icon_name"
    plutil -replace CFBundleIconFile -string "$icon_file" "$STAGED_INFO_PLIST"
    plutil -replace CFBundleIconName -string "$icon_name" "$STAGED_INFO_PLIST"
    plutil -insert CFBundleShortVersionString -string "$version" "$STAGED_INFO_PLIST"
    plutil -insert CFBundleVersion -string "$version" "$STAGED_INFO_PLIST"
    plutil -lint "$STAGED_INFO_PLIST" >/dev/null
}

while (( $# > 0 )); do
    case "$1" in
        --release)
            (( $# >= 2 )) || die "--release requires an annotated release tag"
            RELEASE_TAG="$2"
            shift
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

[[ "$(uname -s)" == "Darwin" ]] || die "macOS packaging must run on macOS"
require_command cargo
require_command cargo-packager
require_command codesign
require_command hdiutil
require_command iconutil
require_command lipo
require_command plutil
require_command rustc
require_command tic
require_command xcodebuild
require_command xcrun
require_packager
require_xcode
[[ -f "$INFO_PLIST_SOURCE" ]] || die "missing Info.plist template: $INFO_PLIST_SOURCE"
[[ -f "$ICON_SOURCE/icon.json" ]] || die "missing Icon Composer source: $ICON_SOURCE"
[[ -f "$TERMINFO_SOURCE" ]] || die "missing terminfo source: $TERMINFO_SOURCE"
[[ -f "$THIRD_PARTY_NOTICES_SOURCE" ]] \
    || die "missing third-party notices: $THIRD_PARTY_NOTICES_SOURCE"
[[ "$(uname -m)" == "arm64" ]] || die "SpaceTerm supports Apple Silicon Macs only"
[[ "$(rustc -vV | awk '/^host:/ { print $2 }')" == "aarch64-apple-darwin" ]] \
    || die "an Apple Silicon Rust toolchain is required"

unset SPACETERM_RELEASE_TAG SPACETERM_SPARKLE_DIR
export MACOSX_DEPLOYMENT_TARGET=26.0
if [[ -n "$RELEASE_TAG" ]]; then
    VERSION="$(python3 "$SCRIPT_DIR/release-version.py" --tag "$RELEASE_TAG" --require-clean --field version)"
    export SPACETERM_RELEASE_TAG="$RELEASE_TAG"
    SPACETERM_SPARKLE_DIR="$(python3 "$SCRIPT_DIR/prepare-sparkle-macos.py")"
    export SPACETERM_SPARKLE_DIR
else
    VERSION="$(python3 "$SCRIPT_DIR/release-version.py" --field bundle_version)"
fi
readonly VERSION
PACKAGE_VERSION="$VERSION"
readonly PACKAGE_VERSION

mkdir -p -- "$DIST_DIR"
rm -rf -- "$PACKAGE_STAGE_DIR"
mkdir -p -- "$PACKAGE_STAGE_DIR" "$STAGED_TERMINFO"

compile_icon
prepare_info_plist "$VERSION"

echo "Compiling xterm-spaceterm terminfo"
tic -x -o "$STAGED_TERMINFO" "$TERMINFO_SOURCE"

echo "Building Apple Silicon release executable"
build_native_binary "$BUILD_TARGET_DIR/release/$APP_NAME"
readonly DMG_ARCH="aarch64"
readonly BINARIES_DIR="$BUILD_TARGET_DIR/release"
METADATA_ARGS=(--version "$VERSION" --binaries "$BINARIES_DIR")
if [[ -n "${SPACETERM_SPARKLE_DIR:-}" ]]; then
    METADATA_ARGS+=(--sparkle "$SPACETERM_SPARKLE_DIR")
fi
python3 "$SCRIPT_DIR/package-metadata-macos.py" "${METADATA_ARGS[@]}"

TEMP_ROOT="$(mktemp -d "$DIST_DIR/.package.XXXXXX")"
readonly TEMP_ROOT
trap 'cleanup $?' EXIT INT TERM
readonly PACKAGER_OUTPUT_DIR="$TEMP_ROOT/output"
mkdir -p -- "$PACKAGER_OUTPUT_DIR"

echo "Packaging $APP_NAME.app and $APP_NAME.dmg with cargo-packager $PACKAGER_VERSION"
CI="${CI:-true}" cargo packager --config "$(cat "$PACKAGE_STAGE_DIR/packager.json")" --out-dir "$PACKAGER_OUTPUT_DIR"

readonly STAGED_APP="$PACKAGER_OUTPUT_DIR/$APP_NAME.app"
readonly PACKAGER_DMG="$PACKAGER_OUTPUT_DIR/${APP_NAME}_${PACKAGE_VERSION}_${DMG_ARCH}.dmg"
readonly STAGED_DMG="$TEMP_ROOT/$APP_NAME.dmg"
[[ -d "$STAGED_APP" ]] || die "cargo-packager did not produce: $STAGED_APP"
[[ -f "$PACKAGER_DMG" ]] || die "cargo-packager did not produce: $PACKAGER_DMG"
mv "$PACKAGER_DMG" "$STAGED_DMG"

VERIFY_ARGS=(--app "$STAGED_APP" --dmg "$STAGED_DMG")
if [[ -n "$RELEASE_TAG" ]]; then
    VERIFY_ARGS+=(--release "$RELEASE_TAG")
fi
"$SCRIPT_DIR/verify-macos-package.sh" "${VERIFY_ARGS[@]}"

rm -rf -- "$OUTPUT_APP"
rm -f -- "$OUTPUT_DMG"
mv "$STAGED_APP" "$OUTPUT_APP"
mv "$STAGED_DMG" "$OUTPUT_DMG"

echo "Created: $OUTPUT_APP"
echo "Created: $OUTPUT_DMG"
