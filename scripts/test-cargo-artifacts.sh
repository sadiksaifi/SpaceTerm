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

echo "cargo artifact guard tests passed"
