#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C

SCRIPT_DIRECTORY="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPOSITORY_ROOT="$(cd -- "$SCRIPT_DIRECTORY/../.." && pwd -P)"
TEMP_CANDIDATE="$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/render-tool-bundle.XXXXXX")"
TEMP_ROOT="$(cd -P -- "$TEMP_CANDIDATE" && pwd -P)"
/bin/chmod 0700 "$TEMP_ROOT"

cleanup() {
    /bin/chmod -R u+w -- "$TEMP_ROOT" 2>/dev/null || true
    /bin/rm -rf -- "$TEMP_ROOT"
}
trap cleanup EXIT INT TERM

fail() { echo "FAIL: $*" >&2; exit 1; }
sha256() { /usr/bin/shasum -a 256 "$1" | /usr/bin/awk '{ print $1 }'; }
kv() {
    /usr/bin/awk -F '\t' -v wanted="$2" \
        '$1 == wanted { count += 1; value = $2 } END { if (count == 1) print value }' "$1"
}

readonly TOOL_SOURCES="scripts/record-release-performance-trace.sh
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
scripts/acceptance/freeze-render-profile-tool-bundle.sh"

make_fixture_repository() {
    local fixture="$1" source_relative
    /bin/mkdir -p "$fixture"
    /usr/bin/git -C "$fixture" init -q
    /usr/bin/git -C "$fixture" config user.name 'Render Tool Bundle Fixture'
    /usr/bin/git -C "$fixture" config user.email 'render-tool-bundle@example.invalid'
    while IFS= read -r source_relative; do
        /bin/mkdir -p "$fixture/$(/usr/bin/dirname -- "$source_relative")"
        /bin/cp -- "$REPOSITORY_ROOT/$source_relative" "$fixture/$source_relative"
    done <<< "$TOOL_SOURCES"
    /usr/bin/git -C "$fixture" add -- scripts
    /usr/bin/git -C "$fixture" commit -qm 'test: create tool bundle fixture'
}

FIXTURE_REPOSITORY="$TEMP_ROOT/repository"
make_fixture_repository "$FIXTURE_REPOSITORY"
FREEZER="$FIXTURE_REPOSITORY/scripts/acceptance/freeze-render-profile-tool-bundle.sh"
SOURCE_COMMIT="$(/usr/bin/git -C "$FIXTURE_REPOSITORY" rev-parse HEAD)"
BUNDLE_PARENT="$TEMP_ROOT/private"
/bin/mkdir -m 0700 "$BUNDLE_PARENT"
BUNDLE="$BUNDLE_PARENT/bundle"
"$FREEZER" --source-commit "$SOURCE_COMMIT" --output-directory "$BUNDLE" >/dev/null
MANIFEST="$BUNDLE/tool-bundle-manifest.tsv"
[[ -f "$MANIFEST" && ! -L "$MANIFEST" \
    && "$(/usr/bin/stat -f '%Lp' "$BUNDLE")" == 555 \
    && "$(/usr/bin/stat -f '%Lp:%l' "$MANIFEST")" == 444:1 \
    && "$(kv "$MANIFEST" format_version)" == 1 \
    && "$(kv "$MANIFEST" schema)" == spaceterm.render-profile-tool-bundle/v1 \
    && "$(kv "$MANIFEST" source_commit)" == "$SOURCE_COMMIT" \
    && "$(kv "$MANIFEST" tool_count)" == 13 \
    && "$(/usr/bin/wc -l < "$MANIFEST" | /usr/bin/tr -d ' ')" == 56 ]] \
    || fail "tool bundle manifest or publication mode is invalid"

for logical in record_release_performance_trace freeze_render_profile_intent \
    finalize_render_profile_evidence render_profile_hmac render_trace_receipt \
    analyze_release_render_profile_case archive_render_trace \
    verify_render_action_video verify_render_trace_archive \
    verify_release_performance_trace inspect_release_performance_process \
    run_release_performance_command freeze_render_profile_tool_bundle; do
    source_path="$(kv "$MANIFEST" "${logical}_source_path")"
    bundle_path="$(kv "$MANIFEST" "${logical}_bundle_path")"
    [[ "$source_path" == /* && "$bundle_path" == "$BUNDLE/scripts/"* \
        && -f "$bundle_path" && ! -L "$bundle_path" \
        && "$(/usr/bin/stat -f '%Lp:%l' "$bundle_path")" == 555:1 \
        && "$(kv "$MANIFEST" "${logical}_source_sha256")" == "$(sha256 "$source_path")" \
        && "$(kv "$MANIFEST" "${logical}_bundle_sha256")" == "$(sha256 "$bundle_path")" \
        && "$(sha256 "$source_path")" == "$(sha256 "$bundle_path")" ]] \
        || fail "tool bundle entry is invalid: $logical"
done

if "$FREEZER" --campaign-secret-file "$TEMP_ROOT/secret" --source-commit "$SOURCE_COMMIT" \
    --output-directory "$TEMP_ROOT/forbidden" >/dev/null 2>&1; then
    fail "pre-secret tool freezer accepted campaign key material"
fi
! /usr/bin/grep -Fq 'campaign_secret' "$MANIFEST" \
    || fail "pre-secret tool manifest contains a campaign secret field"
! /usr/bin/awk -F '\t' \
    '$1 == "hmac" || $1 ~ /_hmac$/ || $1 ~ /manifest_hmac/ { found = 1 } \
        END { exit(found ? 0 : 1) }' \
    "$MANIFEST" \
    || fail "pre-secret tool manifest claims unauthenticated HMAC evidence"

if "$FREEZER" --source-commit "$SOURCE_COMMIT" --output-directory "$BUNDLE" \
    >/dev/null 2>&1; then
    fail "tool freezer overwrote an existing bundle"
fi

ALTERED_FREEZER_REPOSITORY="$TEMP_ROOT/altered-freezer-repository"
make_fixture_repository "$ALTERED_FREEZER_REPOSITORY"
ALTERED_FREEZER_COMMIT="$(/usr/bin/git -C "$ALTERED_FREEZER_REPOSITORY" rev-parse HEAD)"
printf '\n# altered bootstrap fixture\n' >> \
    "$ALTERED_FREEZER_REPOSITORY/scripts/acceptance/freeze-render-profile-tool-bundle.sh"
ALTERED_FREEZER_OUTPUT="$TEMP_ROOT/altered-freezer-bundle"
if "$ALTERED_FREEZER_REPOSITORY/scripts/acceptance/freeze-render-profile-tool-bundle.sh" \
    --source-commit "$ALTERED_FREEZER_COMMIT" \
    --output-directory "$ALTERED_FREEZER_OUTPUT" >/dev/null 2>&1; then
    fail "tool freezer accepted modified executing bootstrap bytes"
fi
[[ ! -e "$ALTERED_FREEZER_OUTPUT" ]] \
    || fail "modified tool freezer created output before self-verification"

DIRTY_REPOSITORY="$TEMP_ROOT/dirty-repository"
make_fixture_repository "$DIRTY_REPOSITORY"
DIRTY_COMMIT="$(/usr/bin/git -C "$DIRTY_REPOSITORY" rev-parse HEAD)"
printf '\n# dirty fixture\n' >> \
    "$DIRTY_REPOSITORY/scripts/acceptance/render-profile-hmac.py"
if "$DIRTY_REPOSITORY/scripts/acceptance/freeze-render-profile-tool-bundle.sh" \
    --source-commit "$DIRTY_COMMIT" --output-directory "$TEMP_ROOT/dirty-bundle" \
    >/dev/null 2>&1; then
    fail "tool freezer accepted a dirty helper source"
fi

STAGED_REPOSITORY="$TEMP_ROOT/staged-repository"
make_fixture_repository "$STAGED_REPOSITORY"
STAGED_COMMIT="$(/usr/bin/git -C "$STAGED_REPOSITORY" rev-parse HEAD)"
printf '\n# staged fixture\n' >> \
    "$STAGED_REPOSITORY/scripts/acceptance/render-profile-hmac.py"
/usr/bin/git -C "$STAGED_REPOSITORY" add -- scripts/acceptance/render-profile-hmac.py
if "$STAGED_REPOSITORY/scripts/acceptance/freeze-render-profile-tool-bundle.sh" \
    --source-commit "$STAGED_COMMIT" --output-directory "$TEMP_ROOT/staged-bundle" \
    >/dev/null 2>&1; then
    fail "tool freezer accepted a staged helper source"
fi

UNTRACKED_REPOSITORY="$TEMP_ROOT/untracked-repository"
make_fixture_repository "$UNTRACKED_REPOSITORY"
/usr/bin/git -C "$UNTRACKED_REPOSITORY" rm --cached -q -- \
    scripts/acceptance/render-profile-hmac.py
/usr/bin/git -C "$UNTRACKED_REPOSITORY" commit -qm 'test: make helper untracked'
UNTRACKED_COMMIT="$(/usr/bin/git -C "$UNTRACKED_REPOSITORY" rev-parse HEAD)"
if "$UNTRACKED_REPOSITORY/scripts/acceptance/freeze-render-profile-tool-bundle.sh" \
    --source-commit "$UNTRACKED_COMMIT" --output-directory "$TEMP_ROOT/untracked-bundle" \
    >/dev/null 2>&1; then
    fail "tool freezer accepted an untracked helper source"
fi

REPLACED_REPOSITORY="$TEMP_ROOT/replaced-repository"
make_fixture_repository "$REPLACED_REPOSITORY"
ORIGINAL_COMMIT="$(/usr/bin/git -C "$REPLACED_REPOSITORY" rev-parse HEAD)"
printf '\n# hostile replacement blob\n' >> \
    "$REPLACED_REPOSITORY/scripts/acceptance/render-profile-hmac.py"
/usr/bin/git -C "$REPLACED_REPOSITORY" add -- scripts/acceptance/render-profile-hmac.py
/usr/bin/git -C "$REPLACED_REPOSITORY" commit -qm 'test: create hostile replacement commit'
REPLACEMENT_COMMIT="$(/usr/bin/git -C "$REPLACED_REPOSITORY" rev-parse HEAD)"
CURRENT_BRANCH="$(/usr/bin/git -C "$REPLACED_REPOSITORY" symbolic-ref HEAD)"
/usr/bin/git -C "$REPLACED_REPOSITORY" restore --source "$ORIGINAL_COMMIT" \
    --staged --worktree -- scripts/acceptance/render-profile-hmac.py
/usr/bin/git -C "$REPLACED_REPOSITORY" update-ref "$CURRENT_BRANCH" "$ORIGINAL_COMMIT"
/usr/bin/git -C "$REPLACED_REPOSITORY" replace "$ORIGINAL_COMMIT" "$REPLACEMENT_COMMIT"
REPLACED_BUNDLE="$TEMP_ROOT/replaced-bundle"
"$REPLACED_REPOSITORY/scripts/acceptance/freeze-render-profile-tool-bundle.sh" \
    --source-commit "$ORIGINAL_COMMIT" --output-directory "$REPLACED_BUNDLE" \
    >/dev/null
REPLACED_MANIFEST="$REPLACED_BUNDLE/tool-bundle-manifest.tsv"
ORIGINAL_BLOB_SHA="$(/usr/bin/git --no-replace-objects -C "$REPLACED_REPOSITORY" \
    show "$ORIGINAL_COMMIT:scripts/acceptance/render-profile-hmac.py" \
    | /usr/bin/shasum -a 256 | /usr/bin/awk '{ print $1 }')"
HOSTILE_BLOB_SHA="$(/usr/bin/git -C "$REPLACED_REPOSITORY" \
    show "$ORIGINAL_COMMIT:scripts/acceptance/render-profile-hmac.py" \
    | /usr/bin/shasum -a 256 | /usr/bin/awk '{ print $1 }')"
[[ "$ORIGINAL_BLOB_SHA" != "$HOSTILE_BLOB_SHA" \
    && "$(kv "$REPLACED_MANIFEST" render_profile_hmac_source_sha256)" \
        == "$ORIGINAL_BLOB_SHA" \
    && "$(sha256 "$REPLACED_BUNDLE/scripts/acceptance/render-profile-hmac.py")" \
        == "$ORIGINAL_BLOB_SHA" ]] \
    || fail "tool freezer honored a hostile Git replacement object"

echo "render profile tool bundle fixtures passed"
