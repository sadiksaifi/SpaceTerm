#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
umask 077

SCRIPT_DIRECTORY="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
TOOL="$SCRIPT_DIRECTORY/performance-driver-receipt.py"
TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-driver-receipt.XXXXXX")"
cleanup() {
    local status=$?
    rm -rf -- "$TEMP_ROOT"
    exit "$status"
}
trap cleanup EXIT INT TERM

fail() { echo "test failure: $*" >&2; exit 1; }
sha256() { shasum -a 256 "$1" | awk '{ print $1 }'; }
expect_failure() {
    local label="$1"
    shift
    if "$@" >"$TEMP_ROOT/failure.stdout" 2>"$TEMP_ROOT/failure.stderr"; then
        fail "$label unexpectedly succeeded"
    fi
}

SECRET="$TEMP_ROOT/secret"
SUBJECT="$TEMP_ROOT/subject.tsv"
WINDOW="$TEMP_ROOT/window.tsv"
PLAN="$TEMP_ROOT/plan.tsv"
BINARY="$TEMP_ROOT/performance-driver"
SOURCE="$TEMP_ROOT/performance-driver.m"
CONTROLLER="$TEMP_ROOT/controller.sh"
EVENTS="$TEMP_ROOT/driver-events.tsv"
INTENT="$TEMP_ROOT/driver-intent.tsv"
RECEIPT="$TEMP_ROOT/driver-receipt.tsv"
NONCE="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
PLAN_START=1000000000

printf '%064d' 0 > "$SECRET"
chmod 0600 "$SECRET"
printf '#!/bin/sh\nexit 0\n' > "$BINARY"
printf 'driver source\n' > "$SOURCE"
printf '#!/bin/sh\nexit 0\n' > "$CONTROLLER"
chmod 0555 "$BINARY"
chmod 0755 "$CONTROLLER"
chmod 0444 "$SOURCE"
cat > "$SUBJECT" <<'EOF'
format_version	1
subject	spaceterm
app_bundle_path	/Applications/SpaceTerm.app
bundle_identifier	dev.spaceterm.SpaceTerm
bundle_version	1.0+1
executable_path	/Applications/SpaceTerm.app/Contents/MacOS/spaceterm
executable_sha256	bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
executable_device	1
executable_inode	2
executable_fsid	1
signature_valid	true
signing_identifier	dev.spaceterm.SpaceTerm
team_identifier	none
cdhash	abc123
process_pid	4242
process_start_identity	100:200
identity_status	frozen
EOF
chmod 0444 "$SUBJECT"
cat > "$WINDOW" <<EOF
format_version	1
subject_identity_sha256	$(sha256 "$SUBJECT")
subject	spaceterm
process_pid	4242
process_start_identity	100:200
bundle_identifier	dev.spaceterm.SpaceTerm
executable_sha256	bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
window_number	77
window_owner_pid_verified	true
window_layer	0
window_onscreen	true
window_minimized	false
window_x	0.000
window_y	0.000
window_width	800.000
window_height	600.000
resolved_continuous_ns	900000000
selector_kind	unique
status	frozen
EOF
cat > "$PLAN" <<'EOF'
event_id	offset_ms	action	arg0	arg1
start	0	checkpoint	0	0
type	1000	input	a	1
finish	2000	stop	0	0
EOF
chmod 0444 "$WINDOW" "$PLAN"

binding_args=(
    --campaign-secret-file "$SECRET" --campaign-id campaign-a --session-id session-a
    --nonce "$NONCE" --driver-output "$EVENTS" --driver-binary "$BINARY"
    --driver-source "$SOURCE" --controller "$CONTROLLER" --scenario-plan "$PLAN"
    --plan-start-continuous-ns "$PLAN_START" --subject-identity "$SUBJECT"
    --window-identity "$WINDOW"
)

"$TOOL" intent "${binding_args[@]}" --output "$INTENT"
cat > "$EVENTS" <<'EOF'
sequence	continuous_ns	event_id	action	target_pid	window_number	requested_a	requested_b	observed_a	observed_b	result
0	1010000000	start	checkpoint	4242	77	0	0	1	1	verified
1	2010000000	type	input	4242	77	1	2	0	1	verified
2	3010000000	finish	stop	4242	77	0	0	1	1	verified
EOF
chmod 0400 "$EVENTS"
"$TOOL" finalize "${binding_args[@]}" --intent "$INTENT" --receipt-output "$RECEIPT"
"$TOOL" verify "${binding_args[@]}" --intent "$INTENT" --receipt "$RECEIPT"

expect_failure "campaign replay" "$TOOL" verify \
    "${binding_args[@]/campaign-a/campaign-b}" --intent "$INTENT" --receipt "$RECEIPT"
expect_failure "nonce replay" "$TOOL" verify \
    "${binding_args[@]/$NONCE/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb}" \
    --intent "$INTENT" --receipt "$RECEIPT"

ORIGINAL_EVENTS="$TEMP_ROOT/original-events.tsv"
cp "$EVENTS" "$ORIGINAL_EVENTS"
chmod 0600 "$EVENTS"
sed 's/\tverified$/\tforged/' "$ORIGINAL_EVENTS" > "$EVENTS"
chmod 0400 "$EVENTS"
expect_failure "mutated result" "$TOOL" verify "${binding_args[@]}" \
    --intent "$INTENT" --receipt "$RECEIPT"
expect_failure "finalize forged result" "$TOOL" finalize "${binding_args[@]}" \
    --intent "$INTENT" --receipt-output "$TEMP_ROOT/forged-result.tsv"

chmod 0600 "$EVENTS"
awk 'NR == 3 {$2 = "2260000001"} {OFS="\t"; print}' "$ORIGINAL_EVENTS" > "$EVENTS"
chmod 0400 "$EVENTS"
expect_failure "wrong cadence" "$TOOL" finalize "${binding_args[@]}" \
    --intent "$INTENT" --receipt-output "$TEMP_ROOT/wrong-cadence.tsv"

chmod 0600 "$EVENTS"
awk 'NR == 3 {print} {print}' "$ORIGINAL_EVENTS" > "$EVENTS"
chmod 0400 "$EVENTS"
expect_failure "duplicate event" "$TOOL" finalize "${binding_args[@]}" \
    --intent "$INTENT" --receipt-output "$TEMP_ROOT/duplicate.tsv"

chmod 0600 "$EVENTS"
head -n 3 "$ORIGINAL_EVENTS" > "$EVENTS"
chmod 0400 "$EVENTS"
expect_failure "truncated event stream" "$TOOL" finalize "${binding_args[@]}" \
    --intent "$INTENT" --receipt-output "$TEMP_ROOT/truncated-events.tsv"

chmod 0600 "$EVENTS"
cp "$ORIGINAL_EVENTS" "$EVENTS"
chmod 0400 "$EVENTS"
UNSIGNED_RECEIPT="$TEMP_ROOT/unsigned-receipt.tsv"
sed 's/receipt_hmac_sha256\t.*/receipt_hmac_sha256\t0000000000000000000000000000000000000000000000000000000000000000/' \
    "$RECEIPT" > "$UNSIGNED_RECEIPT"
chmod 0400 "$UNSIGNED_RECEIPT"
expect_failure "unsigned receipt" "$TOOL" verify "${binding_args[@]}" \
    --intent "$INTENT" --receipt "$UNSIGNED_RECEIPT"

TRUNCATED_RECEIPT="$TEMP_ROOT/truncated-receipt.tsv"
sed '$d' "$RECEIPT" > "$TRUNCATED_RECEIPT"
chmod 0400 "$TRUNCATED_RECEIPT"
expect_failure "truncated receipt" "$TOOL" verify "${binding_args[@]}" \
    --intent "$INTENT" --receipt "$TRUNCATED_RECEIPT"

ALTERED_PLAN="$TEMP_ROOT/altered-plan.tsv"
sed 's/type\t1000/type\t1001/' "$PLAN" > "$ALTERED_PLAN"
chmod 0444 "$ALTERED_PLAN"
expect_failure "plan replay" "$TOOL" verify \
    "${binding_args[@]/$PLAN/$ALTERED_PLAN}" --intent "$INTENT" --receipt "$RECEIPT"

ALTERED_SUBJECT="$TEMP_ROOT/altered-subject.tsv"
sed 's/process_pid\t4242/process_pid\t4243/' "$SUBJECT" > "$ALTERED_SUBJECT"
chmod 0444 "$ALTERED_SUBJECT"
expect_failure "subject replay" "$TOOL" verify \
    "${binding_args[@]/$SUBJECT/$ALTERED_SUBJECT}" --intent "$INTENT" --receipt "$RECEIPT"

ALTERED_WINDOW="$TEMP_ROOT/altered-window.tsv"
sed 's/window_number\t77/window_number\t78/' "$WINDOW" > "$ALTERED_WINDOW"
chmod 0444 "$ALTERED_WINDOW"
expect_failure "window replay" "$TOOL" verify \
    "${binding_args[@]/$WINDOW/$ALTERED_WINDOW}" --intent "$INTENT" --receipt "$RECEIPT"

CLONED_EVENTS="$TEMP_ROOT/cloned-events.tsv"
cp "$EVENTS" "$CLONED_EVENTS"
chmod 0400 "$CLONED_EVENTS"
expect_failure "path replay" "$TOOL" verify \
    "${binding_args[@]/$EVENTS/$CLONED_EVENTS}" --intent "$INTENT" --receipt "$RECEIPT"

SAME_PATH_REPLACEMENT="$TEMP_ROOT/same-path-replacement.tsv"
cp "$EVENTS" "$SAME_PATH_REPLACEMENT"
chmod 0400 "$SAME_PATH_REPLACEMENT"
chmod 0600 "$EVENTS"
rm "$EVENTS"
mv "$SAME_PATH_REPLACEMENT" "$EVENTS"
chmod 0400 "$EVENTS"
REWRITTEN_INODE_RECEIPT="$TEMP_ROOT/rewritten-inode-receipt.tsv"
awk -F '\t' -v inode="$(stat -f '%i' "$EVENTS")" '
    BEGIN { OFS = "\t" }
    $1 == "driver_output_inode" { $2 = inode }
    { print }
' "$RECEIPT" > "$REWRITTEN_INODE_RECEIPT"
chmod 0400 "$REWRITTEN_INODE_RECEIPT"
expect_failure "same-path inode replacement with patched receipt" "$TOOL" verify \
    "${binding_args[@]}" --intent "$INTENT" --receipt "$REWRITTEN_INODE_RECEIPT"

echo "performance driver receipt tests passed"
