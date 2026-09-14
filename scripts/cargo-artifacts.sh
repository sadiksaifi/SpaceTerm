#!/bin/sh
set -eu

readonly DEFAULT_BUDGET_MIB=20480
readonly BUDGET_BREACH_STATUS=75
readonly MONITOR_INTERVAL_SECONDS=1

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
        echo "error: refusing to clean an unverified Cargo target directory" >&2
        return 2
    fi
    cargo clean --manifest-path "$repo_dir/Cargo.toml" --target-dir "$target_dir"
}

target_is_over_budget() {
    current_kib=$(size_kib)
    [ "$current_kib" -gt "$budget_kib" ]
}

clean_previous_if_over_budget() {
    if target_is_over_budget; then
        echo "Cargo target exceeds its disk budget from a previous command; cleaning it." >&2
        clean_target
    fi
}

command_pid=
command_pgid=

command_is_running() {
    [ -n "$command_pid" ] && kill -0 "$command_pid" 2>/dev/null
}

command_group_is_running() {
    [ -n "$command_pgid" ] && kill -0 -"$command_pgid" 2>/dev/null
}

terminate_command_tree() {
    [ -n "$command_pid" ] || return 0

    if [ -n "$command_pgid" ]; then
        kill -TERM -"$command_pgid" 2>/dev/null || true
    else
        kill -TERM "$command_pid" 2>/dev/null || true
    fi

    # Give cooperative processes a short opportunity to stop, then ensure that
    # every descendant in the isolated process group has been terminated.
    sleep 1
    if command_group_is_running; then
        kill -KILL -"$command_pgid" 2>/dev/null || true
    elif command_is_running; then
        kill -KILL "$command_pid" 2>/dev/null || true
    fi

    wait "$command_pid" 2>/dev/null || true

    attempts=0
    while command_group_is_running && [ "$attempts" -lt 5 ]; do
        sleep 1
        attempts=$((attempts + 1))
    done
    ! command_group_is_running
}

handle_signal() {
    signal_status=$1
    trap - HUP INT TERM
    terminate_command_tree || true
    exit "$signal_status"
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

        clean_previous_if_over_budget

        set +e
        # Monitor mode gives the wrapped command its own process group. This
        # lets a budget breach stop Cargo, build scripts, linkers, and their
        # other descendants together without signalling this guard.
        set -m
        CARGO_INCREMENTAL=0 SPACETERM_CARGO_ARTIFACT_GUARD_ACTIVE=1 "$@" &
        command_pid=$!
        set +m
        command_pgid=$(ps -o pgid= -p "$command_pid" 2>/dev/null | awk 'NR == 1 { gsub(/[[:space:]]/, ""); print }')

        if command_is_running && [ "$command_pgid" != "$command_pid" ]; then
            # Never signal a process group that this guard did not create.
            command_pgid=
            terminate_command_tree || true
            die "could not isolate the guarded command process group"
        fi

        trap 'handle_signal 129' HUP
        trap 'handle_signal 130' INT
        trap 'handle_signal 143' TERM

        budget_breached=0
        termination_failed=0
        while command_is_running; do
            if target_is_over_budget; then
                budget_breached=1
                terminate_command_tree || termination_failed=1
                break
            fi
            sleep "$MONITOR_INTERVAL_SECONDS"
        done

        wait "$command_pid" 2>/dev/null
        command_status=$?
        trap - HUP INT TERM

        # A short command can finish between monitor samples. Treat a target
        # that is over budget at completion as the same bounded failure.
        if [ "$budget_breached" -eq 0 ] && target_is_over_budget; then
            budget_breached=1
        fi

        if [ "$budget_breached" -eq 1 ]; then
            cleanup_status=0
            if [ "$termination_failed" -eq 0 ]; then
                clean_target
                cleanup_status=$?
            else
                echo "warning: guarded command termination could not be verified" >&2
            fi
            if [ "$cleanup_status" -ne 0 ]; then
                echo "warning: Cargo artifact cleanup failed with status $cleanup_status" >&2
            fi
            echo "error: Cargo artifact budget exceeded; command stopped" >&2
            exit "$BUDGET_BREACH_STATUS"
        fi

        set -e
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
