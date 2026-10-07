if set -q SPACETERM_SHELL_INTEGRATION_XDG_DIR
    set --local --path spaceterm_xdg $XDG_DATA_DIRS
    set --erase spaceterm_xdg[(contains --index "$SPACETERM_SHELL_INTEGRATION_XDG_DIR" $spaceterm_xdg)]
    set --global --export --unpath XDG_DATA_DIRS $spaceterm_xdg
    set --erase SPACETERM_SHELL_INTEGRATION_XDG_DIR
end

if status --is-interactive; and test "$SPACETERM_SHELL_INTEGRATION_VERSION" = 1; and not set -q _SPACETERM_INTEGRATION_LOADED
    set --global _SPACETERM_INTEGRATION_LOADED 1
    # The session's Prompt Owner marks this shell's own prompt markers; only hex is accepted.
    set --global _spaceterm_owner ''
    if string match --quiet --regex -- '^[0-9a-f]+$' "$SPACETERM_PROMPT_OWNER"
        set --global _spaceterm_owner ";spaceterm=$SPACETERM_PROMPT_OWNER"
    end
    function _spaceterm_prompt --on-event fish_prompt
        printf '\e]7;file://localhost%s\a\e]133;A;redraw=1%s\a' (string escape --style=url -- "$PWD" | string replace --all '%2F' '/') "$_spaceterm_owner"
    end
    function _spaceterm_preexec --on-event fish_preexec
        printf '\e]133;B\a\e]133;C;cmdline=%s%s\a' (string escape --style=url -- "$argv") "$_spaceterm_owner"
    end
    function _spaceterm_postexec --on-event fish_postexec
        printf '\e]133;D;%d%s\a' "$status" "$_spaceterm_owner"
    end
end
