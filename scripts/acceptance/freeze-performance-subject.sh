#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
umask 077

SUBJECT=""
PID=""
APP_BUNDLE=""
OUTPUT=""
TEMP=""

usage() {
    cat <<EOF
Usage: $(basename -- "$0") --subject spaceterm|ghostty --pid PID \\
  --app-bundle PATH --output PATH

Freeze a content-free identity for one already-running signed packaged app.
The private record binds the exact native process, canonical bundle and
executable paths, executable vnode and hash, bundle version, and signing
identity without making claims about application internals.
EOF
}

die() {
    echo "error: $*" >&2
    exit 1
}

cleanup() {
    [[ -z "$TEMP" ]] || rm -f -- "$TEMP"
}

one_line() {
    tr '\t\r\n' '   ' | awk '{$1=$1; print}'
}

plist_value() {
    plutil -extract "$2" raw -o - "$1"
}

process_value() {
    ps -p "$PID" -o "$1=" 2>/dev/null \
        | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//'
}

while (( $# > 0 )); do
    case "$1" in
        --subject) SUBJECT="${2:-}"; shift ;;
        --pid) PID="${2:-}"; shift ;;
        --app-bundle) APP_BUNDLE="${2:-}"; shift ;;
        --output) OUTPUT="${2:-}"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument: $1" ;;
    esac
    shift
done

[[ "$SUBJECT" == spaceterm || "$SUBJECT" == ghostty ]] \
    || die "subject must be spaceterm or ghostty"
[[ "$PID" =~ ^[1-9][0-9]*$ ]] || die "PID must be a positive integer"
[[ -d "$APP_BUNDLE" && "$APP_BUNDLE" == *.app ]] \
    || die "app bundle must be an existing .app directory"
[[ -n "$OUTPUT" && ! -e "$OUTPUT" ]] || die "output path is missing or exists"
for command in awk codesign plutil ps realpath sed shasum stat tr; do
    command -v "$command" >/dev/null 2>&1 || die "required command not found: $command"
done

APP_BUNDLE="$(realpath "$APP_BUNDLE")"
info_plist="$APP_BUNDLE/Contents/Info.plist"
[[ -f "$info_plist" ]] || die "bundle is missing Contents/Info.plist"
executable_name="$(plist_value "$info_plist" CFBundleExecutable)"
[[ "$executable_name" =~ ^[A-Za-z0-9._-]+$ ]] || die "invalid bundle executable name"
executable="$(realpath "$APP_BUNDLE/Contents/MacOS/$executable_name")"
[[ -f "$executable" && -x "$executable" ]] || die "bundle executable is unavailable"

state="$(process_value state)"
[[ -n "$state" && "$state" != Z* ]] || die "target is not a live process"
process_executable="$(process_value comm)"
[[ -n "$process_executable" && -e "$process_executable" ]] \
    || die "target executable cannot be resolved"
[[ "$(realpath "$process_executable")" == "$executable" ]] \
    || die "target process does not execute the supplied bundle"
process_start_identity="$(process_value lstart | one_line)"
[[ -n "$process_start_identity" ]] || die "target start identity is unavailable"

codesign --verify --strict "$APP_BUNDLE" >/dev/null 2>&1 \
    || die "bundle signature verification failed"
signature="$({ codesign --display --verbose=4 "$executable" 2>&1 1>/dev/null; } \
    | awk -F = '
        $1 == "Identifier" { identifier = $2 }
        $1 == "TeamIdentifier" { team = $2 }
        $1 == "CDHash" { cdhash = $2 }
        END {
            if (identifier != "" && cdhash != "") {
                if (team == "" || team == "not set") team = "none"
                print identifier "\t" team "\t" cdhash
            }
        }
    ')"
[[ -n "$signature" ]] || die "signing identity is incomplete"
IFS=$'\t' read -r signing_identifier team_identifier cdhash <<< "$signature"
[[ "$signing_identifier" =~ ^[A-Za-z0-9._-]+$ \
    && "$team_identifier" =~ ^[A-Za-z0-9._-]+$ \
    && "$cdhash" =~ ^[0-9A-Fa-f]+$ ]] || die "signing fields are invalid"
cdhash="$(printf '%s' "$cdhash" | tr '[:upper:]' '[:lower:]')"

bundle_identifier="$(plist_value "$info_plist" CFBundleIdentifier)"
marketing_version="$(plist_value "$info_plist" CFBundleShortVersionString)"
build_version="$(plist_value "$info_plist" CFBundleVersion)"
for value in "$bundle_identifier" "$marketing_version" "$build_version"; do
    [[ "$value" =~ ^[A-Za-z0-9._+-]+$ ]] || die "bundle identity field is invalid"
done

executable_sha256="$(shasum -a 256 "$executable" | awk '{ print $1 }')"
executable_device="$(stat -f '%d' "$executable")"
executable_inode="$(stat -f '%i' "$executable")"
[[ "$executable_device" =~ ^[0-9]+$ && "$executable_inode" =~ ^[0-9]+$ ]] \
    || die "executable vnode identity is unavailable"

# Verify the process and executable once more after hashing and signing reads.
[[ "$(process_value lstart | one_line)" == "$process_start_identity" \
    && "$(realpath "$(process_value comm)")" == "$executable" \
    && "$(stat -f '%d' "$executable")" == "$executable_device" \
    && "$(stat -f '%i' "$executable")" == "$executable_inode" \
    && "$(shasum -a 256 "$executable" | awk '{ print $1 }')" == "$executable_sha256" ]] \
    || die "subject identity changed while it was frozen"

mkdir -p -- "$(dirname -- "$OUTPUT")"
TEMP="${OUTPUT}.tmp.$$"
trap cleanup EXIT INT TERM
{
    printf 'format_version\t1\n'
    printf 'subject\t%s\n' "$SUBJECT"
    printf 'app_bundle_path\t%s\n' "$APP_BUNDLE"
    printf 'bundle_identifier\t%s\n' "$bundle_identifier"
    printf 'bundle_version\t%s+%s\n' "$marketing_version" "$build_version"
    printf 'executable_path\t%s\n' "$executable"
    printf 'executable_sha256\t%s\n' "$executable_sha256"
    printf 'executable_device\t%s\n' "$executable_device"
    printf 'executable_inode\t%s\n' "$executable_inode"
    # Darwin st_dev is the stable filesystem identity used with the vnode.
    printf 'executable_fsid\t%s\n' "$executable_device"
    printf 'signature_valid\ttrue\n'
    printf 'signing_identifier\t%s\n' "$signing_identifier"
    printf 'team_identifier\t%s\n' "$team_identifier"
    printf 'cdhash\t%s\n' "$cdhash"
    printf 'process_pid\t%s\n' "$PID"
    printf 'process_start_identity\t%s\n' "$process_start_identity"
    printf 'identity_status\tfrozen\n'
} > "$TEMP"
chmod 0444 "$TEMP"
ln "$TEMP" "$OUTPUT" || die "output path was created concurrently"
rm -f -- "$TEMP"
TEMP=""
trap - EXIT INT TERM
printf 'subject_identity_sha256\t%s\n' "$(shasum -a 256 "$OUTPUT" | awk '{ print $1 }')"
