#!/bin/bash

set -euo pipefail
IFS=$'\n\t'

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIR
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd -P)"
readonly REPO_ROOT

[[ $# -eq 0 ]] || {
    echo "usage: $(basename -- "$0")" >&2
    exit 2
}
readonly INFO_PLIST_SOURCE="$SCRIPT_DIR/development/Info.plist"
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
    python3 "$REPO_ROOT/scripts/cargo-build-executable.py" \
    --output "$ARTIFACT_PATH" --bin spaceterm -- \
    "${CARGO_ARGUMENTS[@]}"
EXECUTABLE="$(<"$ARTIFACT_PATH")"
rm -f -- "$ARTIFACT_PATH"

BUNDLE_PARENT="$(dirname -- "$(dirname -- "$EXECUTABLE")")/development-apps"
readonly BUNDLE_PARENT
mkdir -p -- "$BUNDLE_PARENT"
STAGING_ROOT="$(mktemp -d "$BUNDLE_PARENT/.bundle.XXXXXX")"
STAGED_BUNDLE="$STAGING_ROOT/$APP_NAME.app"
readonly STAGED_BUNDLE
mkdir -p -- "$STAGED_BUNDLE/Contents/MacOS"
install -m 0644 "$INFO_PLIST_SOURCE" "$STAGED_BUNDLE/Contents/Info.plist"
BUNDLE_VERSION="$(python3 "$REPO_ROOT/packaging/release-version.py" --field bundle_version)"
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

exec "$BUNDLE/Contents/MacOS/$EXECUTABLE_NAME"
