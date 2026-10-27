#!/usr/bin/env python
# MISE description="Serve pairing for bench:latency:protocol on the server's machine"
# MISE hide=true
# USAGE flag "--identity <identity>" help="Server build that owns the pairing" default="spaceterm-preflight" {
# USAGE   choices "spaceterm-preflight" "spaceterm-development"
# USAGE }
"""Serve pairing for bench:latency:protocol on the server's machine.

It reads JSON requests on standard input and answers on standard output: invite, approve or
revoke one named probe device. bench:latency:protocol starts it over SSH.
"""

import argparse
import os

from spaceterm_tasks import main
from spaceterm_tasks.cargo import build_executable
from spaceterm_tasks.latency import PROTOCOL_PROBE


def run(identity):
    executable = build_executable(*PROTOCOL_PROBE, binary="latency_probe", example=True)
    os.execv(executable, [str(executable), "owner", identity])


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--identity",
        choices=("spaceterm-preflight", "spaceterm-development"),
        default="spaceterm-preflight",
    )
    arguments = parser.parse_args()
    main(lambda: run(arguments.identity))
