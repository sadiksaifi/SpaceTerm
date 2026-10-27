#!/usr/bin/env python
# MISE description="Compare keystroke to pixels idle and beside a visible bench:workload Pane"
# USAGE flag "--app <app>" help="SpaceTerm app whose frontmost window has the focused Pane to measure" default="preflight" {
# USAGE   choices "preflight" "development"
# USAGE }
# USAGE flag "--samples <count>" help="Keystrokes to time in each phase" default="150"
# USAGE flag "--mode <mode>" help="bench:workload mode" default="scroll" {
# USAGE   choices "idle" "partial" "scroll" "image"
# USAGE }
# USAGE flag "--directory <directory>" help="Repository checkout to run the workload from on the Pane's host"
# USAGE flag "--route-host <host>" help="Remote host of a remote Pane, for the route guard"
# USAGE flag "--interface <interface>" help="Refuse to run unless the remote host routes through this interface"
"""Compare keystroke to pixels idle and beside a visible bench:workload Pane.

The probe splits the focused Pane to the right and measures the left Pane with the right Pane
idle. It then runs `mise run bench:workload` in the right Pane, measures the left Pane again,
stops the workload and closes the right Pane. The #670 criterion is a p99 increase of at most
10 ms.
"""

import argparse

from spaceterm_tasks import TaskError, main
from spaceterm_tasks.latency import (
    add_route_arguments,
    check_route,
    measure_pixels,
    percentile,
    report,
    shell_directory,
)

# Longer than both measurement phases; the probe stops the workload when it finishes.
WORKLOAD_SECONDS = 900
CRITERION_MS = 10
# The pixel probe waits this long for an echo before it counts a miss.
MISS_MS = 1000


def p99_with_misses(result):
    """p99 counting each miss as the probe's deadline, since its echo took at least that long."""
    return percentile(result["samples"] + [MISS_MS] * result.get("misses", 0), 99)


def run(args):
    check_route(args)
    workload = (
        f"cd {shell_directory(args.directory)} && mise run bench:workload --mode {args.mode} "
        f"--duration {WORKLOAD_SECONDS}"
    )
    results = {
        result["phase"]: result
        for result in measure_pixels(
            args.app,
            "keystroke",
            args.samples,
            workload=workload,
            # Idle and image workloads draw once and then stay still.
            continuous=args.mode in ("partial", "scroll"),
        )
    }
    if set(results) != {"idle", "workload"}:
        raise TaskError("the pixel probe did not report both phases")
    for phase in ("idle", "workload"):
        report(results[phase])
    idle, loaded = (p99_with_misses(results[phase]) for phase in ("idle", "workload"))
    verdict = "meets" if loaded - idle <= CRITERION_MS else "misses"
    print(
        f"p99 during {args.mode} workload minus idle p99: {loaded - idle:.2f} ms; "
        f"{verdict} the {CRITERION_MS} ms criterion"
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", choices=("preflight", "development"), default="preflight")
    parser.add_argument("--samples", type=int, default=150)
    parser.add_argument("--mode", choices=("idle", "partial", "scroll", "image"), default="scroll")
    parser.add_argument("--directory")
    add_route_arguments(parser)
    arguments = parser.parse_args()
    main(lambda: run(arguments))
