# macOS's /bin/bash 3.2 reads this file as its login profile, because SpaceTerm points HOME at
# this directory. It runs no startup file through ENV, so this is the only file that can run
# after the login profile. It keeps to Bash 3.2 syntax.

# Restore the person's HOME first, and the history file Bash named from this directory.
_spaceterm_home=$HOME
if [ "${SPACETERM_BASH_HOME+set}" = set ]; then
    HOME=$SPACETERM_BASH_HOME
    export HOME
    unset SPACETERM_BASH_HOME
fi
[ "${HISTFILE-}" != "$_spaceterm_home/.bash_history" ] || HISTFILE=$HOME/.bash_history
unset _spaceterm_home

# The login profile Bash itself would read: the first of these that is readable.
if [ -r "$HOME/.bash_profile" ]; then
    . "$HOME/.bash_profile"
elif [ -r "$HOME/.bash_login" ]; then
    . "$HOME/.bash_login"
elif [ -r "$HOME/.profile" ]; then
    . "$HOME/.profile"
fi

# The login profile already set PATH, so the server's own CLI goes first now. Every other
# entry keeps its place, empty entries included.
if [ -n "${SPACETERM_CLI:-}" ] && [ "${PATH+set}" = set ]; then
    _spaceterm_cli_dir=${SPACETERM_CLI%/*}
    _spaceterm_path=$PATH:
    _spaceterm_rest=
    while [ -n "$_spaceterm_path" ]; do
        _spaceterm_entry=${_spaceterm_path%%:*}
        _spaceterm_path=${_spaceterm_path#*:}
        [ "$_spaceterm_entry" = "$_spaceterm_cli_dir" ] || _spaceterm_rest=$_spaceterm_rest:$_spaceterm_entry
    done
    PATH=$_spaceterm_cli_dir$_spaceterm_rest
    export PATH
    unset _spaceterm_cli_dir _spaceterm_path _spaceterm_rest _spaceterm_entry
fi
