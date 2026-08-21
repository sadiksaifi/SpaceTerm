#!/usr/bin/env python3

"""Verify SpaceTerm's native v5/v3 performance observation closure."""

from __future__ import annotations

import argparse
import hashlib
import os
import re
import stat
import sys
from pathlib import Path


MAX_RECORD = 128 * 1024
MAX_SAMPLES = 32 * 1024 * 1024
MAX_EVENTS = 16 * 1024 * 1024
MAX_FAILURES = 256 * 1024
SHA256 = re.compile(r"[0-9a-f]{64}\Z")
UNSIGNED = re.compile(r"0|[1-9][0-9]*\Z")
POSITIVE = re.compile(r"[1-9][0-9]*\Z")
NATIVE_KEYS = (
    "schema", "observation.source", "launch.nonce", "run.id", "package.app.sha256",
    "runtime.schema", "runtime.sample_interval_ms", "runtime.transition_capacity",
    "failure.action.schema", "failure.action.enabled", "process.pid", "process.pidversion",
    "process.executable.path", "process.executable.device", "process.executable.inode",
    "process.executable.fsid", "process.signature.cdhash", "process.signature.identifier",
    "process.signature.team_identifier", "terminal_font_selected", "initial_grid.rows",
    "initial_grid.columns", "initial_grid.logical_width", "initial_grid.logical_height",
    "initial_grid.backing_pixel_width", "initial_grid.backing_pixel_height",
    "provisional.observation.sha256",
    "runtime.metadata.schema", "runtime.metadata.path", "runtime.metadata.sha256",
    "failure.result.schema", "failure.actions.path", "failure.actions.sha256",
    "failure.request_count", "failure.result_count", "observation.complete",
)
PROVISIONAL_KEYS = NATIVE_KEYS[:26] + ("observation.complete",)
RUNTIME_KEYS = (
    "schema", "observation.source", "run.id", "package.app.sha256", "process.pid",
    "runtime.samples.path", "runtime.samples.sha256", "runtime.events.path",
    "runtime.events.sha256", "failure.action.schema", "failure.action.enabled",
    "failure.result.schema", "failure.actions.path", "failure.actions.sha256",
    "failure.request_count", "failure.result_count", "observer.started_continuous_ns",
    "observer.ended_continuous_ns", "observer.sample_interval_ms",
    "observer.transition_capacity", "observer.sample_count", "observer.event_count",
    "observer.status", "observation.complete",
)
SUBJECT_KEYS = (
    "format_version", "subject", "app_bundle_path", "bundle_identifier", "bundle_version",
    "executable_path", "executable_sha256", "executable_device", "executable_inode",
    "executable_fsid", "signature_valid", "signing_identifier", "team_identifier",
    "cdhash", "process_pid", "process_start_identity", "identity_status",
)
SAMPLES_HEADER = (
    "sequence\tcontinuous_ns\tworker_generation\tscreens_published\tscreens_enqueued\t"
    "screens_superseded\tevent_queue_length\tevent_queue_high_water\tui_dispatches\t"
    "ui_screen_events\tui_drain_high_water\tui_latest_generation\trender_latest_generation\t"
    "next_frame_generation\tnext_frame_count\tpresentable\tminimized\toccluded\t"
    "workspace_visible\tpane_visible\tlive_resize\tviewport_total_rows\t"
    "viewport_visible_rows\tviewport_offset_rows\tselection_present\tresize_requests\t"
    "resize_notifications\tresize_applied\tresize_coalesced\tpty_rows\tpty_columns\t"
    "pty_pixel_width\tpty_pixel_height\tterminal_inputs_accepted\tlifecycle\tobserver_drops"
)
EVENTS_HEADER = "sequence\tcontinuous_ns\tkind\tgeneration\taux0\taux1"
FAILURE_HEADER = (
    "request_id\tsequence\tcase_id\taction\tresult\tpane_id\tpane_state\tfailure_class\t"
    "failure_recoverability\tfailure_operation\tstate_revision\tlatest_generation\t"
    "last_valid_generation\tvisible_generation\tpending_recovery\tterminal_input_usable\t"
    "session_attached\tresource_staged_count\tresource_staged_bytes\t"
    "resource_rolled_back_count\tresource_rolled_back_bytes"
)


class Invalid(Exception):
    pass


def read_stable(path_text: str, maximum: int) -> bytes:
    path = Path(path_text)
    before = path.lstat()
    if not stat.S_ISREG(before.st_mode) or stat.S_ISLNK(before.st_mode) \
            or before.st_uid != os.geteuid() or before.st_mode & 0o0222 \
            or before.st_size <= 0 or before.st_size > maximum:
        raise Invalid("unsafe-or-invalid-file")
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        data = b""
        while len(data) <= maximum:
            chunk = os.read(descriptor, min(65536, maximum + 1 - len(data)))
            if not chunk:
                break
            data += chunk
        after = os.fstat(descriptor)
    finally:
        os.close(descriptor)
    path_after = path.lstat()
    fields = ("st_dev", "st_ino", "st_mode", "st_uid", "st_size", "st_mtime_ns", "st_ctime_ns")
    if any(getattr(before, key) != getattr(opened, key) for key in fields) \
            or any(getattr(before, key) != getattr(after, key) for key in fields) \
            or any(getattr(before, key) != getattr(path_after, key) for key in fields) \
            or len(data) != before.st_size:
        raise Invalid("file-changed")
    return data


def decode_value(raw: bytes) -> str:
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as error:
        raise Invalid("invalid-utf8") from error
    result: list[str] = []
    index = 0
    escapes = {"25": "%", "09": "\t", "0D": "\r", "0A": "\n"}
    while index < len(text):
        if text[index] != "%":
            result.append(text[index])
            index += 1
            continue
        code = text[index + 1:index + 3]
        if len(code) != 2 or code not in escapes:
            raise Invalid("invalid-percent-encoding")
        result.append(escapes[code])
        index += 3
    return "".join(result)


def parse_exact(data: bytes, keys: tuple[str, ...], *, allow_empty: set[str] | None = None) -> dict[str, str]:
    if not data.endswith(b"\n") or b"\0" in data or b"\r" in data:
        raise Invalid("invalid-record-encoding")
    lines = data[:-1].split(b"\n")
    if len(lines) != len(keys):
        raise Invalid("record-schema-width")
    values: dict[str, str] = {}
    for expected, line in zip(keys, lines):
        fields = line.split(b"\t", 1)
        if len(fields) != 2 or fields[0].decode("ascii") != expected:
            raise Invalid("record-order-or-key")
        value = decode_value(fields[1])
        if not value and (allow_empty is None or expected not in allow_empty):
            raise Invalid("empty-record-value")
        values[expected] = value
    return values


def unsigned(value: str, *, positive: bool = False) -> int:
    if (POSITIVE if positive else UNSIGNED).fullmatch(value) is None:
        raise Invalid("invalid-integer")
    result = int(value)
    if result > (1 << 64) - 1:
        raise Invalid("integer-overflow")
    return result


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def parse_stream(data: bytes, header: str, width: int, expected_count: int) -> list[list[str]]:
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError as error:
        raise Invalid("stream-not-utf8") from error
    if not text.endswith("\n") or "\r" in text or "\0" in text:
        raise Invalid("stream-encoding")
    lines = text[:-1].split("\n")
    if not lines or lines[0] != header or len(lines) - 1 != expected_count:
        raise Invalid("stream-header-or-count")
    rows = [line.split("\t") for line in lines[1:]]
    if any(len(row) != width for row in rows):
        raise Invalid("stream-row-width")
    return rows


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--subject-identity", required=True)
    parser.add_argument("--provisional-observation")
    parser.add_argument("--native-observation")
    parser.add_argument("--runtime-metadata")
    parser.add_argument("--runtime-samples")
    parser.add_argument("--runtime-events")
    parser.add_argument("--failure-actions")
    args = parser.parse_args()
    try:
        subject_data = read_stable(args.subject_identity, MAX_RECORD)
        provisional_data = read_stable(args.provisional_observation, MAX_RECORD)
        provisional = parse_exact(
            provisional_data, PROVISIONAL_KEYS, allow_empty={"process.signature.team_identifier"},
        )
        if provisional["schema"] != "spaceterm.acceptance.native-launch-proof/v5" \
                or provisional["observation.source"] != "production-app" \
                or provisional["failure.action.enabled"] != "false" \
                or provisional["process.pid"] != parse_exact(subject_data, SUBJECT_KEYS)["process_pid"] \
                or provisional["observation.complete"] != "true":
            raise Invalid("provisional-observation-binding")
        closure_arguments = (
            args.native_observation, args.runtime_metadata, args.runtime_samples,
            args.runtime_events, args.failure_actions,
        )
        if not any(closure_arguments):
            print(f"native_provisional_observation_sha256\t{digest(provisional_data)}")
            return 0
        if not all(closure_arguments):
            raise Invalid("incomplete-native-closure-arguments")
        observation_data = read_stable(args.native_observation, MAX_RECORD)
        metadata_data = read_stable(args.runtime_metadata, MAX_RECORD)
        samples_data = read_stable(args.runtime_samples, MAX_SAMPLES)
        events_data = read_stable(args.runtime_events, MAX_EVENTS)
        failure_data = read_stable(args.failure_actions, MAX_FAILURES)
        subject = parse_exact(subject_data, SUBJECT_KEYS)
        observation = parse_exact(
            observation_data, NATIVE_KEYS, allow_empty={"process.signature.team_identifier"},
        )
        metadata = parse_exact(metadata_data, RUNTIME_KEYS)
        if subject["subject"] != "spaceterm" or subject["identity_status"] != "frozen" \
                or subject["signature_valid"] != "true":
            raise Invalid("subject-is-not-frozen-spaceterm")
        expected_team = "" if subject["team_identifier"] == "none" else subject["team_identifier"]
        if observation["schema"] != "spaceterm.acceptance.native-launch-proof/v5" \
                or observation["observation.source"] != "production-app" \
                or observation["runtime.schema"] != "spaceterm.acceptance.runtime-stream/v1" \
                or observation["runtime.sample_interval_ms"] != "1000" \
                or observation["runtime.transition_capacity"] != "64" \
                or observation["failure.action.schema"] != "spaceterm.acceptance.failure-action/v1" \
                or observation["failure.action.enabled"] != "false" \
                or observation["provisional.observation.sha256"] != digest(provisional_data) \
                or observation["process.pid"] != subject["process_pid"] \
                or observation["process.executable.path"] != subject["executable_path"] \
                or observation["process.executable.device"] != subject["executable_device"] \
                or observation["process.executable.inode"] != subject["executable_inode"] \
                or observation["process.signature.cdhash"].lower() != subject["cdhash"].lower() \
                or observation["process.signature.identifier"] != subject["signing_identifier"] \
                or observation["process.signature.team_identifier"] != expected_team \
                or observation["runtime.metadata.schema"] != "spaceterm.acceptance.runtime-observation-metadata/v3" \
                or observation["runtime.metadata.path"] != "runtime-metadata.tsv" \
                or observation["runtime.metadata.sha256"] != digest(metadata_data) \
                or observation["failure.result.schema"] != "spaceterm.acceptance.failure-action-result/v2" \
                or observation["failure.actions.path"] != "failure-actions.tsv" \
                or observation["failure.actions.sha256"] != digest(failure_data) \
                or observation["failure.request_count"] != "0" \
                or observation["failure.result_count"] != "0" \
                or observation["observation.complete"] != "true":
            raise Invalid("native-observation-binding")
        for key in PROVISIONAL_KEYS[:-1]:
            if observation[key] != provisional[key]:
                raise Invalid("final-observation-does-not-reconstruct-provisional")
        if metadata["schema"] != observation["runtime.metadata.schema"] \
                or metadata["observation.source"] != "production-app" \
                or metadata["run.id"] != observation["run.id"] \
                or metadata["package.app.sha256"] != observation["package.app.sha256"] \
                or metadata["process.pid"] != subject["process_pid"] \
                or metadata["runtime.samples.path"] != "runtime-samples.tsv" \
                or metadata["runtime.samples.sha256"] != digest(samples_data) \
                or metadata["runtime.events.path"] != "runtime-events.tsv" \
                or metadata["runtime.events.sha256"] != digest(events_data) \
                or metadata["failure.action.schema"] != observation["failure.action.schema"] \
                or metadata["failure.action.enabled"] != "false" \
                or metadata["failure.result.schema"] != observation["failure.result.schema"] \
                or metadata["failure.actions.path"] != "failure-actions.tsv" \
                or metadata["failure.actions.sha256"] != digest(failure_data) \
                or metadata["failure.request_count"] != "0" \
                or metadata["failure.result_count"] != "0" \
                or metadata["observer.sample_interval_ms"] != "1000" \
                or metadata["observer.transition_capacity"] != "64" \
                or metadata["observer.status"] != "complete" \
                or metadata["observation.complete"] != "true":
            raise Invalid("runtime-metadata-binding")
        started = unsigned(metadata["observer.started_continuous_ns"], positive=True)
        ended = unsigned(metadata["observer.ended_continuous_ns"], positive=True)
        sample_count = unsigned(metadata["observer.sample_count"], positive=True)
        event_count = unsigned(metadata["observer.event_count"])
        if ended < started or sample_count > 43201 or event_count > 65536:
            raise Invalid("runtime-counter-bound")
        samples = parse_stream(samples_data, SAMPLES_HEADER, 36, sample_count)
        events = parse_stream(events_data, EVENTS_HEADER, 6, event_count)
        prior = 0
        for sequence, row in enumerate(samples):
            timestamp = unsigned(row[1], positive=True)
            if row[0] != str(sequence) or timestamp <= prior:
                raise Invalid("runtime-sample-order")
            prior = timestamp
        if int(samples[0][1]) != started or int(samples[-1][1]) != ended:
            raise Invalid("runtime-sample-interval-binding")
        prior = 0
        for sequence, row in enumerate(events):
            timestamp = unsigned(row[1], positive=True)
            if row[0] != str(sequence) or timestamp < started or timestamp > ended \
                    or timestamp <= prior:
                raise Invalid("runtime-event-order")
            prior = timestamp
        if failure_data != (FAILURE_HEADER + "\n").encode("ascii"):
            raise Invalid("failure-result-not-header-only-v2")
        for key in (
            "process.pid", "process.pidversion", "process.executable.device",
            "process.executable.inode", "initial_grid.rows", "initial_grid.columns",
            "initial_grid.backing_pixel_width", "initial_grid.backing_pixel_height",
        ):
            unsigned(observation[key], positive=True)
        if SHA256.fullmatch(observation["package.app.sha256"]) is None \
                or SHA256.fullmatch(observation["launch.nonce"]) is None:
            raise Invalid("native-observation-hash")
        print(f"native_observation_sha256\t{digest(observation_data)}")
        print(f"native_runtime_metadata_sha256\t{digest(metadata_data)}")
        print(f"native_failure_actions_sha256\t{digest(failure_data)}")
    except (Invalid, OSError, UnicodeError, ValueError) as error:
        print(f"performance native closure failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
