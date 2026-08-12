#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
umask 077

readonly DEFAULT_RESIZE_LINES=10000
readonly INPUT_POLL_INTERVAL=32

SCENARIO=""
DURATION_SECONDS=""
FIXED_ITERATIONS=""
METRICS_PATH=""
RESIZE_LINES="$DEFAULT_RESIZE_LINES"
METRICS_TEMP=""
INPUT_QUEUE_DIRECTORY=""
INPUT_READER_PID=""
NEXT_INPUT_EVENT=1
CLEANED_UP=false
CAMPAIGN_ID=""
SESSION_ID=""
OUTPUT_MODE="regular-file-test"
OPOST_DISABLED=false
ORIGINAL_TERM_STATE=""
TERM_FD_OPEN=false
CAMPAIGN_SECRET_FILE=""

usage() {
    cat <<EOF
Usage: $(basename -- "$0") --scenario NAME --metrics PATH [RUN LIMIT]

Emit deterministic terminal performance workloads to stdout and write exact byte
accounting to a private metrics file.

Scenarios:
  ascii           High-rate printable ASCII output.
  unicode-styles  Unicode, ANSI styles, OSC 8 links, and drawing symbols.
  resize-seed     Exactly 10,000 or more mixed rows for resize/reflow acceptance.

Run limits for ascii and unicode-styles (choose exactly one):
  --duration-seconds N  Run for at least N elapsed seconds.
  --iterations N        Emit exactly N seed chunks; intended for focused checks.

Options:
  --metrics PATH        Required private key/value TSV output.
  --resize-lines N      Resize seed row count; must be at least 10000.
  --campaign-id ID      Required campaign binding for duration workloads.
  --session-id ID       Required terminal-session binding for duration workloads.
  --campaign-secret-file PATH  Owner-private campaign authentication key.
  -h, --help            Show this help.

While a sustained workload runs, enter a line and press Return. The workload
consumes it and emits a content-free acknowledgement with only its byte count.
EOF
}

die() {
    echo "error: $*" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

is_positive_integer() {
    [[ "$1" =~ ^[1-9][0-9]*$ ]]
}

is_safe_label() {
    [[ "$1" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]]
}

cleanup() {
    [[ "$CLEANED_UP" == false ]] || return 0
    CLEANED_UP=true
    if [[ -n "$INPUT_READER_PID" ]]; then
        if kill -0 "$INPUT_READER_PID" 2>/dev/null; then
            kill "$INPUT_READER_PID" 2>/dev/null || true
        fi
        wait "$INPUT_READER_PID" 2>/dev/null || true
        INPUT_READER_PID=""
    fi
    if [[ -n "$INPUT_QUEUE_DIRECTORY" ]]; then
        rm -rf -- "$INPUT_QUEUE_DIRECTORY"
    fi
    if [[ -n "$METRICS_TEMP" ]]; then
        rm -f -- "$METRICS_TEMP"
    fi
    if [[ -n "$ORIGINAL_TERM_STATE" ]]; then
        stty "$ORIGINAL_TERM_STATE" <&7 2>/dev/null || true
        ORIGINAL_TERM_STATE=""
    fi
    if [[ "$TERM_FD_OPEN" == true ]]; then
        exec 7>&-
        TERM_FD_OPEN=false
    fi
}

handle_signal() {
    cleanup
    trap - EXIT INT TERM
    exit 130
}

start_input_reader() {
    INPUT_QUEUE_DIRECTORY="$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-performance-input.XXXXXX")"
    exec 9<&0
    (
        sequence=1
        while IFS= read -r input; do
            event_temp="$INPUT_QUEUE_DIRECTORY/${sequence}.tmp"
            printf '%d\n' "${#input}" > "$event_temp"
            mv -- "$event_temp" "$INPUT_QUEUE_DIRECTORY/${sequence}.ready"
            ((sequence += 1))
        done
    ) <&9 &
    INPUT_READER_PID=$!
    if [[ -n "${SPACETERM_TEST_READER_PID_PATH:-}" ]]; then
        printf '%s\n' "$INPUT_READER_PID" > "$SPACETERM_TEST_READER_PID_PATH"
    fi
    exec 9<&-
}

build_ascii_seed() {
    local index
    for ((index = 0; index < 128; index += 1)); do
        printf 'ASCII-%03d 0123456789 abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ !@#$%%^&*()[]{}\r\n' \
            "$index"
    done
    printf '\033[0m'
}

build_unicode_styles_seed() {
    local index
    for ((index = 0; index < 64; index += 1)); do
        printf '\033[1;3;38;2;120;180;255mSTYLE-%03d\033[0m ' "$index"
        printf 'combining=e\314\201 wide=\347\225\214 emoji=\360\237\221\251\342\200\215\360\237\222\273 '
        printf '\033[4:3;58;2;255;120;180mcurly\033[0m '
        printf '\033[9;53mstrike-overline\033[0m '
        printf '\033[5mblink\033[0m '
        printf '\033]8;;https://example.test/spaceterm/%03d\033\\link-%03d\033]8;;\033\\ ' \
            "$index" "$index"
        printf 'draw=\342\224\200\342\224\202\342\224\214\342\224\230 block=\342\226\210 braille=\342\240\277 powerline=\356\202\260\r\n'
    done
    printf '\033[0m'
}

build_resize_seed() {
    local index
    local long_text
    long_text="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
    long_text+="$long_text$long_text$long_text$long_text$long_text$long_text$long_text"

    for ((index = 0; index < RESIZE_LINES; index += 1)); do
        case $((index % 5)) in
            0)
                printf 'short-%05d\r\n' "$index"
                ;;
            1)
                printf 'soft-wrap-%05d %s\r\n' "$index" "$long_text"
                ;;
            2)
                printf '\033[1;38;5;33;48;5;235mstyled-%05d\033[0m\r\n' "$index"
                ;;
            3)
                printf '\r\n'
                ;;
            4)
                printf 'wide-%05d \347\225\214\347\225\214 \360\237\230\200 e\314\201 \342\224\200\342\224\202\342\224\214\342\224\230\r\n' "$index"
                ;;
        esac
    done
    printf '\033[0m'
}

while (( $# > 0 )); do
    case "$1" in
        --scenario)
            (( $# >= 2 )) || die "--scenario requires a value"
            SCENARIO="$2"
            shift
            ;;
        --duration-seconds)
            (( $# >= 2 )) || die "--duration-seconds requires a value"
            DURATION_SECONDS="$2"
            shift
            ;;
        --iterations)
            (( $# >= 2 )) || die "--iterations requires a value"
            FIXED_ITERATIONS="$2"
            shift
            ;;
        --metrics)
            (( $# >= 2 )) || die "--metrics requires a path"
            METRICS_PATH="$2"
            shift
            ;;
        --resize-lines)
            (( $# >= 2 )) || die "--resize-lines requires a value"
            RESIZE_LINES="$2"
            shift
            ;;
        --campaign-id)
            (( $# >= 2 )) || die "--campaign-id requires a value"
            CAMPAIGN_ID="$2"
            shift
            ;;
        --session-id)
            (( $# >= 2 )) || die "--session-id requires a value"
            SESSION_ID="$2"
            shift
            ;;
        --campaign-secret-file)
            (( $# >= 2 )) || die "--campaign-secret-file requires a value"
            CAMPAIGN_SECRET_FILE="$2"
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            die "unknown argument: $1"
            ;;
    esac
    shift
done

[[ -n "$SCENARIO" ]] || die "--scenario is required"
[[ -n "$METRICS_PATH" ]] || die "--metrics is required"
[[ ! -e "$METRICS_PATH" ]] || die "metrics path already exists: $METRICS_PATH"
require_command date
require_command shasum
require_command awk
require_command mktemp
require_command grep
require_command python3
require_command stat
require_command wc

case "$SCENARIO" in
    ascii|unicode-styles)
        if [[ -n "$DURATION_SECONDS" && -n "$FIXED_ITERATIONS" ]]; then
            die "choose only one of --duration-seconds or --iterations"
        fi
        if [[ -z "$DURATION_SECONDS" && -z "$FIXED_ITERATIONS" ]]; then
            die "$SCENARIO requires --duration-seconds or --iterations"
        fi
        if [[ -n "$DURATION_SECONDS" ]] && ! is_positive_integer "$DURATION_SECONDS"; then
            die "duration must be a positive integer"
        fi
        if [[ -n "$DURATION_SECONDS" ]]; then
            is_safe_label "$CAMPAIGN_ID" || die "duration workload requires a campaign ID"
            is_safe_label "$SESSION_ID" || die "duration workload requires a session ID"
            [[ -f "$CAMPAIGN_SECRET_FILE" ]] \
                || die "duration workload requires a campaign secret file"
            campaign_secret_mode="$(stat -f '%Lp' "$CAMPAIGN_SECRET_FILE")"
            (( (8#$campaign_secret_mode & 8#077) == 0 )) \
                || die "campaign secret file must not be group/world accessible"
            (( $(wc -c < "$CAMPAIGN_SECRET_FILE") >= 32 )) \
                || die "campaign secret must contain at least 32 bytes"
        fi
        if [[ -n "$FIXED_ITERATIONS" ]] && ! is_positive_integer "$FIXED_ITERATIONS"; then
            die "iterations must be a positive integer"
        fi
        ;;
    resize-seed)
        [[ -z "$DURATION_SECONDS" && -z "$FIXED_ITERATIONS" ]] \
            || die "resize-seed does not accept a run limit"
        is_positive_integer "$RESIZE_LINES" || die "resize line count must be a positive integer"
        (( RESIZE_LINES >= DEFAULT_RESIZE_LINES )) \
            || die "resize-seed requires at least $DEFAULT_RESIZE_LINES rows"
        ;;
    *)
        die "unknown scenario: $SCENARIO"
        ;;
esac

mkdir -p -- "$(dirname -- "$METRICS_PATH")"
METRICS_TEMP="${METRICS_PATH}.tmp.$$"
trap cleanup EXIT
trap handle_signal INT TERM

if [[ -n "$DURATION_SECONDS" ]]; then
    if [[ -t 1 ]]; then
        require_command stty
        exec 7>&1
        TERM_FD_OPEN=true
        ORIGINAL_TERM_STATE="$(stty -g <&7)"
        stty -opost <&7
        stty -a <&7 | grep -Eq '(^|[ ;])-opost([ ;]|$)' \
            || die "could not disable PTY output post-processing"
        OUTPUT_MODE="pty-no-opost"
        OPOST_DISABLED=true
    elif [[ "${SPACETERM_TEST_ALLOW_REGULAR_OUTPUT:-0}" != 1 ]]; then
        die "duration workload requires a PTY stdout so OPOST can be disabled"
    fi
fi

seed=""
case "$SCENARIO" in
    ascii)
        seed="$(build_ascii_seed)"
        ;;
    unicode-styles)
        seed="$(build_unicode_styles_seed)"
        ;;
    resize-seed)
        seed="$(build_resize_seed)"
        ;;
esac

readonly seed
readonly seed_bytes=${#seed}
seed_sha256="$(printf '%s' "$seed" | shasum -a 256 | awk '{ print $1 }')"
readonly seed_sha256

started_epoch_seconds="$(date +%s)"
readonly started_epoch_seconds
SECONDS=0
iterations=0
workload_bytes=0
input_events=0
input_bytes=0
input_ack_bytes=0

emit_input_acknowledgement() {
    local input_byte_count=""
    local acknowledgement=""
    local event_path="$INPUT_QUEUE_DIRECTORY/${NEXT_INPUT_EVENT}.ready"
    while [[ -f "$event_path" ]]; do
        input_byte_count="$(< "$event_path")"
        rm -f -- "$event_path"
        ((input_events += 1))
        ((input_bytes += input_byte_count))
        printf -v acknowledgement \
            '\r\nSPACETERM-PERF-INPUT event=%d input-bytes=%d\r\n' \
            "$input_events" "$input_byte_count"
        printf '%s' "$acknowledgement"
        ((input_ack_bytes += ${#acknowledgement}))
        ((NEXT_INPUT_EVENT += 1))
        event_path="$INPUT_QUEUE_DIRECTORY/${NEXT_INPUT_EVENT}.ready"
    done
}

emit_seed() {
    printf '%s' "$seed"
    ((iterations += 1))
    ((workload_bytes += seed_bytes))
    if (( iterations % INPUT_POLL_INTERVAL == 0 )); then
        emit_input_acknowledgement
    fi
}

case "$SCENARIO" in
    ascii|unicode-styles)
        start_input_reader
        if [[ -n "$FIXED_ITERATIONS" ]]; then
            while (( iterations < FIXED_ITERATIONS )); do
                emit_seed
            done
        else
            while (( SECONDS < DURATION_SECONDS )); do
                emit_seed
            done
        fi
        ;;
    resize-seed)
        emit_seed
        ;;
esac

if [[ -n "$INPUT_READER_PID" && ! -t 0 ]]; then
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        kill -0 "$INPUT_READER_PID" 2>/dev/null || break
        sleep 0.01
    done
    if ! kill -0 "$INPUT_READER_PID" 2>/dev/null; then
        wait "$INPUT_READER_PID" 2>/dev/null || true
        INPUT_READER_PID=""
    fi
fi
if [[ -n "$INPUT_QUEUE_DIRECTORY" ]]; then
    emit_input_acknowledgement
fi

elapsed_seconds=$SECONDS
payload_bytes=$((workload_bytes + input_ack_bytes))
sentinel=""
printf -v sentinel \
    '\r\nSPACETERM-PERF-END scenario=%s seed-sha256=%s iterations=%d workload-bytes=%d input-events=%d\r\n' \
    "$SCENARIO" "$seed_sha256" "$iterations" "$workload_bytes" "$input_events"
readonly sentinel
readonly sentinel_bytes=${#sentinel}
printf '%s' "$sentinel"
readonly emitted_bytes=$((payload_bytes + sentinel_bytes))
finished_epoch_seconds="$(date +%s)"
readonly finished_epoch_seconds

{
    printf 'format_version\t2\n'
    printf 'scenario\t%s\n' "$SCENARIO"
    printf 'campaign_id\t%s\n' "$CAMPAIGN_ID"
    printf 'session_id\t%s\n' "$SESSION_ID"
    printf 'output_mode\t%s\n' "$OUTPUT_MODE"
    printf 'opost_disabled\t%s\n' "$OPOST_DISABLED"
    printf 'seed_sha256\t%s\n' "$seed_sha256"
    printf 'seed_bytes\t%d\n' "$seed_bytes"
    printf 'resize_seed_lines\t%d\n' "$([[ "$SCENARIO" == "resize-seed" ]] && printf '%d' "$RESIZE_LINES" || printf '0')"
    printf 'requested_duration_seconds\t%s\n' "${DURATION_SECONDS:-0}"
    printf 'requested_iterations\t%s\n' "${FIXED_ITERATIONS:-0}"
    printf 'elapsed_seconds\t%d\n' "$elapsed_seconds"
    printf 'iterations\t%d\n' "$iterations"
    printf 'workload_bytes\t%d\n' "$workload_bytes"
    printf 'input_events\t%d\n' "$input_events"
    printf 'input_bytes\t%d\n' "$input_bytes"
    printf 'input_ack_bytes\t%d\n' "$input_ack_bytes"
    printf 'sentinel_bytes\t%d\n' "$sentinel_bytes"
    printf 'emitted_bytes\t%d\n' "$emitted_bytes"
    printf 'started_epoch_seconds\t%s\n' "$started_epoch_seconds"
    printf 'finished_epoch_seconds\t%s\n' "$finished_epoch_seconds"
    printf 'status\tcomplete\n'
} > "$METRICS_TEMP"

if [[ -n "$DURATION_SECONDS" ]]; then
    metrics_hmac_sha256="$(python3 - "$METRICS_TEMP" "$CAMPAIGN_SECRET_FILE" <<'PY'
import hashlib
import hmac
import pathlib
import sys

payload = pathlib.Path(sys.argv[1]).read_bytes()
secret = pathlib.Path(sys.argv[2]).read_bytes()
if len(secret) < 32:
    raise SystemExit("campaign secret must contain at least 32 bytes")
print(hmac.new(secret, payload, hashlib.sha256).hexdigest())
PY
)"
    printf 'metrics_hmac_sha256\t%s\n' "$metrics_hmac_sha256" >> "$METRICS_TEMP"
fi

mv -- "$METRICS_TEMP" "$METRICS_PATH"
METRICS_TEMP=""
cleanup
trap - EXIT INT TERM
