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
        _spaceterm_report_directory() {
            printf '\033]7;file://localhost%s\007' "$PWD" > /dev/tty
        }
        # Write directly to the PTY so the invisible report adds no prompt width.
        PS1='$(_spaceterm_report_directory)'${PS1-'$ '}
        ;;
esac
