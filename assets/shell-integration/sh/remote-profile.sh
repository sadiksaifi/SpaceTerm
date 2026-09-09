# Restore the account before its login profile chooses an interactive ENV file.
case ${HISTFILE-} in
    "$HOME/.sh_history"|"$HOME/.bash_history")
        HISTFILE="$SPACETERM_SH_HOME/${HISTFILE##*/}"
        ;;
esac
export HOME="$SPACETERM_SH_HOME"
unset SPACETERM_SH_HOME
[ ! -r "$HOME/.profile" ] || . "$HOME/.profile"

if [ "${ENV+set}" = set ]; then
    export SPACETERM_SH_ENV="$ENV"
fi
export ENV="$SPACETERM_SH_INTEGRATION"
unset SPACETERM_SH_INTEGRATION
