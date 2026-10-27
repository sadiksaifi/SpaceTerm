#!/usr/bin/env python
# MISE description="Measure TCP connect time to a remote port"
# USAGE arg "<host>" help="Remote address or name"
# USAGE arg "[port]" help="TCP port" default="22"
# USAGE flag "--samples <count>" help="Connections to time" default="200"
# USAGE flag "--interface <interface>" help="Refuse to run unless the host routes through this interface"
"""Measure TCP connect time to a remote port, one connection every 50 ms."""

import argparse
import socket
import time

from spaceterm_tasks import main
from spaceterm_tasks.latency import add_route_arguments, check_route, report


def run(args):
    check_route(args, args.host)
    samples = []
    for _ in range(args.samples):
        connection = socket.socket()
        connection.settimeout(3)
        start = time.perf_counter()
        try:
            connection.connect((args.host, args.port))
            samples.append((time.perf_counter() - start) * 1000)
        except OSError:
            pass
        finally:
            connection.close()
        time.sleep(0.05)
    report(
        {
            "measurement": "tcp connect",
            "app": f"{args.host}:{args.port}",
            "samples": samples,
            "misses": args.samples - len(samples),
        }
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("host")
    parser.add_argument("port", nargs="?", type=int, default=22)
    parser.add_argument("--samples", type=int, default=200)
    add_route_arguments(parser, host=False)
    arguments = parser.parse_args()
    main(lambda: run(arguments))
