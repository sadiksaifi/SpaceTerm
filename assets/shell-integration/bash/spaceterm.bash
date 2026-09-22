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
        builtin printf '%s' "$1" | command od -An -v -tx1 | command awk '
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
