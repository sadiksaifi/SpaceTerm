#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIR
readonly DRIVER="$SCRIPT_DIR/native-ax-probe.sh"
readonly SOURCE="$SCRIPT_DIR/native-ax-probe.m"
TEST_ROOT_RAW="$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-native-ax-probe-test.XXXXXX")"
TEST_ROOT="$(cd -- "$TEST_ROOT_RAW" && pwd -P)"
readonly TEST_ROOT
trap 'rm -rf -- "$TEST_ROOT"' EXIT
chmod 0700 "$TEST_ROOT"

fail() {
    echo "native AX probe test: $*" >&2
    exit 1
}

grep -Fq 'The probe never launches or discovers an application.' < <("$DRIVER" --help)

binary="$($DRIVER compile "$TEST_ROOT")"
readonly binary
[[ "$binary" == "$TEST_ROOT/identity/native-ax-probe" && -x "$binary" ]] \
    || fail "compile did not create the fixed run-owned binary"
[[ "$(stat -f '%Lp' "$binary")" == "700" ]] || fail "binary is not owner-private"
codesign --verify --strict "$binary"

concurrent_root="$TEST_ROOT/concurrent"
mkdir -m 0700 "$concurrent_root" "$concurrent_root/identity"
set +e
"$DRIVER" compile "$concurrent_root" >"$TEST_ROOT/concurrent-1.out" \
    2>"$TEST_ROOT/concurrent-1.err" &
concurrent_pid_1=$!
"$DRIVER" compile "$concurrent_root" >"$TEST_ROOT/concurrent-2.out" \
    2>"$TEST_ROOT/concurrent-2.err" &
concurrent_pid_2=$!
wait "$concurrent_pid_1"
concurrent_status_1=$?
wait "$concurrent_pid_2"
concurrent_status_2=$?
set -e
[[ "$(( concurrent_status_1 + concurrent_status_2 ))" -ne 0 && \
    -x "$concurrent_root/identity/native-ax-probe" ]] \
    || fail "concurrent compiles did not publish exactly one binary"
[[ "$concurrent_status_1" == 0 || "$concurrent_status_2" == 0 ]] \
    || fail "both concurrent compiles failed"
[[ "$concurrent_status_1" != 0 || "$concurrent_status_2" != 0 ]] \
    || fail "both concurrent compiles overwrote the same path"
codesign --verify --strict "$concurrent_root/identity/native-ax-probe"
[[ -z "$(find "$concurrent_root/identity" -name '.native-ax-probe.*' -print -quit)" ]] \
    || fail "concurrent compile left a private temporary binary"

if "$DRIVER" compile "$TEST_ROOT" >"$TEST_ROOT/recompile.out" 2>"$TEST_ROOT/recompile.err"; then
    fail "compile overwrote an existing binary"
fi
grep -Fq 'binary already exists' "$TEST_ROOT/recompile.err"

"$binary" --self-test >"$TEST_ROOT/self-test.out"
grep -Fxq 'native AX probe self-test: PASS' "$TEST_ROOT/self-test.out"

if "$binary" --run-dir "$TEST_ROOT" --identity "$TEST_ROOT/identity/ax-subject.tsv" \
        --output "$TEST_ROOT/mode-result.tsv" --expected-run-id fixture-run \
        --privacy metadata-only --expected-pane-count 1 --pane-order 0 \
        --probe-line 0 --probe-index 0 --probe-range 0:1 \
        >"$TEST_ROOT/mode.out" 2>"$TEST_ROOT/mode.err"; then
    fail "missing expected failure-action controller mode was accepted"
fi
grep -Fq 'run directory, identity, output, controller mode, pane count/order, line/index/range are required' \
    "$TEST_ROOT/mode.err"
if "$binary" --run-dir "$TEST_ROOT" --identity "$TEST_ROOT/identity/ax-subject.tsv" \
        --output "$TEST_ROOT/mode-invalid-result.tsv" --expected-run-id fixture-run \
        --expected-failure-action-enabled sometimes --privacy metadata-only \
        --expected-pane-count 1 --pane-order 0 --probe-line 0 --probe-index 0 \
        --probe-range 0:1 >"$TEST_ROOT/mode-invalid.out" 2>"$TEST_ROOT/mode-invalid.err"; then
    fail "non-boolean expected failure-action controller mode was accepted"
fi
grep -Fq 'expected failure-action enabled must be true or false' \
    "$TEST_ROOT/mode-invalid.err"

fixture_bundle="$TEST_ROOT/Fixture/SpaceTerm.app"
mkdir -p "$fixture_bundle/Contents/MacOS" "$fixture_bundle/Contents/Resources"
printf '#!/bin/sh\nexit 0\n' >"$fixture_bundle/Contents/MacOS/SpaceTerm"
chmod 0755 "$fixture_bundle/Contents/MacOS/SpaceTerm"
printf 'first resource\n' >"$fixture_bundle/Contents/Resources/fixture.txt"
bundle_hash() {
    SPACETERM_ACCEPTANCE_IDENTITY_LIBRARY_ONLY=1 bash -c \
        'source "$1"; bundle_tree_sha256 "$2"' -- \
        "$SCRIPT_DIR/../acceptance-identity.sh" "$1"
}
fixture_hash_before="$(bundle_hash "$fixture_bundle")"
"$binary" --self-test-bundle "$fixture_bundle" "$fixture_hash_before" \
    >"$TEST_ROOT/bundle-before.out"
grep -Fxq 'native AX probe bundle-tree self-test: PASS' "$TEST_ROOT/bundle-before.out"
printf 'changed resources with the same executable\n' \
    >"$fixture_bundle/Contents/Resources/fixture.txt"
fixture_hash_after="$(bundle_hash "$fixture_bundle")"
[[ "$fixture_hash_after" != "$fixture_hash_before" ]] \
    || fail "resource-only bundle change did not change the canonical app digest"
if "$binary" --self-test-bundle "$fixture_bundle" "$fixture_hash_before" \
        >"$TEST_ROOT/bundle-stale.out" 2>"$TEST_ROOT/bundle-stale.err"; then
    fail "same executable with different resources accepted a stale app digest"
fi
"$binary" --self-test-bundle "$fixture_bundle" "$fixture_hash_after" \
    >"$TEST_ROOT/bundle-after.out"

if "$binary" --run-dir "$TEST_ROOT" --identity "$TEST_ROOT/identity/ax-subject.tsv" \
        --output "$TEST_ROOT/result.tsv" --expected-run-id fixture-run \
        --expected-failure-action-enabled false --privacy metadata-only \
        --expected-pane-count 1 --pane-order 0 --probe-line 0 --probe-index 0 \
        --probe-range 0:1 >"$TEST_ROOT/missing.out" \
        2>"$TEST_ROOT/missing.err"; then
    fail "missing identity was accepted"
fi
grep -Fq 'identity file is not an owner-private regular file' "$TEST_ROOT/missing.err"
[[ ! -e "$TEST_ROOT/result.tsv" ]] || fail "identity failure created evidence"

: > "$TEST_ROOT/identity-target.tsv"
chmod 0600 "$TEST_ROOT/identity-target.tsv"
ln -s "$TEST_ROOT/identity-target.tsv" "$TEST_ROOT/identity-link.tsv"
mv "$TEST_ROOT/identity-link.tsv" "$TEST_ROOT/identity/ax-subject.tsv"
if "$binary" --run-dir "$TEST_ROOT" --identity "$TEST_ROOT/identity/ax-subject.tsv" \
        --output "$TEST_ROOT/symlink-result.tsv" --expected-run-id fixture-run \
        --expected-failure-action-enabled false \
        --privacy metadata-only \
        --expected-pane-count 1 --pane-order 0 --probe-line 0 --probe-index 0 \
        --probe-range 0:1 >"$TEST_ROOT/symlink.out" 2>"$TEST_ROOT/symlink.err"; then
    fail "symlinked subject identity was accepted"
fi
grep -Fq 'identity file is not an owner-private regular file' "$TEST_ROOT/symlink.err"
[[ ! -e "$TEST_ROOT/symlink-result.tsv" ]] || fail "symlink failure created evidence"
rm -f -- "$TEST_ROOT/identity/ax-subject.tsv"

if "$binary" --run-dir "$TEST_ROOT" --identity "$TEST_ROOT/identity-target.tsv" \
        --output "$TEST_ROOT/privacy-result.tsv" --expected-run-id fixture-run \
        --expected-failure-action-enabled false \
        --privacy fixture-sentinel \
        --expected-pane-count 1 --pane-order 0 --probe-line 0 --probe-index 0 \
        --probe-range 0:1 >"$TEST_ROOT/privacy.out" 2>"$TEST_ROOT/privacy.err"; then
    fail "fixture privacy was accepted without exact sentinel opt-in"
fi
grep -Fq 'fixture-sentinel privacy requires an exact fixture file and SHA-256 opt-in' \
    "$TEST_ROOT/privacy.err"
[[ ! -e "$TEST_ROOT/privacy-result.tsv" ]] || fail "privacy failure created evidence"

mkdir -m 0755 "$TEST_ROOT/public"
if "$DRIVER" compile "$TEST_ROOT/public" >"$TEST_ROOT/public.out" \
        2>"$TEST_ROOT/public.err"; then
    fail "group-readable run directory was accepted"
fi
grep -Fq 'must not grant group or other access' "$TEST_ROOT/public.err"

ln -s "$TEST_ROOT" "$TEST_ROOT-link"
if "$DRIVER" compile "$TEST_ROOT-link" >"$TEST_ROOT/link.out" \
        2>"$TEST_ROOT/link.err"; then
    fail "symlinked run directory was accepted"
fi
rm -f -- "$TEST_ROOT-link"

! grep -En '\b(openApplicationAtURL|launchApplication|NSWorkspaceOpenConfiguration)\b' "$SOURCE" \
    || fail "probe source contains an application-launch API"
! grep -En '\bNSDate\b' "$SOURCE" \
    || fail "probe source uses wall time for notification causality or deadlines"
grep -Fq 'mach_continuous_time()' "$SOURCE" \
    || fail "probe source does not use a sleep-aware monotonic clock"
! grep -Fq 'UINT32_MAX' "$SOURCE" \
    || fail "probe imposes a bundle file-size limit absent from the controller"
grep -Fq 'sha256_file_streaming' "$SOURCE" \
    || fail "probe does not stream canonical bundle file hashing"
! grep -En 'AXUIElementCopyAttributeValue\([^,]+,[[:space:]]*kAXValueAttribute' "$SOURCE" \
    || fail "probe bypasses the fixture-gated AXValue helper"

echo "native AX probe tests: PASS"
