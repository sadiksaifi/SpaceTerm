#!/bin/bash

set -euo pipefail
IFS=$'\n\t'

readonly FIXTURE_VERSION="1"
readonly CLEAN_PATH="/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"
readonly PROGRAM_IDS=(
    bash
    zsh
    vim
    neovim
    tmux
    less
    fzf
    btop
    yazi
    claude-code
    pi-coding-agent
)

usage() {
    cat <<'EOF'
Usage:
  native-programs.sh list
  native-programs.sh prepare NEW_FIXTURE_ROOT
  native-programs.sh command PROGRAM FIXTURE_ROOT
  native-programs.sh run PROGRAM FIXTURE_ROOT
  native-programs.sh command-authenticated claude-code FIXTURE_ROOT anthropic ANTHROPIC_API_KEY
  native-programs.sh command-authenticated pi-coding-agent FIXTURE_ROOT PROVIDER CREDENTIAL_ENV
  native-programs.sh run-authenticated PROGRAM FIXTURE_ROOT PROVIDER CREDENTIAL_ENV
  native-programs.sh app-env bash|zsh FIXTURE_ROOT

`prepare` creates credential-free, run-owned configuration and content fixtures.
Every command starts with `env -i` and an explicit, printed environment allowlist.
`command` prints cwd<TAB>PATH and invocation<TAB>SHELL_COMMAND records; `run`
executes the same command from that clean Workspace. Unauthenticated agent commands
block startup network (pi's offline mode; Claude's bare mode plus closed proxies).
Authenticated commands inject exactly one named ambient credential through a
validated provider mapping without printing its value. Never attach credentials.
`app-env` prints reset/key records for a packaged-app launcher.
EOF
}

die() {
    echo "native-programs.sh: $*" >&2
    exit 1
}

validate_new_root() {
    local root="$1"
    [[ "$root" == /* ]] || die "fixture root must be absolute"
    [[ "$root" != "/" ]] || die "fixture root cannot be /"
    [[ "$root" != *$'\n'* && "$root" != *$'\t'* ]] || die "fixture root cannot contain tabs or newlines"
    [[ ! -e "$root" ]] || die "fixture root already exists: $root"
}

validate_prepared_root() {
    local root="$1"
    [[ "$root" == /* && "$root" != "/" ]] || die "fixture root must be an absolute non-root path"
    [[ -f "$root/.spaceterm-native-fixtures" ]] || die "fixture root was not prepared by this script"
    [[ "$(tr -d '[:space:]' < "$root/.spaceterm-native-fixtures")" == "$FIXTURE_VERSION" ]] \
        || die "unsupported fixture version"
}

prepare_root() {
    local root="$1"
    validate_new_root "$root"
    umask 077
    mkdir -p -- \
        "$root/bin" \
        "$root/cache" \
        "$root/config/btop" \
        "$root/config/yazi" \
        "$root/config/zsh" \
        "$root/data" \
        "$root/home/bash" \
        "$root/home/btop" \
        "$root/home/claude" \
        "$root/home/fzf" \
        "$root/home/less" \
        "$root/home/neovim" \
        "$root/home/pi" \
        "$root/home/tmux" \
        "$root/home/vim" \
        "$root/home/yazi" \
        "$root/home/zsh" \
        "$root/state/claude" \
        "$root/state/neovim" \
        "$root/state/pi/sessions" \
        "$root/tmp/tmux" \
        "$root/workspace/yazi/subdirectory"

    printf '%s\n' "$FIXTURE_VERSION" > "$root/.spaceterm-native-fixtures"
    printf '%s\n' \
        "export PS1='SPACETERM-BASH> '" \
        "unset HISTFILE" \
        "set +o history" \
        > "$root/home/bash/.bash_profile"
    printf '%s\n' \
        "unset HISTFILE" \
        "export SAVEHIST=0" \
        > "$root/config/zsh/.zshenv"
    printf '%s\n' "PROMPT='SPACETERM-ZSH> '" > "$root/config/zsh/.zshrc"
    : > "$root/config/zsh/.zprofile"
    : > "$root/config/zsh/.zlogin"

    printf '%s\n' \
        'set -g mouse on' \
        'set -g history-limit 10000' \
        'set -g prefix C-b' \
        'set -g status on' \
        > "$root/config/tmux.conf"

    printf '%s\n' \
        'color_theme = "Default"' \
        'theme_background = true' \
        'truecolor = true' \
        'disable_mouse = false' \
        'terminal_sync = true' \
        'graph_symbol = "braille"' \
        'shown_boxes = "cpu mem net proc"' \
        'update_ms = 500' \
        'vim_keys = true' \
        > "$root/config/btop/btop.conf"

    printf '%s\n' \
        '[mgr]' \
        'ratio = [ 1, 4, 0 ]' \
        '' \
        '[plugin]' \
        'preloaders = []' \
        'previewers = []' \
        > "$root/config/yazi/yazi.toml"

    printf '%s\n' \
        'SpaceTerm editor acceptance fixture' \
        'ordinary ASCII' \
        'Unicode: 你好 é 😀 👨‍👩‍👧‍👦' \
        'Drawing: ┌─┬─┐ │█│░│ └─┴─┘ ⣿' \
        > "$root/workspace/editor.txt"
    printf '%s\n' \
        'alpha' \
        'álgebra' \
        'emoji 😀' \
        '你好 world' \
        'omega' \
        > "$root/workspace/fzf.txt"
    printf '%s\n' 'Quick Look acceptance target' > "$root/workspace/local-link.txt"
    printf '%s\n' 'Yazi open/return target' > "$root/workspace/yazi/subdirectory/inside.txt"
    printf '%s\n' 'Yazi selection target' > "$root/workspace/yazi/selected.txt"

    local line
    for (( line = 1; line <= 320; line += 1 )); do
        printf 'pager-%03d Unicode 你好 é 😀 https://example.com/spaceterm-less\n' "$line"
    done > "$root/workspace/pager.txt"
}

program_is_known() {
    local requested="$1"
    local program_id
    for program_id in "${PROGRAM_IDS[@]}"; do
        [[ "$program_id" == "$requested" ]] && return 0
    done
    return 1
}

resolve_executable() {
    local executable
    executable="$(command -v -- "$1" 2>/dev/null)" || die "required executable not found: $1"
    [[ "$executable" == /* && -x "$executable" ]] \
        || die "executable must resolve to an absolute executable path: $1"
    printf '%s' "$executable"
}

configure_authentication() {
    local program="$1"
    local provider="$2"
    local credential="$3"
    case "$program/$provider/$credential" in
        claude-code/anthropic/ANTHROPIC_API_KEY|\
        pi-coding-agent/anthropic/ANTHROPIC_API_KEY|\
        pi-coding-agent/openai/OPENAI_API_KEY|\
        pi-coding-agent/google/GEMINI_API_KEY)
            ;;
        *)
            die "unsupported authenticated provider/credential mapping: $program/$provider/$credential"
            ;;
    esac
    authenticated=true
    agent_provider="$provider"
    credential_name="$credential"
}

build_clean_environment() {
    local home="$1"
    local shell_path="$2"
    local fixture_root="$3"
    clean_environment=(
        /usr/bin/env -i
        "HOME=$home"
        USER=spaceterm-acceptance
        LOGNAME=spaceterm-acceptance
        "PATH=$CLEAN_PATH"
        LANG=en_US.UTF-8
        LC_CTYPE=en_US.UTF-8
        "SHELL=$shell_path"
        "TERM=${TERM:-xterm-ghostty}"
        "COLORTERM=${COLORTERM:-truecolor}"
        "TERM_PROGRAM=${TERM_PROGRAM:-SpaceTerm}"
        "TERM_PROGRAM_VERSION=${TERM_PROGRAM_VERSION:-unknown}"
        SPACETERM=1
        "TMPDIR=$fixture_root/tmp/"
    )
}

build_command() {
    local program="$1"
    local root="$2"
    local common_home="$root/home/$program"
    local executable
    build_clean_environment "$common_home" /bin/zsh "$root"
    case "$program" in
        bash)
            build_clean_environment "$root/home/bash" /bin/bash "$root"
            command=("${clean_environment[@]}" HISTFILE=/dev/null /bin/bash --noprofile --norc -i)
            ;;
        zsh)
            build_clean_environment "$root/home/zsh" /bin/zsh "$root"
            command=("${clean_environment[@]}" ZDOTDIR="$root/config/zsh" HISTFILE=/dev/null /bin/zsh -di)
            ;;
        vim)
            command=("${clean_environment[@]}" /usr/bin/vim --clean -n -i NONE -c 'set mouse=a' "$root/workspace/editor.txt")
            ;;
        neovim)
            executable="$(resolve_executable nvim)"
            command=("${clean_environment[@]}" XDG_CONFIG_HOME="$root/config/neovim" XDG_DATA_HOME="$root/data/neovim" XDG_STATE_HOME="$root/state/neovim" XDG_CACHE_HOME="$root/cache/neovim" "$executable" --clean -n -i NONE --cmd 'set mouse=a' "$root/workspace/editor.txt")
            ;;
        tmux)
            executable="$(resolve_executable tmux)"
            command=("${clean_environment[@]}" TMUX_TMPDIR="$root/tmp/tmux" "$executable" -S "$root/tmp/tmux/acceptance.sock" -f "$root/config/tmux.conf" new-session -s spaceterm-acceptance)
            ;;
        less)
            executable="$(resolve_executable less)"
            command=("${clean_environment[@]}" LESS= LESSHISTFILE=- LESSSECURE=1 "$executable" -R "$root/workspace/pager.txt")
            ;;
        fzf)
            executable="$(resolve_executable fzf)"
            command=("${clean_environment[@]}" FZF_DEFAULT_COMMAND= FZF_DEFAULT_OPTS= "$executable" --ansi --cycle --mouse)
            command_stdin="$root/workspace/fzf.txt"
            ;;
        btop)
            executable="$(resolve_executable btop)"
            command=("${clean_environment[@]}" "$executable" --config "$root/config/btop/btop.conf" --update 500)
            ;;
        yazi)
            executable="$(resolve_executable yazi)"
            command=("${clean_environment[@]}" YAZI_CONFIG_HOME="$root/config/yazi" XDG_CONFIG_HOME="$root/config/yazi" XDG_CACHE_HOME="$root/cache/yazi" XDG_DATA_HOME="$root/data/yazi" XDG_STATE_HOME="$root/state/yazi" "$executable" "$root/workspace/yazi")
            ;;
        claude-code)
            build_clean_environment "$root/home/claude" /bin/zsh "$root"
            executable="$(resolve_executable claude)"
            if [[ "$authenticated" == true ]]; then
                command=("${clean_environment[@]}" CLAUDE_CONFIG_DIR="$root/state/claude" "$credential_name=$credential_value" "$executable" --bare --safe-mode --no-chrome --disable-slash-commands --permission-mode plan)
            else
                command=("${clean_environment[@]}" CLAUDE_CONFIG_DIR="$root/state/claude" CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 "HTTP_PROXY=http://127.0.0.1:9" "HTTPS_PROXY=http://127.0.0.1:9" "ALL_PROXY=http://127.0.0.1:9" "NO_PROXY=localhost,127.0.0.1" "$executable" --bare --safe-mode --no-chrome --disable-slash-commands --permission-mode plan)
            fi
            ;;
        pi-coding-agent)
            build_clean_environment "$root/home/pi" /bin/zsh "$root"
            executable="$(resolve_executable pi)"
            local node
            node="$(resolve_executable node)"
            if [[ "$authenticated" == true ]]; then
                command=("${clean_environment[@]}" PI_CODING_AGENT_DIR="$root/state/pi" "$credential_name=$credential_value" "$node" "$executable" --provider "$agent_provider" --session-dir "$root/state/pi/sessions" --no-session --no-tools --no-extensions --no-skills --no-prompt-templates --no-context-files --no-approve --tui-mode regular)
            else
                command=("${clean_environment[@]}" PI_CODING_AGENT_DIR="$root/state/pi" PI_OFFLINE=1 "$node" "$executable" --offline --session-dir "$root/state/pi/sessions" --no-session --no-tools --no-extensions --no-skills --no-prompt-templates --no-context-files --no-approve --tui-mode regular)
            fi
            ;;
        *)
            die "unknown program: $program"
            ;;
    esac
}

print_command() {
    local argument credential_assignment
    printf 'cwd\t%s\n' "$root/workspace"
    if [[ "$authenticated" == true ]]; then
        printf 'provider\t%s\n' "$agent_provider"
        printf 'credential_env\t%s\n' "$credential_name"
    fi
    printf 'invocation\tcd '
    printf '%q' "$root/workspace"
    printf ' && exec '
    credential_assignment="${credential_name:-}=${credential_value:-}"
    for argument in "${command[@]}"; do
        if [[ "$authenticated" == true && "$argument" == "$credential_assignment" ]]; then
            # The generated shell expression expands only when the recorded
            # invocation is run; its value must never enter this output.
            # shellcheck disable=SC2016
            printf '%s="${%s:?set %s for this authenticated run}" ' \
                "$credential_name" "$credential_name" "$credential_name"
        else
            printf '%q ' "$argument"
        fi
    done
    if [[ -n "${command_stdin:-}" ]]; then
        printf '< %q' "$command_stdin"
    fi
    printf '\n'
}

run_program() {
    local program="$1"
    local root="$2"
    cd -- "$root/workspace"
    if [[ -n "${command_stdin:-}" ]]; then
        exec "${command[@]}" < "$command_stdin"
    else
        exec "${command[@]}"
    fi
}

emit_app_environment() {
    local shell_name="$1"
    local root="$2"
    local shell_path home
    case "$shell_name" in
        bash)
            shell_path=/bin/bash
            home="$root/home/bash"
            ;;
        zsh)
            shell_path=/bin/zsh
            home="$root/home/zsh"
            ;;
        *)
            die "app-env supports only bash or zsh"
            ;;
    esac
    printf 'reset\t/usr/bin/env -i\n'
    printf 'HOME\t%s\n' "$home"
    printf 'USER\tspaceterm-acceptance\n'
    printf 'LOGNAME\tspaceterm-acceptance\n'
    printf 'PATH\t%s\n' "$CLEAN_PATH"
    printf 'LANG\ten_US.UTF-8\n'
    printf 'LC_CTYPE\ten_US.UTF-8\n'
    printf 'SHELL\t%s\n' "$shell_path"
    printf 'TMPDIR\t%s/\n' "$root/tmp"
    printf 'XDG_CONFIG_HOME\t%s\n' "$root/config"
    printf 'XDG_CACHE_HOME\t%s\n' "$root/cache"
    printf 'XDG_DATA_HOME\t%s\n' "$root/data"
    printf 'XDG_STATE_HOME\t%s\n' "$root/state"
    printf 'SPACETERM_SHELL_INTEGRATION\t1\n'
    if [[ "$shell_name" == zsh ]]; then
        printf 'ZDOTDIR\t%s\n' "$root/config/zsh"
    fi
}

(( $# > 0 )) || {
    usage >&2
    exit 2
}

case "$1" in
    list)
        (( $# == 1 )) || die "list takes no arguments"
        printf '%s\n' "${PROGRAM_IDS[@]}"
        ;;
    prepare)
        (( $# == 2 )) || die "prepare requires one new fixture root"
        prepare_root "$2"
        ;;
    command|run)
        (( $# == 3 )) || die "$1 requires PROGRAM and FIXTURE_ROOT"
        operation="$1"
        requested_program="$2"
        root="$3"
        program_is_known "$requested_program" || die "unknown program: $requested_program"
        validate_prepared_root "$root"
        command=()
        command_stdin=""
        authenticated=false
        agent_provider=""
        credential_name=""
        credential_value=""
        build_command "$requested_program" "$root"
        if [[ "$operation" == command ]]; then
            print_command
        else
            run_program "$requested_program" "$root"
        fi
        ;;
    command-authenticated|run-authenticated)
        (( $# == 5 )) || die "$1 requires PROGRAM, FIXTURE_ROOT, PROVIDER, and CREDENTIAL_ENV"
        operation="$1"
        requested_program="$2"
        root="$3"
        program_is_known "$requested_program" || die "unknown program: $requested_program"
        validate_prepared_root "$root"
        command=()
        command_stdin=""
        authenticated=false
        agent_provider=""
        credential_name=""
        credential_value=""
        configure_authentication "$requested_program" "$4" "$5"
        if [[ "$operation" == run-authenticated ]]; then
            [[ -n "${!credential_name+x}" && -n "${!credential_name}" ]] \
                || die "authenticated run requires nonempty ambient $credential_name"
            credential_value="${!credential_name}"
        else
            credential_value="__SPACETERM_REDACTED_CREDENTIAL__"
        fi
        build_command "$requested_program" "$root"
        if [[ "$operation" == command-authenticated ]]; then
            print_command
        else
            run_program "$requested_program" "$root"
        fi
        ;;
    app-env)
        (( $# == 3 )) || die "app-env requires bash|zsh and FIXTURE_ROOT"
        validate_prepared_root "$3"
        emit_app_environment "$2" "$3"
        ;;
    -h|--help)
        usage
        ;;
    *)
        usage >&2
        die "unknown command: $1"
        ;;
esac
