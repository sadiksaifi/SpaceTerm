#!/bin/bash

set -euo pipefail
IFS=$'\n\t'

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIR
readonly PROBE="$SCRIPT_DIR/native-probe.sh"
readonly PAYLOADS="$SCRIPT_DIR/native-payloads.sh"
readonly PROGRAMS="$SCRIPT_DIR/native-programs.sh"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-native-probes-test.XXXXXX")"
readonly TEST_ROOT

cleanup() {
    local exit_status=$?
    rm -rf -- "$TEST_ROOT"
    return "$exit_status"
}
trap cleanup EXIT INT TERM

fail() {
    echo "test-native-probes.sh: $*" >&2
    exit 1
}

for script in "$PROBE" "$PAYLOADS" "$PROGRAMS" "$0"; do
    /bin/bash -n "$script"
done

readonly binary="$TEST_ROOT/native-probe"
"$PROBE" compile "$binary"
"$binary" --help | grep -Fq 'native-probe capture --log PATH'
if "$PROBE" compile "$binary" >/dev/null 2>&1; then
    fail "probe compilation replaced an existing binary"
fi

readonly fixture_root="$TEST_ROOT/fixtures"
"$PROGRAMS" prepare "$fixture_root"
[[ -f "$fixture_root/workspace/editor.txt" ]] || fail "editor fixture is missing"
[[ -f "$fixture_root/workspace/local-link.txt" ]] || fail "local-link fixture is missing"
grep -Fq 'ratio = [ 1, 4, 0 ]' "$fixture_root/config/yazi/yazi.toml"
grep -Fq 'preloaders = []' "$fixture_root/config/yazi/yazi.toml"
grep -Fq 'previewers = []' "$fixture_root/config/yazi/yazi.toml"
grep -Fq 'disable_mouse = false' "$fixture_root/config/btop/btop.conf"
grep -Fq 'set -g mouse on' "$fixture_root/config/tmux.conf"

readonly expected_programs="$TEST_ROOT/expected-programs"
readonly actual_programs="$TEST_ROOT/actual-programs"
printf '%s\n' \
    bash zsh vim neovim tmux less fzf btop yazi claude-code pi-coding-agent \
    > "$expected_programs"
"$PROGRAMS" list > "$actual_programs"
cmp "$expected_programs" "$actual_programs"

while IFS= read -r program; do
    command_record="$(
        LESS=ambient-less \
        TMUX=ambient-tmux \
        FZF_DEFAULT_COMMAND=ambient-fzf-command \
        FZF_DEFAULT_OPTS=ambient-fzf-options \
        ENV=ambient-shell-env \
        BASH_ENV=ambient-bash-env \
        ANTHROPIC_API_KEY=ambient-secret \
            "$PROGRAMS" command "$program" "$fixture_root"
    )"
    [[ "$(printf '%s\n' "$command_record" | wc -l | tr -d '[:space:]')" == 2 ]] \
        || fail "command record must contain exact cwd and invocation fields for $program"
    grep -Fxq $'cwd\t'"$fixture_root/workspace" <<< "$command_record"
    command_line="$(sed -n $'s/^invocation\\t//p' <<< "$command_record")"
    [[ "$command_line" == cd\ *' && exec '* ]] || fail "incomplete invocation for $program"
    /bin/bash -n -c "$command_line"
    [[ "$command_line" == *'/usr/bin/env -i '* ]] || fail "$program does not reset its environment"
    for ambient in ambient-less ambient-tmux ambient-fzf-command ambient-fzf-options \
        ambient-shell-env ambient-bash-env ambient-secret; do
        [[ "$command_line" != *"$ambient"* ]] || fail "$program inherited $ambient"
    done
    case "$program" in
        bash)
            [[ "$command_line" == *'--noprofile --norc -i'* ]] || fail "bash is not isolated"
            ;;
        zsh)
            [[ "$command_line" == *'ZDOTDIR='* && "$command_line" == *'/bin/zsh -di'* ]] \
                || fail "zsh is not isolated"
            ;;
        vim)
            [[ "$command_line" == *'--clean -n -i NONE'* ]] || fail "Vim is not isolated"
            ;;
        neovim)
            [[ "$command_line" == *'XDG_CONFIG_HOME='* && "$command_line" == *'--clean -n -i NONE'* ]] \
                || fail "Neovim is not isolated"
            ;;
        tmux)
            [[ "$command_line" == *' -S '* && "$command_line" == *' -f '* ]] \
                || fail "tmux is not isolated"
            ;;
        less)
            [[ "$command_line" == *'LESS='* && "$command_line" == *'LESSHISTFILE=-'* \
                && "$command_line" == *'LESSSECURE=1'* ]] \
                || fail "less is not isolated"
            ;;
        fzf)
            [[ "$command_line" == *'FZF_DEFAULT_OPTS='* && "$command_line" == *' < '* ]] \
                || fail "fzf is not deterministic"
            ;;
        btop)
            [[ "$command_line" == *' --config '* ]] || fail "btop lacks an explicit config"
            ;;
        yazi)
            [[ "$command_line" == *'YAZI_CONFIG_HOME='* ]] || fail "Yazi is not isolated"
            ;;
        claude-code)
            [[ "$command_line" == *'HTTP_PROXY=http://127.0.0.1:9'* \
                && "$command_line" == *' --bare --safe-mode '* \
                && "$command_line" == *' --permission-mode plan'* ]] \
                || fail "Claude Code is not in its clean safe mode"
            ;;
        pi-coding-agent)
            [[ "$command_line" == *'PI_OFFLINE=1'* && "$command_line" == *' --offline '* \
                && "$command_line" == *' --no-session --no-tools --no-extensions --no-skills '* \
                && "$command_line" == *' --no-context-files --no-approve '* ]] \
                || fail "pi-coding-agent is not isolated"
            ;;
    esac
done < "$actual_programs"

readonly fake_credential='must-not-appear-in-command-record'
authenticated_claude="$(
    ANTHROPIC_API_KEY="$fake_credential" \
        "$PROGRAMS" command-authenticated claude-code "$fixture_root" \
            anthropic ANTHROPIC_API_KEY
)"
[[ "$authenticated_claude" == *"ANTHROPIC_API_KEY=\"\${ANTHROPIC_API_KEY:"* ]] \
    || fail "authenticated Claude command lacks explicit credential injection"
[[ "$authenticated_claude" == *$'provider\tanthropic'* \
    && "$authenticated_claude" == *$'credential_env\tANTHROPIC_API_KEY'* ]] \
    || fail "authenticated Claude command lacks provider metadata"
[[ "$authenticated_claude" != *"$fake_credential"* ]] \
    || fail "authenticated Claude command disclosed a credential"
[[ "$authenticated_claude" != *'HTTP_PROXY='* ]] \
    || fail "authenticated Claude command retained the offline proxy"
authenticated_pi="$(
    OPENAI_API_KEY="$fake_credential" \
        "$PROGRAMS" command-authenticated pi-coding-agent "$fixture_root" \
            openai OPENAI_API_KEY
)"
[[ "$authenticated_pi" == *"OPENAI_API_KEY=\"\${OPENAI_API_KEY:"* \
    && "$authenticated_pi" == *' --provider openai '* ]] \
    || fail "authenticated pi command lacks explicit provider/credential injection"
[[ "$authenticated_pi" == *$'provider\topenai'* \
    && "$authenticated_pi" == *$'credential_env\tOPENAI_API_KEY'* ]] \
    || fail "authenticated pi command lacks provider metadata"
[[ "$authenticated_pi" != *"$fake_credential"* && "$authenticated_pi" != *' --offline '* ]] \
    || fail "authenticated pi command disclosed a credential or remained offline"
if "$PROGRAMS" command-authenticated bash "$fixture_root" anthropic ANTHROPIC_API_KEY \
    >/dev/null 2>&1; then
    fail "authentication was accepted for a non-agent program"
fi

readonly bash_environment="$TEST_ROOT/bash-environment"
readonly zsh_environment="$TEST_ROOT/zsh-environment"
"$PROGRAMS" app-env bash "$fixture_root" > "$bash_environment"
"$PROGRAMS" app-env zsh "$fixture_root" > "$zsh_environment"
grep -Fq $'SHELL\t/bin/bash' "$bash_environment"
grep -Fq $'SHELL\t/bin/zsh' "$zsh_environment"
grep -Fq $'ZDOTDIR\t' "$zsh_environment"
grep -Fxq $'reset\t/usr/bin/env -i' "$bash_environment"
grep -Fxq $'PATH\t/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin' \
    "$bash_environment"
if "$PROGRAMS" prepare "$fixture_root" >/dev/null 2>&1; then
    fail "prepare replaced an existing fixture root"
fi

readonly style_output="$TEST_ROOT/styles"
readonly unicode_output="$TEST_ROOT/unicode"
readonly link_output="$TEST_ROOT/links"
readonly scrollback_output="$TEST_ROOT/scrollback"
"$PAYLOADS" styles > "$style_output"
"$PAYLOADS" unicode > "$unicode_output"
readonly spaced_link="$fixture_root/workspace/local é link.txt"
printf '%s\n' 'percent-encoding target' > "$spaced_link"
"$PAYLOADS" links "$spaced_link" > "$link_output"
"$PAYLOADS" scrollback 37 > "$scrollback_output"
grep -Fq $'\033[39;49m' "$style_output"
grep -Fq $'\033[31;42m' "$style_output"
grep -Fq $'\033[38;5;196;48;5;25m' "$style_output"
grep -Fq $'\033[38;2;20;210;140;48;2;45;20;95m' "$style_output"
grep -Fq $'\033[4:3m' "$style_output"
grep -Fq '👨‍👩‍👧‍👦' "$unicode_output"
grep -Fq $'\033]8;;file://localhost' "$link_output"
grep -Fq '%20' "$link_output"
grep -Fq '%C3%A9' "$link_output"
[[ "$({ grep -Fo '%FFFFFFFF' "$link_output" || true; })" == "" ]] \
    || fail "UTF-8 path encoding sign-extended a byte"
grep -Fq $'\033]8;;javascript:alert(1)\033\\' "$link_output"
grep -Fq $'\033]8;;:// malformed target\033\\' "$link_output"
grep -Fq $'\033]8;;file://remote.invalid/tmp/missing\033\\' "$link_output"
grep -Fq '.spaceterm-missing' "$link_output"
[[ "$(wc -l < "$scrollback_output" | tr -d '[:space:]')" == 37 ]] \
    || fail "scrollback fixture has the wrong line count"

readonly geometry_output="$TEST_ROOT/geometry"
/usr/bin/script -q /dev/null "$binary" geometry > "$geometry_output"
grep -Eq 'geometry rows=[0-9]+ cols=[0-9]+ pixel_width=[0-9]+ pixel_height=[0-9]+' \
    "$geometry_output"

readonly event_log="$TEST_ROOT/events.log"
readonly capture_output="$TEST_ROOT/capture-output"
readonly capture_input="$TEST_ROOT/capture-input"
printf '\033[I\033[O\033[200~' > "$capture_input"
printf '\033[I\033[O\033[97;5:2u\033[<0;12;7M\033[<0;12;7m' >> "$capture_input"
printf '\033[201~\033[97;5:2u\033[<0;12;7M\033[<0;12;7m\035' >> "$capture_input"
{
    sleep 0.3
    /bin/cat "$capture_input"
} | /usr/bin/script -q /dev/null "$binary" capture --log "$event_log" \
        --focus --bracketed-paste --kitty-keyboard --mouse=any --timeout=3 \
        > "$capture_output"
grep -Fq $'\033[?1004h' "$capture_output"
grep -Fq $'\033[?2004h' "$capture_output"
grep -Fq $'\033[>11u' "$capture_output"
grep -Fq $'\033[?1003h' "$capture_output"
grep -Fq $'\033[?1006h' "$capture_output"
grep -Fq $'\033[?1004l' "$capture_output"
grep -Fq $'\033[?2004l' "$capture_output"
grep -Fq $'\033[<u' "$capture_output"
grep -Fq $'\033[?1003l' "$capture_output"
grep -Fq $'\033[?1006l' "$capture_output"
[[ "$({ grep -Fc 'marker name=focus-in' "$event_log" || true; })" == 1 ]] \
    || fail "pasted focus-in bytes were semantically annotated"
[[ "$({ grep -Fc 'marker name=focus-out' "$event_log" || true; })" == 1 ]] \
    || fail "pasted focus-out bytes were semantically annotated"
[[ "$({ grep -Fc 'marker name=kitty-keyboard' "$event_log" || true; })" == 1 ]] \
    || fail "pasted Kitty bytes were semantically annotated"
[[ "$({ grep -Fc 'marker name=mouse-press-or-motion' "$event_log" || true; })" == 1 ]] \
    || fail "pasted mouse press bytes were semantically annotated"
[[ "$({ grep -Fc 'marker name=mouse-release' "$event_log" || true; })" == 1 ]] \
    || fail "pasted mouse release bytes were semantically annotated"
readonly expected_markers="$TEST_ROOT/expected-markers"
readonly actual_markers="$TEST_ROOT/actual-markers"
printf '%s\n' focus-in focus-out bracketed-paste-begin bracketed-paste-end \
    kitty-keyboard mouse-press-or-motion mouse-release control-right-bracket-stop \
    > "$expected_markers"
sed -n 's/^marker name=\([^ ]*\).*/\1/p' "$event_log" > "$actual_markers"
cmp "$expected_markers" "$actual_markers"
[[ "$({ grep -c '^byte ' "$event_log" || true; })" == "$(wc -c < "$capture_input" | tr -d '[:space:]')" ]] \
    || fail "event log did not preserve every input byte"
grep -Fq 'timestamp_semantics=post-read-log-time' "$event_log"
grep -Fq 'capture-end ' "$event_log"
grep -Fq 'result=control-right-bracket-stop' "$event_log"

readonly termios_runner="$TEST_ROOT/termios-runner.sh"
# These single-quoted lines intentionally defer expansion to the generated runner.
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/bash' \
    'set -e' \
    'stty -g > "$BEFORE_TERMIOS"' \
    '"$PROBE_BINARY" capture --log "$PROBE_LOG" --focus --timeout=3' \
    'stty -g > "$AFTER_TERMIOS"' \
    'cmp "$BEFORE_TERMIOS" "$AFTER_TERMIOS"' \
    > "$termios_runner"
chmod +x "$termios_runner"
readonly focus_log="$TEST_ROOT/focus.log"
readonly focus_output="$TEST_ROOT/focus-output"
{
    sleep 0.3
    printf '\035'
} | PROBE_BINARY="$binary" \
    PROBE_LOG="$focus_log" \
    BEFORE_TERMIOS="$TEST_ROOT/termios-before" \
    AFTER_TERMIOS="$TEST_ROOT/termios-after" \
        /usr/bin/script -q /dev/null "$termios_runner" > "$focus_output"
grep -Fq $'\033[?1004h' "$focus_output"
grep -Fq $'\033[?1004l' "$focus_output"
for foreign_cleanup in $'\033[?2004l' $'\033[?1000l' $'\033[?1002l' \
    $'\033[?1003l' $'\033[?1006l' $'\033[?1016l' $'\033[<u'; do
    [[ "$(< "$focus_output")" != *"$foreign_cleanup"* ]] \
        || fail "focus-only capture cleaned a mode it did not own"
done

readonly signal_runner="$TEST_ROOT/signal-runner.sh"
# These single-quoted lines intentionally defer expansion to the generated runner.
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/bash' \
    'set +e' \
    'stty -g > "$BEFORE_TERMIOS"' \
    '"$PROBE_BINARY" capture --log "$PROBE_LOG" --focus --timeout=10' \
    'probe_status=$?' \
    'stty -g > "$AFTER_TERMIOS"' \
    'cmp "$BEFORE_TERMIOS" "$AFTER_TERMIOS" || exit 99' \
    'exit "$probe_status"' \
    > "$signal_runner"
chmod +x "$signal_runner"

assert_signal_exit() {
    local signal_name="$1"
    local expected_status="$2"
    local signal_key
    signal_key="$(tr '[:upper:]' '[:lower:]' <<< "$signal_name")"
    local log="$TEST_ROOT/$signal_key.log"
    local output="$TEST_ROOT/$signal_key.output"
    local stop_input="$TEST_ROOT/$signal_key.stop-input"
    local before_termios="$TEST_ROOT/$signal_key.termios-before"
    local after_termios="$TEST_ROOT/$signal_key.termios-after"
    while [[ ! -e "$stop_input" ]]; do
        printf 'signal-fixture'
        sleep 0.05
    done | \
    PROBE_BINARY="$binary" PROBE_LOG="$log" \
        BEFORE_TERMIOS="$before_termios" AFTER_TERMIOS="$after_termios" \
        /usr/bin/script -q /dev/null "$signal_runner" > "$output" 2>&1 &
    local script_pid=$!
    local attempt
    for (( attempt = 1; attempt <= 50; attempt += 1 )); do
        [[ -s "$log" ]] && break
        sleep 0.02
    done
    if [[ ! -s "$log" ]]; then
        sed -n '1,20p' "$output" >&2
        fail "$signal_name capture did not start"
    fi
    local probe_pid
    probe_pid="$(sed -n 's/.* pid=\([0-9][0-9]*\)$/\1/p' "$log" | head -1)"
    [[ -n "$probe_pid" ]] || fail "$signal_name capture did not record its pid"
    kill -s "$signal_name" "$probe_pid"
    : > "$stop_input"
    local signal_status
    set +e
    wait "$script_pid"
    signal_status=$?
    set -e
    [[ "$signal_status" == "$expected_status" ]] \
        || fail "$signal_name capture exited $signal_status instead of $expected_status"
    grep -Fq "termination signal=" "$log"
    grep -Fq "name=SIG$signal_name" "$log"
    grep -Fq 'capture-end ' "$log"
    grep -Fq 'result=signal' "$log"
    grep -Fq $'\033[?1004l' "$output"
    cmp "$before_termios" "$after_termios"
}

assert_signal_exit QUIT 131
assert_signal_exit TSTP 146

readonly header_failure_runner="$TEST_ROOT/header-failure-runner.sh"
# These single-quoted lines intentionally defer expansion to the generated runner.
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/bash' \
    'set +e' \
    'ulimit -f 0' \
    '"$PROBE_BINARY" capture --log "$PROBE_LOG" --focus --timeout=3' \
    'probe_status=$?' \
    'exit "$probe_status"' \
    > "$header_failure_runner"
chmod +x "$header_failure_runner"
readonly header_failure_output="$TEST_ROOT/header-failure-output"
set +e
PROBE_BINARY="$binary" \
    PROBE_LOG="$TEST_ROOT/header-limited.log" \
        /usr/bin/script -q /dev/null "$header_failure_runner" \
            > "$header_failure_output" 2>&1
header_failure_status=$?
set -e
[[ "$header_failure_status" == 1 ]] \
    || fail "event-log header failure exited $header_failure_status instead of 1"
grep -Fq 'native-probe: event log I/O failed:' "$header_failure_output"
[[ "$(< "$header_failure_output")" != *$'\033[?1004h'* ]] \
    || fail "capture entered terminal modes after its header write failed"

readonly log_failure_runner="$TEST_ROOT/log-failure-runner.sh"
# These single-quoted lines intentionally defer expansion to the generated runner.
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/bash' \
    'set +e' \
    'stty -g > "$BEFORE_TERMIOS"' \
    'ulimit -f 1' \
    '"$PROBE_BINARY" capture --log "$PROBE_LOG" --focus --timeout=3' \
    'probe_status=$?' \
    'stty -g > "$AFTER_TERMIOS"' \
    'cmp "$BEFORE_TERMIOS" "$AFTER_TERMIOS" || exit 99' \
    'exit "$probe_status"' \
    > "$log_failure_runner"
chmod +x "$log_failure_runner"
readonly log_failure_output="$TEST_ROOT/log-failure-output"
set +e
{
    sleep 0.3
    for (( chunk = 1; chunk <= 128; chunk += 1 )); do
        printf '%01024d' 0
        sleep 0.005
    done
} | PROBE_BINARY="$binary" \
    PROBE_LOG="$TEST_ROOT/limited.log" \
    BEFORE_TERMIOS="$TEST_ROOT/limited-termios-before" \
    AFTER_TERMIOS="$TEST_ROOT/limited-termios-after" \
        /usr/bin/script -q /dev/null "$log_failure_runner" > "$log_failure_output" 2>&1
log_failure_status=$?
set -e
[[ "$log_failure_status" == 1 ]] \
    || fail "event-log I/O failure exited $log_failure_status instead of 1"
grep -Fq 'native-probe: event log I/O failed:' "$log_failure_output"
grep -Fq $'\033[?1004l' "$log_failure_output"

echo "native acceptance probes: PASS"
