#!/usr/bin/env python
# MISE description="Compare paired WSS and QUIC latency with and without netem loss and delay on the server's host"
# USAGE flag "--owner-host <host>" help="SSH host whose Linux Development server is measured and impaired"
# USAGE flag "--owner-directory <directory>" help="Repository checkout on the owner host; defaults to this checkout's path from ~"
# USAGE flag "--owner-target <directory>" help="CARGO_TARGET_DIR on the owner host; defaults to the checkout's target"
# USAGE flag "--jail <directory>" help="Private Development server state on the owner host, mounted with bwrap, so another Development server there keeps running"
# USAGE flag "--expose <address>" help="Owner host address and port the server listens on, for TCP and UDP"
# USAGE flag "--client-address <address>" help="This machine's IPv4 address as the owner host sees it; only traffic to it is impaired"
# USAGE flag "--device <device>" help="Owner host network device to impair" default="eno1"
# USAGE flag "--loss <percent>" help="netem packet loss" default="2"
# USAGE flag "--delay <ms>" help="netem added delay" default="25"
# USAGE flag "--rounds <count>" help="Interleaved rounds of all four cells" default="1"
# USAGE flag "--samples <count>" help="Keys to echo per cell and round" default="300"
# USAGE flag "--requests <count>" help="Requests to time per cell and round" default="300"
# USAGE flag "--pages <count>" help="Pages of output per cell and round" default="300"
# USAGE flag "--fetches <count>" help="Scrollback pages per cell and round" default="300"
# USAGE flag "--output <directory>" help="Directory for raw results and the summary table"
# USAGE flag "--interface <interface>" help="Refuse to run unless the owner host routes through this interface"
"""Compare paired WSS and QUIC latency with and without netem loss and delay on the server's host.

The task builds the protocol probe here and the server and its owner on the owner host, both with
the iroh QUIC prototype of #671. It starts a foreground Development server there, which ends when
the task closes its input. Each cell runs bench:latency:protocol's probe with a fresh pairing:
WSS and QUIC, clean and impaired, in alternating order. Impairment adds a `prio` qdisc with netem
on the owner host's device, limited to traffic to this machine, so only the server's direction is
impaired. The task removes the qdisc after every impaired cell, also when a cell fails or the task
is interrupted. The owner host needs passwordless sudo for `tc`.
"""

import argparse
import json
import shlex
import subprocess
import sys
import time
from pathlib import Path

from spaceterm_tasks import TaskError, main
from spaceterm_tasks.cargo import build_executable
from spaceterm_tasks.latency import (
    QUIC_PROBE,
    add_route_arguments,
    check_route,
    netem_active,
    netem_commands,
    parse_interface_addresses,
    report,
    shell_directory,
    table,
)

READY = "spaceterm-development-ready-v1"
IDENTITY = "spaceterm-development"


def remote(args, command, **options):
    """Runs `command` in a login shell on the owner host, with `tc` on its PATH."""
    command = f'PATH="$PATH:/usr/sbin:/sbin" && {command}'
    script = f'exec "$SHELL" -lc {shlex.quote(command)}'
    return subprocess.run(["ssh", "-T", args.owner_host, script], text=True, **options)


def checkout(args):
    directory = shell_directory(args.owner_directory)
    target = shlex.quote(args.owner_target) if args.owner_target else f"{directory}/target"
    return directory, target


def jailed(args, command):
    """Prefixes `command` with a bwrap mount namespace over the Development server's state."""
    if not args.jail:
        return command
    jail = shlex.quote(args.jail)
    return (
        f'bwrap --dev-bind / / --bind {jail}/share "$HOME/.local/share/{IDENTITY}" '
        f'--bind {jail}/runtime "/tmp/{IDENTITY}-server-$(id -u)" -- {command}'
    )


def build_owner(args):
    directory, target = checkout(args)
    command = (
        f"cd {directory} && CARGO_TARGET_DIR={target} cargo build {' '.join(QUIC_PROBE)} "
        "--bin spaceterm-server --example latency_probe"
    )
    if args.jail:
        jail = shlex.quote(args.jail)
        command += (
            f' && mkdir -p {jail}/share {jail}/runtime "$HOME/.local/share/{IDENTITY}" '
            f'"/tmp/{IDENTITY}-server-$(id -u)" && chmod 700 {jail}/share {jail}/runtime'
        )
    if remote(args, command).returncode:
        raise TaskError("the owner host build failed")
    return target


def start_server(args, target):
    command = jailed(args, f"{target}/release/spaceterm-server development-serve {IDENTITY}")
    server = subprocess.Popen(
        ["ssh", "-T", args.owner_host, f"exec sh -c {shlex.quote(command)}"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        text=True,
    )
    line = server.stdout.readline().strip() if server.stdout else ""
    if line != READY:
        stop_server(server)
        raise TaskError("the owner host's Development server did not start")
    print(f"netem: Development server ready on {args.owner_host}", file=sys.stderr)
    return server


def stop_server(server):
    # Closing its input ends the foreground server, its Terminal Sessions and its runtime files.
    if server.stdin:
        server.stdin.close()
    try:
        server.wait(timeout=30)
    except subprocess.TimeoutExpired:
        server.terminate()
        server.wait()


def qdiscs(args):
    shown = remote(args, f"tc qdisc show dev {shlex.quote(args.device)}", capture_output=True)
    if shown.returncode:
        raise TaskError("tc on the owner host failed")
    return shown.stdout


def impair(args):
    if netem_active(qdiscs(args)):
        raise TaskError(f"{args.device} on the owner host already has a prio or netem qdisc")
    commands = netem_commands(args.device, args.client_address, args.loss, args.delay)
    if remote(args, " && ".join(commands)).returncode:
        raise TaskError("adding the netem qdisc failed")


def clear(args):
    """Removes the root qdisc when a run added one, and verifies the device is clean."""
    if netem_active(qdiscs(args)):
        remote(args, f"sudo -n tc qdisc del dev {shlex.quote(args.device)} root")
    if netem_active(qdiscs(args)):
        raise TaskError(f"the netem qdisc on {args.device} could not be removed")


def ping(args, impaired):
    """Round trips to the owner host. An impaired cell refuses to run unless they show the delay."""
    host = args.expose.rpartition(":")[0].strip("[]")
    result = subprocess.run(
        ["ping", "-q", "-c", "40", "-i", "0.1", host], capture_output=True, text=True
    )
    lines = [line for line in result.stdout.splitlines() if "loss" in line or "round-trip" in line]
    average = None
    for line in lines:
        if line.startswith("round-trip"):
            average = float(line.split("=")[1].split("/")[1])
    if impaired and (average is None or average < args.delay * 0.8):
        raise TaskError("pings to the owner host do not show the netem delay")
    return " ".join(line.strip() for line in lines)


def measure(args, probe, owner, transport, condition):
    command = [
        probe,
        "measure",
        IDENTITY,
        "paired",
        "--transport",
        transport,
        "--owner-command",
        owner,
        "--expose",
        args.expose,
        "--samples",
        str(args.samples),
        "--requests",
        str(args.requests),
        "--pages",
        str(args.pages),
        "--fetches",
        str(args.fetches),
    ]
    output = subprocess.run(command, stdout=subprocess.PIPE, text=True)
    results = []
    for line in output.stdout.splitlines():
        if line.strip():
            result = json.loads(line)
            result["transport"] = transport
            result["condition"] = condition
            result["app"] = f"{transport}, {condition}"
            report(result, samples=False)
            results.append(result)
    if output.returncode:
        raise TaskError(f"the probe failed over {transport}, {condition}")
    return results


def check_client(args):
    """Refuses a client address that is not this machine's, so netem never impairs another host."""
    configured = subprocess.run(["ifconfig"], capture_output=True, text=True, check=False)
    if args.client_address not in parse_interface_addresses(configured.stdout):
        raise TaskError(f"{args.client_address} is not an address of this machine")


def run(args):
    check_route(args, args.expose.rpartition(":")[0].strip("[]"))
    check_client(args)
    output = Path(args.output) if args.output else None
    if output:
        output.mkdir(parents=True, exist_ok=True)
    probe = str(build_executable(*QUIC_PROBE, binary="latency_probe", example=True))
    target = build_owner(args)
    owner = shlex.join(
        [
            "ssh",
            "-T",
            args.owner_host,
            "exec sh -c "
            + shlex.quote(
                jailed(args, f"{target}/release/examples/latency_probe owner {IDENTITY}")
            ),
        ]
    )
    # A qdisc left by an earlier interrupted run would impair the clean cells.
    clear(args)
    server = start_server(args, target)
    results = []
    pings = []
    try:
        for round in range(args.rounds):
            transports = ["wss", "quic"] if round % 2 == 0 else ["quic", "wss"]
            for condition in ["clean", "impaired"]:
                for transport in transports:
                    print(f"netem: round {round + 1}, {transport}, {condition}", file=sys.stderr)
                    try:
                        if condition == "impaired":
                            impair(args)
                        pings.append(f"{condition}: {ping(args, condition == 'impaired')}")
                        started = time.monotonic()
                        results += measure(args, probe, owner, transport, condition)
                        print(
                            f"netem: cell took {time.monotonic() - started:.0f} s",
                            file=sys.stderr,
                        )
                    finally:
                        if condition == "impaired":
                            clear(args)
    finally:
        clear(args)
        stop_server(server)
        if output:
            with (output / "results.jsonl").open("w") as file:
                for result in results:
                    file.write(json.dumps(result) + "\n")
    summary = table(results)
    print(summary)
    if output:
        settings = (
            f"loss {args.loss:g}% and delay {args.delay:g} ms on {args.device} of "
            f"{args.owner_host}, toward {args.client_address} only; {args.rounds} rounds"
        )
        (output / "summary.md").write_text(
            f"{settings}\n\n{summary}\n\nping:\n" + "\n".join(pings) + "\n"
        )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--owner-host", required=True)
    parser.add_argument("--owner-directory")
    parser.add_argument("--owner-target")
    parser.add_argument("--jail")
    parser.add_argument("--expose", required=True)
    parser.add_argument("--client-address", required=True)
    parser.add_argument("--device", default="eno1")
    parser.add_argument("--loss", type=float, default=2)
    parser.add_argument("--delay", type=float, default=25)
    parser.add_argument("--rounds", type=int, default=1)
    parser.add_argument("--samples", type=int, default=300)
    parser.add_argument("--requests", type=int, default=300)
    parser.add_argument("--pages", type=int, default=300)
    parser.add_argument("--fetches", type=int, default=300)
    parser.add_argument("--output")
    add_route_arguments(parser, host=False)
    main(lambda: run(parser.parse_args()))
