#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C

readonly APP_NAME="SpaceTerm"
readonly BINARY_NAME="spaceterm"
readonly ARM_TARGET="aarch64-apple-darwin"
readonly INTEL_TARGET="x86_64-apple-darwin"

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIR
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
readonly REPO_ROOT
readonly DIST_DIR="$REPO_ROOT/dist"
readonly OUTPUT_APP="$DIST_DIR/$APP_NAME.app"
readonly OUTPUT_DMG="$DIST_DIR/$APP_NAME.dmg"
readonly INFO_PLIST_SOURCE="$REPO_ROOT/packaging/macos/Info.plist"
readonly ICON_SOURCE="$REPO_ROOT/assets/macos/AppIcon.png"
readonly SHELL_INTEGRATION_SOURCE="$REPO_ROOT/assets/shell-integration"
readonly BUILD_TARGET_DIR="$REPO_ROOT/target"

UNIVERSAL=0
BUILD_NUMBER="1"
TEMP_ROOT=""

usage() {
    cat <<EOF
Usage: $(basename -- "$0") [--universal] [--build-number NUMBER]

Build and package SpaceTerm for macOS.

  --universal  Build a universal arm64 + x86_64 application.
  --build-number NUMBER
               Set CFBundleVersion (default: 1).
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
    if [[ -n "$TEMP_ROOT" && -d "$TEMP_ROOT" ]]; then
        rm -rf -- "$TEMP_ROOT"
    fi
    return "$exit_status"
}

generate_icon() {
    local iconset_dir="$1"
    local output_icon="$2"
    local source_width source_height source_format

    source_width="$(sips -g pixelWidth "$ICON_SOURCE" 2>/dev/null | awk '/pixelWidth:/ { print $2 }')"
    source_height="$(sips -g pixelHeight "$ICON_SOURCE" 2>/dev/null | awk '/pixelHeight:/ { print $2 }')"
    source_format="$(sips -g format "$ICON_SOURCE" 2>/dev/null | awk '/format:/ { print $2 }')"
    [[ "$source_format" == "png" ]] || die "app icon source must be a PNG: $ICON_SOURCE"
    [[ "$source_width" =~ ^[0-9]+$ && "$source_height" =~ ^[0-9]+$ ]] \
        || die "could not read app icon dimensions: $ICON_SOURCE"
    [[ "$source_width" == "$source_height" ]] \
        || die "app icon source must be square, got ${source_width}x${source_height}"
    (( source_width >= 1024 )) \
        || die "app icon source must be at least 1024x1024, got ${source_width}x${source_height}"

    mkdir -p -- "$iconset_dir"
    render_icon_size 16 "$iconset_dir/icon_16x16.png"
    render_icon_size 32 "$iconset_dir/icon_16x16@2x.png"
    render_icon_size 32 "$iconset_dir/icon_32x32.png"
    render_icon_size 64 "$iconset_dir/icon_32x32@2x.png"
    render_icon_size 128 "$iconset_dir/icon_128x128.png"
    render_icon_size 256 "$iconset_dir/icon_128x128@2x.png"
    render_icon_size 256 "$iconset_dir/icon_256x256.png"
    render_icon_size 512 "$iconset_dir/icon_256x256@2x.png"
    render_icon_size 512 "$iconset_dir/icon_512x512.png"
    render_icon_size 1024 "$iconset_dir/icon_512x512@2x.png"
    iconutil --convert icns --output "$output_icon" "$iconset_dir"
}

render_icon_size() {
    local size="$1"
    local output="$2"
    sips --resampleHeightWidth "$size" "$size" "$ICON_SOURCE" --out "$output" >/dev/null
}

build_native_binary() {
    local output="$1"
    CARGO_TARGET_DIR="$BUILD_TARGET_DIR" cargo build --release --locked \
        --manifest-path "$REPO_ROOT/Cargo.toml"
    local binary="$BUILD_TARGET_DIR/release/$BINARY_NAME"
    [[ -x "$binary" ]] || die "release binary was not produced: $binary"
    install -m 0755 "$binary" "$output"
}

build_universal_binary() {
    local output="$1"
    require_command lipo

    CARGO_TARGET_DIR="$BUILD_TARGET_DIR" cargo build --release --locked \
        --manifest-path "$REPO_ROOT/Cargo.toml" --target "$ARM_TARGET"
    CARGO_TARGET_DIR="$BUILD_TARGET_DIR" cargo build --release --locked \
        --manifest-path "$REPO_ROOT/Cargo.toml" --target "$INTEL_TARGET"

    local arm_binary="$BUILD_TARGET_DIR/$ARM_TARGET/release/$BINARY_NAME"
    local intel_binary="$BUILD_TARGET_DIR/$INTEL_TARGET/release/$BINARY_NAME"
    [[ -x "$arm_binary" ]] || die "arm64 release binary was not produced: $arm_binary"
    [[ -x "$intel_binary" ]] || die "x86_64 release binary was not produced: $intel_binary"

    lipo -create "$arm_binary" "$intel_binary" -output "$output"
    chmod 0755 "$output"
    lipo -verify_arch arm64 x86_64 "$output" \
        || die "failed to create a universal arm64 + x86_64 executable"
}

create_dmg() {
    local app_path="$1"
    local output_dmg="$2"
    local dmg_root="$TEMP_ROOT/dmg-root"

    mkdir -p -- "$dmg_root"
    ditto "$app_path" "$dmg_root/$APP_NAME.app"
    ln -s /Applications "$dmg_root/Applications"
    hdiutil create \
        -volname "$APP_NAME" \
        -srcfolder "$dmg_root" \
        -format UDZO \
        -ov \
        "$output_dmg" >/dev/null
}

while (( $# > 0 )); do
    case "$1" in
        --universal)
            UNIVERSAL=1
            ;;
        --build-number)
            (( $# >= 2 )) || die "--build-number requires a value"
            BUILD_NUMBER="$2"
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
require_command codesign
require_command ditto
require_command hdiutil
require_command iconutil
require_command plutil
require_command sips
[[ -f "$INFO_PLIST_SOURCE" ]] || die "missing Info.plist template: $INFO_PLIST_SOURCE"
[[ -f "$ICON_SOURCE" ]] || die "missing app icon source: $ICON_SOURCE"
[[ -f "$SHELL_INTEGRATION_SOURCE/VERSION" ]] \
    || die "missing shell integration resources: $SHELL_INTEGRATION_SOURCE"

PACKAGE_VERSION="$(
    cargo metadata --no-deps --format-version 1 --manifest-path "$REPO_ROOT/Cargo.toml" \
        | plutil -extract packages.0.version raw -o - -- -
)"
readonly PACKAGE_VERSION
VERSION="${PACKAGE_VERSION%%[-+]*}"
readonly VERSION
[[ -n "$VERSION" ]] || die "could not read the package version from Cargo.toml"
[[ "$VERSION" =~ ^[0-9]+([.][0-9]+){2}$ ]] \
    || die "Cargo package version must start with a three-component numeric version, got: $PACKAGE_VERSION"
[[ "$BUILD_NUMBER" =~ ^[0-9]+([.][0-9]+){0,2}$ ]] \
    || die "build number must contain one to three numeric components, got: $BUILD_NUMBER"
readonly BUILD_NUMBER

TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-package.XXXXXX")"
readonly TEMP_ROOT
trap cleanup EXIT INT TERM

readonly STAGED_APP="$TEMP_ROOT/$APP_NAME.app"
readonly STAGED_DMG="$TEMP_ROOT/$APP_NAME.dmg"
readonly CONTENTS_DIR="$STAGED_APP/Contents"
readonly MACOS_DIR="$CONTENTS_DIR/MacOS"
readonly RESOURCES_DIR="$CONTENTS_DIR/Resources"
readonly BUNDLE_BINARY="$MACOS_DIR/$APP_NAME"
readonly BUNDLE_ICON="$RESOURCES_DIR/$APP_NAME.icns"

mkdir -p -- "$MACOS_DIR" "$RESOURCES_DIR" "$DIST_DIR"
cp "$INFO_PLIST_SOURCE" "$CONTENTS_DIR/Info.plist"
plutil -replace CFBundleShortVersionString -string "$VERSION" "$CONTENTS_DIR/Info.plist"
plutil -replace CFBundleVersion -string "$BUILD_NUMBER" "$CONTENTS_DIR/Info.plist"
plutil -lint "$CONTENTS_DIR/Info.plist" >/dev/null

echo "Generating $APP_NAME.icns"
generate_icon "$TEMP_ROOT/$APP_NAME.iconset" "$BUNDLE_ICON"

echo "Installing shell integration resources"
ditto "$SHELL_INTEGRATION_SOURCE" "$RESOURCES_DIR/shell-integration"

if (( UNIVERSAL )); then
    echo "Building universal release executable (arm64 + x86_64)"
    build_universal_binary "$BUNDLE_BINARY"
else
    echo "Building native release executable"
    build_native_binary "$BUNDLE_BINARY"
fi

echo "Ad-hoc signing $APP_NAME.app"
codesign --force --sign - --timestamp=none "$STAGED_APP"

echo "Creating $APP_NAME.dmg"
create_dmg "$STAGED_APP" "$STAGED_DMG"

VERIFY_ARGS=(--app "$STAGED_APP" --dmg "$STAGED_DMG")
if (( UNIVERSAL )); then
    VERIFY_ARGS+=(--universal)
fi
"$SCRIPT_DIR/verify-macos-package.sh" "${VERIFY_ARGS[@]}"

rm -rf -- "$OUTPUT_APP"
rm -f -- "$OUTPUT_DMG"
mv "$STAGED_APP" "$OUTPUT_APP"
mv "$STAGED_DMG" "$OUTPUT_DMG"

echo "Created: $OUTPUT_APP"
echo "Created: $OUTPUT_DMG"
