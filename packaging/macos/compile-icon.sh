#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C

# Every identity compiles under the one internal icon name that the bundle templates expect.
readonly ICON_NAME="SpaceTerm"

usage() {
    cat <<USAGE
Usage: $(basename -- "$0") IDENTITY_DIR OUTPUT_DIR

Compile the Icon Composer document of one application identity into OUTPUT_DIR as
$ICON_NAME.icns, Assets.car, and IconPartialInfo.plist.
USAGE
}

die() {
    echo "error: $*" >&2
    exit 1
}

(( $# == 2 )) || {
    usage >&2
    exit 2
}
readonly IDENTITY_DIR="$1"
readonly OUTPUT_DIR="$2"
readonly INFO_PLIST="$IDENTITY_DIR/Info.plist"
[[ -f "$INFO_PLIST" ]] || die "missing Info.plist template: $INFO_PLIST"
APP_NAME="$(plutil -extract CFBundleName raw -o - "$INFO_PLIST")"
readonly APP_NAME
BUNDLE_IDENTIFIER="$(plutil -extract CFBundleIdentifier raw -o - "$INFO_PLIST")"
readonly BUNDLE_IDENTIFIER
readonly ICON_SOURCE="$IDENTITY_DIR/$APP_NAME.icon"
[[ -f "$ICON_SOURCE/icon.json" ]] || die "missing Icon Composer source: $ICON_SOURCE"

mkdir -p -- "$OUTPUT_DIR"
STAGING_ROOT="$(mktemp -d "$OUTPUT_DIR/.icon.XXXXXX")"
readonly STAGING_ROOT
trap 'rm -rf -- "$STAGING_ROOT"' EXIT
cp -R -- "$ICON_SOURCE" "$STAGING_ROOT/$ICON_NAME.icon"

echo "Compiling layered $ICON_NAME.icon for $APP_NAME"
xcrun actool "$STAGING_ROOT/$ICON_NAME.icon" \
    --compile "$OUTPUT_DIR" \
    --platform macosx \
    --minimum-deployment-target 26.0 \
    --app-icon "$ICON_NAME" \
    --output-partial-info-plist "$OUTPUT_DIR/IconPartialInfo.plist" \
    --enable-on-demand-resources NO \
    --development-region en \
    --target-device mac \
    --bundle-identifier "$BUNDLE_IDENTIFIER" >/dev/null
for output in "$ICON_NAME.icns" Assets.car IconPartialInfo.plist; do
    [[ -f "$OUTPUT_DIR/$output" ]] || die "actool did not produce: $OUTPUT_DIR/$output"
done
for key in CFBundleIconFile CFBundleIconName; do
    value="$(plutil -extract "$key" raw -o - "$OUTPUT_DIR/IconPartialInfo.plist")"
    [[ "$value" == "$ICON_NAME" ]] || die "actool emitted unexpected $key: $value"
done
