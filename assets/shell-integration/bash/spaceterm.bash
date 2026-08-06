if [[ -n "${SPACETERM_BASH_INJECT:-}" ]]; then
    unset SPACETERM_BASH_INJECT
    if [[ -n "${SPACETERM_BASH_ENV+set}" ]]; then
        export ENV="$SPACETERM_BASH_ENV"
        unset SPACETERM_BASH_ENV
    else
        unset ENV
    fi
    set +o posix
    [[ ! -r /etc/profile ]] || source /etc/profile
    if [[ -r "$HOME/.bash_profile" ]]; then
        source "$HOME/.bash_profile"
    elif [[ -r "$HOME/.bash_login" ]]; then
        source "$HOME/.bash_login"
    elif [[ -r "$HOME/.profile" ]]; then
        source "$HOME/.profile"
    fi
fi

if [[ $- == *i* && "$SPACETERM_SHELL_INTEGRATION_VERSION" == 1 && -z "${_SPACETERM_INTEGRATION_LOADED:-}" ]]; then
    _SPACETERM_INTEGRATION_LOADED=1
    _spaceterm_command_active=0
    _spaceterm_prompt() {
        local status=$?
        if (( _spaceterm_command_active )); then
            printf '\e]133;D;%d\a' "$status"
        fi
        printf '\e]7;file://localhost%s\a\e]133;A\a' "$PWD"
        _spaceterm_command_active=1
    }
    PROMPT_COMMAND="_spaceterm_prompt${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
    PS0='\[\e]133;C\a\]'
    PS1="${PS1}\[\e]133;B\a\]"
fi
