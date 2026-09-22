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
                    printf -v _spaceterm_code '%d' "'$_spaceterm_character"
                    printf '%%%02X' "$((_spaceterm_code & 255))"
                    ;;
            esac
            _spaceterm_value=$_spaceterm_rest
        done
    )
    _spaceterm_prompt() {
        local status=$?
        if (( _spaceterm_command_active )); then
            printf '\e]133;D;%d\a' "$status"
        fi
        printf '\e]7;file://localhost%s\a\e]133;A\a' "$(_spaceterm_encode "$PWD")"
        _spaceterm_command_active=1
    }
    PROMPT_COMMAND="_spaceterm_prompt${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
    PS0='\[\e]133;C\a\]'
    PS1="${PS1}\[\e]133;B\a\]"
fi
