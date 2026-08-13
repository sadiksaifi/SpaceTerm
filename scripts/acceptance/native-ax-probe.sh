#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIR
readonly SOURCE="$SCRIPT_DIR/native-ax-probe.m"
readonly BINARY_RELATIVE_PATH="identity/native-ax-probe"

usage() {
    cat <<'EOF'
Usage:
  native-ax-probe.sh compile RUN_DIR
  native-ax-probe.sh run RUN_DIR PROBE_ARGUMENT...

Compile the native macOS accessibility probe to RUN_DIR/identity/native-ax-probe,
or run that exact run-owned binary. RUN_DIR and every output parent must be real,
owner-private directories. Compilation and evidence creation never overwrite files.

The probe never launches or discovers an application. `run` requires a frozen,
owner-private live-subject identity through the probe's --identity argument.
EOF
}

die() {
    echo "native-ax-probe.sh: $*" >&2
    exit 1
}

private_real_directory() {
    local directory="$1"
    [[ "$directory" == /* ]] || die "run directory must be absolute"
    [[ -d "$directory" && ! -L "$directory" ]] || die "run directory is not a real directory"
    local canonical
    canonical="$(cd -- "$directory" && pwd -P)"
    [[ "$canonical" == "$directory" ]] || die "run directory must be canonical: $directory"
    local owner mode
    owner="$(stat -f '%u' "$directory")"
    mode="$(stat -f '%Lp' "$directory")"
    [[ "$owner" == "$(id -u)" ]] || die "run directory is not owned by the current user"
    (( (8#$mode & 077) == 0 )) || die "run directory must not grant group or other access"
}

(( $# > 0 )) || {
    usage >&2
    exit 2
}

case "$1" in
    compile)
        (( $# == 2 )) || die "compile requires exactly RUN_DIR"
        readonly run_dir="$2"
        private_real_directory "$run_dir"
        readonly binary="$run_dir/$BINARY_RELATIVE_PATH"
        [[ ! -e "$binary" && ! -L "$binary" ]] || die "binary already exists: $binary"
        if [[ ! -e "$run_dir/identity" ]]; then
            mkdir -m 0700 -- "$run_dir/identity"
        fi
        private_real_directory "$run_dir/identity"
        temporary_binary="$(mktemp "$run_dir/identity/.native-ax-probe.XXXXXX")"
        readonly temporary_binary
        cleanup_compile() {
            if [[ -n "${temporary_binary:-}" && -e "$temporary_binary" ]]; then
                unlink -- "$temporary_binary"
            fi
        }
        trap cleanup_compile EXIT INT TERM
        xcrun --sdk macosx clang \
            -fobjc-arc \
            -fblocks \
            -std=c17 \
            -Wall \
            -Wextra \
            -Werror \
            -Wpedantic \
            -mmacosx-version-min=11.0 \
            -framework AppKit \
            -framework ApplicationServices \
            -framework Foundation \
            -framework Security \
            "$SOURCE" \
            -o "$temporary_binary"
        codesign --force --sign - "$temporary_binary" >/dev/null 2>&1
        codesign --verify --strict "$temporary_binary"
        chmod 0700 "$temporary_binary"
        ln "$temporary_binary" "$binary" 2>/dev/null \
            || die "binary already exists: $binary"
        unlink -- "$temporary_binary"
        trap - EXIT INT TERM
        printf '%s\n' "$binary"
        ;;
    run)
        (( $# >= 4 )) || die "run requires RUN_DIR and probe arguments"
        readonly run_dir="$2"
        private_real_directory "$run_dir"
        readonly binary="$run_dir/$BINARY_RELATIVE_PATH"
        [[ -x "$binary" && ! -L "$binary" ]] || die "run-owned probe is missing: $binary"
        shift 2
        exec "$binary" --run-dir "$run_dir" "$@"
        ;;
    -h|--help)
        usage
        ;;
    *)
        usage >&2
        die "unknown command: $1"
        ;;
esac
