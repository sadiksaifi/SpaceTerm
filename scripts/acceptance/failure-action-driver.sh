#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C

usage() {
    echo "Usage: $(basename -- "$0") --control FIFO --case CASE" >&2
}

die() {
    echo "error: $*" >&2
    exit 1
}

control=""
case_id=""
while (( $# > 0 )); do
    case "$1" in
        --control)
            (( $# >= 2 )) || die "--control requires a FIFO path"
            control="$2"
            shift
            ;;
        --case)
            (( $# >= 2 )) || die "--case requires a fixed case ID"
            case_id="$2"
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            usage
            die "unknown argument: $1"
            ;;
    esac
    shift
done

[[ "$control" == /* ]] || die "--control must be an absolute path"
[[ -p "$control" && ! -L "$control" ]] || die "control is not a real FIFO"
status="$control.status"
[[ -p "$status" && ! -L "$status" ]] || die "status is not a real FIFO"
parent="$(dirname -- "$control")"
[[ -d "$parent" && ! -L "$parent" ]] || die "control parent must be a real directory"
[[ "$(stat -f '%u' "$control")" == "$(id -u)" && \
    "$(stat -f '%Lp' "$control")" == "600" ]] \
    || die "control FIFO is not owner-private"
[[ "$(stat -f '%u' "$status")" == "$(id -u)" && \
    "$(stat -f '%Lp' "$status")" == "600" ]] \
    || die "status FIFO is not owner-private"
[[ "$(stat -f '%u' "$parent")" == "$(id -u)" && \
    "$(stat -f '%Lp' "$parent")" == "700" ]] \
    || die "control parent is not owner-private"

case "$case_id" in
    presentation-invalid-scale|presentation-glyph|renderer-image-preflight|\
    renderer-resource-before-sync|renderer-resource-after-staging|pasteboard-write|\
    pty-fatal|emulator-fatal|normal-exit-control)
        ;;
    *)
        die "unknown failure action case"
        ;;
esac
command -v uuidgen >/dev/null 2>&1 || die "uuidgen is required"

# The verifier owns authentication, request IDs, sequence numbers, and the app peer. This FIFO
# accepts one fixed case name plus an opaque one-request correlation token; neither can carry
# terminal, clipboard, path, environment, or command content.
correlation="$(printf '%s%s' "$(uuidgen)" "$(uuidgen)" | tr -d '-' | tr '[:upper:]' '[:lower:]')"
[[ "$correlation" =~ ^[0-9a-f]{64}$ ]] || die "could not create a correlation token"
exec 3<> "$status"
printf '%s\t%s\n' "$case_id" "$correlation" > "$control"
accepted=""
IFS= read -r -t 30 accepted <&3 \
    || die "failure action was not accepted before timeout"
[[ "$accepted" == $'accepted\t'"$correlation" ]] \
    || die "failure action returned an invalid accepted status"
completed=""
IFS= read -r -t 7200 completed <&3 \
    || die "failure action did not complete before timeout"
[[ "$completed" == $'completed\t'"$correlation" ]] \
    || die "failure action returned an invalid completed status"
