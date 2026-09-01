#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C

readonly APP_NAME="SpaceTerm"
readonly BINARY_NAME="spaceterm"
readonly PACKAGER_VERSION="0.11.8"
readonly MINIMUM_XCODE_MAJOR="26"
readonly ARM_TARGET="aarch64-apple-darwin"
readonly INTEL_TARGET="x86_64-apple-darwin"
readonly UNIVERSAL_TARGET="universal-apple-darwin"

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
readonly BUILD_TARGET_DIR="$REPO_ROOT/target"
readonly PACKAGE_STAGE_DIR="$BUILD_TARGET_DIR/package-macos"
readonly STAGED_INFO_PLIST="$PACKAGE_STAGE_DIR/Info.plist"
readonly ICON_PARTIAL_PLIST="$PACKAGE_STAGE_DIR/IconPartialInfo.plist"
readonly STAGED_ICON="$PACKAGE_STAGE_DIR/$APP_NAME.icns"
readonly STAGED_ASSET_CATALOG="$PACKAGE_STAGE_DIR/Assets.car"
readonly STAGED_TERMINFO="$PACKAGE_STAGE_DIR/terminfo"

UNIVERSAL=0
BUILD_NUMBER="1"
TEMP_ROOT=""

usage() {
    cat <<EOF
Usage: $(basename -- "$0") [--universal] [--build-number NUMBER]

Build and package SpaceTerm for macOS with cargo-packager.

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
    local exit_status="$1"
    if [[ -n "$TEMP_ROOT" && -d "$TEMP_ROOT" ]]; then
        rm -rf -- "$TEMP_ROOT"
    fi
    return "$exit_status"
}

require_packager() {
    local version
    version="$(cargo packager --version 2>/dev/null)" \
        || die "cargo-packager is unavailable; run: just install-packager"
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

require_universal_targets() {
    local target
    local installed_targets
    installed_targets="$(rustup target list --installed)"
    for target in "$ARM_TARGET" "$INTEL_TARGET"; do
        grep -Fxq "$target" <<<"$installed_targets" \
            || die "missing Rust target: $target; install both with: rustup target add $ARM_TARGET $INTEL_TARGET"
    done
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

build_universal_binary() {
    local output="$1"
    local arm_binary="$BUILD_TARGET_DIR/$ARM_TARGET/release/$BINARY_NAME"
    local intel_binary="$BUILD_TARGET_DIR/$INTEL_TARGET/release/$BINARY_NAME"

    CARGO_TARGET_DIR="$BUILD_TARGET_DIR" cargo build --release --locked \
        --manifest-path "$REPO_ROOT/Cargo.toml" --target "$ARM_TARGET"
    CARGO_TARGET_DIR="$BUILD_TARGET_DIR" cargo build --release --locked \
        --manifest-path "$REPO_ROOT/Cargo.toml" --target "$INTEL_TARGET"
    [[ -x "$arm_binary" ]] || die "arm64 release binary was not produced: $arm_binary"
    [[ -x "$intel_binary" ]] || die "x86_64 release binary was not produced: $intel_binary"

    mkdir -p -- "$(dirname -- "$output")"
    lipo -create "$arm_binary" "$intel_binary" -output "$output"
    chmod 0755 "$output"
    lipo -verify_arch arm64 x86_64 "$output" \
        || die "failed to create a universal arm64 + x86_64 executable"
}

compile_icon() {
    echo "Compiling layered $APP_NAME.icon"
    xcrun actool "$ICON_SOURCE" \
        --compile "$PACKAGE_STAGE_DIR" \
        --platform macosx \
        --minimum-deployment-target 11.0 \
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
    plutil -replace CFBundleShortVersionString -string "$version" "$STAGED_INFO_PLIST"
    plutil -replace CFBundleVersion -string "$BUILD_NUMBER" "$STAGED_INFO_PLIST"
    plutil -lint "$STAGED_INFO_PLIST" >/dev/null
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
if (( UNIVERSAL )); then
    require_command rustup
    require_universal_targets
fi

PACKAGE_ID="$(cargo pkgid --manifest-path "$REPO_ROOT/Cargo.toml" --package "$BINARY_NAME")"
readonly PACKAGE_ID
PACKAGE_VERSION="${PACKAGE_ID##*@}"
readonly PACKAGE_VERSION
VERSION="${PACKAGE_VERSION%%[-+]*}"
readonly VERSION
[[ -n "$VERSION" && "$VERSION" =~ ^[0-9]+([.][0-9]+){2}$ ]] \
    || die "Cargo package version must start with a three-component numeric version, got: $PACKAGE_VERSION"
[[ "$BUILD_NUMBER" =~ ^[0-9]+([.][0-9]+){0,2}$ ]] \
    || die "build number must contain one to three numeric components, got: $BUILD_NUMBER"
readonly BUILD_NUMBER

mkdir -p -- "$DIST_DIR"
rm -rf -- "$PACKAGE_STAGE_DIR"
mkdir -p -- "$PACKAGE_STAGE_DIR" "$STAGED_TERMINFO"

compile_icon
prepare_info_plist "$VERSION"

echo "Compiling xterm-spaceterm terminfo"
tic -x -o "$STAGED_TERMINFO" "$TERMINFO_SOURCE"

if (( UNIVERSAL )); then
    echo "Building universal release executable (arm64 + x86_64)"
    build_universal_binary "$BUILD_TARGET_DIR/$UNIVERSAL_TARGET/release/$APP_NAME"
    DMG_ARCH="universal"
else
    HOST_TARGET="$(rustc -vV | awk '/^host:/ { print $2 }')"
    case "$HOST_TARGET" in
        aarch64-apple-darwin) DMG_ARCH="aarch64" ;;
        x86_64-apple-darwin) DMG_ARCH="x64" ;;
        *) die "unsupported native macOS Rust host target: $HOST_TARGET" ;;
    esac
    echo "Building native release executable"
    build_native_binary "$BUILD_TARGET_DIR/release/$APP_NAME"
fi
readonly DMG_ARCH

TEMP_ROOT="$(mktemp -d "$DIST_DIR/.package.XXXXXX")"
readonly TEMP_ROOT
trap 'cleanup $?' EXIT INT TERM
readonly PACKAGER_OUTPUT_DIR="$TEMP_ROOT/output"
mkdir -p -- "$PACKAGER_OUTPUT_DIR"

echo "Packaging $APP_NAME.app and $APP_NAME.dmg with cargo-packager $PACKAGER_VERSION"
if (( UNIVERSAL )); then
    cargo packager --release --out-dir "$PACKAGER_OUTPUT_DIR" --target "$UNIVERSAL_TARGET"
else
    cargo packager --release --out-dir "$PACKAGER_OUTPUT_DIR"
fi

readonly STAGED_APP="$PACKAGER_OUTPUT_DIR/$APP_NAME.app"
readonly PACKAGER_DMG="$PACKAGER_OUTPUT_DIR/${APP_NAME}_${PACKAGE_VERSION}_${DMG_ARCH}.dmg"
readonly STAGED_DMG="$TEMP_ROOT/$APP_NAME.dmg"
[[ -d "$STAGED_APP" ]] || die "cargo-packager did not produce: $STAGED_APP"
[[ -f "$PACKAGER_DMG" ]] || die "cargo-packager did not produce: $PACKAGER_DMG"
mv "$PACKAGER_DMG" "$STAGED_DMG"

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
