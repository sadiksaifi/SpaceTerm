#!/usr/bin/python3
"""Fail closed unless a render action video has a decodable, spanning stream."""

from __future__ import annotations

import argparse
from collections.abc import Callable
from decimal import Decimal, InvalidOperation
import json
import os
from pathlib import Path
import selectors
import stat
import subprocess
import sys
import time


FORMAT_VERSION = 1
DEFAULT_MAXIMUM_VIDEO_BYTES = 20 * 1024 * 1024 * 1024
DEFAULT_MAXIMUM_FFPROBE_OUTPUT_BYTES = 16 * 1024 * 1024
DEFAULT_MAXIMUM_FULL_TIMELINE_BYTES = 64 * 1024 * 1024
DEFAULT_MAXIMUM_FULL_TIMELINE_LINES = 100_000
READ_CHUNK_BYTES = 64 * 1024


class EvidenceError(Exception):
    """A stable, user-facing fail-closed reason."""


def verdict(result: str, reason: str, **metrics: object) -> None:
    print(f"format_version\t{FORMAT_VERSION}")
    print(f"result\t{result}")
    print(f"reason\t{reason}")
    for key, value in metrics.items():
        print(f"{key}\t{value}")


def fail(reason: str) -> None:
    raise EvidenceError(reason)


def positive_integer(value: str) -> int:
    try:
        parsed = int(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError("must be an integer") from error
    if parsed <= 0:
        raise argparse.ArgumentTypeError("must be positive")
    return parsed


def decimal_value(value: object, reason: str) -> Decimal:
    try:
        parsed = Decimal(str(value))
    except (InvalidOperation, ValueError):
        fail(reason)
    if not parsed.is_finite():
        fail(reason)
    return parsed


def positive_count(value: object) -> int:
    try:
        parsed = int(str(value))
    except ValueError:
        return 0
    return parsed if parsed > 0 else 0


def ffprobe(
    executable: str,
    arguments: list[str],
    timeout_seconds: int,
    failure_reason: str,
    maximum_output_bytes: int,
) -> bytes:
    output = bytearray()

    def collect(chunk: bytes) -> None:
        output.extend(chunk)

    run_ffprobe_stream(
        executable,
        arguments,
        timeout_seconds,
        failure_reason,
        "render-action-video-ffprobe-output-exceeds-limit",
        maximum_output_bytes,
        collect,
    )
    return bytes(output)


def run_ffprobe_stream(
    executable: str,
    arguments: list[str],
    timeout_seconds: int,
    failure_reason: str,
    limit_reason: str,
    maximum_output_bytes: int,
    consume: Callable[[bytes], None],
) -> None:
    try:
        process = subprocess.Popen(
            [executable, "-v", "error", *arguments],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )
    except OSError:
        fail(failure_reason)
    assert process.stdout is not None
    selector = selectors.DefaultSelector()
    total_bytes = 0
    deadline = time.monotonic() + timeout_seconds
    try:
        selector.register(process.stdout, selectors.EVENT_READ)
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                fail(failure_reason)
            events = selector.select(min(remaining, 0.25))
            if not events:
                if process.poll() is not None:
                    break
                continue
            chunk = os.read(process.stdout.fileno(), READ_CHUNK_BYTES)
            if not chunk:
                break
            total_bytes += len(chunk)
            if total_bytes > maximum_output_bytes:
                fail(limit_reason)
            consume(chunk)
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            fail(failure_reason)
        try:
            return_code = process.wait(timeout=remaining)
        except subprocess.TimeoutExpired:
            fail(failure_reason)
        if return_code != 0:
            fail(failure_reason)
    finally:
        selector.close()
        if process.poll() is None:
            process.kill()
            process.wait()


class TimelineAccumulator:
    def __init__(self) -> None:
        self.first: Decimal | None = None
        self.last_timestamp: Decimal | None = None
        self.last_end: Decimal | None = None
        self.previous_timestamp: Decimal | None = None
        self.maximum_gap = Decimal(0)
        self.maximum_duration = Decimal(0)
        self.count = 0

    def add_line(self, line: str) -> None:
        values: dict[str, str] = {}
        for component in line.split("|"):
            key, separator, value = component.partition("=")
            if separator:
                values[key] = value
        if values.get("media_type") != "video":
            return
        width = positive_count(values.get("width"))
        height = positive_count(values.get("height"))
        if width == 0 or height == 0:
            fail("render-action-video-decoded-frame-dimensions-invalid")
        timestamp_value = next(
            (
                values[key]
                for key in (
                    "best_effort_timestamp_time",
                    "pts_time",
                    "pkt_dts_time",
                )
                if values.get(key) not in {None, "N/A"}
            ),
            None,
        )
        if timestamp_value is None:
            fail("render-action-video-frame-timestamp-missing")
        timestamp = decimal_value(
            timestamp_value, "render-action-video-frame-timestamp-invalid"
        )
        if self.previous_timestamp is not None:
            gap = timestamp - self.previous_timestamp
            if gap < 0:
                fail("render-action-video-frame-timestamps-not-monotonic")
            self.maximum_gap = max(self.maximum_gap, gap)
        self.previous_timestamp = timestamp
        self.last_timestamp = timestamp
        duration = Decimal(0)
        if values.get("pkt_duration_time") not in {None, "N/A"}:
            duration = decimal_value(
                values["pkt_duration_time"],
                "render-action-video-frame-duration-invalid",
            )
            if duration < 0:
                fail("render-action-video-frame-duration-invalid")
            self.maximum_duration = max(self.maximum_duration, duration)
        self.first = timestamp if self.first is None else min(self.first, timestamp)
        frame_end = timestamp + duration
        self.last_end = frame_end if self.last_end is None else max(self.last_end, frame_end)
        self.count += 1

    def result(self) -> tuple[Decimal, Decimal, Decimal, Decimal, Decimal, int]:
        if (
            self.first is None
            or self.last_timestamp is None
            or self.last_end is None
            or self.count == 0
        ):
            fail("render-action-video-has-no-decodable-frames")
        return (
            self.first,
            self.last_timestamp,
            self.last_end,
            self.maximum_gap,
            self.maximum_duration,
            self.count,
        )


def decoded_frame_timeline(
    output: bytes,
) -> tuple[Decimal, Decimal, Decimal, Decimal, Decimal, int]:
    try:
        lines = output.decode("utf-8", errors="strict").splitlines()
    except UnicodeDecodeError:
        fail("render-action-video-frame-inspection-invalid")
    accumulator = TimelineAccumulator()
    for line in lines:
        accumulator.add_line(line)
    return accumulator.result()


def ffprobe_timeline(
    executable: str,
    arguments: list[str],
    timeout_seconds: int,
    maximum_output_bytes: int,
    maximum_lines: int,
) -> tuple[Decimal, Decimal, Decimal, Decimal, Decimal, int]:
    accumulator = TimelineAccumulator()
    pending = bytearray()
    line_count = 0

    def consume(chunk: bytes) -> None:
        nonlocal line_count
        pending.extend(chunk)
        while True:
            separator = pending.find(b"\n")
            if separator < 0:
                break
            raw_line = bytes(pending[:separator])
            del pending[: separator + 1]
            line_count += 1
            if line_count > maximum_lines:
                fail("render-action-video-full-stream-output-exceeds-limit")
            try:
                accumulator.add_line(raw_line.rstrip(b"\r").decode("utf-8", errors="strict"))
            except UnicodeDecodeError:
                fail("render-action-video-frame-inspection-invalid")

    run_ffprobe_stream(
        executable,
        arguments,
        timeout_seconds,
        "render-action-video-full-stream-cannot-be-decoded",
        "render-action-video-full-stream-output-exceeds-limit",
        maximum_output_bytes,
        consume,
    )
    if pending:
        line_count += 1
        if line_count > maximum_lines:
            fail("render-action-video-full-stream-output-exceeds-limit")
        try:
            accumulator.add_line(bytes(pending).rstrip(b"\r").decode("utf-8", errors="strict"))
        except UnicodeDecodeError:
            fail("render-action-video-frame-inspection-invalid")
    return accumulator.result()


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--video", required=True, type=Path)
    parser.add_argument("--ffprobe", default="ffprobe")
    parser.add_argument("--minimum-duration-ms", required=True, type=positive_integer)
    parser.add_argument("--maximum-duration-ms", required=True, type=positive_integer)
    parser.add_argument("--coverage-tolerance-ms", type=positive_integer, default=1000)
    parser.add_argument(
        "--maximum-inter-frame-gap-ms", type=positive_integer, default=1000
    )
    parser.add_argument(
        "--maximum-video-bytes",
        type=positive_integer,
        default=DEFAULT_MAXIMUM_VIDEO_BYTES,
    )
    parser.add_argument(
        "--maximum-ffprobe-output-bytes",
        type=positive_integer,
        default=DEFAULT_MAXIMUM_FFPROBE_OUTPUT_BYTES,
    )
    parser.add_argument(
        "--maximum-full-timeline-bytes",
        type=positive_integer,
        default=DEFAULT_MAXIMUM_FULL_TIMELINE_BYTES,
    )
    parser.add_argument(
        "--maximum-full-timeline-lines",
        type=positive_integer,
        default=DEFAULT_MAXIMUM_FULL_TIMELINE_LINES,
    )
    parser.add_argument("--command-timeout-seconds", type=positive_integer, default=600)
    return parser.parse_args()


def main() -> int:
    arguments = parse_arguments()
    try:
        try:
            metadata = arguments.video.lstat()
        except OSError:
            fail("render-action-video-missing")
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size == 0:
            fail("render-action-video-missing-or-invalid")
        if metadata.st_size > arguments.maximum_video_bytes:
            fail("render-action-video-bytes-exceed-limit")
        if arguments.maximum_duration_ms < arguments.minimum_duration_ms:
            fail("render-action-video-duration-bounds-invalid")

        inspection = ffprobe(
            arguments.ffprobe,
            [
                "-count_frames",
                "-count_packets",
                "-show_entries",
                (
                    "format=duration:"
                    "stream=index,codec_name,codec_type,width,height,duration,start_time,"
                    "nb_read_frames,nb_read_packets:stream_disposition=attached_pic"
                ),
                "-of",
                "json",
                str(arguments.video),
            ],
            arguments.command_timeout_seconds,
            "render-action-video-container-inspection-failed",
            arguments.maximum_ffprobe_output_bytes,
        )
        try:
            document = json.loads(inspection)
        except (UnicodeDecodeError, json.JSONDecodeError, TypeError):
            fail("render-action-video-container-inspection-invalid")
        if not isinstance(document, dict):
            fail("render-action-video-container-inspection-invalid")
        format_record = document.get("format")
        if not isinstance(format_record, dict):
            fail("render-action-video-container-duration-missing")
        container_duration = decimal_value(
            format_record.get("duration"),
            "render-action-video-container-duration-invalid",
        )
        minimum_seconds = Decimal(arguments.minimum_duration_ms) / 1000
        maximum_seconds = Decimal(arguments.maximum_duration_ms) / 1000
        tolerance_seconds = Decimal(arguments.coverage_tolerance_ms) / 1000
        maximum_inter_frame_gap_seconds = (
            Decimal(arguments.maximum_inter_frame_gap_ms) / 1000
        )
        if not minimum_seconds <= container_duration <= maximum_seconds:
            fail("render-action-video-container-duration-out-of-bounds")

        streams = document.get("streams")
        if not isinstance(streams, list):
            fail("render-action-video-stream-list-missing")
        selected: dict[str, object] | None = None
        selected_ordinal = -1
        video_ordinal = -1
        for stream in streams:
            if not isinstance(stream, dict) or stream.get("codec_type") != "video":
                continue
            video_ordinal += 1
            disposition = stream.get("disposition")
            attached_picture = (
                isinstance(disposition, dict)
                and positive_count(disposition.get("attached_pic")) > 0
            )
            if attached_picture:
                continue
            codec_name = stream.get("codec_name")
            if (
                positive_count(stream.get("width")) > 0
                and positive_count(stream.get("height")) > 0
                and isinstance(codec_name, str)
                and codec_name not in {"", "unknown"}
            ):
                selected = stream
                selected_ordinal = video_ordinal
                break
        if selected is None:
            fail("render-action-video-has-no-usable-video-stream")
        raw_stream_index = selected.get("index")
        selected_index = positive_count(raw_stream_index)
        if raw_stream_index in (0, "0"):
            selected_index = 0
        elif selected_index == 0:
            fail("render-action-video-stream-index-invalid")
        packet_count = positive_count(selected.get("nb_read_packets"))
        frame_count = positive_count(selected.get("nb_read_frames"))
        if packet_count == 0:
            fail("render-action-video-has-no-video-packets")
        if frame_count == 0:
            fail("render-action-video-has-no-decodable-video-frames")
        if frame_count > arguments.maximum_full_timeline_lines:
            fail("render-action-video-full-stream-output-exceeds-limit")
        minimum_cadence_count = max(1, int(minimum_seconds * 5))
        if packet_count < minimum_cadence_count or frame_count < minimum_cadence_count:
            fail("render-action-video-frame-cadence-is-too-sparse")

        stream_duration: Decimal | None = None
        if selected.get("duration") not in {None, "N/A"}:
            stream_duration = decimal_value(
                selected["duration"],
                "render-action-video-stream-duration-invalid",
            )
            if not minimum_seconds - tolerance_seconds <= stream_duration <= maximum_seconds:
                fail("render-action-video-stream-duration-out-of-bounds")

        frame_entries = (
            "frame=media_type,best_effort_timestamp_time,pts_time,pkt_dts_time,"
            "pkt_duration_time,width,height"
        )
        first_output = ffprobe(
            arguments.ffprobe,
            [
                "-read_intervals",
                "%+2",
                "-select_streams",
                f"v:{selected_ordinal}",
                "-show_frames",
                "-show_entries",
                frame_entries,
                "-of",
                "compact=p=0:nk=0",
                str(arguments.video),
            ],
            arguments.command_timeout_seconds,
            "render-action-video-first-frames-cannot-be-decoded",
            arguments.maximum_ffprobe_output_bytes,
        )
        seek_seconds = max(Decimal(0), minimum_seconds - Decimal(5))
        last_output = ffprobe(
            arguments.ffprobe,
            [
                "-read_intervals",
                f"{seek_seconds}%+7",
                "-select_streams",
                f"v:{selected_ordinal}",
                "-show_frames",
                "-show_entries",
                frame_entries,
                "-of",
                "compact=p=0:nk=0",
                str(arguments.video),
            ],
            arguments.command_timeout_seconds,
            "render-action-video-ending-frames-cannot-be-decoded",
            arguments.maximum_ffprobe_output_bytes,
        )
        (
            first_start,
            _,
            first_end,
            _,
            first_maximum_duration,
            first_decoded,
        ) = decoded_frame_timeline(first_output)
        (
            last_start,
            last_timestamp,
            last_end,
            _,
            last_maximum_duration,
            last_decoded,
        ) = decoded_frame_timeline(last_output)
        del first_end, last_start
        maximum_probe_duration = max(
            first_maximum_duration, last_maximum_duration
        )
        if maximum_probe_duration > maximum_inter_frame_gap_seconds:
            fail("render-action-video-frame-duration-exceeds-continuity-limit")
        if first_start > tolerance_seconds:
            fail("render-action-video-decoded-stream-starts-too-late")
        if last_end < minimum_seconds - tolerance_seconds:
            fail("render-action-video-decoded-stream-does-not-span-required-duration")
        if last_end > maximum_seconds + tolerance_seconds:
            fail("render-action-video-decoded-stream-exceeds-duration-bound")
        if last_timestamp < minimum_seconds - maximum_inter_frame_gap_seconds:
            fail("render-action-video-has-no-frame-near-required-end")

        (
            full_start,
            full_last_timestamp,
            full_end,
            maximum_inter_frame_gap,
            maximum_frame_duration,
            full_decoded,
        ) = ffprobe_timeline(
            arguments.ffprobe,
            [
                "-select_streams",
                f"v:{selected_ordinal}",
                "-show_frames",
                "-show_entries",
                frame_entries,
                "-of",
                "compact=p=0:nk=0",
                str(arguments.video),
            ],
            arguments.command_timeout_seconds,
            arguments.maximum_full_timeline_bytes,
            arguments.maximum_full_timeline_lines,
        )
        if maximum_frame_duration > maximum_inter_frame_gap_seconds:
            fail("render-action-video-frame-duration-exceeds-continuity-limit")
        if full_start > tolerance_seconds:
            fail("render-action-video-decoded-stream-starts-too-late")
        if full_end < minimum_seconds - tolerance_seconds:
            fail("render-action-video-decoded-stream-does-not-span-required-duration")
        if full_end > maximum_seconds + tolerance_seconds:
            fail("render-action-video-decoded-stream-exceeds-duration-bound")
        if full_last_timestamp < minimum_seconds - maximum_inter_frame_gap_seconds:
            fail("render-action-video-has-no-frame-near-required-end")
        if maximum_inter_frame_gap > maximum_inter_frame_gap_seconds:
            fail("render-action-video-frame-continuity-gap-exceeds-limit")
        if full_decoded < minimum_cadence_count:
            fail("render-action-video-frame-cadence-is-too-sparse")
        if full_decoded != frame_count:
            fail("render-action-video-decoded-frame-count-mismatch")

        verdict(
            "PASS",
            "render-action-video-stream-and-duration-verified",
            stream_index=selected_index,
            container_duration_seconds=container_duration,
            stream_duration_seconds=(stream_duration if stream_duration is not None else "derived"),
            video_packet_count=packet_count,
            video_frame_count=frame_count,
            decoded_first_frame_seconds=first_start,
            decoded_last_frame_end_seconds=last_end,
            decoded_probe_frame_count=first_decoded + last_decoded,
            decoded_full_stream_frame_count=full_decoded,
            maximum_inter_frame_gap_seconds=maximum_inter_frame_gap,
            maximum_frame_duration_seconds=maximum_frame_duration,
        )
        return 0
    except EvidenceError as error:
        verdict("NOT-RUN", str(error))
        return 2


if __name__ == "__main__":
    sys.exit(main())
