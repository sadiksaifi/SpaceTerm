#!/usr/bin/env python
# MISE description="Measure the TCP traffic a SpaceTerm app sends while its visible Panes stay idle"
# USAGE flag "--app <app>" help="SpaceTerm app to observe" default="preflight" {
# USAGE   choices "preflight" "development"
# USAGE }
# USAGE flag "--seconds <seconds>" help="Observation window" default="30"
# USAGE flag "--route-host <host>" help="Count only connections to this server address"
# USAGE flag "--interface <interface>" help="Refuse to run unless the route host routes through this interface"
"""Measure the TCP traffic a SpaceTerm app sends while its visible Panes stay idle.

Show a remote Pane, leave it idle and run this task. nettop reports the app's byte counters for
each TCP connection, so this covers paired WSS connections. Account-local IPC uses a Unix socket
that nettop does not count; a connection-level test covers that path. A client that sends no
periodic requests sends only its keepalive: one ping after 5 seconds of silence.
"""

import argparse
import subprocess
import time

from spaceterm_tasks import TaskError, main
from spaceterm_tasks.latency import APPS, add_route_arguments, check_route, resolve

# The keepalive sends one ping after this much silence.
KEEPALIVE_SECONDS = 5
# A generous ceiling for one ping on the wire, with WebSocket and TLS framing.
PING_BYTES = 128
BUDGET = PING_BYTES / KEEPALIVE_SECONDS


def app_pid(name):
    listing = subprocess.run(
        ["ps", "-axo", "pid=,comm="], capture_output=True, text=True, check=True
    ).stdout
    for line in listing.splitlines():
        pid, _, command = line.strip().partition(" ")
        if command.endswith(f".app/Contents/MacOS/{name}"):
            return int(pid)
    raise TaskError(f"{name} is not running")


def parse_connections(text):
    """Map each TCP connection in nettop CSV output to its received and sent byte counters."""
    counters = {}
    for line in text.splitlines():
        fields = line.split(",")
        if len(fields) >= 3 and "<->" in fields[0] and fields[1] and fields[2]:
            connection = fields[0].split(" ", 1)[-1]
            counters[connection] = (int(fields[1]), int(fields[2]))
    return counters


def remote_host(connection):
    remote = connection.split("<->")[1]
    # nettop separates an IPv6 port with a dot and an IPv4 port with a colon.
    return remote.rsplit(".", 1)[0] if remote.count(":") > 1 else remote.rsplit(":", 1)[0]


def counters(pid, host):
    output = subprocess.run(
        ["nettop", "-p", str(pid), "-m", "tcp", "-L", "1", "-n", "-x", "-J", "bytes_in,bytes_out"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    connections = parse_connections(output)
    return {
        connection: values
        for connection, values in connections.items()
        if host is None or remote_host(connection) == host
    }


def run(args):
    check_route(args)
    host = resolve(args.route_host) if args.route_host else None
    pid = app_pid(APPS[args.app])
    before = counters(pid, host)
    start = time.monotonic()
    time.sleep(args.seconds)
    after = counters(pid, host)
    elapsed = time.monotonic() - start
    lasting = sorted(set(before) & set(after))
    if not lasting:
        raise TaskError("the app kept no TCP connection open through the window")
    if set(before) != set(after):
        print("note: connections opened or closed during the window; only lasting ones count")
    print(
        f"keepalive budget: one ping of at most {PING_BYTES} B after {KEEPALIVE_SECONDS} s "
        f"of silence, {BUDGET:.1f} B/s from client to server"
    )
    for connection in lasting:
        received = (after[connection][0] - before[connection][0]) / elapsed
        sent = (after[connection][1] - before[connection][1]) / elapsed
        verdict = "within" if sent <= BUDGET else "over"
        print(
            f"{connection}: client to server {sent:.1f} B/s, "
            f"server to client {received:.1f} B/s over {elapsed:.1f} s; {verdict} the budget"
        )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", choices=("preflight", "development"), default="preflight")
    parser.add_argument("--seconds", type=float, default=30)
    add_route_arguments(parser)
    arguments = parser.parse_args()
    main(lambda: run(arguments))
