# This profile belongs to one temporary remote login environment.
# Restore the account home before its profile, history, or logout files are used.
if [[ "$HISTFILE" == "$HOME/.bash_history" ]]; then
    HISTFILE="$SPACETERM_BASH_HOME/.bash_history"
fi
export HOME="$SPACETERM_BASH_HOME"
unset SPACETERM_BASH_HOME

if [[ -r "$HOME/.bash_profile" ]]; then
    source "$HOME/.bash_profile"
elif [[ -r "$HOME/.bash_login" ]]; then
    source "$HOME/.bash_login"
elif [[ -r "$HOME/.profile" ]]; then
    source "$HOME/.profile"
fi

source "$SPACETERM_BASH_INTEGRATION"
unset SPACETERM_BASH_INTEGRATION
