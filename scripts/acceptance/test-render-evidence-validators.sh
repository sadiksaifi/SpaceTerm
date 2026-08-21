#!/bin/bash

set -euo pipefail
IFS=$'\n\t'
export LC_ALL=C

SCRIPT_DIRECTORY="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIRECTORY
readonly ARCHIVE_VALIDATOR="$SCRIPT_DIRECTORY/verify-render-trace-archive.py"
readonly VIDEO_VALIDATOR="$SCRIPT_DIRECTORY/verify-render-action-video.py"
TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/render-evidence-validators.XXXXXX")"

cleanup() {
    rm -rf -- "$TEMP_ROOT"
}
trap cleanup EXIT INT TERM

fail_test() {
    printf 'FAIL: %s\n' "$1" >&2
    exit 1
}

metric() {
    local file="$1"
    local key="$2"
    awk -F '\t' -v wanted="$key" '$1 == wanted { count += 1; value = $2 } \
        END { if (count == 1) print value }' "$file"
}

expect_result() {
    local expected_status="$1"
    local expected_result="$2"
    local expected_reason="$3"
    local output="$4"
    shift 4
    local status=0
    set +e
    "$@" > "$output"
    status=$?
    set -e
    [[ "$status" == "$expected_status" ]] \
        || fail_test "expected status $expected_status, got $status for $expected_reason"
    [[ "$(metric "$output" result)" == "$expected_result" ]] \
        || fail_test "expected $expected_result for $expected_reason"
    [[ "$(metric "$output" reason)" == "$expected_reason" ]] \
        || fail_test "wrong reason for $expected_reason: $(metric "$output" reason)"
}

make_zip() {
    local output="$1"
    local kind="$2"
    local trace_id="${3:-alpha}"
    python3 - "$output" "$kind" "$trace_id" <<'PY'
from pathlib import Path
import stat
import sys
import warnings
import zipfile

output, kind, trace_id = Path(sys.argv[1]), sys.argv[2], sys.argv[3]
warnings.filterwarnings("ignore", message="Duplicate name:.*")
with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_DEFLATED) as archive:
    if kind == "valid":
        archive.writestr("Capture.trace/data/id", trace_id)
    elif kind == "absolute":
        archive.writestr("/Capture.trace/data/id", trace_id)
    elif kind == "traversal":
        archive.writestr("Capture.trace/../escape", trace_id)
    elif kind == "sibling":
        archive.writestr("Capture.trace/data/id", trace_id)
        archive.writestr("sibling.txt", "bad")
    elif kind == "wrapper":
        archive.writestr("wrapper/Capture.trace/data/id", trace_id)
    elif kind == "duplicate":
        archive.writestr("Capture.trace/data/id", trace_id)
        archive.writestr("Capture.trace/data/id", trace_id)
    elif kind == "symlink":
        link = zipfile.ZipInfo("Capture.trace/data/link")
        link.create_system = 3
        link.external_attr = (stat.S_IFLNK | 0o777) << 16
        archive.writestr(link, "elsewhere")
    elif kind == "special":
        fifo = zipfile.ZipInfo("Capture.trace/data/fifo")
        fifo.create_system = 3
        fifo.external_attr = (stat.S_IFIFO | 0o600) << 16
        archive.writestr(fifo, "not-a-file")
    elif kind == "ratio":
        archive.writestr("Capture.trace/data/zeros", b"\0" * (2 * 1024 * 1024))
    else:
        raise SystemExit(f"unknown fixture kind: {kind}")
PY
}

write_expected_exports() {
    local directory="$1"
    local trace_id="$2"
    mkdir -p -- "$directory"
    printf '<toc trace="%s"/>\n' "$trace_id" > "$directory/toc.xml"
    printf '<time-profile trace="%s"/>\n' "$trace_id" > "$directory/time-profile.xml"
    printf '<allocations trace="%s"/>\n' "$trace_id" > "$directory/allocations.xml"
    printf '<hangs trace="%s"/>\n' "$trace_id" > "$directory/hangs.xml"
    printf 'format_version\t1\ntrace_id\t%s\n' "$trace_id" > "$directory/verification.tsv"
}

FAKE_XCRUN="$TEMP_ROOT/fake-xcrun"
# The test fixture executable is generated at runtime; keep its behavior visible here.
python3 - "$FAKE_XCRUN" <<'PY'
from pathlib import Path
import sys

Path(sys.argv[1]).write_text(r'''#!/bin/bash
set -euo pipefail
[[ "$1" == xctrace && "$2" == export ]]
shift 2
input=""
output=""
mode=""
xpath=""
while (( $# > 0 )); do
    case "$1" in
        --input) input="$2"; shift ;;
        --output) output="$2"; shift ;;
        --toc) mode=toc ;;
        --xpath) mode=xpath; xpath="$2"; shift ;;
        *) exit 64 ;;
    esac
    shift
done
[[ -n "$input" && -n "$output" && -n "$mode" ]]
trace_id="$(<"$input/data/id")"
if [[ "$mode" == toc ]]; then
    printf '<toc trace="%s"/>\n' "$trace_id" > "$output"
else
    case "$xpath" in
        '/trace-toc/run[@number="1"]/data/table[@schema="time-profile"]')
            printf '<time-profile trace="%s"/>\n' "$trace_id" > "$output"
            ;;
        '/trace-toc/run[@number="1"]/tracks/track[@name="Allocations"]/details/detail[@name="Allocations List"]')
            printf '<allocations trace="%s"/>\n' "$trace_id" > "$output"
            ;;
        '/trace-toc/run[@number="1"]/data/table[@schema="potential-hangs"]')
            printf '<hangs trace="%s"/>\n' "$trace_id" > "$output"
            ;;
        *) exit 65 ;;
    esac
fi
''')
PY
chmod +x "$FAKE_XCRUN"

FAKE_VERIFIER="$TEMP_ROOT/fake-verifier"
python3 - "$FAKE_VERIFIER" <<'PY'
from pathlib import Path
import sys

Path(sys.argv[1]).write_text(r'''#!/usr/bin/python3
import argparse
import re
from pathlib import Path

parser = argparse.ArgumentParser()
for name in ("toc", "time-profile", "allocations", "hangs", "pid",
             "process-name", "requested-seconds", "command-elapsed-seconds"):
    parser.add_argument(f"--{name}", required=True)
arguments = parser.parse_args()
toc_match = re.search(r'trace="([^"]+)"', Path(arguments.toc).read_text())
if toc_match is None:
    raise SystemExit(1)
trace_id = toc_match.group(1)
for path, tag in (
    (arguments.time_profile, "time-profile"),
    (arguments.allocations, "allocations"),
    (arguments.hangs, "hangs"),
):
    text = Path(path).read_text()
    if not text.startswith(f"<{tag} ") or f'trace="{trace_id}"' not in text:
        raise SystemExit(1)
print("format_version\t1")
print(f"trace_id\t{trace_id}")
''')
PY
chmod +x "$FAKE_VERIFIER"

archive_command() {
    local archive="$1"
    local output_directory="$2"
    local expected_directory="$3"
    shift 3
    local -a test_disk_override=(--maximum-members 250000)
    if [[ -n "${TEST_AVAILABLE_DISK_BYTES:-}" ]]; then
        test_disk_override=(
            --available-disk-bytes-for-testing "$TEST_AVAILABLE_DISK_BYTES"
        )
    fi
    "$ARCHIVE_VALIDATOR" \
        --archive "$archive" \
        --output-directory "$output_directory" \
        --xcrun "$FAKE_XCRUN" \
        --toc "$expected_directory/toc.xml" \
        --time-profile "$expected_directory/time-profile.xml" \
        --allocations "$expected_directory/allocations.xml" \
        --hangs "$expected_directory/hangs.xml" \
        --trace-verifier "$FAKE_VERIFIER" \
        --python /usr/bin/python3 \
        --verification "$expected_directory/verification.tsv" \
        --pid 123 \
        --process-name SpaceTerm \
        --requested-seconds 120 \
        --command-elapsed-seconds 121 \
        "${test_disk_override[@]}" \
        "$@"
}

EXPECTED_ALPHA="$TEMP_ROOT/expected-alpha"
EXPECTED_BETA="$TEMP_ROOT/expected-beta"
write_expected_exports "$EXPECTED_ALPHA" alpha
write_expected_exports "$EXPECTED_BETA" beta
VALID_ZIP="$TEMP_ROOT/valid.zip"
make_zip "$VALID_ZIP" valid alpha
expect_result 0 PASS trace-archive-and-regenerated-exports-verified \
    "$TEMP_ROOT/valid.out" archive_command "$VALID_ZIP" \
    "$TEMP_ROOT/validate-valid" "$EXPECTED_ALPHA"

for unsafe_kind in absolute traversal sibling wrapper duplicate symlink special ratio; do
    unsafe_zip="$TEMP_ROOT/$unsafe_kind.zip"
    make_zip "$unsafe_zip" "$unsafe_kind"
    case "$unsafe_kind" in
        absolute|traversal) expected_reason=trace-archive-member-path-unsafe ;;
        sibling) expected_reason=trace-archive-has-sibling-members ;;
        wrapper) expected_reason=trace-archive-does-not-have-one-trace-root ;;
        duplicate) expected_reason=trace-archive-has-duplicate-members ;;
        symlink) expected_reason=trace-archive-member-is-symlink ;;
        special) expected_reason=trace-archive-member-is-special ;;
        ratio) expected_reason=trace-archive-member-compression-ratio-exceeds-limit ;;
    esac
    expect_result 2 NOT-RUN "$expected_reason" "$TEMP_ROOT/$unsafe_kind.out" \
        archive_command "$unsafe_zip" "$TEMP_ROOT/validate-$unsafe_kind" \
        "$EXPECTED_ALPHA"
done

expect_result 2 NOT-RUN trace-archive-uncompressed-bytes-exceed-limit \
    "$TEMP_ROOT/bytes.out" archive_command "$VALID_ZIP" \
    "$TEMP_ROOT/validate-bytes" "$EXPECTED_ALPHA" \
    --maximum-uncompressed-bytes 2
expect_result 2 NOT-RUN trace-archive-bytes-exceed-limit \
    "$TEMP_ROOT/archive-bytes.out" archive_command "$VALID_ZIP" \
    "$TEMP_ROOT/validate-archive-bytes" "$EXPECTED_ALPHA" \
    --maximum-archive-bytes 1

TEST_AVAILABLE_DISK_BYTES=64
export SPACETERM_RENDER_PROFILE_TEST_OVERRIDES=1
expect_result 2 NOT-RUN trace-archive-insufficient-extraction-disk-headroom \
    "$TEMP_ROOT/disk-headroom.out" archive_command "$VALID_ZIP" \
    "$TEMP_ROOT/validate-disk-headroom" "$EXPECTED_ALPHA" \
    --extraction-safety-reserve-bytes 64
unset TEST_AVAILABLE_DISK_BYTES SPACETERM_RENDER_PROFILE_TEST_OVERRIDES

REGENERATED_FIXTURE_BYTES=0
for regenerated_fixture in \
    "$EXPECTED_ALPHA/toc.xml" \
    "$EXPECTED_ALPHA/time-profile.xml" \
    "$EXPECTED_ALPHA/allocations.xml" \
    "$EXPECTED_ALPHA/hangs.xml" \
    "$EXPECTED_ALPHA/verification.tsv"; do
    regenerated_fixture_bytes="$(wc -c < "$regenerated_fixture")"
    REGENERATED_FIXTURE_BYTES=$((
        REGENERATED_FIXTURE_BYTES + regenerated_fixture_bytes
    ))
done
# The archive declares five bytes. This amount covers extraction plus the reserve,
# but is one byte short once the known regenerated artifacts are included.
TEST_AVAILABLE_DISK_BYTES=$((5 + 64 + REGENERATED_FIXTURE_BYTES - 1))
export SPACETERM_RENDER_PROFILE_TEST_OVERRIDES=1
expect_result 2 NOT-RUN trace-archive-insufficient-extraction-disk-headroom \
    "$TEMP_ROOT/export-disk-headroom.out" archive_command "$VALID_ZIP" \
    "$TEMP_ROOT/validate-export-disk-headroom" "$EXPECTED_ALPHA" \
    --extraction-safety-reserve-bytes 64
unset TEST_AVAILABLE_DISK_BYTES SPACETERM_RENDER_PROFILE_TEST_OVERRIDES

SWAPPED_ZIP="$TEMP_ROOT/swapped.zip"
make_zip "$SWAPPED_ZIP" valid beta
expect_result 2 NOT-RUN time-profile-export-does-not-match-archive \
    "$TEMP_ROOT/swapped.out" archive_command "$SWAPPED_ZIP" \
    "$TEMP_ROOT/validate-swapped" "$EXPECTED_ALPHA" \
    --toc "$EXPECTED_BETA/toc.xml"

FAKE_FFPROBE="$TEMP_ROOT/fake-ffprobe"
python3 - "$FAKE_FFPROBE" <<'PY'
from pathlib import Path
import sys

Path(sys.argv[1]).write_text(r'''#!/bin/bash
set -euo pipefail
video="${!#}"
mode="$(<"$video")"
if [[ " $* " == *" -of json "* ]]; then
    case "$mode" in
        audio-only)
            printf '{"format":{"duration":"120.5"},"streams":[{"index":0,"codec_type":"audio"}]}\n'
            ;;
        zero-frame)
            printf '{"format":{"duration":"120.5"},"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"duration":"120.5","nb_read_frames":"0","nb_read_packets":"3600","disposition":{"attached_pic":0}}]}\n'
            ;;
        sparse)
            printf '{"format":{"duration":"120.5"},"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"duration":"120.5","nb_read_frames":"2","nb_read_packets":"2","disposition":{"attached_pic":0}}]}\n'
            ;;
        clustered|invalid-timestamp|missing-timestamp|long-terminal-frame|oversized-timeline)
            printf '{"format":{"duration":"120.5"},"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"duration":"120.5","nb_read_frames":"600","nb_read_packets":"600","disposition":{"attached_pic":0}}]}\n'
            ;;
        count-mismatch)
            printf '{"format":{"duration":"120.5"},"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"duration":"120.5","nb_read_frames":"602","nb_read_packets":"602","disposition":{"attached_pic":0}}]}\n'
            ;;
        invalid-dimensions)
            printf '{"format":{"duration":"120.5"},"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":0,"height":1080,"duration":"120.5","nb_read_frames":"3600","nb_read_packets":"3600","disposition":{"attached_pic":0}}]}\n'
            ;;
        *)
            printf '{"format":{"duration":"120.5"},"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"duration":"120.5","nb_read_frames":"601","nb_read_packets":"601","disposition":{"attached_pic":0}}]}\n'
            ;;
    esac
    exit 0
fi
interval=""
while (( $# > 0 )); do
    if [[ "$1" == -read_intervals ]]; then interval="$2"; break; fi
    shift
done
if [[ "$mode" == decode-error ]]; then
    exit 65
elif [[ "$interval" == "%+2" ]]; then
    printf 'media_type=video|best_effort_timestamp_time=0.000|pkt_duration_time=0.033|width=1920|height=1080\n'
    printf 'media_type=video|best_effort_timestamp_time=1.000|pkt_duration_time=0.033|width=1920|height=1080\n'
elif [[ "$mode" == short-span ]]; then
    printf 'media_type=video|best_effort_timestamp_time=10.000|pkt_duration_time=0.033|width=1920|height=1080\n'
elif [[ -z "$interval" && "$mode" == clustered ]]; then
    awk 'BEGIN {
        for (frame = 0; frame < 300; frame++) {
            printf "media_type=video|best_effort_timestamp_time=%.3f|pkt_duration_time=0.003|width=1920|height=1080\n", frame / 150
        }
        for (frame = 0; frame < 300; frame++) {
            printf "media_type=video|best_effort_timestamp_time=%.3f|pkt_duration_time=0.003|width=1920|height=1080\n", 118 + frame / 150
        }
    }'
elif [[ -z "$interval" && "$mode" == invalid-timestamp ]]; then
    printf 'media_type=video|best_effort_timestamp_time=0.000|pkt_duration_time=0.033|width=1920|height=1080\n'
    printf 'media_type=video|best_effort_timestamp_time=unknown|pkt_duration_time=0.033|width=1920|height=1080\n'
    printf 'media_type=video|best_effort_timestamp_time=120.000|pkt_duration_time=0.033|width=1920|height=1080\n'
elif [[ -z "$interval" && "$mode" == missing-timestamp ]]; then
    printf 'media_type=video|best_effort_timestamp_time=0.000|pkt_duration_time=0.033|width=1920|height=1080\n'
    printf 'media_type=video|best_effort_timestamp_time=N/A|pts_time=N/A|pkt_dts_time=N/A|pkt_duration_time=0.033|width=1920|height=1080\n'
    printf 'media_type=video|best_effort_timestamp_time=120.000|pkt_duration_time=0.033|width=1920|height=1080\n'
elif [[ -z "$interval" && "$mode" == long-terminal-frame ]]; then
    awk 'BEGIN {
        for (frame = 0; frame < 599; frame++) {
            printf "media_type=video|best_effort_timestamp_time=%.3f|pkt_duration_time=0.100|width=1920|height=1080\n", frame / 10
        }
        print "media_type=video|best_effort_timestamp_time=60.000|pkt_duration_time=60.000|width=1920|height=1080"
    }'
elif [[ -z "$interval" && "$mode" == oversized-timeline ]]; then
    printf 'media_type=video|best_effort_timestamp_time=0.000|pkt_duration_time=0.033|width=1920|height=1080\n'
    printf 'media_type=video|best_effort_timestamp_time=40.000|pkt_duration_time=0.033|width=1920|height=1080\n'
    printf 'media_type=video|best_effort_timestamp_time=80.000|pkt_duration_time=0.033|width=1920|height=1080\n'
    printf 'media_type=video|best_effort_timestamp_time=120.000|pkt_duration_time=0.033|width=1920|height=1080\n'
elif [[ -z "$interval" ]]; then
    awk 'BEGIN {
        for (frame = 0; frame <= 600; frame++) {
            printf "media_type=video|best_effort_timestamp_time=%.3f|pkt_duration_time=0.033|width=1920|height=1080\n", frame / 5
        }
    }'
else
    printf 'media_type=video|best_effort_timestamp_time=119.967|pkt_duration_time=0.033|width=1920|height=1080\n'
fi
''')
PY
chmod +x "$FAKE_FFPROBE"

video_command() {
    local video="$1"
    shift
    "$VIDEO_VALIDATOR" \
        --video "$video" \
        --ffprobe "$FAKE_FFPROBE" \
        --minimum-duration-ms 120000 \
        --maximum-duration-ms 180000 \
        "$@"
}

for video_kind in valid audio-only zero-frame sparse clustered invalid-timestamp missing-timestamp long-terminal-frame count-mismatch invalid-dimensions short-span decode-error oversized-timeline; do
    printf '%s\n' "$video_kind" > "$TEMP_ROOT/$video_kind.mov"
done
expect_result 0 PASS render-action-video-stream-and-duration-verified \
    "$TEMP_ROOT/video-valid.out" video_command "$TEMP_ROOT/valid.mov"
expect_result 2 NOT-RUN render-action-video-has-no-usable-video-stream \
    "$TEMP_ROOT/video-audio.out" video_command "$TEMP_ROOT/audio-only.mov"
expect_result 2 NOT-RUN render-action-video-has-no-decodable-video-frames \
    "$TEMP_ROOT/video-zero.out" video_command "$TEMP_ROOT/zero-frame.mov"
expect_result 2 NOT-RUN render-action-video-frame-cadence-is-too-sparse \
    "$TEMP_ROOT/video-sparse.out" video_command "$TEMP_ROOT/sparse.mov"
expect_result 2 NOT-RUN render-action-video-frame-continuity-gap-exceeds-limit \
    "$TEMP_ROOT/video-clustered.out" video_command "$TEMP_ROOT/clustered.mov"
expect_result 2 NOT-RUN render-action-video-frame-timestamp-invalid \
    "$TEMP_ROOT/video-invalid-timestamp.out" video_command "$TEMP_ROOT/invalid-timestamp.mov"
expect_result 2 NOT-RUN render-action-video-frame-timestamp-missing \
    "$TEMP_ROOT/video-missing-timestamp.out" video_command "$TEMP_ROOT/missing-timestamp.mov"
expect_result 2 NOT-RUN render-action-video-frame-duration-exceeds-continuity-limit \
    "$TEMP_ROOT/video-long-terminal-frame.out" \
    video_command "$TEMP_ROOT/long-terminal-frame.mov"
expect_result 2 NOT-RUN render-action-video-decoded-frame-count-mismatch \
    "$TEMP_ROOT/video-count-mismatch.out" video_command "$TEMP_ROOT/count-mismatch.mov"
expect_result 2 NOT-RUN render-action-video-has-no-usable-video-stream \
    "$TEMP_ROOT/video-dimensions.out" video_command "$TEMP_ROOT/invalid-dimensions.mov"
expect_result 2 NOT-RUN render-action-video-decoded-stream-does-not-span-required-duration \
    "$TEMP_ROOT/video-span.out" video_command "$TEMP_ROOT/short-span.mov"
expect_result 2 NOT-RUN render-action-video-first-frames-cannot-be-decoded \
    "$TEMP_ROOT/video-decode.out" video_command "$TEMP_ROOT/decode-error.mov"
expect_result 2 NOT-RUN render-action-video-full-stream-output-exceeds-limit \
    "$TEMP_ROOT/video-timeline-lines.out" video_command \
    "$TEMP_ROOT/oversized-timeline.mov" --maximum-full-timeline-lines 3
expect_result 2 NOT-RUN render-action-video-full-stream-output-exceeds-limit \
    "$TEMP_ROOT/video-timeline-bytes.out" video_command \
    "$TEMP_ROOT/oversized-timeline.mov" --maximum-full-timeline-bytes 128

printf 'render evidence validator fixtures passed\n'
