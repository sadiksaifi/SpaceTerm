#!/bin/sh
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
guard=$script_dir/cargo-artifacts.sh
temp_root=$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-cargo-artifacts.XXXXXX")
trap 'rm -rf -- "$temp_root"' EXIT HUP INT TERM

make_oversized_target() {
    target=$1
    mkdir -p "$target"
    printf '{}\n' > "$target/.rustc_info.json"
    dd if=/dev/zero of="$target/artifact" bs=1048576 count=2 >/dev/null 2>&1
}

target=$temp_root/pre/target
make_oversized_target "$target"
CARGO_TARGET_DIR="$target" SPACETERM_CARGO_TARGET_BUDGET_MIB=1 \
    "$guard" run -- sh -c \
    "test ! -e \"\$CARGO_TARGET_DIR/artifact\" && test \"\$CARGO_INCREMENTAL\" = 0"

target=$temp_root/post/target
mkdir -p "$target"
printf '{}\n' > "$target/.rustc_info.json"
set +e
CARGO_TARGET_DIR="$target" SPACETERM_CARGO_TARGET_BUDGET_MIB=1 \
    "$guard" run -- sh -c \
    "dd if=/dev/zero of=\"\$CARGO_TARGET_DIR/artifact\" bs=1048576 count=2 >/dev/null 2>&1"
status=$?
set -e
test "$status" -eq 75
test ! -e "$target/artifact"

target=$temp_root/failure/target
mkdir -p "$target"
printf '{}\n' > "$target/.rustc_info.json"
set +e
CARGO_TARGET_DIR="$target" SPACETERM_CARGO_TARGET_BUDGET_MIB=1 \
    "$guard" run -- sh -c 'exit 37'
status=$?
set -e
test "$status" -eq 37

target=$temp_root/dash/target
mkdir -p "$target"
printf '{}\n' > "$target/.rustc_info.json"
set +e
CARGO_TARGET_DIR="$target" SPACETERM_CARGO_TARGET_BUDGET_MIB=1 \
    dash "$guard" run -- sh -c 'exit 23'
status=$?
set -e
test "$status" -eq 23

target=$temp_root/startup-signal/target
started=$temp_root/startup-signal-started
mkdir -p "$target"
printf '{}\n' > "$target/.rustc_info.json"
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
mkdir -p "$target"
printf '{}\n' > "$target/.rustc_info.json"
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
mkdir -p "$target"
printf '{}\n' > "$target/.rustc_info.json"
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
mkdir -p "$target"
printf '{}\n' > "$target/.rustc_info.json"
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
mkdir -p "$target"
printf '{}\n' > "$target/.rustc_info.json"
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
mkdir -p "$target" "$temp_root/bin"
printf '{}\n' > "$target/.rustc_info.json"
cat > "$temp_root/bin/cargo" <<'EOF'
#!/bin/sh
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

if grep -Eq 'cargo run (--features appearance-exerciser )?--locked' "$script_dir/../.mise.toml"; then
    echo "interactive development tasks must build before launching" >&2
    exit 1
fi

echo "cargo artifact guard tests passed"
