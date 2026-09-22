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
            _spaceterm_value=$1
            while [ -n "$_spaceterm_value" ]; do
                _spaceterm_rest=${_spaceterm_value#?}
                _spaceterm_character=${_spaceterm_value%"$_spaceterm_rest"}
                case "$_spaceterm_character" in
                    [a-zA-Z0-9/._~-]) printf '%s' "$_spaceterm_character" ;;
                    *)
                        _spaceterm_code=$(printf '%d' "'$_spaceterm_character")
                        printf '%%%02X' "$((_spaceterm_code & 255))"
                        ;;
                esac
                _spaceterm_value=$_spaceterm_rest
            done
        )
        _spaceterm_report_directory() {
            printf '\033]7;file://localhost%s\007' "$(_spaceterm_encode "$PWD")" > /dev/tty
        }
        # Write directly to the PTY so the invisible report adds no prompt width.
        PS1='$(_spaceterm_report_directory)'${PS1-'$ '}
        ;;
esac
