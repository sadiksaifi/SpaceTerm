#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C

readonly ICON_NAME="SpaceTerm"
readonly BINARY_NAME="spaceterm"
readonly PACKAGER_VERSION="0.11.8"
readonly MINIMUM_XCODE_MAJOR="26"

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIR
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd -P)"
readonly REPO_ROOT
readonly DIST_DIR="$REPO_ROOT/dist"
readonly TERMINFO_SOURCE="$REPO_ROOT/assets/terminfo/xterm-spaceterm.terminfo"
readonly THIRD_PARTY_NOTICES_SOURCE="$REPO_ROOT/assets/THIRD-PARTY-NOTICES.txt"
readonly BUILD_TARGET_DIR="$REPO_ROOT/target"
readonly PACKAGE_STAGE_DIR="$BUILD_TARGET_DIR/package-macos"
readonly STAGED_INFO_PLIST="$PACKAGE_STAGE_DIR/Info.plist"
readonly STAGED_TERMINFO="$PACKAGE_STAGE_DIR/terminfo"

RELEASE_TAG=""
TEMP_ROOT=""

usage() {
    cat <<EOF
Usage: $(basename -- "$0") [--release TAG]

Build and package SpaceTerm Preflight for macOS with cargo-packager.

  --release TAG
               Package an annotated arm64 release tag as SpaceTerm with in-app updates.
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
        || die "Xcode $MINIMUM_XCODE_MAJOR or newer is required to compile $ICON_NAME.icon, got: $xcode_version"
    xcrun --find actool >/dev/null \
        || die "Xcode actool is required to compile $ICON_NAME.icon"
    xcrun --find assetutil >/dev/null \
        || die "Xcode assetutil is required to verify $ICON_NAME.icon"
}

build_native_binary() {
    local output="$1"
    local artifact_path="$PACKAGE_STAGE_DIR/executable-path"
    local binary

    CARGO_TARGET_DIR="$BUILD_TARGET_DIR" python3 "$REPO_ROOT/scripts/build-cargo-executable.py" \
        --output "$artifact_path" --bin "$BINARY_NAME" -- --release --locked --no-default-features \
        --manifest-path "$REPO_ROOT/Cargo.toml"
    binary="$(<"$artifact_path")"
    [[ -x "$binary" ]] || die "release binary was not produced: $binary"
    install -m 0755 "$binary" "$output"
}

prepare_info_plist() {
    local version="$1"

    cp "$INFO_PLIST_SOURCE" "$STAGED_INFO_PLIST"
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
# Only a validated release tag may carry the SpaceTerm identity; see ADR 0012.
if [[ -n "$RELEASE_TAG" ]]; then
    IDENTITY_DIR="$SCRIPT_DIR/spaceterm"
else
    IDENTITY_DIR="$SCRIPT_DIR/preflight"
fi
readonly IDENTITY_DIR
readonly INFO_PLIST_SOURCE="$IDENTITY_DIR/Info.plist"
[[ -f "$INFO_PLIST_SOURCE" ]] || die "missing Info.plist template: $INFO_PLIST_SOURCE"
APP_NAME="$(plutil -extract CFBundleName raw -o - "$INFO_PLIST_SOURCE")"
readonly APP_NAME
EXECUTABLE_NAME="$(plutil -extract CFBundleExecutable raw -o - "$INFO_PLIST_SOURCE")"
readonly EXECUTABLE_NAME
readonly OUTPUT_APP="$DIST_DIR/$APP_NAME.app"
readonly OUTPUT_DMG="$DIST_DIR/$APP_NAME.dmg"
[[ -f "$TERMINFO_SOURCE" ]] || die "missing terminfo source: $TERMINFO_SOURCE"
[[ -f "$THIRD_PARTY_NOTICES_SOURCE" ]] \
    || die "missing third-party notices: $THIRD_PARTY_NOTICES_SOURCE"
[[ "$(uname -m)" == "arm64" ]] || die "SpaceTerm supports Apple Silicon Macs only"
[[ "$(rustc -vV | awk '/^host:/ { print $2 }')" == "aarch64-apple-darwin" ]] \
    || die "an Apple Silicon Rust toolchain is required"

unset SPACETERM_RELEASE_TAG SPACETERM_SPARKLE_DIR
export MACOSX_DEPLOYMENT_TARGET=26.0
# Leaves the SpaceTerm Development identity; see ADR 0012.
export SPACETERM_PACKAGED=1
if [[ -n "$RELEASE_TAG" ]]; then
    VERSION="$(python3 "$REPO_ROOT/packaging/resolve-release-version.py" --tag "$RELEASE_TAG" --require-clean --field version)"
    export SPACETERM_RELEASE_TAG="$RELEASE_TAG"
    SPACETERM_SPARKLE_DIR="$(python3 "$SCRIPT_DIR/prepare-sparkle.py")"
    export SPACETERM_SPARKLE_DIR
else
    VERSION="$(python3 "$REPO_ROOT/packaging/resolve-release-version.py" --field bundle_version)"
fi
readonly VERSION
PACKAGE_VERSION="$VERSION"
readonly PACKAGE_VERSION

mkdir -p -- "$DIST_DIR"
rm -rf -- "$PACKAGE_STAGE_DIR"
mkdir -p -- "$PACKAGE_STAGE_DIR" "$STAGED_TERMINFO"

"$SCRIPT_DIR/compile-icon.sh" "$IDENTITY_DIR" "$PACKAGE_STAGE_DIR"
prepare_info_plist "$VERSION"

echo "Compiling xterm-spaceterm terminfo"
tic -x -o "$STAGED_TERMINFO" "$TERMINFO_SOURCE"

echo "Building Apple Silicon release executable"
readonly BINARIES_DIR="$PACKAGE_STAGE_DIR/binaries"
mkdir -p -- "$BINARIES_DIR"
build_native_binary "$BINARIES_DIR/$EXECUTABLE_NAME"
readonly DMG_ARCH="aarch64"
METADATA_ARGS=(--version "$VERSION" --binaries "$BINARIES_DIR")
if [[ -n "${SPACETERM_SPARKLE_DIR:-}" ]]; then
    METADATA_ARGS+=(--sparkle "$SPACETERM_SPARKLE_DIR")
fi
python3 "$SCRIPT_DIR/generate-package-metadata.py" "${METADATA_ARGS[@]}"

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
"$SCRIPT_DIR/verify-package.sh" "${VERIFY_ARGS[@]}"

rm -rf -- "$OUTPUT_APP"
rm -f -- "$OUTPUT_DMG"
mv "$STAGED_APP" "$OUTPUT_APP"
mv "$STAGED_DMG" "$OUTPUT_DMG"

echo "Created: $OUTPUT_APP"
echo "Created: $OUTPUT_DMG"
