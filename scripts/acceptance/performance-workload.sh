#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C

usage() {
    cat <<'EOF'
Usage:
  performance-workload.sh --producer ABSOLUTE_EXECUTABLE -- PRODUCER_ARGUMENT...

Run an explicitly built native performance producer. This wrapper never compiles,
copies, or replaces a producer. It uses exec so the native producer exclusively
owns PTY termios restoration and the final post-termios output sentinel.
EOF
}

die() {
    echo "performance-workload.sh: $*" >&2
    exit 2
}

(( $# > 0 )) || {
    usage >&2
    exit 2
}

if [[ "$1" == "-h" || "$1" == "--help" ]]; then
    (( $# == 1 )) || die "--help takes no arguments"
    usage
    exit 0
fi

[[ "$1" == "--producer" ]] || die "first argument must be --producer"
(( $# >= 4 )) || die "--producer requires an executable and producer arguments"
readonly producer="$2"
[[ "$3" == "--" ]] || die "--producer must be followed by --"
[[ "$producer" == /* ]] || die "producer path must be absolute"
[[ "$producer" != *$'\n'* && "$producer" != *$'\t'* ]] \
    || die "producer path cannot contain a tab or newline"
[[ -f "$producer" && -x "$producer" && ! -L "$producer" ]] \
    || die "producer must be an existing, non-symlink regular executable"

shift 3
exec "$producer" "$@"
