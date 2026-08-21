#!/bin/bash
# shellcheck disable=SC2016 # Awk programs and generated fixture scripts use literal dollar fields.

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
umask 077

SCRIPT_DIRECTORY="$(cd -- "$(dirname -- "$0")" && pwd)"
TEMP_ROOT_CREATED="$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-render-evidence.XXXXXX")"
TEMP_ROOT="$(cd -P -- "$TEMP_ROOT_CREATED" && pwd -P)"

cleanup() {
    chmod -R u+w "$TEMP_ROOT" 2>/dev/null || true
    rm -rf -- "$TEMP_ROOT"
}
fail() { echo "FAIL: $*" >&2; exit 1; }
sha256() { shasum -a 256 "$1" | awk '{ print $1 }'; }
metric() { awk -F '\t' -v key="$2" '$1 == key { print $2 }' "$1"; }
trap cleanup EXIT INT TERM

expect_failure() {
    local label="$1"
    shift
    if "$@" >"$TEMP_ROOT/failure.stdout" 2>"$TEMP_ROOT/failure.stderr"; then
        fail "$label unexpectedly succeeded"
    fi
}

write_identity() {
    local subject="$1"
    local pid="$2"
    local output="$3"
    {
        printf 'format_version\t1\n'
        printf 'subject\t%s\n' "$subject"
        printf 'app_bundle_path\t/Applications/%s.app\n' "$subject"
        printf 'bundle_identifier\tdev.spaceterm.fixture.%s\n' "$subject"
        printf 'bundle_version\t1\n'
        printf 'executable_path\t/Applications/%s.app/Contents/MacOS/%s\n' \
            "$subject" "$subject"
        printf 'executable_sha256\t%s\n' \
            'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
        printf 'executable_device\t1\n'
        printf 'executable_inode\t2\n'
        printf 'executable_fsid\t1\n'
        printf 'signature_valid\ttrue\n'
        printf 'signing_identifier\tdev.spaceterm.fixture.%s\n' "$subject"
        printf 'team_identifier\tTESTTEAM\n'
        printf 'cdhash\tabcdef0123456789\n'
        printf 'process_pid\t%s\n' "$pid"
        printf 'process_start_identity\t100:200\n'
        printf 'identity_status\tfrozen\n'
    } > "$output"
    chmod 0444 "$output"
}

write_driver_events() {
    local plan="$1"
    local pid="$2"
    local output="$3"
    {
        printf 'sequence\tcontinuous_ns\tevent_id\taction\ttarget_pid\twindow_number\trequested_a\trequested_b\tobserved_a\tobserved_b\tresult\n'
        awk -F '\t' -v pid="$pid" -v base=1000000000000 '
            NR == 1 { next }
            {
                sequence = NR - 2
                timestamp = base + ($2 * 1000000) + sequence + 1
                observed_a = $3 == "resize-grid" ? $4 : 1
                observed_b = $3 == "resize-grid" ? $5 : 1
                printf "%d\t%.0f\t%s\t%s\t%s\t77\t%s\t%s\t%s\t%s\tverified\n", \
                    sequence, timestamp, $1, $3, pid, $4, $5, observed_a, observed_b
            }
        ' "$plan"
    } > "$output"
    chmod 0444 "$output"
}

SPACETERM_IDENTITY="$TEMP_ROOT/spaceterm-identity.tsv"
GHOSTTY_IDENTITY="$TEMP_ROOT/ghostty-identity.tsv"
write_identity spaceterm 4242 "$SPACETERM_IDENTITY"
write_identity ghostty 4343 "$GHOSTTY_IDENTITY"
SPACETERM_PROVISIONAL="$TEMP_ROOT/spaceterm-provisional.tsv"
cat > "$SPACETERM_PROVISIONAL" <<'EOF'
schema	spaceterm.acceptance.native-launch-proof/v5
observation.source	production-app
launch.nonce	cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
run.id	render-protocol
package.app.sha256	dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd
runtime.schema	spaceterm.acceptance.runtime-stream/v1
runtime.sample_interval_ms	1000
runtime.transition_capacity	64
failure.action.schema	spaceterm.acceptance.failure-action/v1
failure.action.enabled	false
process.pid	4242
process.pidversion	7
process.executable.path	/Applications/spaceterm.app/Contents/MacOS/spaceterm
process.executable.device	1
process.executable.inode	2
process.executable.fsid	1
process.signature.cdhash	abcdef0123456789
process.signature.identifier	dev.spaceterm.fixture.spaceterm
process.signature.team_identifier	TESTTEAM
terminal_font_selected	JetBrains Mono
initial_grid.rows	24
initial_grid.columns	80
initial_grid.logical_width	800
initial_grid.logical_height	480
initial_grid.backing_pixel_width	1600
initial_grid.backing_pixel_height	960
observation.complete	true
EOF
chmod 0400 "$SPACETERM_PROVISIONAL"

WORKLOAD="$TEMP_ROOT/workload"
COMMAND_MANIFEST="$TEMP_ROOT/command.tsv"
ENVIRONMENT_MANIFEST="$TEMP_ROOT/environment.tsv"
FONT_MANIFEST="$TEMP_ROOT/font.tsv"
INITIAL_GRID_MANIFEST="$TEMP_ROOT/initial-grid.tsv"
printf 'render workload fixture\n' > "$WORKLOAD"
printf 'command fixture\n' > "$COMMAND_MANIFEST"
printf 'environment fixture\n' > "$ENVIRONMENT_MANIFEST"
printf 'font fixture\n' > "$FONT_MANIFEST"
printf 'grid fixture\n' > "$INITIAL_GRID_MANIFEST"
chmod 0444 "$WORKLOAD" "$COMMAND_MANIFEST" "$ENVIRONMENT_MANIFEST" \
    "$FONT_MANIFEST" "$INITIAL_GRID_MANIFEST"

HMAC_SECRET="$TEMP_ROOT/hmac-secret.hex"
printf '%s\n' '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' \
    > "$HMAC_SECRET"
chmod 0400 "$HMAC_SECRET"
NONCE='abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789'

TOOL_SOURCE_REPOSITORY="$TEMP_ROOT/tool-source"
TOOL_BUNDLE="$TEMP_ROOT/tool-bundle"
mkdir -p "$TOOL_SOURCE_REPOSITORY/scripts/acceptance"
TOOL_RELATIVES='scripts/record-release-performance-trace.sh
scripts/acceptance/freeze-render-profile-intent.sh
scripts/acceptance/finalize-render-profile-evidence.sh
scripts/acceptance/render-profile-hmac.py
scripts/acceptance/render-trace-receipt.py
scripts/acceptance/analyze-release-render-profile-case.sh
scripts/acceptance/archive-render-trace.py
scripts/acceptance/verify-render-action-video.py
scripts/acceptance/verify-render-trace-archive.py
scripts/verify-release-performance-trace.py
scripts/inspect-release-performance-process.py
scripts/run-release-performance-command.py
scripts/acceptance/freeze-render-profile-tool-bundle.sh'
tool_index=0
for relative in $TOOL_RELATIVES; do
    source="$TOOL_SOURCE_REPOSITORY/$relative"
    mkdir -p "$(dirname "$source")"
    case "$relative" in
        scripts/acceptance/freeze-render-profile-intent.sh)
            cp "$SCRIPT_DIRECTORY/freeze-render-profile-intent.sh" "$source" ;;
        scripts/acceptance/finalize-render-profile-evidence.sh)
            cp "$SCRIPT_DIRECTORY/finalize-render-profile-evidence.sh" "$source" ;;
        scripts/acceptance/render-profile-hmac.py)
            cp "$SCRIPT_DIRECTORY/render-profile-hmac.py" "$source" ;;
        scripts/acceptance/freeze-render-profile-tool-bundle.sh)
            cp "$SCRIPT_DIRECTORY/freeze-render-profile-tool-bundle.sh" "$source" ;;
        *) printf '#!/bin/sh\nexit 0\n' > "$source" ;;
    esac
    chmod 0555 "$source"
    tool_index=$((tool_index + 1))
done
/usr/bin/git init -q "$TOOL_SOURCE_REPOSITORY"
/usr/bin/git -C "$TOOL_SOURCE_REPOSITORY" add .
GIT_AUTHOR_NAME=fixture GIT_AUTHOR_EMAIL=fixture@example.test \
GIT_COMMITTER_NAME=fixture GIT_COMMITTER_EMAIL=fixture@example.test \
    /usr/bin/git -C "$TOOL_SOURCE_REPOSITORY" commit -qm fixture
TOOL_SOURCE_COMMIT="$(/usr/bin/git -C "$TOOL_SOURCE_REPOSITORY" rev-parse HEAD)"
"$TOOL_SOURCE_REPOSITORY/scripts/acceptance/freeze-render-profile-tool-bundle.sh" \
    --source-commit "$TOOL_SOURCE_COMMIT" --output-directory "$TOOL_BUNDLE" >/dev/null
TOOL_BUNDLE_MANIFEST="$TOOL_BUNDLE/tool-bundle-manifest.tsv"
INTENT_TOOL="$TOOL_BUNDLE/scripts/acceptance/freeze-render-profile-intent.sh"
FINALIZER_TOOL="$TOOL_BUNDLE/scripts/acceptance/finalize-render-profile-evidence.sh"
declare -a TOOL_BUNDLE_ARGS=(
    --render-tool-bundle-manifest "$TOOL_BUNDLE_MANIFEST"
    --expected-source-commit "$TOOL_SOURCE_COMMIT"
    --trusted-source-repository "$TOOL_SOURCE_REPOSITORY"
)
UNTRUSTED_CHECKOUT="$TEMP_ROOT/untrusted-checkout"
mkdir -p "$UNTRUSTED_CHECKOUT"
cp "$SCRIPT_DIRECTORY/freeze-render-profile-intent.sh" \
    "$SCRIPT_DIRECTORY/finalize-render-profile-evidence.sh" "$UNTRUSTED_CHECKOUT/"
for checkout_consumer in freeze-render-profile-intent.sh finalize-render-profile-evidence.sh; do
    checkout_marker="$TEMP_ROOT/$checkout_consumer.secret-read"
    checkout_error="$TEMP_ROOT/$checkout_consumer.checkout.err"
    cat > "$UNTRUSTED_CHECKOUT/render-profile-hmac.py" <<PY
#!/usr/bin/python3
from pathlib import Path
Path("$checkout_marker").write_text("secret helper invoked\\n")
raise SystemExit(99)
PY
    chmod 0755 "$UNTRUSTED_CHECKOUT/render-profile-hmac.py"
    if "$UNTRUSTED_CHECKOUT/$checkout_consumer" "${TOOL_BUNDLE_ARGS[@]}" \
        --hmac-secret "$HMAC_SECRET" \
        >/dev/null 2>"$checkout_error"; then
        fail "$checkout_consumer accepted checkout execution"
    fi
    grep -Fq 'not the selected frozen bundle tool' "$checkout_error" \
        || fail "$checkout_consumer did not reject checkout execution before secret validation"
    [[ ! -e "$checkout_marker" ]] \
        || fail "$checkout_consumer invoked an untrusted secret helper before rejection"
done

declare -a SCENARIOS=(
    perf-render-idle-cursor-blink
    perf-render-text-blink
    perf-render-sustained-output
    perf-render-selection
    perf-render-marked-text
    perf-render-live-resize
)

freeze_intent() {
    local scenario="$1"
    local tag="$2"
    local expected_driver="$3"
    local final_metadata="$4"
    local intent="$5"
    local action_video="$6"
    "$INTENT_TOOL" \
        --subject spaceterm \
        --scenario "$scenario" \
        --campaign-id render-campaign \
        --session-id render-session \
        --nonce "$NONCE" \
        --plan "$TEMP_ROOT/$scenario-plan.tsv" \
        --plan-metadata "$TEMP_ROOT/$scenario-plan-metadata.tsv" \
        --pair-metadata "$TEMP_ROOT/$scenario-pair.tsv" \
        --run-intent "$TEMP_ROOT/$scenario-run-intent.tsv" \
        --command-manifest "$COMMAND_MANIFEST" \
        --environment-manifest "$ENVIRONMENT_MANIFEST" \
        --font-manifest "$FONT_MANIFEST" \
        --initial-grid-manifest "$INITIAL_GRID_MANIFEST" \
        --subject-identity "$SPACETERM_IDENTITY" \
        --expected-driver-events "$expected_driver" \
        --action-video "$action_video" \
        --final-metadata "$final_metadata" \
        --hmac-secret "$HMAC_SECRET" \
        --output "$intent" "${TOOL_BUNDLE_ARGS[@]}" >/dev/null || return
    [[ ! -w "$intent" ]] || fail "$tag intent is mutable"
}

finalize_evidence() {
    local scenario="$1"
    local intent="$2"
    local driver="$3"
    local video="$4"
    local output="$5"
    local workload_metadata="$6"
    "$FINALIZER_TOOL" \
        --intent "$intent" \
        --plan "$TEMP_ROOT/$scenario-plan.tsv" \
        --plan-metadata "$TEMP_ROOT/$scenario-plan-metadata.tsv" \
        --pair-metadata "$TEMP_ROOT/$scenario-pair.tsv" \
        --run-intent "$TEMP_ROOT/$scenario-run-intent.tsv" \
        --command-manifest "$COMMAND_MANIFEST" \
        --environment-manifest "$ENVIRONMENT_MANIFEST" \
        --font-manifest "$FONT_MANIFEST" \
        --initial-grid-manifest "$INITIAL_GRID_MANIFEST" \
        --subject-identity "$SPACETERM_IDENTITY" \
        --driver-events "$driver" \
        --action-video "$video" \
        --render-workload-metadata "$workload_metadata" \
        --hmac-secret "$HMAC_SECRET" \
        --output "$output" "${TOOL_BUNDLE_ARGS[@]}"
}

mkdir -p "$TEMP_ROOT/intents" "$TEMP_ROOT/drivers" "$TEMP_ROOT/final"
for scenario in "${SCENARIOS[@]}"; do
    plan="$TEMP_ROOT/$scenario-plan.tsv"
    plan_metadata="$TEMP_ROOT/$scenario-plan-metadata.tsv"
    pair_metadata="$TEMP_ROOT/$scenario-pair.tsv"
    run_intent="$TEMP_ROOT/$scenario-run-intent.tsv"
    "$SCRIPT_DIRECTORY/performance-plan.sh" \
        --scenario "$scenario" --plan "$plan" --metadata "$plan_metadata" >/dev/null
    "$SCRIPT_DIRECTORY/freeze-performance-pair.sh" \
        --pair-id "pair-$scenario" \
        --scenario "$scenario" \
        --plan "$plan" \
        --plan-metadata "$plan_metadata" \
        --workload-binary "$WORKLOAD" \
        --command-manifest "$COMMAND_MANIFEST" \
        --environment-manifest "$ENVIRONMENT_MANIFEST" \
        --font-manifest "$FONT_MANIFEST" \
        --initial-grid-manifest "$INITIAL_GRID_MANIFEST" \
        --spaceterm-identity "$SPACETERM_IDENTITY" \
        --ghostty-identity "$GHOSTTY_IDENTITY" \
        --output "$pair_metadata" >/dev/null
    "$SCRIPT_DIRECTORY/freeze-performance-run-intent.sh" \
        --subject spaceterm \
        --pair-metadata "$pair_metadata" \
        --subject-identity "$SPACETERM_IDENTITY" \
        --plan "$plan" \
        --workload-binary "$WORKLOAD" \
        --command-manifest "$COMMAND_MANIFEST" \
        --environment-manifest "$ENVIRONMENT_MANIFEST" \
        --font-manifest "$FONT_MANIFEST" \
        --initial-grid-manifest "$INITIAL_GRID_MANIFEST" \
        --campaign-id render-campaign --session-id render-session --nonce "$NONCE" \
        --native-provisional-observation "$SPACETERM_PROVISIONAL" \
        --output "$run_intent" >/dev/null
    freeze_intent "$scenario" "$scenario" \
        "$TEMP_ROOT/drivers/$scenario.tsv" "$TEMP_ROOT/final/$scenario.tsv" \
        "$TEMP_ROOT/intents/$scenario.tsv" "$TEMP_ROOT/$scenario-actions.mov"
done

VALID_SCENARIO=perf-render-live-resize
VALID_INTENT="$TEMP_ROOT/intents/$VALID_SCENARIO.tsv"
VALID_DRIVER="$TEMP_ROOT/drivers/$VALID_SCENARIO.tsv"
VALID_FINAL="$TEMP_ROOT/final/$VALID_SCENARIO.tsv"
VALID_VIDEO="$TEMP_ROOT/$VALID_SCENARIO-actions.mov"
VALID_WORKLOAD_METADATA="$TEMP_ROOT/$VALID_SCENARIO-render-workload.tsv"
write_driver_events "$TEMP_ROOT/$VALID_SCENARIO-plan.tsv" 4242 "$VALID_DRIVER"
printf 'action video fixture\n' > "$VALID_VIDEO"
chmod 0444 "$VALID_VIDEO"
"$SCRIPT_DIRECTORY/freeze-render-profile-workload.sh" \
    --subject spaceterm --scenario "$VALID_SCENARIO" \
    --plan "$TEMP_ROOT/$VALID_SCENARIO-plan.tsv" \
    --plan-metadata "$TEMP_ROOT/$VALID_SCENARIO-plan-metadata.tsv" \
    --pair-metadata "$TEMP_ROOT/$VALID_SCENARIO-pair.tsv" \
    --subject-identity "$SPACETERM_IDENTITY" \
    --driver-events "$VALID_DRIVER" --action-video "$VALID_VIDEO" \
    --output "$VALID_WORKLOAD_METADATA" >/dev/null
finalize_evidence "$VALID_SCENARIO" "$VALID_INTENT" "$VALID_DRIVER" \
    "$VALID_VIDEO" "$VALID_FINAL" "$VALID_WORKLOAD_METADATA" >/dev/null
[[ ! -w "$VALID_FINAL" \
    && "$(metric "$VALID_FINAL" intent_sha256)" == "$(sha256 "$VALID_INTENT")" \
    && "$(metric "$VALID_FINAL" driver_events_sha256)" == "$(sha256 "$VALID_DRIVER")" \
    && "$(metric "$VALID_FINAL" action_video_sha256)" == "$(sha256 "$VALID_VIDEO")" \
    && "$(metric "$VALID_FINAL" render_workload_metadata_sha256)" \
        == "$(sha256 "$VALID_WORKLOAD_METADATA")" \
    && "$(metric "$VALID_FINAL" required_action_count)" == 180 \
    && "$(metric "$VALID_FINAL" completed_action_count)" == 180 \
    && "$(metric "$VALID_FINAL" action_interval_ms)" == 1000 \
    && "$(metric "$VALID_FINAL" result)" == verified ]] \
    || fail "final evidence did not bind the verified capture"

EVIDENCE_BODY="$TEMP_ROOT/evidence-body.tsv"
EVIDENCE_AUTH="$TEMP_ROOT/evidence-auth.bin"
sed '$d' "$VALID_FINAL" > "$EVIDENCE_BODY"
{
    printf 'SPACETERM_RENDER_PROFILE_EVIDENCE_V1\0'
    cat "$EVIDENCE_BODY"
} > "$EVIDENCE_AUTH"
EXPECTED_HMAC="$(/usr/bin/python3 - "$HMAC_SECRET" "$EVIDENCE_AUTH" <<'PY'
import hashlib
import hmac
import pathlib
import sys

key_hex = pathlib.Path(sys.argv[1]).read_bytes()
authenticated = pathlib.Path(sys.argv[2]).read_bytes()
print(hmac.new(bytes.fromhex(key_hex[:-1].decode("ascii")), authenticated,
               hashlib.sha256).hexdigest())
PY
)"
[[ "$EXPECTED_HMAC" == "$(metric "$VALID_FINAL" evidence_hmac_sha256)" ]] \
    || fail "final evidence HMAC is not canonical"

# The trusted HMAC helper reads the key itself. Its instrumented process audit
# proves the key is absent from both argv and the environment for intent and
# final-evidence authentication, while the independently computed HMAC above
# proves the output remains canonical.
PROCESS_AUDIT="$TEMP_ROOT/hmac-process-audit.jsonl"
SECRET_CONSUMER_FAKE_PATH="$TEMP_ROOT/secret-consumer-fake-path"
SECRET_CONSUMER_PATH_MARKER="$TEMP_ROOT/secret-consumer-path-marker"
mkdir -p -- "$SECRET_CONSUMER_FAKE_PATH"
for fake_tool in awk basename chmod dirname head ln od python3 rm sed shasum stat tail tr wc; do
    {
        printf '#!/bin/bash\n'
        printf 'printf "%%s\\n" "$0" >> %q\n' "$SECRET_CONSUMER_PATH_MARKER"
        printf 'exit 99\n'
    } > "$SECRET_CONSUMER_FAKE_PATH/$fake_tool"
    chmod 0755 "$SECRET_CONSUMER_FAKE_PATH/$fake_tool"
done
AUDIT_SCENARIO=perf-render-selection
AUDIT_INTENT="$TEMP_ROOT/intents/audit.tsv"
AUDIT_DRIVER="$TEMP_ROOT/drivers/audit.tsv"
AUDIT_FINAL="$TEMP_ROOT/final/audit.tsv"
AUDIT_VIDEO="$TEMP_ROOT/audit-actions.mov"
AUDIT_WORKLOAD="$TEMP_ROOT/audit-render-workload.tsv"
PATH="$SECRET_CONSUMER_FAKE_PATH:/usr/bin:/bin:/usr/sbin:/sbin" \
    SPACETERM_RENDER_PROFILE_PROCESS_AUDIT="$PROCESS_AUDIT" \
    freeze_intent "$AUDIT_SCENARIO" audit "$AUDIT_DRIVER" "$AUDIT_FINAL" \
        "$AUDIT_INTENT" "$AUDIT_VIDEO"
write_driver_events "$TEMP_ROOT/$AUDIT_SCENARIO-plan.tsv" 4242 "$AUDIT_DRIVER"
printf 'audit action video fixture\n' > "$AUDIT_VIDEO"
chmod 0444 "$AUDIT_VIDEO"
"$SCRIPT_DIRECTORY/freeze-render-profile-workload.sh" \
    --subject spaceterm --scenario "$AUDIT_SCENARIO" \
    --plan "$TEMP_ROOT/$AUDIT_SCENARIO-plan.tsv" \
    --plan-metadata "$TEMP_ROOT/$AUDIT_SCENARIO-plan-metadata.tsv" \
    --pair-metadata "$TEMP_ROOT/$AUDIT_SCENARIO-pair.tsv" \
    --subject-identity "$SPACETERM_IDENTITY" \
    --driver-events "$AUDIT_DRIVER" --action-video "$AUDIT_VIDEO" \
    --output "$AUDIT_WORKLOAD" >/dev/null
PATH="$SECRET_CONSUMER_FAKE_PATH:/usr/bin:/bin:/usr/sbin:/sbin" \
    SPACETERM_RENDER_PROFILE_PROCESS_AUDIT="$PROCESS_AUDIT" \
    finalize_evidence "$AUDIT_SCENARIO" "$AUDIT_INTENT" "$AUDIT_DRIVER" \
        "$AUDIT_VIDEO" "$AUDIT_FINAL" "$AUDIT_WORKLOAD" >/dev/null
[[ ! -e "$SECRET_CONSUMER_PATH_MARKER" ]] \
    || fail "intent/final evidence secret consumers invoked an injected PATH tool"
[[ -s "$PROCESS_AUDIT" \
    && "$(wc -l < "$PROCESS_AUDIT" | tr -d ' ')" -ge 4 ]] \
    || fail "HMAC process audit is incomplete"
/usr/bin/python3 - "$PROCESS_AUDIT" <<'PY' || fail "HMAC secret reached child argv/environment"
import json
import pathlib
import sys

reports = [json.loads(line) for line in pathlib.Path(sys.argv[1]).read_text().splitlines()]
if not reports or any(
    report["argv_contains_secret"] or report["environment_contains_secret"]
    for report in reports
):
    raise SystemExit(1)
PY

# Public and multiply linked key material is rejected even when its contents
# would otherwise produce the same HMAC.
PUBLIC_SECRET="$TEMP_ROOT/public-secret.hex"
cp "$HMAC_SECRET" "$PUBLIC_SECRET"
chmod 0444 "$PUBLIC_SECRET"
expect_failure "public HMAC secret" \
    /usr/bin/python3 "$SCRIPT_DIRECTORY/render-profile-hmac.py" \
        --secret "$PUBLIC_SECRET" --domain fixture --body "$EVIDENCE_BODY"
WRITABLE_SECRET="$TEMP_ROOT/writable-secret.hex"
cp "$HMAC_SECRET" "$WRITABLE_SECRET"
chmod 0600 "$WRITABLE_SECRET"
expect_failure "owner-writable HMAC secret" \
    /usr/bin/python3 "$SCRIPT_DIRECTORY/render-profile-hmac.py" \
        --secret "$WRITABLE_SECRET" --domain fixture --body "$EVIDENCE_BODY"
HARDLINK_SECRET="$TEMP_ROOT/hardlink-secret.hex"
HARDLINK_ALIAS="$TEMP_ROOT/hardlink-secret-alias.hex"
cp "$HMAC_SECRET" "$HARDLINK_SECRET"
chmod 0400 "$HARDLINK_SECRET"
ln "$HARDLINK_SECRET" "$HARDLINK_ALIAS"
expect_failure "hardlinked HMAC secret" \
    /usr/bin/python3 "$SCRIPT_DIRECTORY/render-profile-hmac.py" \
        --secret "$HARDLINK_SECRET" --domain fixture --body "$EVIDENCE_BODY"

# A verified result cannot override native geometry that contradicts the
# requested resize. The driver itself allows at most eight pixels of delta.
VALID_DRIVER_BACKUP="$TEMP_ROOT/valid-driver-backup.tsv"
cp "$VALID_DRIVER" "$VALID_DRIVER_BACKUP"
chmod 0644 "$VALID_DRIVER"
awk -F '\t' -v OFS='\t' '
    $4 == "resize-grid" && !changed { $9 = $7 + 9; changed = 1 }
    { print }
' "$VALID_DRIVER_BACKUP" > "$VALID_DRIVER"
chmod 0444 "$VALID_DRIVER"
expect_failure "mismatched observed resize geometry in workload freezer" \
    "$SCRIPT_DIRECTORY/freeze-render-profile-workload.sh" \
        --subject spaceterm --scenario "$VALID_SCENARIO" \
        --plan "$TEMP_ROOT/$VALID_SCENARIO-plan.tsv" \
        --plan-metadata "$TEMP_ROOT/$VALID_SCENARIO-plan-metadata.tsv" \
        --pair-metadata "$TEMP_ROOT/$VALID_SCENARIO-pair.tsv" \
        --subject-identity "$SPACETERM_IDENTITY" \
        --driver-events "$VALID_DRIVER" --action-video "$VALID_VIDEO" \
        --output "$TEMP_ROOT/bad-observed-resize-workload.tsv"
expect_failure "mismatched observed resize geometry in finalizer" \
    finalize_evidence "$VALID_SCENARIO" "$VALID_INTENT" "$VALID_DRIVER" \
        "$VALID_VIDEO" "$TEMP_ROOT/final/bad-observed-resize.tsv" \
        "$VALID_WORKLOAD_METADATA"
chmod 0644 "$VALID_DRIVER"
cp "$VALID_DRIVER_BACKUP" "$VALID_DRIVER"
chmod 0444 "$VALID_DRIVER"

# A checkpoint marked verified must still prove the target window remained
# onscreen; its focus observation is the driver contract's boolean second field.
CHECKPOINT_SCENARIO=perf-render-idle-cursor-blink
BAD_CHECKPOINT_DRIVER="$TEMP_ROOT/bad-checkpoint-observation.tsv"
write_driver_events "$TEMP_ROOT/$CHECKPOINT_SCENARIO-plan.tsv" 4242 \
    "$BAD_CHECKPOINT_DRIVER"
chmod 0644 "$BAD_CHECKPOINT_DRIVER"
awk -F '\t' -v OFS='\t' '
    $4 == "checkpoint" && !changed { $9 = 0; changed = 1 }
    { print }
' "$BAD_CHECKPOINT_DRIVER" > "$TEMP_ROOT/bad-checkpoint-observation.tmp"
mv "$TEMP_ROOT/bad-checkpoint-observation.tmp" "$BAD_CHECKPOINT_DRIVER"
chmod 0444 "$BAD_CHECKPOINT_DRIVER"
expect_failure "verified offscreen checkpoint observation" \
    "$SCRIPT_DIRECTORY/freeze-render-profile-workload.sh" \
        --subject spaceterm --scenario "$CHECKPOINT_SCENARIO" \
        --plan "$TEMP_ROOT/$CHECKPOINT_SCENARIO-plan.tsv" \
        --plan-metadata "$TEMP_ROOT/$CHECKPOINT_SCENARIO-plan-metadata.tsv" \
        --pair-metadata "$TEMP_ROOT/$CHECKPOINT_SCENARIO-pair.tsv" \
        --subject-identity "$SPACETERM_IDENTITY" \
        --driver-events "$BAD_CHECKPOINT_DRIVER" --action-video "$VALID_VIDEO" \
        --output "$TEMP_ROOT/bad-checkpoint-workload.tsv"

# A raw driver stream cannot exist when intent is frozen.
EXISTING_DRIVER="$TEMP_ROOT/drivers/already-exists.tsv"
printf 'already exists\n' > "$EXISTING_DRIVER"
chmod 0444 "$EXISTING_DRIVER"
expect_failure "preexisting driver path" freeze_intent \
    perf-render-selection existing "$EXISTING_DRIVER" \
    "$TEMP_ROOT/final/existing.tsv" "$TEMP_ROOT/intents/existing.tsv" \
    "$TEMP_ROOT/existing-actions.mov"

# Tampering with any intent field without the secret must fail authentication.
TAMPERED_INTENT="$TEMP_ROOT/intents/tampered.tsv"
cp "$VALID_INTENT" "$TAMPERED_INTENT"
chmod 0644 "$TAMPERED_INTENT"
sed -i '' 's/render-campaign/tampered-campaign/' "$TAMPERED_INTENT"
chmod 0444 "$TAMPERED_INTENT"
expect_failure "tampered intent" finalize_evidence "$VALID_SCENARIO" \
    "$TAMPERED_INTENT" "$VALID_DRIVER" "$VALID_VIDEO" \
    "$TEMP_ROOT/final/tampered.tsv" "$VALID_WORKLOAD_METADATA"

# A different HMAC secret cannot finalize an otherwise valid intent.
WRONG_SECRET="$TEMP_ROOT/wrong-secret.hex"
printf '%s\n' 'ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff' \
    > "$WRONG_SECRET"
chmod 0400 "$WRONG_SECRET"
expect_failure "wrong HMAC secret" \
    "$FINALIZER_TOOL" \
        --intent "$VALID_INTENT" \
        --plan "$TEMP_ROOT/$VALID_SCENARIO-plan.tsv" \
        --plan-metadata "$TEMP_ROOT/$VALID_SCENARIO-plan-metadata.tsv" \
        --pair-metadata "$TEMP_ROOT/$VALID_SCENARIO-pair.tsv" \
        --run-intent "$TEMP_ROOT/$VALID_SCENARIO-run-intent.tsv" \
        --command-manifest "$COMMAND_MANIFEST" \
        --environment-manifest "$ENVIRONMENT_MANIFEST" \
        --font-manifest "$FONT_MANIFEST" \
        --initial-grid-manifest "$INITIAL_GRID_MANIFEST" \
        --subject-identity "$SPACETERM_IDENTITY" \
        --driver-events "$VALID_DRIVER" \
        --action-video "$VALID_VIDEO" \
        --render-workload-metadata "$VALID_WORKLOAD_METADATA" \
        --hmac-secret "$WRONG_SECRET" \
        --output "$TEMP_ROOT/final/wrong-secret.tsv" \
        "${TOOL_BUNDLE_ARGS[@]}"

# Any non-verified native result invalidates the full one-to-one stream.
chmod 0644 "$VALID_DRIVER"
sed -i '' '2s/verified$/failed/' "$VALID_DRIVER"
chmod 0444 "$VALID_DRIVER"
expect_failure "failed driver result" finalize_evidence "$VALID_SCENARIO" \
    "$VALID_INTENT" "$VALID_DRIVER" "$VALID_VIDEO" \
    "$TEMP_ROOT/final/failed-result.tsv" "$VALID_WORKLOAD_METADATA"
chmod 0644 "$VALID_DRIVER"
cp "$VALID_DRIVER_BACKUP" "$VALID_DRIVER"
chmod 0444 "$VALID_DRIVER"

# Replacing the expected driver parent after intent changes its inode and is
# rejected even though the textual driver path and its contents are valid.
SWAP_SCENARIO=perf-render-selection
SWAP_PARENT="$TEMP_ROOT/swap-driver-parent"
SWAP_OLD_PARENT="$TEMP_ROOT/swap-driver-parent-old"
mkdir "$SWAP_PARENT"
SWAP_DRIVER="$SWAP_PARENT/events.tsv"
SWAP_INTENT="$TEMP_ROOT/intents/swap-parent.tsv"
SWAP_FINAL="$TEMP_ROOT/final/swap-parent.tsv"
freeze_intent "$SWAP_SCENARIO" swap-parent "$SWAP_DRIVER" "$SWAP_FINAL" \
    "$SWAP_INTENT" "$TEMP_ROOT/swap-actions.mov"
mv "$SWAP_PARENT" "$SWAP_OLD_PARENT"
mkdir "$SWAP_PARENT"
write_driver_events "$TEMP_ROOT/$SWAP_SCENARIO-plan.tsv" 4242 "$SWAP_DRIVER"
expect_failure "replaced driver parent" finalize_evidence "$SWAP_SCENARIO" \
    "$SWAP_INTENT" "$SWAP_DRIVER" "$TEMP_ROOT/swap-actions.mov" "$SWAP_FINAL" \
    "$VALID_WORKLOAD_METADATA"

# A writable capture artifact is not immutable final evidence.
MUTABLE_SCENARIO=perf-render-marked-text
MUTABLE_DRIVER="$TEMP_ROOT/drivers/mutable.tsv"
MUTABLE_INTENT="$TEMP_ROOT/intents/mutable.tsv"
MUTABLE_FINAL="$TEMP_ROOT/final/mutable.tsv"
freeze_intent "$MUTABLE_SCENARIO" mutable "$MUTABLE_DRIVER" \
    "$MUTABLE_FINAL" "$MUTABLE_INTENT" "$TEMP_ROOT/mutable-actions.mov"
write_driver_events "$TEMP_ROOT/$MUTABLE_SCENARIO-plan.tsv" 4242 "$MUTABLE_DRIVER"
chmod 0644 "$MUTABLE_DRIVER"
expect_failure "mutable driver stream" finalize_evidence "$MUTABLE_SCENARIO" \
    "$MUTABLE_INTENT" "$MUTABLE_DRIVER" "$TEMP_ROOT/mutable-actions.mov" \
    "$MUTABLE_FINAL" "$VALID_WORKLOAD_METADATA"

printf 'render profile evidence protocol fixtures passed\n'
