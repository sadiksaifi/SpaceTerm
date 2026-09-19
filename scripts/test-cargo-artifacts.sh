#!/bin/sh
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
guard=$script_dir/cargo-artifacts.sh
repo_dir=$(CDPATH='' cd -- "$script_dir/.." && pwd -P)
temp_root=$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-cargo-artifacts.XXXXXX")
trap 'rm -rf -- "$temp_root"' EXIT HUP INT TERM

prepare_owned_target() {
    target=$1
    mkdir -p "$target"
    printf '%s\n' "$repo_dir" > "$target/.spaceterm-cargo-target-owner"
    printf '{}\n' > "$target/.rustc_info.json"
}

make_oversized_target() {
    target=$1
    prepare_owned_target "$target"
    dd if=/dev/zero of="$target/artifact" bs=1048576 count=2 >/dev/null 2>&1
}

target=$temp_root/pre/target
make_oversized_target "$target"
CARGO_TARGET_DIR="$target" SPACETERM_CARGO_TARGET_BUDGET_MIB=1 \
    "$guard" run -- sh -c \
    "test ! -e \"\$CARGO_TARGET_DIR/artifact\" && test \"\$CARGO_INCREMENTAL\" = 0 && test -f \"\$CARGO_TARGET_DIR/.spaceterm-cargo-target-owner\""

target=$temp_root/post/target
prepare_owned_target "$target"
set +e
CARGO_TARGET_DIR="$target" SPACETERM_CARGO_TARGET_BUDGET_MIB=1 \
    "$guard" run -- sh -c \
    "dd if=/dev/zero of=\"\$CARGO_TARGET_DIR/artifact\" bs=1048576 count=2 >/dev/null 2>&1"
status=$?
set -e
test "$status" -eq 75
test ! -e "$target/artifact"
test -f "$target/.spaceterm-cargo-target-owner"

target=$temp_root/failure/target
prepare_owned_target "$target"
set +e
CARGO_TARGET_DIR="$target" SPACETERM_CARGO_TARGET_BUDGET_MIB=1 \
    "$guard" run -- sh -c 'exit 37'
status=$?
set -e
test "$status" -eq 37

measurement_bin=$temp_root/measurement-bin
measurement_sentinel=$temp_root/measurement-failed-once
real_du=$(command -v du)
mkdir -p "$measurement_bin"
cat > "$measurement_bin/du" <<'EOF'
#!/bin/sh
if [ "${FAIL_DU_ALWAYS:-0}" = 1 ]; then
    exit 1
fi
if [ "${FAIL_DU_AFTER_FIRST:-0}" = 1 ]; then
    if [ -e "$DU_FIRST_SUCCESS_SENTINEL" ]; then
        while [ ! -s "$ACTIVE_CHILD_PID_FILE" ]; do
            sleep 0.01
        done
        exit 1
    fi
    : > "$DU_FIRST_SUCCESS_SENTINEL"
    exec "$REAL_DU" "$@"
fi
if [ ! -e "$TRANSIENT_DU_SENTINEL" ]; then
    : > "$TRANSIENT_DU_SENTINEL"
    exit 1
fi
exec "$REAL_DU" "$@"
EOF
chmod +x "$measurement_bin/du"

target=$temp_root/transient-measurement/target
prepare_owned_target "$target"
PATH="$measurement_bin:$PATH" REAL_DU="$real_du" \
    TRANSIENT_DU_SENTINEL="$measurement_sentinel" \
    CARGO_TARGET_DIR="$target" SPACETERM_CARGO_TARGET_BUDGET_MIB=1 \
    "$guard" run -- sh -c 'exit 0'
test -e "$measurement_sentinel"

target=$temp_root/persistent-measurement-failure/target
prepare_owned_target "$target"
set +e
PATH="$measurement_bin:$PATH" REAL_DU="$real_du" FAIL_DU_ALWAYS=1 \
    CARGO_TARGET_DIR="$target" SPACETERM_CARGO_TARGET_BUDGET_MIB=1 \
    "$guard" run -- sh -c 'exit 0' >/dev/null 2>&1
status=$?
set -e
test "$status" -eq 2

target=$temp_root/active-measurement-failure/target
active_child_pid_file=$temp_root/active-measurement-failure-child
first_success_sentinel=$temp_root/measurement-succeeded-once
prepare_owned_target "$target"
set +e
# shellcheck disable=SC2016
PATH="$measurement_bin:$PATH" REAL_DU="$real_du" FAIL_DU_AFTER_FIRST=1 \
    DU_FIRST_SUCCESS_SENTINEL="$first_success_sentinel" \
    ACTIVE_CHILD_PID_FILE="$active_child_pid_file" \
    CARGO_TARGET_DIR="$target" SPACETERM_CARGO_TARGET_BUDGET_MIB=1 \
    "$guard" run -- sh -c 'printf "%s\n" "$$" > "$1"; sleep 30' \
    sh "$active_child_pid_file" >/dev/null 2>&1
status=$?
set -e
if [ "$status" -ne 2 ]; then
    echo "active measurement failure returned status $status instead of 2" >&2
    if [ -s "$active_child_pid_file" ]; then
        active_child_pid=$(cat "$active_child_pid_file")
        kill -TERM "-$active_child_pid" 2>/dev/null || true
    fi
    exit 1
fi
if [ ! -s "$active_child_pid_file" ]; then
    echo "guarded command did not publish its process ID" >&2
    exit 1
fi
active_child_pid=$(cat "$active_child_pid_file")
if kill -0 "$active_child_pid" 2>/dev/null; then
    kill -TERM "-$active_child_pid" 2>/dev/null || true
    echo "guarded command survived artifact measurement failure" >&2
    exit 1
fi

target=$temp_root/dash/target
prepare_owned_target "$target"
set +e
CARGO_TARGET_DIR="$target" SPACETERM_CARGO_TARGET_BUDGET_MIB=1 \
    dash "$guard" run -- sh -c 'exit 23'
status=$?
set -e
test "$status" -eq 23

target=$temp_root/startup-signal/target
started=$temp_root/startup-signal-started
prepare_owned_target "$target"
CARGO_TARGET_DIR="$target" SPACETERM_CARGO_TARGET_BUDGET_MIB=1 \
    "$guard" run -- sh -c \
    "printf '%s\\n' \\\$\\\$ > \"\$1\"; sleep 30" \
    sh "$started" &
supervisor_pid=$!
attempts=0
while [ ! -s "$started" ] && [ "$attempts" -lt 100 ]; do
    sleep 0.02
    attempts=$((attempts + 1))
done
test -s "$started"
guarded_pid=$(cat "$started")
kill -TERM "$supervisor_pid"
set +e
wait "$supervisor_pid"
status=$?
set -e
test "$status" -eq 143
if kill -0 "$guarded_pid" 2>/dev/null; then
    echo "guarded command survived supervisor termination" >&2
    exit 1
fi

target=$temp_root/child-signal/target
prepare_owned_target "$target"
set +e
CARGO_TARGET_DIR="$target" SPACETERM_CARGO_TARGET_BUDGET_MIB=1 \
    "$guard" run -- sh -c 'trap "exit 41" TERM; kill -TERM $$; sleep 30' &
supervisor_pid=$!
attempts=0
while kill -0 "$supervisor_pid" 2>/dev/null && [ "$attempts" -lt 100 ]; do
    sleep 0.02
    attempts=$((attempts + 1))
done
if kill -0 "$supervisor_pid" 2>/dev/null; then
    kill -TERM "$supervisor_pid"
    wait "$supervisor_pid"
    set -e
    echo "guarded command inherited a blocked TERM signal" >&2
    exit 1
fi
wait "$supervisor_pid"
status=$?
set -e
test "$status" -eq 41

target=$temp_root/live/target
continuation=$temp_root/live-continued
prepare_owned_target "$target"
set +e
CARGO_TARGET_DIR="$target" SPACETERM_CARGO_TARGET_BUDGET_MIB=1 \
    "$guard" run -- sh -c \
    "dd if=/dev/zero of=\"\$CARGO_TARGET_DIR/artifact\" bs=1048576 count=2 >/dev/null 2>&1; (sleep 4; : > \"\$1\") & wait" \
    sh "$continuation"
status=$?
set -e
test "$status" -eq 75
test ! -e "$continuation"
test ! -e "$target/artifact"

target=$temp_root/leader-exit/target
continuation=$temp_root/leader-exit-continued
prepare_owned_target "$target"
set +e
CARGO_TARGET_DIR="$target" SPACETERM_CARGO_TARGET_BUDGET_MIB=1 \
    "$guard" run -- sh -c \
    "(sleep 1; dd if=/dev/zero of=\"\$CARGO_TARGET_DIR/artifact\" bs=1048576 count=2 >/dev/null 2>&1; sleep 3; : > \"\$1\") & exit 0" \
    sh "$continuation"
status=$?
set -e
test "$status" -eq 75
test ! -e "$continuation"
test ! -e "$target/artifact"

target=$temp_root/termination-before-clean/target
prepare_owned_target "$target"
cat > "$temp_root/write-after-term.py" <<'PY'
import os
from pathlib import Path
import signal
import sys
import time

target = Path(os.environ["CARGO_TARGET_DIR"])


def finish_after_term(_signum, _frame):
    time.sleep(0.2)
    (target / "after-term").write_text("stopped", encoding="utf-8")
    raise SystemExit(0)


signal.signal(signal.SIGTERM, finish_after_term)
(target / "artifact").write_bytes(bytes(2 * 1024 * 1024))
while True:
    time.sleep(1)
PY
set +e
CARGO_TARGET_DIR="$target" SPACETERM_CARGO_TARGET_BUDGET_MIB=1 \
    "$guard" run -- python3 "$temp_root/write-after-term.py"
status=$?
set -e
test "$status" -eq 75
test ! -e "$target/after-term"
test ! -e "$target/artifact"

target=$temp_root/cleanup-failure/target
prepare_owned_target "$target"
mkdir -p "$temp_root/bin"
cat > "$temp_root/bin/cargo" <<'EOF'
#!/bin/sh
if [ "${1:-}" = metadata ]; then
    printf '{"target_directory":"%s"}\n' "$CARGO_TARGET_DIR"
    exit 0
fi
exit 55
EOF
chmod +x "$temp_root/bin/cargo"
set +e
PATH="$temp_root/bin:$PATH" \
    CARGO_TARGET_DIR="$target" SPACETERM_CARGO_TARGET_BUDGET_MIB=1 \
    "$guard" run -- sh -c \
    "dd if=/dev/zero of=\"\$CARGO_TARGET_DIR/artifact\" bs=1048576 count=2 >/dev/null 2>&1; exit 37" \
    >/dev/null 2>&1
status=$?
set -e
test "$status" -eq 75
test -e "$target/artifact"

target=$temp_root/nested/target
make_oversized_target "$target"
CARGO_TARGET_DIR="$target" SPACETERM_CARGO_TARGET_BUDGET_MIB=1 \
    SPACETERM_CARGO_ARTIFACT_GUARD_ACTIVE=1 \
    "$guard" run -- sh -c "test -e \"\$CARGO_TARGET_DIR/artifact\""

unsafe_target=$temp_root/unsafe
mkdir -p "$unsafe_target"
dd if=/dev/zero of="$unsafe_target/artifact" bs=1048576 count=2 >/dev/null 2>&1
set +e
CARGO_TARGET_DIR="$unsafe_target" SPACETERM_CARGO_TARGET_BUDGET_MIB=1 \
    "$guard" run -- sh -c 'exit 99' >/dev/null 2>&1
status=$?
set -e
test "$status" -eq 2
test -e "$unsafe_target/artifact"

foreign_target=$temp_root/foreign/target
mkdir -p "$foreign_target"
printf '%s\n' /another/repository > "$foreign_target/.spaceterm-cargo-target-owner"
printf 'must survive\n' > "$foreign_target/unrelated-artifact"
set +e
CARGO_TARGET_DIR="$foreign_target" SPACETERM_CARGO_TARGET_BUDGET_MIB=1 \
    "$guard" run -- sh -c 'exit 99' >/dev/null 2>&1
status=$?
set -e
test "$status" -eq 2
test -e "$foreign_target/unrelated-artifact"

configured_target=$temp_root/configured/target
configured_home=$temp_root/configured/cargo-home
cargo_bin_dir=$(rustc --print sysroot)/bin
mkdir -p "$configured_home" "$configured_target"
configured_target=$(CDPATH='' cd -- "$configured_target" && pwd -P)
cat > "$configured_home/config.toml" <<EOF
[build]
target-dir = "$configured_target"
EOF
# shellcheck disable=SC2016
PATH="$cargo_bin_dir:$PATH" CARGO_HOME="$configured_home" \
    SPACETERM_CARGO_TARGET_BUDGET_MIB=1 \
    "$guard" run -- sh -c \
    'test "$CARGO_TARGET_DIR" = "$1" && test -f "$1/.spaceterm-cargo-target-owner"' \
    sh "$configured_target"

fake_bin=$temp_root/configured-triple-bin
fake_executable=$temp_root/configured-triple-target/aarch-vendor-os/debug/spaceterm
artifact_path=$temp_root/configured-triple-artifact-path
mkdir -p "$fake_bin" "$(dirname -- "$fake_executable")"
: > "$fake_executable"
fake_executable=$(CDPATH='' cd -- "$(dirname -- "$fake_executable")" && pwd -P)/spaceterm
cat > "$fake_bin/cargo" <<'EOF'
#!/bin/sh
printf '{"reason":"compiler-artifact","target":{"name":"spaceterm"},"executable":"%s"}\n' \
    "$FAKE_EXECUTABLE"
EOF
chmod +x "$fake_bin/cargo"
PATH="$fake_bin:$PATH" FAKE_EXECUTABLE="$fake_executable" \
    python3 "$script_dir/cargo-build-executable.py" \
    --output "$artifact_path" --bin spaceterm -- --locked
test "$(cat "$artifact_path")" = "$fake_executable"

if grep -Eq 'cargo run (--features appearance-exerciser )?--locked|target/debug/spaceterm' \
    "$script_dir/../.mise.toml" "$script_dir/run-development-app-macos.sh"; then
    echo "interactive development tasks must build before launching" >&2
    exit 1
fi

echo "cargo artifact guard tests passed"
