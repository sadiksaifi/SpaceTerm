#!/usr/bin/env python
# MISE description="Measure one-byte SSH echo through a raw cat on a remote host"
# USAGE arg "<destination>" help="SSH destination; an address keeps the route guard exact"
# USAGE flag "--ssh-option <option>" var=#true help="An ssh -o option, such as ObscureKeystrokeTiming=no"
# USAGE flag "--samples <count>" help="Bytes to echo" default="200"
# USAGE flag "--interface <interface>" help="Refuse to run unless the destination routes through this interface"
"""Measure one-byte SSH echo through a raw cat on a remote host.

Bytes follow each other after a human-like gap of 80 to 160 ms. OpenSSH 9.5 and newer delay
interactive keystrokes unless ObscureKeystrokeTiming is off.
"""

import argparse
import os
import pty
import random
import select
import signal
import time
import tty

from spaceterm_tasks import TaskError, main
from spaceterm_tasks.latency import add_route_arguments, check_route, report

READY = b"SPACETERM-ECHO-READY"


def read_until(descriptor, marker, timeout):
    received = b""
    deadline = time.monotonic() + timeout
    while marker not in received:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return False
        if select.select([descriptor], [], [], remaining)[0]:
            chunk = os.read(descriptor, 4096)
            if not chunk:
                return False
            received += chunk
    return True


def run(args):
    host = args.destination.rpartition("@")[2]
    check_route(args, host)
    pid, descriptor = pty.fork()
    if pid == 0:
        command = f"stty raw -echo; printf '%s\\n' {READY.decode()}; exec cat"
        options = [argument for option in args.ssh_option for argument in ("-o", option)]
        os.execvp("ssh", ["ssh", "-tt", *options, args.destination, command])
    try:
        if not read_until(descriptor, READY, 20):
            raise TaskError("the remote cat did not start within 20 seconds")
        tty.setraw(descriptor)
        time.sleep(0.5)
        while select.select([descriptor], [], [], 0.2)[0]:
            os.read(descriptor, 4096)
        samples = []
        for index in range(args.samples):
            byte = bytes([ord("a") + index % 26])
            start = time.perf_counter()
            os.write(descriptor, byte)
            if read_until(descriptor, byte, 2):
                samples.append((time.perf_counter() - start) * 1000)
            time.sleep(random.uniform(0.08, 0.16))
        report(
            {
                "measurement": "ssh echo",
                "app": " ".join([args.destination, *args.ssh_option]),
                "samples": samples,
                "misses": args.samples - len(samples),
            }
        )
    finally:
        # Closing the terminal hangs up ssh; SIGKILL covers an ssh that ignores the hangup.
        os.close(descriptor)
        deadline = time.monotonic() + 5
        while os.waitpid(pid, os.WNOHANG) == (0, 0):
            if time.monotonic() > deadline:
                os.kill(pid, signal.SIGKILL)
                os.waitpid(pid, 0)
                break
            time.sleep(0.05)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination")
    parser.add_argument("--ssh-option", action="append", default=[])
    parser.add_argument("--samples", type=int, default=200)
    add_route_arguments(parser, host=False)
    arguments = parser.parse_args()
    main(lambda: run(arguments))
