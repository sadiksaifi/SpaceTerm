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

# Exercise measurement failures with an actual owned process group. The fixture
# always cleans up its own children, including when the supervisor is broken.
python3 - "$script_dir/cargo-artifact-supervisor.py" "$repo_dir" "$temp_root" <<'PY'
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

supervisor, repo, root = sys.argv[1:]
failures = []


def live(pid):
    result = subprocess.run(
        ["ps", "-p", str(pid), "-o", "stat="], capture_output=True, text=True
    )
    return result.returncode == 0 and any(
        line.strip() and not line.strip().startswith("Z")
        for line in result.stdout.splitlines()
    )


for mode in ("transient", "persistent"):
    fixture = Path(root) / f"measurement-{mode}"
    target = fixture / "target"
    target.mkdir(parents=True)
    (target / ".spaceterm-cargo-target-owner").write_text(repo + "\n")
    (target / "retained-artifact").write_text("must survive an unknown size")
    binary = fixture / "bin"
    binary.mkdir()
    du = binary / "du"
    du.write_text("""#!/usr/bin/env python3
import os
from pathlib import Path
import sys
root = Path(os.environ["MEASUREMENT_FIXTURE"])
if (root / "ready").exists():
    counter = root / "failures"
    count = int(counter.read_text()) if counter.exists() else 0
    if os.environ["MEASUREMENT_MODE"] == "persistent" or count < 2:
        counter.write_text(str(count + 1))
        print("999999 partial-result")
        sys.exit(1)
    (root / "recovered").touch()
print("0 complete-result")
""")
    du.chmod(0o755)
    command = fixture / "command.py"
    command.write_text("""import os
from pathlib import Path
import signal
import subprocess
import sys
import time
root = Path(os.environ["MEASUREMENT_FIXTURE"])
signal.signal(signal.SIGTERM, signal.SIG_IGN)
child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"])
(root / "pids").write_text(f"{os.getpid()} {child.pid}")
(root / "ready").touch()
while not (root / "recovered").exists():
    time.sleep(0.02)
child.kill()
child.wait()
sys.exit(37)
""")
    env = dict(os.environ, PATH=str(binary) + os.pathsep + os.environ["PATH"],
               MEASUREMENT_FIXTURE=str(fixture), MEASUREMENT_MODE=mode)
    errors = (fixture / "stderr").open("wb")
    process = subprocess.Popen(
        [sys.executable, supervisor, "--repo-dir", repo, "--target-dir", str(target),
         "--budget-kib", "1024", "--", sys.executable, str(command)],
        env=env, stdout=subprocess.DEVNULL, stderr=errors,
    )
    try:
        process.wait(timeout=15)
        stderr = (fixture / "stderr").read_bytes()
        expected = 37 if mode == "transient" else 2
        assert process.returncode == expected, f"{mode}: status {process.returncode}"
        assert (target / "retained-artifact").exists(), f"{mode}: cleaned unknown size"
        assert (fixture / "failures").exists(), f"{mode}: no measurement failure injected"
        if mode == "persistent":
            pids = [int(value) for value in (fixture / "pids").read_text().split()]
            assert not any(live(pid) for pid in pids), "persistent: owned process survived"
            assert b"could not measure Cargo artifact usage" in stderr
    except (AssertionError, subprocess.TimeoutExpired) as error:
        failures.append(str(error))
    finally:
        if (fixture / "pids").exists():
            pids = [int(value) for value in (fixture / "pids").read_text().split()]
            if any(live(pid) for pid in pids):
                try:
                    os.killpg(pids[0], signal.SIGKILL)
                except ProcessLookupError:
                    pass
        if process.poll() is None:
            process.kill()
        process.wait(timeout=5)
        errors.close()

if failures:
    raise SystemExit("; ".join(failures))
PY

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
