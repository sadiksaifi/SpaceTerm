#!/bin/bash

set -euo pipefail
IFS=$'\n\t'

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIR
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
readonly REPO_ROOT

PROFILE="${1:-}"
case "$PROFILE" in
    dev)
        CARGO_ARGUMENTS=(
            --manifest-path "$REPO_ROOT/Cargo.toml"
            --features development-app
            --locked
        )
        APPLICATION_COMMAND=(env)
        ;;
    appearance)
        CARGO_ARGUMENTS=(
            --manifest-path "$REPO_ROOT/Cargo.toml"
            --features appearance-exerciser
            --locked
        )
        APPLICATION_COMMAND=(env SPACETERM_APPEARANCE_EXERCISER=1)
        ;;
    *)
        echo "usage: $(basename -- "$0") dev|appearance" >&2
        exit 2
        ;;
esac
readonly PROFILE

command -v tic >/dev/null || {
    echo "error: tic is required to compile the SpaceTerm terminfo entry; run mise run doctor:linux" >&2
    exit 1
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

"$SCRIPT_DIR/cargo-artifacts.sh" run -- \
    python3 "$SCRIPT_DIR/cargo-build-executable.py" \
    --output "$ARTIFACT_PATH" --bin spaceterm -- \
    "${CARGO_ARGUMENTS[@]}"
EXECUTABLE="$(<"$ARTIFACT_PATH")"
rm -f -- "$ARTIFACT_PATH"

# The private prefix mirrors an installed layout, so resources resolve from <prefix>/share/spaceterm.
PREFIX_PARENT="$(dirname -- "$(dirname -- "$EXECUTABLE")")/development-apps"
readonly PREFIX_PARENT
mkdir -p -- "$PREFIX_PARENT"
STAGING_ROOT="$(mktemp -d "$PREFIX_PARENT/.prefix.XXXXXX")"
install -D -m 0755 "$EXECUTABLE" "$STAGING_ROOT/bin/spaceterm"
mkdir -p -- "$STAGING_ROOT/share/spaceterm"
cp -R -- "$REPO_ROOT/assets/shell-integration" "$STAGING_ROOT/share/spaceterm/"
tic -x -o "$STAGING_ROOT/share/spaceterm/terminfo" "$REPO_ROOT/assets/terminfo/xterm-spaceterm.terminfo"
[[ -f "$STAGING_ROOT/share/spaceterm/terminfo/x/xterm-spaceterm" ]] || {
    echo "error: tic did not produce the SpaceTerm terminfo entry" >&2
    exit 1
}

PREFIX="$PREFIX_PARENT/$PROFILE"
readonly PREFIX
rm -rf -- "$PREFIX"
mv -- "$STAGING_ROOT" "$PREFIX"
STAGING_ROOT=""

APPLICATION_COMMAND+=("$PREFIX/bin/spaceterm")
exec "${APPLICATION_COMMAND[@]}"
