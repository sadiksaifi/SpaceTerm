#!/usr/bin/env python
# MISE description="Measure request round trip and key-to-echo through the SpaceTerm protocol"
# USAGE flag "--identity <identity>" help="Server build to measure" default="spaceterm-preflight" {
# USAGE   choices "spaceterm-preflight" "spaceterm-development"
# USAGE }
# USAGE flag "--route <route>" help="Account-local IPC or a paired WSS connection" default="local" {
# USAGE   choices "local" "paired"
# USAGE }
# USAGE flag "--owner-host <host>" help="SSH host whose server a paired run measures; omit for this machine"
# USAGE flag "--owner-directory <directory>" help="Repository checkout on the owner host; defaults to this checkout's path from ~"
# USAGE flag "--expose <address>" help="Enable this machine's server exposure at this address and port for the run"
# USAGE flag "--samples <count>" help="Keys to echo" default="200"
# USAGE flag "--requests <count>" help="Requests to time" default="50"
# USAGE flag "--interface <interface>" help="Refuse to run unless the owner host routes through this interface"
"""Measure request round trip and key-to-echo through the SpaceTerm protocol.

The probe creates a Workspace whose Pane runs a raw cat, times requests that change nothing and
keys until their echo reaches the client, then closes the Workspace. A paired run enrolls a fresh
pairing through an owner on the server's machine and revokes it afterwards. The owner host needs
this repository at the same commit, with mise on its login shell's PATH.
"""

import argparse
import json
import shlex
import subprocess

from spaceterm_tasks import TaskError, main
from spaceterm_tasks.cargo import build_executable
from spaceterm_tasks.latency import (
    PROTOCOL_PROBE,
    add_route_arguments,
    check_route,
    report,
    shell_directory,
)


def owner_command(args):
    owner = (
        f"cd {shell_directory(args.owner_directory)} && "
        f"mise run --quiet bench:latency:protocol:owner --identity {args.identity}"
    )
    remote = f'exec "$SHELL" -lc {shlex.quote(owner)}'
    return shlex.join(["ssh", "-T", args.owner_host, remote])


def run(args):
    if args.route == "local" and (args.owner_host or args.expose):
        raise TaskError("--owner-host and --expose apply only to --route paired")
    if args.owner_host and args.expose:
        raise TaskError("--expose changes only this machine's server")
    check_route(args, args.owner_host)
    executable = build_executable(*PROTOCOL_PROBE, binary="latency_probe", example=True)
    command = [executable, "measure", args.identity, args.route]
    command += ["--samples", str(args.samples), "--requests", str(args.requests)]
    if args.owner_host:
        command += ["--owner-command", owner_command(args)]
    if args.expose:
        command += ["--expose", args.expose]
    probe = subprocess.run(command, stdout=subprocess.PIPE, text=True, check=False)
    for line in probe.stdout.splitlines():
        if line.strip():
            report(json.loads(line))
    if probe.returncode:
        raise TaskError("the protocol probe stopped before it finished")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--identity",
        choices=("spaceterm-preflight", "spaceterm-development"),
        default="spaceterm-preflight",
    )
    parser.add_argument("--route", choices=("local", "paired"), default="local")
    parser.add_argument("--owner-host")
    parser.add_argument("--owner-directory")
    parser.add_argument("--expose")
    parser.add_argument("--samples", type=int, default=200)
    parser.add_argument("--requests", type=int, default=50)
    add_route_arguments(parser, host=False)
    arguments = parser.parse_args()
    main(lambda: run(arguments))
