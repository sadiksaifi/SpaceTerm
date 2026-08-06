if set -q SPACETERM_SHELL_INTEGRATION_XDG_DIR
    set --local --path spaceterm_xdg $XDG_DATA_DIRS
    set --erase spaceterm_xdg[(contains --index "$SPACETERM_SHELL_INTEGRATION_XDG_DIR" $spaceterm_xdg)]
    set --global --export --unpath XDG_DATA_DIRS $spaceterm_xdg
    set --erase SPACETERM_SHELL_INTEGRATION_XDG_DIR
end

if status --is-interactive; and test "$SPACETERM_SHELL_INTEGRATION_VERSION" = 1; and not set -q _SPACETERM_INTEGRATION_LOADED
    set --global _SPACETERM_INTEGRATION_LOADED 1
    function _spaceterm_prompt --on-event fish_prompt
        printf '\e]7;file://localhost%s\a\e]133;A\a' "$PWD"
    end
    function _spaceterm_preexec --on-event fish_preexec
        printf '\e]133;B\a\e]133;C;cmdline=%s\a' "$argv"
    end
    function _spaceterm_postexec --on-event fish_postexec
        printf '\e]133;D;%d\a' "$status"
    end
end
