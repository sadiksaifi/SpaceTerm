#!/bin/bash

set -euo pipefail
IFS=$'\n\t'

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIR
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd -P)"
readonly REPO_ROOT

LAUNCH=true
case "${1:-}" in
    "") ;;
    --no-launch) LAUNCH=false ;;
    *)
        echo "usage: $(basename -- "$0") [--no-launch]" >&2
        exit 2
        ;;
esac
readonly LAUNCH
readonly IDENTITY_DIR="$SCRIPT_DIR/development"
readonly INFO_PLIST_SOURCE="$IDENTITY_DIR/Info.plist"
readonly CARGO_ARGUMENTS=(--manifest-path "$REPO_ROOT/Cargo.toml" --locked)

plist_value() {
    local key="$1"
    plutil -extract "$key" raw -o - "$INFO_PLIST_SOURCE"
}

APP_NAME="$(plist_value CFBundleName)"
readonly APP_NAME
EXECUTABLE_NAME="$(plist_value CFBundleExecutable)"
readonly EXECUTABLE_NAME
[[ "$APP_NAME" == "$EXECUTABLE_NAME" ]] || {
    echo "error: development bundle name and executable name must match" >&2
    exit 2
}

ARTIFACT_PATH="$(mktemp "${TMPDIR:-/tmp}/spaceterm-development-executable.XXXXXX")"
STAGING_ROOT=""
cleanup() {
    local exit_status=$?
    rm -f -- "$ARTIFACT_PATH"
    if [[ -n "$STAGING_ROOT" && -d "$STAGING_ROOT" ]]; then
        rm -rf -- "$STAGING_ROOT"
    fi
    return "$exit_status"
}
trap cleanup EXIT HUP INT TERM

"$REPO_ROOT/scripts/cargo-artifacts.sh" run -- \
    python3 "$REPO_ROOT/scripts/build-cargo-executable.py" \
    --output "$ARTIFACT_PATH" --bin spaceterm -- \
    "${CARGO_ARGUMENTS[@]}"
EXECUTABLE="$(<"$ARTIFACT_PATH")"
rm -f -- "$ARTIFACT_PATH"

BUNDLE_PARENT="$(dirname -- "$(dirname -- "$EXECUTABLE")")/development-apps"
readonly BUNDLE_PARENT
mkdir -p -- "$BUNDLE_PARENT"

# actool takes seconds, so each version of the icon document compiles once.
ICON_DIGEST="$(find "$IDENTITY_DIR/$APP_NAME.icon" -type f -print0 | LC_ALL=C sort -z \
    | xargs -0 shasum -a 256 | shasum -a 256 | cut -c1-16)"
readonly ICON_CACHE="$BUNDLE_PARENT/icons/$ICON_DIGEST"
if [[ ! -d "$ICON_CACHE" ]]; then
    rm -rf -- "$ICON_CACHE.partial"
    "$SCRIPT_DIR/compile-icon.sh" "$IDENTITY_DIR" "$ICON_CACHE.partial"
    mv -- "$ICON_CACHE.partial" "$ICON_CACHE"
fi

STAGING_ROOT="$(mktemp -d "$BUNDLE_PARENT/.bundle.XXXXXX")"
STAGED_BUNDLE="$STAGING_ROOT/$APP_NAME.app"
readonly STAGED_BUNDLE
mkdir -p -- "$STAGED_BUNDLE/Contents/MacOS" "$STAGED_BUNDLE/Contents/Resources"
install -m 0644 "$ICON_CACHE/SpaceTerm.icns" "$ICON_CACHE/Assets.car" "$STAGED_BUNDLE/Contents/Resources/"
install -m 0644 "$INFO_PLIST_SOURCE" "$STAGED_BUNDLE/Contents/Info.plist"
BUNDLE_VERSION="$(python3 "$REPO_ROOT/packaging/resolve-release-version.py" --field bundle_version)"
plutil -insert CFBundleShortVersionString -string "$BUNDLE_VERSION" "$STAGED_BUNDLE/Contents/Info.plist"
plutil -insert CFBundleVersion -string "$BUNDLE_VERSION" "$STAGED_BUNDLE/Contents/Info.plist"
install -m 0755 "$EXECUTABLE" "$STAGED_BUNDLE/Contents/MacOS/$EXECUTABLE_NAME"
codesign --force --sign - --timestamp=none "$STAGED_BUNDLE" >/dev/null

BUNDLE="$BUNDLE_PARENT/$APP_NAME.app"
readonly BUNDLE
rm -rf -- "$BUNDLE"
mv -- "$STAGED_BUNDLE" "$BUNDLE"
rmdir -- "$STAGING_ROOT"
STAGING_ROOT=""

if [[ "$LAUNCH" == false ]]; then
    echo "$BUNDLE"
    exit 0
fi
# LaunchServices makes the bundle the responsible process for its own privacy checks, as it is for
# an installed SpaceTerm. Executing the binary from here would attribute those checks to the
# terminal that ran this script, so System Permissions would follow that terminal instead.
OUTPUT=/dev/null
if [[ -t 0 ]]; then
    OUTPUT="$(tty)"
fi
readonly OUTPUT
exec open -W -n --stdout "$OUTPUT" --stderr "$OUTPUT" "$BUNDLE"
