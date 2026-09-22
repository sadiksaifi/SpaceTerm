# Preserve the ENV chosen by the user's login profile for this shell and its children.
if [ "${SPACETERM_SH_ENV+set}" = set ]; then
    export ENV="$SPACETERM_SH_ENV"
    unset SPACETERM_SH_ENV
    [ ! -r "$ENV" ] || . "$ENV"
else
    unset ENV
fi

case $- in
    *i*)
        # Encode protocol fields bytewise so delimiters and UTF-8 survive OSC parsing.
        _spaceterm_encode() (
            LC_ALL=C
            export LC_ALL
            command printf '%s' "$1" | command od -An -v -tx1 | command awk '
                function byte_value(hex, digits) {
                    digits = "0123456789abcdef"
                    hex = tolower(hex)
                    return (index(digits, substr(hex, 1, 1)) - 1) * 16 \
                        + index(digits, substr(hex, 2, 1)) - 1
                }
                {
                    for (index_ = 1; index_ <= NF; index_++) {
                        byte = byte_value($index_)
                        if ((byte >= 48 && byte <= 57) \
                                || (byte >= 65 && byte <= 90) \
                                || (byte >= 97 && byte <= 122) \
                                || byte == 45 || byte == 46 || byte == 47 \
                                || byte == 95 || byte == 126) {
                            printf "%c", byte
                        } else {
                            printf "%%%s", toupper($index_)
                        }
                    }
                }
            '
        )
        _spaceterm_report_directory() {
            printf '\033]7;file://localhost%s\007' "$(_spaceterm_encode "$PWD")" > /dev/tty
        }
        # Write directly to the PTY so the invisible report adds no prompt width.
        PS1='$(_spaceterm_report_directory)'${PS1-'$ '}
        ;;
esac
