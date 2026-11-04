if [[ -n "${SPACETERM_ZSH_ZDOTDIR+set}" ]]; then
    builtin export ZDOTDIR="$SPACETERM_ZSH_ZDOTDIR"
    builtin unset SPACETERM_ZSH_ZDOTDIR
else
    builtin unset ZDOTDIR
fi

typeset _spaceterm_user_zshenv="${ZDOTDIR:-$HOME}/.zshenv"
[[ ! -r "$_spaceterm_user_zshenv" ]] || builtin source -- "$_spaceterm_user_zshenv"
if [[ -o interactive ]]; then
    # The startup files run after this one and may set PATH, so the server's own CLI goes first
    # at the first prompt, with or without the rest of the integration.
    _spaceterm_cli_path() {
        add-zsh-hook -d precmd _spaceterm_cli_path
        [[ -n "${SPACETERM_CLI:-}" ]] || return 0
        builtin local dir="${SPACETERM_CLI:h}"
        path=("$dir" "${(@)path:#${(b)dir}}")
    }
    autoload -Uz add-zsh-hook
    add-zsh-hook precmd _spaceterm_cli_path
fi
if [[ -o interactive && "$SPACETERM_SHELL_INTEGRATION_VERSION" == 1 ]]; then
    typeset _spaceterm_integration="${${(%):-%x}:A:h}/spaceterm-integration"
    [[ ! -r "$_spaceterm_integration" ]] || builtin source -- "$_spaceterm_integration"
fi
builtin unset _spaceterm_user_zshenv _spaceterm_integration
