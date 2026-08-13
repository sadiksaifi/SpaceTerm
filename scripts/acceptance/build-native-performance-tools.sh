#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C
umask 077

RUN_DIRECTORY=""
OUTPUT_DIRECTORY=""
ARCHITECTURE=""
TEMP_DIRECTORY=""

usage() {
    cat <<EOF
Usage: $(basename -- "$0") --run-directory ABSOLUTE_DIRECTORY \\
  --output-directory ABSENT_DIRECT_CHILD --architecture arm64|x86_64

Build the issue #43 native performance producer, driver, RSS sampler, and
authenticated window resolver into an absent owner-private directory inside
one acceptance run. The repository is never used as a binary output path.
Compiler, SDK, flags, sources, and binaries are hashed into immutable metadata.
EOF
}

die() { echo "error: $*" >&2; exit 1; }
sha256() { shasum -a 256 "$1" | awk '{ print $1 }'; }
one_line() { tr '\t\r\n' '   ' | awk '{$1=$1; print}'; }

cleanup() {
    [[ -z "$TEMP_DIRECTORY" ]] || rm -rf -- "$TEMP_DIRECTORY"
}

while (( $# > 0 )); do
    case "$1" in
        --run-directory) RUN_DIRECTORY="${2:-}"; shift ;;
        --output-directory) OUTPUT_DIRECTORY="${2:-}"; shift ;;
        --architecture) ARCHITECTURE="${2:-}"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown argument: $1" ;;
    esac
    shift
done

[[ "$RUN_DIRECTORY" == /* && -d "$RUN_DIRECTORY" && ! -L "$RUN_DIRECTORY" ]] \
    || die "--run-directory must be an absolute non-symlink directory"
[[ "$OUTPUT_DIRECTORY" == /* && ! -e "$OUTPUT_DIRECTORY" && ! -L "$OUTPUT_DIRECTORY" ]] \
    || die "--output-directory must be an absent absolute path"
[[ "$ARCHITECTURE" == arm64 || "$ARCHITECTURE" == x86_64 ]] \
    || die "--architecture must be arm64 or x86_64"
for command in awk chmod dirname mktemp mv realpath rm shasum xcrun; do
    command -v "$command" >/dev/null 2>&1 || die "required command not found: $command"
done

RUN_DIRECTORY="$(realpath "$RUN_DIRECTORY")"
output_parent="$(dirname -- "$OUTPUT_DIRECTORY")"
[[ -d "$output_parent" && ! -L "$output_parent" ]] \
    || die "output parent must be an existing non-symlink directory"
output_parent="$(realpath "$output_parent")"
OUTPUT_DIRECTORY="$output_parent/$(basename -- "$OUTPUT_DIRECTORY")"
[[ "$output_parent" == "$RUN_DIRECTORY" ]] \
    || die "output directory must be a direct child of the run directory"
[[ "$OUTPUT_DIRECTORY" == "$RUN_DIRECTORY/"* ]] \
    || die "output directory escaped the run directory"

SCRIPT_DIRECTORY="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIRECTORY
readonly DEPLOYMENT_TARGET=11.0
BUILDER_PATH="$SCRIPT_DIRECTORY/$(basename -- "$0")"
readonly BUILDER_PATH
readonly WORKLOAD_SOURCE="$SCRIPT_DIRECTORY/performance-workload.c"
readonly DRIVER_SOURCE="$SCRIPT_DIRECTORY/performance-driver.m"
readonly RSS_SOURCE="$SCRIPT_DIRECTORY/performance-rss-sampler.m"
readonly WINDOW_SOURCE="$SCRIPT_DIRECTORY/performance-window-resolver.m"
readonly TERMINATOR_SOURCE="$SCRIPT_DIRECTORY/performance-appkit-terminate.m"
for source in "$WORKLOAD_SOURCE" "$DRIVER_SOURCE" "$RSS_SOURCE" "$WINDOW_SOURCE" \
    "$TERMINATOR_SOURCE"; do
    [[ -f "$source" && ! -L "$source" ]] || die "source must be a non-symlink regular file: $source"
done

COMPILER="$(xcrun --sdk macosx --find clang)"
SDK_PATH="$(xcrun --sdk macosx --show-sdk-path)"
SDK_VERSION="$(xcrun --sdk macosx --show-sdk-version)"
readonly COMPILER SDK_PATH SDK_VERSION
[[ "$COMPILER" == /* && -x "$COMPILER" && -f "$COMPILER" ]] \
    || die "xcrun did not resolve an executable compiler"
[[ "$SDK_PATH" == /* && -d "$SDK_PATH" ]] || die "xcrun did not resolve a macOS SDK"
COMPILER_VERSION="$("$COMPILER" --version | one_line)"
COMPILER_SHA256="$(sha256 "$COMPILER")"
BUILDER_SHA256="$(sha256 "$BUILDER_PATH")"
WORKLOAD_SOURCE_SHA256="$(sha256 "$WORKLOAD_SOURCE")"
DRIVER_SOURCE_SHA256="$(sha256 "$DRIVER_SOURCE")"
RSS_SOURCE_SHA256="$(sha256 "$RSS_SOURCE")"
WINDOW_SOURCE_SHA256="$(sha256 "$WINDOW_SOURCE")"
TERMINATOR_SOURCE_SHA256="$(sha256 "$TERMINATOR_SOURCE")"
readonly COMPILER_VERSION COMPILER_SHA256 BUILDER_SHA256
readonly WORKLOAD_SOURCE_SHA256 DRIVER_SOURCE_SHA256 RSS_SOURCE_SHA256
readonly WINDOW_SOURCE_SHA256
readonly TERMINATOR_SOURCE_SHA256

TEMP_DIRECTORY="$(mktemp -d "$RUN_DIRECTORY/.native-performance-tools.XXXXXX")"
trap cleanup EXIT INT TERM HUP

# shellcheck disable=SC2054
declare -a common_flags=(
    -O2 -g0 -Wall -Wextra -Werror -Wpedantic
    -arch "$ARCHITECTURE" -isysroot "$SDK_PATH"
    "-mmacosx-version-min=$DEPLOYMENT_TARGET" -Wl,-dead_strip
)
declare -a objc_flags=(-fobjc-arc -fblocks)
declare -a objc_frameworks=(
    -framework AppKit -framework ApplicationServices -framework Security
)
declare -a driver_libraries=(-lbsm)

"$COMPILER" -std=c17 "${common_flags[@]}" \
    "$WORKLOAD_SOURCE" -o "$TEMP_DIRECTORY/performance-workload"
"$COMPILER" "${objc_flags[@]}" "${common_flags[@]}" \
    "$DRIVER_SOURCE" "${objc_frameworks[@]}" "${driver_libraries[@]}" \
    -o "$TEMP_DIRECTORY/performance-driver"
"$COMPILER" "${objc_flags[@]}" "${common_flags[@]}" \
    "$RSS_SOURCE" "${objc_frameworks[@]}" -o "$TEMP_DIRECTORY/performance-rss-sampler"
"$COMPILER" "${objc_flags[@]}" "${common_flags[@]}" \
    "$WINDOW_SOURCE" "${objc_frameworks[@]}" -o "$TEMP_DIRECTORY/performance-window-resolver"
"$COMPILER" "${objc_flags[@]}" "${common_flags[@]}" \
    "$TERMINATOR_SOURCE" -framework AppKit -framework Foundation \
    -o "$TEMP_DIRECTORY/performance-appkit-terminate"

[[ "$(sha256 "$WORKLOAD_SOURCE")" == "$WORKLOAD_SOURCE_SHA256" \
    && "$(sha256 "$DRIVER_SOURCE")" == "$DRIVER_SOURCE_SHA256" \
    && "$(sha256 "$RSS_SOURCE")" == "$RSS_SOURCE_SHA256" \
    && "$(sha256 "$WINDOW_SOURCE")" == "$WINDOW_SOURCE_SHA256" \
    && "$(sha256 "$TERMINATOR_SOURCE")" == "$TERMINATOR_SOURCE_SHA256" \
    && "$(sha256 "$BUILDER_PATH")" == "$BUILDER_SHA256" \
    && "$(sha256 "$COMPILER")" == "$COMPILER_SHA256" ]] \
    || die "compiler, builder, or source changed during the build"

for binary in performance-workload performance-driver performance-rss-sampler \
    performance-window-resolver performance-appkit-terminate; do
    [[ -f "$TEMP_DIRECTORY/$binary" && -x "$TEMP_DIRECTORY/$binary" \
        && ! -L "$TEMP_DIRECTORY/$binary" ]] \
        || die "compiler did not produce the expected binary: $binary"
    chmod 0500 "$TEMP_DIRECTORY/$binary"
done

metadata="$TEMP_DIRECTORY/native-performance-tools.tsv"
{
    printf 'format_version\t1\n'
    printf 'builder_sha256\t%s\n' "$BUILDER_SHA256"
    printf 'compiler_path\t%s\n' "$COMPILER"
    printf 'compiler_sha256\t%s\n' "$COMPILER_SHA256"
    printf 'compiler_version\t%s\n' "$COMPILER_VERSION"
    printf 'sdk_path\t%s\n' "$SDK_PATH"
    printf 'sdk_version\t%s\n' "$SDK_VERSION"
    printf 'architecture\t%s\n' "$ARCHITECTURE"
    printf 'deployment_target\t%s\n' "$DEPLOYMENT_TARGET"
    printf 'c_flags\t-std=c17 -O2 -g0 -Wall -Wextra -Werror -Wpedantic -arch %s -isysroot %s -mmacosx-version-min=%s -Wl,-dead_strip\n' \
        "$ARCHITECTURE" "$SDK_PATH" "$DEPLOYMENT_TARGET"
    printf 'objc_flags\t-fobjc-arc -fblocks -O2 -g0 -Wall -Wextra -Werror -Wpedantic -arch %s -isysroot %s -mmacosx-version-min=%s -Wl,-dead_strip\n' \
        "$ARCHITECTURE" "$SDK_PATH" "$DEPLOYMENT_TARGET"
    printf 'link_frameworks\t%s\n' 'AppKit ApplicationServices Security'
    printf 'driver_link_libraries\t%s\n' 'bsm'
    printf 'performance_workload_source\t%s\n' 'scripts/acceptance/performance-workload.c'
    printf 'performance_workload_source_sha256\t%s\n' "$WORKLOAD_SOURCE_SHA256"
    printf 'performance_workload_binary\t%s\n' 'performance-workload'
    printf 'performance_workload_binary_sha256\t%s\n' \
        "$(sha256 "$TEMP_DIRECTORY/performance-workload")"
    printf 'performance_driver_source\t%s\n' 'scripts/acceptance/performance-driver.m'
    printf 'performance_driver_source_sha256\t%s\n' "$DRIVER_SOURCE_SHA256"
    printf 'performance_driver_binary\t%s\n' 'performance-driver'
    printf 'performance_driver_binary_sha256\t%s\n' \
        "$(sha256 "$TEMP_DIRECTORY/performance-driver")"
    printf 'performance_rss_sampler_source\t%s\n' 'scripts/acceptance/performance-rss-sampler.m'
    printf 'performance_rss_sampler_source_sha256\t%s\n' "$RSS_SOURCE_SHA256"
    printf 'performance_rss_sampler_binary\t%s\n' 'performance-rss-sampler'
    printf 'performance_rss_sampler_binary_sha256\t%s\n' \
        "$(sha256 "$TEMP_DIRECTORY/performance-rss-sampler")"
    printf 'performance_window_resolver_source\t%s\n' \
        'scripts/acceptance/performance-window-resolver.m'
    printf 'performance_window_resolver_source_sha256\t%s\n' "$WINDOW_SOURCE_SHA256"
    printf 'performance_window_resolver_binary\t%s\n' 'performance-window-resolver'
    printf 'performance_window_resolver_binary_sha256\t%s\n' \
        "$(sha256 "$TEMP_DIRECTORY/performance-window-resolver")"
    printf 'performance_appkit_terminator_source\t%s\n' \
        'scripts/acceptance/performance-appkit-terminate.m'
    printf 'performance_appkit_terminator_source_sha256\t%s\n' "$TERMINATOR_SOURCE_SHA256"
    printf 'performance_appkit_terminator_binary\t%s\n' 'performance-appkit-terminate'
    printf 'performance_appkit_terminator_binary_sha256\t%s\n' \
        "$(sha256 "$TEMP_DIRECTORY/performance-appkit-terminate")"
    printf 'status\tcomplete\n'
} > "$metadata"
chmod 0400 "$metadata"

mv -- "$TEMP_DIRECTORY" "$OUTPUT_DIRECTORY"
TEMP_DIRECTORY=""
trap - EXIT INT TERM HUP
printf 'tools_metadata_sha256\t%s\n' \
    "$(sha256 "$OUTPUT_DIRECTORY/native-performance-tools.tsv")"
