#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C

usage() {
    cat <<'EOF'
Usage:
  native-payloads.sh styles
  native-payloads.sh unicode
  native-payloads.sh links EXISTING_LOCAL_FILE
  native-payloads.sh scrollback [LINE_COUNT]
  native-payloads.sh attention
  native-payloads.sh all EXISTING_LOCAL_FILE

Emit deterministic conventional-terminal payloads to the current PTY. The `all`
command emits styles, Unicode, links, and attention, but not the large Scrollback fixture.
EOF
}

die() {
    echo "native-payloads.sh: $*" >&2
    exit 1
}

percent_encode_path() {
    local value="$1"
    local encoded=""
    local character byte numeric_byte
    local index=0
    while (( index < ${#value} )); do
        character="${value:index:1}"
        case "$character" in
            [a-zA-Z0-9/._~-])
                encoded+="$character"
                ;;
            *)
                # Bash 3.2 sign-extends bytes >= 0x80 when a character
                # constant is formatted directly. Mask to one unsigned byte.
                printf -v numeric_byte '%d' "'$character"
                printf -v byte '%%%02X' "$(( numeric_byte & 255 ))"
                encoded+="$byte"
                ;;
        esac
        (( index += 1 ))
    done
    printf '%s' "$encoded"
}

emit_styles() {
    printf '\033[0mSpaceTerm semantic style fixture\n'
    printf '\033[39;49msemantic-default\033[0m | \033[31;42mansi-red-on-green\033[0m | '
    printf '\033[94;103mbright-blue-on-bright-yellow\033[0m\n'
    printf '\033[38;5;196;48;5;25mindexed-fg-196-bg-25\033[0m | '
    printf '\033[38;2;20;210;140;48;2;45;20;95mrgb-fg-and-bg\033[0m\n'
    printf '\033[1mbold\033[0m \033[2mfaint\033[0m \033[3mitalic\033[0m '
    printf '\033[7mreverse\033[0m [\033[8minvisible\033[0m] \033[9mstrike\033[0m '
    printf '\033[53moverline\033[55m\n'
    printf '\033[4:1msingle\033[4:0m \033[4:2mdouble\033[4:0m '
    printf '\033[4:3mcurly\033[4:0m \033[4:4mdotted\033[4:0m '
    printf '\033[4:5mdashed\033[4:0m '
    printf '\033[58:2::240:120:80;4munderline-rgb\033[59;24m\n'
    printf '\033[5mblink-request\033[25m drawing: ┌─┬─┐ │█│░│ └─┴─┘ ⣿  \n'
    printf '\033[0m'
}

emit_unicode() {
    printf 'Unicode fixture\n'
    printf 'combining: é a⃝ Ż | precomposed: é Å\n'
    printf 'wide: 你好 世界 한글 日本語 | emoji: 😀 👨‍👩‍👧‍👦 🏳️‍🌈 ✈️\n'
    printf 'variation: ♥ ♥️ ☕︎ ☕️ | bidi-isolated: [⁨مرحبا⁩] [⁨שלום⁩]\n'
    printf 'symbols: ┏━┳━┓ ┃█┃▒┃ ┗━┻━┛ ⠿⣷⣿ \n'
}

emit_links() {
    local local_file="$1"
    [[ "$local_file" == /* ]] || die "local link target must be absolute"
    [[ -f "$local_file" ]] || die "local link target must be an existing regular file"
    local encoded
    local missing_file="${local_file}.spaceterm-missing"
    [[ ! -e "$missing_file" ]] || die "deterministic missing target already exists: $missing_file"
    local encoded_missing
    encoded="$(percent_encode_path "$local_file")"
    encoded_missing="$(percent_encode_path "$missing_file")"
    printf 'OSC 8 web: \033]8;;https://example.com/spaceterm-acceptance\033\\SpaceTerm web link\033]8;;\033\\\n'
    printf 'OSC 8 local: \033]8;;file://localhost%s\033\\SpaceTerm local file\033]8;;\033\\\n' "$encoded"
    printf 'Detected URL: https://example.com/spaceterm-detected\n'
    printf 'OSC 8 disallowed scheme: \033]8;;javascript:alert(1)\033\\inert javascript target\033]8;;\033\\\n'
    printf 'OSC 8 malformed target: \033]8;;:// malformed target\033\\inert malformed target\033]8;;\033\\\n'
    printf 'OSC 8 remote file: \033]8;;file://remote.invalid/tmp/missing\033\\inert remote file\033]8;;\033\\\n'
    printf 'OSC 8 missing local file: \033]8;;file://localhost%s\033\\inert missing local file\033]8;;\033\\\n' "$encoded_missing"
}

emit_scrollback() {
    local count="${1:-10050}"
    [[ "$count" =~ ^[0-9]+$ ]] || die "line count must be an integer"
    (( count > 0 && count <= 20000 )) || die "line count must be between 1 and 20000"
    local line
    for (( line = 1; line <= count; line += 1 )); do
        case $(( line % 5 )) in
            0)
                printf 'scrollback-%05d short hard line\n' "$line"
                ;;
            1)
                printf 'scrollback-%05d soft-wrap abcdefghijklmnopqrstuvwxyz0123456789-abcdefghijklmnopqrstuvwxyz0123456789-abcdefghijklmnopqrstuvwxyz0123456789-abcdefghijklmnopqrstuvwxyz0123456789-abcdefghijklmnopqrstuvwxyz0123456789-abcdefghijklmnopqrstuvwxyz0123456789\n' "$line"
                ;;
            2)
                printf '\033[38;5;%dmback-%05d styled\033[0m\n' "$(( line % 256 ))" "$line"
                ;;
            3)
                printf '\n'
                ;;
            4)
                printf 'scrollback-%05d wide 你好 😀 é ┃⣿┃\n' "$line"
                ;;
        esac
    done
}

emit_attention() {
    printf 'BEL follows once: \a\n'
}

(( $# > 0 )) || {
    usage >&2
    exit 2
}

case "$1" in
    styles)
        (( $# == 1 )) || die "styles takes no arguments"
        emit_styles
        ;;
    unicode)
        (( $# == 1 )) || die "unicode takes no arguments"
        emit_unicode
        ;;
    links)
        (( $# == 2 )) || die "links requires one existing local file"
        emit_links "$2"
        ;;
    scrollback)
        (( $# <= 2 )) || die "scrollback accepts at most one line count"
        emit_scrollback "${2:-10050}"
        ;;
    attention)
        (( $# == 1 )) || die "attention takes no arguments"
        emit_attention
        ;;
    all)
        (( $# == 2 )) || die "all requires one existing local file"
        emit_styles
        emit_unicode
        emit_links "$2"
        emit_attention
        ;;
    -h|--help)
        usage
        ;;
    *)
        usage >&2
        die "unknown payload: $1"
        ;;
esac
