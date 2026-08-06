if [[ -n "${SPACETERM_ZSH_ZDOTDIR+set}" ]]; then
    builtin export ZDOTDIR="$SPACETERM_ZSH_ZDOTDIR"
    builtin unset SPACETERM_ZSH_ZDOTDIR
else
    builtin unset ZDOTDIR
fi

typeset _spaceterm_user_zshenv="${ZDOTDIR:-$HOME}/.zshenv"
[[ ! -r "$_spaceterm_user_zshenv" ]] || builtin source -- "$_spaceterm_user_zshenv"
if [[ -o interactive && "$SPACETERM_SHELL_INTEGRATION_VERSION" == 1 ]]; then
    typeset _spaceterm_integration="${${(%):-%x}:A:h}/spaceterm-integration"
    [[ ! -r "$_spaceterm_integration" ]] || builtin source -- "$_spaceterm_integration"
fi
builtin unset _spaceterm_user_zshenv _spaceterm_integration
