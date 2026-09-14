#!/bin/sh
set -eu

readonly DEFAULT_BUDGET_MIB=20480

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
repo_dir=$(CDPATH='' cd -- "$script_dir/.." && pwd -P)

usage() {
    cat <<'EOF'
Usage: cargo-artifacts.sh run -- COMMAND [ARG...]
       cargo-artifacts.sh status
       cargo-artifacts.sh clean

Keep the active Cargo target directory within a bounded disk budget. The default
budget is 20 GiB. Set SPACETERM_CARGO_TARGET_BUDGET_MIB to a positive integer to
override it. CARGO_TARGET_DIR is honored; relative paths resolve from the caller's
working directory, as they do for Cargo.
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
    else
        target_dir=$repo_dir/target
    fi

    case "$target_dir" in
        *'/../'*|*/..|*'/./'*|*/.)
            die "CARGO_TARGET_DIR must not contain . or .. path segments"
            ;;
    esac

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
    [ "$target_dir" = "$repo_dir/target" ] || [ -f "$target_dir/.rustc_info.json" ]
}

clean_target() {
    [ -d "$target_dir" ] || return 0
    if ! is_cargo_target; then
        echo "error: refusing to clean an unverified Cargo target directory: $target_dir" >&2
        return 2
    fi
    cargo clean --manifest-path "$repo_dir/Cargo.toml" --target-dir "$target_dir"
}

clean_if_over_budget() {
    phase=$1
    current_kib=$(size_kib)
    if [ "$current_kib" -gt "$budget_kib" ]; then
        current_mib=$((current_kib / 1024))
        echo "Cargo target is ${current_mib} MiB after $phase; budget is ${budget_mib} MiB. Cleaning $target_dir." >&2
        clean_target
    fi
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

        clean_if_over_budget "the previous build"
        set +e
        CARGO_INCREMENTAL=0 SPACETERM_CARGO_ARTIFACT_GUARD_ACTIVE=1 "$@"
        command_status=$?
        clean_if_over_budget "the command"
        cleanup_status=$?
        set -e
        if [ "$cleanup_status" -ne 0 ]; then
            echo "warning: Cargo artifact cleanup failed with status $cleanup_status" >&2
        fi
        exit "$command_status"
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
