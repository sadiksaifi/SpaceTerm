#!/bin/bash

set -euo pipefail
IFS=$'\n\t'

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIR
readonly SOURCE="$SCRIPT_DIR/native-probe.c"

usage() {
    cat <<'EOF'
Usage:
  native-probe.sh compile OUTPUT
  native-probe.sh run BINARY PROBE_ARGUMENT...

Compile the native acceptance probe into a run-owned directory, then invoke it.
The compile command refuses to replace an existing file.
EOF
}

die() {
    echo "native-probe.sh: $*" >&2
    exit 1
}

(( $# > 0 )) || {
    usage >&2
    exit 2
}

case "$1" in
    compile)
        (( $# == 2 )) || die "compile requires exactly one output path"
        readonly output="$2"
        [[ "$output" == /* ]] || die "output path must be absolute"
        [[ ! -e "$output" ]] || die "output already exists: $output"
        [[ -d "$(dirname -- "$output")" ]] || die "output directory does not exist"
        xcrun --sdk macosx clang \
            -std=c11 \
            -Wall \
            -Wextra \
            -Werror \
            -pedantic \
            "$SOURCE" \
            -o "$output"
        ;;
    run)
        (( $# >= 3 )) || die "run requires a binary and probe command"
        readonly binary="$2"
        shift 2
        [[ -x "$binary" ]] || die "probe binary is not executable: $binary"
        exec "$binary" "$@"
        ;;
    -h|--help)
        usage
        ;;
    *)
        usage >&2
        die "unknown command: $1"
        ;;
esac
