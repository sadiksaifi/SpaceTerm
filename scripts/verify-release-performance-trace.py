#!/usr/bin/env python3
"""Fail-closed verification of privacy-safe xctrace exports for issue #43."""

from __future__ import annotations

import argparse
import datetime as dt
import math
import sys
import xml.etree.ElementTree as ET


def fail(reason: str) -> "NoReturn":
    print(f"reason\t{reason}")
    raise SystemExit(1)


def parse_xml(path: str) -> ET.Element:
    try:
        return ET.parse(path).getroot()
    except (ET.ParseError, OSError):
        fail("invalid-trace-xml")


def parse_time(value: str) -> dt.datetime:
    try:
        return dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        fail("invalid-trace-time")


def text(element: ET.Element | None) -> str:
    return "" if element is None or element.text is None else element.text.strip()


def process_ids(root: ET.Element, pid: int) -> set[str]:
    identifiers: set[str] = set()
    for process in root.findall(".//process"):
        if text(process.find("pid")) == str(pid) and process.get("id"):
            identifiers.add(process.get("id", ""))
    return identifiers


def row_targets_pid(row: ET.Element, pid: int, identifiers: set[str]) -> bool:
    for process in row.findall(".//process"):
        if text(process.find("pid")) == str(pid):
            return True
        if process.get("ref") in identifiers:
            return True
    return False


def numeric(value: str) -> float:
    try:
        result = float(value)
    except ValueError:
        fail("invalid-numeric-trace-field")
    if not math.isfinite(result) or result < 0:
        fail("invalid-numeric-trace-field")
    return result


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--toc", required=True)
    parser.add_argument("--time-profile", required=True)
    parser.add_argument("--allocations", required=True)
    parser.add_argument("--hangs", required=True)
    parser.add_argument("--pid", type=int, required=True)
    parser.add_argument("--process-name", required=True)
    parser.add_argument("--requested-seconds", type=float, required=True)
    parser.add_argument("--command-elapsed-seconds", type=float, required=True)
    arguments = parser.parse_args()

    toc = parse_xml(arguments.toc)
    runs = [run for run in toc.findall("run") if run.get("number") == "1"]
    if len(runs) != 1:
        fail("trace-run-is-not-unique")
    run = runs[0]
    target = run.findall("./info/target/process")
    if len(target) != 1 or target[0].get("type") != "attached":
        fail("trace-target-is-not-a-single-attachment")
    if target[0].get("pid") != str(arguments.pid) or target[0].get(
        "name"
    ) != arguments.process_name:
        fail("trace-target-identity-mismatch")
    processes = run.findall("./processes/process")
    matching = [
        process
        for process in processes
        if process.get("pid") == str(arguments.pid)
        and process.get("name") == arguments.process_name
    ]
    if len(processes) != 1 or len(matching) != 1:
        fail("trace-process-scope-is-not-single-target")

    summary = run.find("./info/summary")
    if summary is None:
        fail("trace-summary-missing")
    started = parse_time(text(summary.find("start-date")))
    ended = parse_time(text(summary.find("end-date")))
    duration = numeric(text(summary.find("duration")))
    date_span = (ended - started).total_seconds()
    if date_span <= 0 or abs(date_span - duration) > 0.25:
        fail("trace-summary-duration-is-inconsistent")
    if duration < arguments.requested_seconds:
        fail("requested-duration-not-covered")
    if arguments.command_elapsed_seconds + 0.25 < duration:
        fail("command-elapsed-does-not-cover-trace")

    time_root = parse_xml(arguments.time_profile)
    if time_root.find("./node/schema") is None or time_root.find(
        "./node/schema"
    ).get("name") != "time-profile":
        fail("time-profile-schema-mismatch")
    time_rows = time_root.findall("./node/row")
    time_ids = process_ids(time_root, arguments.pid)
    if len(time_rows) < 2:
        fail("time-profile-samples-insufficient")
    sample_times: list[float] = []
    for row in time_rows:
        if row.find("sample-time") is None or row.find("weight") is None:
            fail("time-profile-row-invalid")
        if not row_targets_pid(row, arguments.pid, time_ids):
            fail("time-profile-row-target-mismatch")
        sample_times.append(numeric(text(row.find("sample-time"))))
    if sample_times != sorted(sample_times):
        fail("time-profile-samples-not-monotonic")
    one_second_ns = 1_000_000_000
    if sample_times[0] > one_second_ns:
        fail("time-profile-start-coverage-missing")
    if sample_times[-1] < max(0, duration - 1) * one_second_ns:
        fail("time-profile-end-coverage-missing")
    if any(
        later - earlier > one_second_ns
        for earlier, later in zip(sample_times, sample_times[1:])
    ):
        fail("time-profile-continuity-missing")

    allocation_details = run.findall(
        './tracks/track[@name="Allocations"]/details/detail[@name="Allocations List"]'
    )
    if len(allocation_details) != 1:
        fail("allocations-detail-is-not-exact")
    allocation_root = parse_xml(arguments.allocations)
    allocation_rows = allocation_root.findall("./node/row")
    if not allocation_rows:
        fail("allocation-events-empty")
    for row in allocation_rows:
        if not all(row.get(attribute) for attribute in ("timestamp", "identifier", "size")):
            fail("allocation-row-invalid")
        # Current Allocations List exports have no per-row process column.
        # The singleton attached process/run scope checked above is therefore
        # the only exact target binding available for this detail export.

    hangs_root = parse_xml(arguments.hangs)
    hangs_schema = hangs_root.find("./node/schema")
    if hangs_schema is None or hangs_schema.get("name") != "potential-hangs":
        fail("hangs-schema-mismatch")
    hang_rows = hangs_root.findall("./node/row")
    # Zero potential-hang events is a valid clean recording. Instrument
    # presence and full-duration target coverage are proven by the exact TOC
    # table plus the target-bound continuous Time Profiler export above.
    hang_ids = process_ids(hangs_root, arguments.pid)
    for row in hang_rows:
        if any(row.find(field) is None for field in ("start-time", "duration", "hang-type")):
            fail("hang-row-invalid")
        if not row_targets_pid(row, arguments.pid, hang_ids):
            fail("hang-row-target-mismatch")

    print("reason\tnone")
    print(f"trace_started_at\t{text(summary.find('start-date'))}")
    print(f"trace_ended_at\t{text(summary.find('end-date'))}")
    print(f"actual_record_duration_seconds\t{duration:.6f}")
    print(f"time_profiler_sample_count\t{len(time_rows)}")
    print(f"allocations_event_count\t{len(allocation_rows)}")
    print(f"hangs_event_count\t{len(hang_rows)}")


if __name__ == "__main__":
    main()
