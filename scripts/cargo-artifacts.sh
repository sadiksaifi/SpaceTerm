#!/bin/sh
set -eu

readonly DEFAULT_BUDGET_MIB=20480

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
repo_dir=$(CDPATH='' cd -- "$script_dir/.." && pwd -P)
supervisor=$script_dir/cargo-artifact-supervisor.py

usage() {
    cat <<'EOF'
Usage: cargo-artifacts.sh run -- COMMAND [ARG...]
       cargo-artifacts.sh status
       cargo-artifacts.sh clean

Keep the active Cargo target directory within a bounded disk budget. The default
budget is 20 GiB. Set SPACETERM_CARGO_TARGET_BUDGET_MIB to a positive integer to
override it. CARGO_TARGET_DIR is honored; relative paths resolve from the caller's
working directory. Cargo configuration is honored through `cargo metadata`, and
the resolved directory is passed back to every guarded Cargo process. An external
target must be empty when SpaceTerm first claims it for automatic cleanup.
EOF
}

die() {
    echo "error: $*" >&2
    exit 2
}

resolve_target_dir() {
    if [ -n "${CARGO_TARGET_DIR:-}" ]; then
        case "$CARGO_TARGET_DIR" in
            /*) target_dir=$CARGO_TARGET_DIR ;;
            *) target_dir=$PWD/$CARGO_TARGET_DIR ;;
        esac
        case "$target_dir" in
        *'/../'*|*/..|*'/./'*|*/.)
            die "CARGO_TARGET_DIR must not contain . or .. path segments"
            ;;
        esac
        metadata=$(cd "$repo_dir" && CARGO_TARGET_DIR="$target_dir" cargo metadata \
            --format-version 1 --no-deps --manifest-path "$repo_dir/Cargo.toml") \
            || die "could not resolve Cargo target directory"
    else
        metadata=$(cd "$repo_dir" && cargo metadata \
            --format-version 1 --no-deps --manifest-path "$repo_dir/Cargo.toml") \
            || die "could not resolve Cargo target directory"
    fi

    target_dir=$(printf '%s\n' "$metadata" | python3 -c \
        'import json, sys; print(json.load(sys.stdin)["target_directory"])') \
        || die "Cargo metadata did not report a target directory"

    if [ -d "$target_dir" ]; then
        target_dir=$(CDPATH='' cd -- "$target_dir" && pwd -P)
    fi

    case "$target_dir" in
        /|"$repo_dir"|"${HOME:-/nonexistent}")
            die "refusing unsafe Cargo target directory: $target_dir"
            ;;
    esac
}

read_budget() {
    budget_mib=${SPACETERM_CARGO_TARGET_BUDGET_MIB:-$DEFAULT_BUDGET_MIB}
    case "$budget_mib" in
        ''|*[!0-9]*) die "SPACETERM_CARGO_TARGET_BUDGET_MIB must be a positive integer" ;;
    esac
    [ "$budget_mib" -gt 0 ] \
        || die "SPACETERM_CARGO_TARGET_BUDGET_MIB must be greater than zero"
    [ "$budget_mib" -le 1048576 ] \
        || die "SPACETERM_CARGO_TARGET_BUDGET_MIB must not exceed 1048576"
    budget_kib=$((budget_mib * 1024))
}

size_kib() {
    if [ -d "$target_dir" ]; then
        du -sk -- "$target_dir" | awk '{print $1}'
    else
        echo 0
    fi
}

is_cargo_target() {
    [ -f "$target_dir/.spaceterm-cargo-target-owner" ] \
        && [ "$(cat "$target_dir/.spaceterm-cargo-target-owner")" = "$repo_dir" ]
}

clean_target() {
    [ -d "$target_dir" ] || return 0
    if ! is_cargo_target; then
        echo "error: refusing to clean a Cargo target directory not owned by this repository" >&2
        return 2
    fi
    printf 'Signature: 8a477f597d28d172789f06886806bc55\n' > "$target_dir/CACHEDIR.TAG"
    cargo clean --manifest-path "$repo_dir/Cargo.toml" --target-dir "$target_dir"
}

resolve_target_dir
read_budget

command=${1:-}
case "$command" in
    run)
        shift
        [ "${1:-}" = "--" ] || die "run requires -- before the command"
        shift
        [ "$#" -gt 0 ] || die "run requires a command"

        # A nested guarded mise task inherits this marker and executes directly.
        if [ "${SPACETERM_CARGO_ARTIFACT_GUARD_ACTIVE:-}" = 1 ]; then
            exec "$@"
        fi

        exec python3 "$supervisor" \
            --repo-dir "$repo_dir" \
            --target-dir "$target_dir" \
            --budget-kib "$budget_kib" \
            -- "$@"
        ;;
    status)
        current_kib=$(size_kib)
        current_mib=$((current_kib / 1024))
        printf 'Cargo target: %s\nSize: %s MiB\nBudget: %s MiB\n' \
            "$target_dir" "$current_mib" "$budget_mib"
        ;;
    clean)
        clean_target
        ;;
    -h|--help|help)
        usage
        ;;
    *)
        usage >&2
        die "expected run, status, or clean"
        ;;
esac
