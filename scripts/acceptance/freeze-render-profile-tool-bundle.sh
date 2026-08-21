#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
umask 077

OUTPUT_DIRECTORY=""
SOURCE_COMMIT=""
TEMP_DIRECTORY=""

SCRIPT_DIRECTORY="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPOSITORY_ROOT="$(cd -- "$SCRIPT_DIRECTORY/../.." && pwd -P)"
readonly SCRIPT_DIRECTORY REPOSITORY_ROOT
readonly SCHEMA="spaceterm.render-profile-tool-bundle/v1"

readonly TOOL_NAMES="record_release_performance_trace
freeze_render_profile_intent
finalize_render_profile_evidence
render_profile_hmac
render_trace_receipt
analyze_release_render_profile_case
archive_render_trace
verify_render_action_video
verify_render_trace_archive
verify_release_performance_trace
inspect_release_performance_process
run_release_performance_command
freeze_render_profile_tool_bundle"
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

usage() {
    cat <<EOF
Usage: $(basename -- "$0") --source-commit REVIEWED_HEAD_COMMIT \
    --output-directory ABSENT_PRIVATE_BUNDLE

Freeze the render-profile recorder, analyzer, and repository-owned helpers into
one nonwritable campaign tool bundle while preserving their scripts/ relative
layout. This pre-secret bootstrap never accepts or reads campaign key material.
Its canonical manifest binds the externally selected source commit, source
paths and hashes, and
final bundle paths and hashes for later authenticated case-manifest binding.
EOF
}

die() { echo "error: $*" >&2; exit 1; }
sha256() { /usr/bin/shasum -a 256 "$1" | /usr/bin/awk '{ print $1 }'; }
cleanup() { [[ -z "$TEMP_DIRECTORY" ]] || /bin/rm -rf -- "$TEMP_DIRECTORY"; }
stable_identity() { /usr/bin/stat -f '%d:%i:%z:%m:%c' "$1"; }
git_safe() {
    /usr/bin/env -i \
        PATH=/usr/bin:/bin:/usr/sbin:/sbin \
        HOME=/var/empty XDG_CONFIG_HOME=/var/empty \
        GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null \
        GIT_CONFIG_SYSTEM=/dev/null GIT_NO_REPLACE_OBJECTS=1 \
        /usr/bin/git --no-replace-objects "$@"
}
committed_source_sha256() {
    local source_relative="$1" source_path tree_entry tree_metadata tree_path
    local tree_mode tree_type tree_object worktree_hash blob_hash
    source_path="$REPOSITORY_ROOT/$source_relative"
    [[ -f "$source_path" && ! -L "$source_path" && -s "$source_path" ]] \
        || die "required tool source is unavailable: $source_relative"
    git_safe -C "$REPOSITORY_ROOT" ls-files --error-unmatch -- "$source_relative" \
        >/dev/null 2>&1 \
        || die "tool source is not tracked: $source_relative"
    tree_entry="$(git_safe -C "$REPOSITORY_ROOT" ls-tree "$SOURCE_COMMIT" -- "$source_relative")"
    tree_metadata="${tree_entry%%$'\t'*}"
    tree_path="${tree_entry#*$'\t'}"
    tree_mode="${tree_metadata%% *}"
    tree_metadata="${tree_metadata#* }"
    tree_type="${tree_metadata%% *}"
    tree_object="${tree_metadata##* }"
    [[ ( "$tree_mode" == 100644 || "$tree_mode" == 100755 ) \
        && "$tree_type" == blob && "$tree_object" =~ ^[0-9a-f]{40}$ \
        && "$tree_path" == "$source_relative" ]] \
        || die "tool source is not a regular file at the selected commit: $source_relative"
    [[ -z "$(git_safe -C "$REPOSITORY_ROOT" status --porcelain=v1 \
        --untracked-files=all -- "$source_relative")" ]] \
        || die "tool source has staged, dirty, or untracked changes: $source_relative"
    worktree_hash="$(sha256 "$source_path")"
    blob_hash="$(git_safe -C "$REPOSITORY_ROOT" show \
        "$SOURCE_COMMIT:$source_relative" | /usr/bin/shasum -a 256 \
        | /usr/bin/awk '{ print $1 }')"
    [[ "$worktree_hash" == "$blob_hash" ]] \
        || die "tool source differs from the selected commit: $source_relative"
    printf '%s\n' "$blob_hash"
}

while (( $# > 0 )); do
    case "$1" in
        --source-commit) SOURCE_COMMIT="${2:-}"; shift ;;
        --output-directory) OUTPUT_DIRECTORY="${2:-}"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument: $1" ;;
    esac
    shift
done

[[ "$SOURCE_COMMIT" =~ ^[0-9a-f]{40}$ ]] \
    || die "source commit must be a lowercase 40-character SHA-1"

[[ "$OUTPUT_DIRECTORY" == /* && ! -e "$OUTPUT_DIRECTORY" ]] \
    || die "output directory must be an absent absolute path"
output_parent="$(/usr/bin/dirname -- "$OUTPUT_DIRECTORY")"
output_name="$(/usr/bin/basename -- "$OUTPUT_DIRECTORY")"
[[ -n "$output_name" && "$output_name" != . && "$output_name" != .. \
    && -d "$output_parent" && ! -L "$output_parent" ]] \
    || die "output parent is unavailable or symbolic"
physical_parent="$(cd -P -- "$output_parent" && pwd -P)"
[[ "$OUTPUT_DIRECTORY" == "$physical_parent/$output_name" ]] \
    || die "output directory must be canonical and physical"
parent_mode="$(/usr/bin/stat -f '%Lp' "$physical_parent")"
[[ "$parent_mode" =~ ^[0-7]{3,4}$ ]] || die "output parent mode is invalid"
(( (8#$parent_mode & 077) == 0 )) || die "output parent must be private"
parent_identity="$(/usr/bin/stat -f '%d:%i' "$physical_parent")"

[[ "$(git_safe -C "$REPOSITORY_ROOT" cat-file -t "$SOURCE_COMMIT")" == commit ]] \
    || die "source commit is not a commit object"
head_commit="$(git_safe -C "$REPOSITORY_ROOT" rev-parse --verify 'HEAD^{commit}')"
[[ "$head_commit" == "$SOURCE_COMMIT" ]] \
    || die "source commit must equal the repository HEAD"
self_relative="scripts/acceptance/freeze-render-profile-tool-bundle.sh"
[[ "$SCRIPT_DIRECTORY/freeze-render-profile-tool-bundle.sh" \
    == "$REPOSITORY_ROOT/$self_relative" ]] \
    || die "tool freezer must execute from its selected repository path"
self_hash="$(committed_source_sha256 "$self_relative")"
[[ "$self_hash" == "$(sha256 "${BASH_SOURCE[0]}")" ]] \
    || die "executing tool freezer differs from the selected commit"
tool_count="$(printf '%s\n' "$TOOL_NAMES" | /usr/bin/wc -l | /usr/bin/tr -d ' ')"
source_count="$(printf '%s\n' "$TOOL_SOURCES" | /usr/bin/wc -l | /usr/bin/tr -d ' ')"
[[ "$tool_count" =~ ^[1-9][0-9]*$ && "$tool_count" == "$source_count" ]] \
    || die "tool bundle source table is invalid"

TEMP_DIRECTORY="$physical_parent/.${output_name}.$$.tmp"
trap cleanup EXIT INT TERM
/bin/mkdir -m 0700 -- "$TEMP_DIRECTORY"
/bin/mkdir -m 0700 -- "$TEMP_DIRECTORY/scripts" "$TEMP_DIRECTORY/scripts/acceptance"

declare -a source_paths=()
declare -a source_hashes=()
declare -a source_identities=()
tool_index=0
while IFS= read -r source_relative; do
    tool_index=$((tool_index + 1))
    source_path="$REPOSITORY_ROOT/$source_relative"
    bundle_path="$TEMP_DIRECTORY/$source_relative"
    source_identity="$(stable_identity "$source_path")"
    source_hash="$(committed_source_sha256 "$source_relative")"
    git_safe -C "$REPOSITORY_ROOT" show "$SOURCE_COMMIT:$source_relative" \
        > "$bundle_path"
    /bin/chmod 0555 "$bundle_path"
    [[ "$source_identity" == "$(stable_identity "$source_path")" \
        && "$source_hash" == "$(sha256 "$source_path")" \
        && "$source_hash" == "$(sha256 "$bundle_path")" \
        && "$(/usr/bin/stat -f '%l' "$bundle_path")" == 1 ]] \
        || die "tool source changed or copy did not match: $source_relative"
    source_paths+=("$source_path")
    source_hashes+=("$source_hash")
    source_identities+=("$source_identity")
done <<< "$TOOL_SOURCES"

manifest="$TEMP_DIRECTORY/tool-bundle-manifest.tsv"
{
    printf 'format_version\t1\n'
    printf 'schema\t%s\n' "$SCHEMA"
    printf 'source_commit\t%s\n' "$SOURCE_COMMIT"
    printf 'tool_count\t%s\n' "$tool_count"
    tool_index=0
    while IFS= read -r tool_name; do
        source_relative="$(printf '%s\n' "$TOOL_SOURCES" | /usr/bin/sed -n "$((tool_index + 1))p")"
        printf '%s_source_path\t%s\n' "$tool_name" "${source_paths[tool_index]}"
        printf '%s_source_sha256\t%s\n' "$tool_name" "${source_hashes[tool_index]}"
        printf '%s_bundle_path\t%s\n' "$tool_name" "$OUTPUT_DIRECTORY/$source_relative"
        printf '%s_bundle_sha256\t%s\n' "$tool_name" \
            "$(sha256 "$TEMP_DIRECTORY/$source_relative")"
        tool_index=$((tool_index + 1))
    done <<< "$TOOL_NAMES"
} > "$manifest"
/bin/chmod 0444 "$manifest"

tool_index=0
while IFS= read -r source_relative; do
    source_path="${source_paths[tool_index]}"
    [[ "${source_identities[tool_index]}" == "$(stable_identity "$source_path")" \
        && "${source_hashes[tool_index]}" == "$(sha256 "$source_path")" \
        && -z "$(git_safe -C "$REPOSITORY_ROOT" status --porcelain=v1 \
            --untracked-files=all -- "$source_relative")" ]] \
        || die "tool source changed before bundle publication: $source_relative"
    tool_index=$((tool_index + 1))
done <<< "$TOOL_SOURCES"
[[ ! -e "$OUTPUT_DIRECTORY" \
    && "$(/usr/bin/stat -f '%d:%i' "$physical_parent")" == "$parent_identity" ]] \
    || die "bundle output or parent identity changed during freezing"
/bin/chmod 0555 "$TEMP_DIRECTORY/scripts/acceptance" "$TEMP_DIRECTORY/scripts"
/bin/chmod 0555 "$TEMP_DIRECTORY"
/bin/mv -- "$TEMP_DIRECTORY" "$OUTPUT_DIRECTORY"
TEMP_DIRECTORY=""
trap - EXIT INT TERM
printf 'render_profile_tool_bundle_manifest_sha256\t%s\n' \
    "$(sha256 "$OUTPUT_DIRECTORY/tool-bundle-manifest.tsv")"
