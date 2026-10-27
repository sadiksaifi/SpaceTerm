#!/usr/bin/env python
# MISE description="Measure wheel scrolling to pixels in the frontmost SpaceTerm or Terminal.app window"
# USAGE flag "--app <app>" help="App whose frontmost window has the focused Pane to measure" default="preflight" {
# USAGE   choices "preflight" "development" "terminal"
# USAGE }
# USAGE flag "--samples <count>" help="Wheel events to time" default="80"
# USAGE flag "--route-host <host>" help="Remote host of a remote Pane, for the route guard"
# USAGE flag "--interface <interface>" help="Refuse to run unless the remote host routes through this interface"
"""Measure wheel scrolling to pixels in the frontmost SpaceTerm or Terminal.app window.

The focused Pane must show a shell prompt. The probe prints 3000 lines there, then alternates
wheel events of three lines up and down within that history.
"""

import argparse

from spaceterm_tasks import main
from spaceterm_tasks.latency import APPS, add_route_arguments, check_route, measure_pixels, report


def run(args):
    check_route(args)
    for result in measure_pixels(args.app, "scroll", args.samples):
        report(result)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", choices=sorted(APPS), default="preflight")
    parser.add_argument("--samples", type=int, default=80)
    add_route_arguments(parser)
    arguments = parser.parse_args()
    main(lambda: run(arguments))
