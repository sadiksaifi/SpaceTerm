"""Shared code for the bench:latency tasks: percentiles, the route guard and the pixel probe."""

import hashlib
import ipaddress
import json
import os
import shlex
import socket
import statistics
import subprocess
import sys
import time
from contextlib import contextmanager
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError

# Tasks that inject input or capture the screen hold this lock, so two probes never share a screen.
SCREEN_LOCK = Path("/tmp/spaceterm-screen.lock")
# Window owner names of the apps the pixel tasks can measure.
APPS = {
    "preflight": "SpaceTerm Preflight",
    "development": "SpaceTerm Development",
    "terminal": "Terminal",
}
PIXEL_SOURCE = Path(__file__).with_name("latency_pixels.swift")
# Cargo arguments for the protocol probe example. Release crypto keeps WSS timings realistic.
PROTOCOL_PROBE = (
    "--package",
    "spaceterm-server-host",
    "--features",
    "client",
    "--release",
    "--locked",
)
PERMISSIONS = (
    "The app that runs this task, such as Terminal, needs Accessibility and Screen Recording "
    "permission in System Settings > Privacy & Security."
)


def percentile(values, rank):
    """Return the `rank` percentile of `values` with linear interpolation between neighbors."""
    ordered = sorted(values)
    position = (len(ordered) - 1) * rank / 100
    lower = int(position)
    upper = min(lower + 1, len(ordered) - 1)
    return ordered[lower] + (ordered[upper] - ordered[lower]) * (position - lower)


def summary(name, values, *, unit="ms", misses=None):
    """One report line: count, p50, p90, p99, max, min and mean."""
    missed = "" if misses is None else f" misses={misses}"
    if not values:
        return f"{name}: n=0{missed}"
    return (
        f"{name}: n={len(values)}{missed} p50={percentile(values, 50):.2f} "
        f"p90={percentile(values, 90):.2f} p99={percentile(values, 99):.2f} "
        f"max={max(values):.2f} min={min(values):.2f} mean={statistics.mean(values):.2f} {unit}"
    )


def report(result, *, samples=True):
    """Print one probe result: its summary line, then its raw samples."""
    labels = [label for label in (result.get("app"), result.get("phase")) if label]
    name = result["measurement"] + (f" ({', '.join(labels)})" if labels else "")
    values = result["samples"]
    print(summary(name, values, unit=result.get("unit", "ms"), misses=result.get("misses")))
    if samples and values:
        print("samples:", " ".join(f"{value:.2f}" for value in values))
    sys.stdout.flush()


def parse_route_interface(text):
    """Return the interface in `route -n get` output, or None."""
    for line in text.splitlines():
        key, _, value = line.strip().partition(":")
        if key == "interface" and value.strip():
            return value.strip()
    return None


def parse_interface_addresses(text):
    """Return the IPv4 and IPv6 addresses in `ifconfig` output for one interface."""
    addresses = set()
    for line in text.splitlines():
        fields = line.split()
        if len(fields) >= 2 and fields[0] in ("inet", "inet6"):
            addresses.add(fields[1].split("%")[0])
    return addresses


def parse_local_addresses(text):
    """Return the local address of each distinct connection in `lsof -F n` output."""
    connections = {line[1:] for line in text.splitlines() if line.startswith("n") and "->" in line}
    return [
        connection.split("->")[0].rsplit(":", 1)[0].strip("[]")
        for connection in sorted(connections)
    ]


def resolve(host):
    try:
        return str(ipaddress.ip_address(host))
    except ValueError:
        pass
    try:
        return socket.getaddrinfo(host, None, type=socket.SOCK_STREAM)[0][4][0]
    except OSError:
        raise TaskError("the route guard could not resolve its host") from None


def guard_route(host, interface):
    """Refuse to continue unless `host` routes through `interface`.

    Established TCP connections to `host`, such as a paired desktop's WSS connection, must also
    use a local address on `interface`; a connection opened before the route changed keeps its
    old interface.
    """
    address = resolve(host)
    family = "-inet6" if ":" in address else "-inet"
    route = subprocess.run(
        ["route", "-n", "get", family, address], capture_output=True, text=True, check=False
    )
    actual = parse_route_interface(route.stdout)
    if actual != interface:
        raise TaskError(f"the route to {address} uses {actual or 'no interface'}, not {interface}")
    configured = subprocess.run(
        ["ifconfig", interface], capture_output=True, text=True, check=False
    )
    local = parse_interface_addresses(configured.stdout)
    connections = subprocess.run(
        ["lsof", "-nP", f"-iTCP@{address}", "-sTCP:ESTABLISHED", "-F", "n"],
        capture_output=True,
        text=True,
        check=False,
    )
    established = parse_local_addresses(connections.stdout)
    elsewhere = [source for source in established if source not in local]
    if elsewhere:
        raise TaskError(
            f"{len(elsewhere)} established connections to {address} use another interface; "
            "reconnect them before measuring"
        )
    print(
        f"route guard: {address} uses {interface}; "
        f"{len(established)} established connections use it too"
    )


def add_route_arguments(parser, *, host=True):
    """Add the route guard options to a task's argument parser."""
    if host:
        parser.add_argument("--route-host", help="Remote host whose route the guard checks")
    parser.add_argument(
        "--interface", help="Refuse to run unless the remote host routes through this interface"
    )


def check_route(args, host=None):
    """Run the route guard when the task's arguments ask for it."""
    host = host or getattr(args, "route_host", None)
    if args.interface:
        if not host:
            raise TaskError("--interface needs a remote host to check")
        guard_route(host, args.interface)


@contextmanager
def screen_lock():
    """Hold the screen and keyboard lock while a probe injects input or captures the screen."""
    waiting = False
    while True:
        try:
            SCREEN_LOCK.mkdir()
            break
        except FileExistsError:
            if not waiting:
                print("waiting for another probe to release the screen", file=sys.stderr)
                waiting = True
            time.sleep(5)
    try:
        yield
    finally:
        SCREEN_LOCK.rmdir()


def pixel_probe():
    """Compile the Swift pixel probe once per source revision and return its path."""
    source = PIXEL_SOURCE.read_bytes()
    directory = ROOT / "target" / "bench"
    executable = directory / f"latency-pixels-{hashlib.sha256(source).hexdigest()[:16]}"
    if not os.access(executable, os.X_OK):
        directory.mkdir(parents=True, exist_ok=True)
        partial = executable.with_suffix(".partial")
        compiled = subprocess.run(
            ["xcrun", "swiftc", "-O", "-swift-version", "5", str(PIXEL_SOURCE), "-o", partial],
            check=False,
        )
        if compiled.returncode:
            raise TaskError("the Swift pixel probe did not compile; run mise doctor")
        partial.replace(executable)
    return executable


def measure_pixels(app, mode, samples, workload=None):
    """Run the pixel probe under the screen lock and return its results."""
    executable = pixel_probe()
    check = subprocess.run([executable, "check"], check=False)
    if check.returncode:
        raise TaskError(PERMISSIONS)
    command = [executable, "measure", "--app", APPS[app], "--mode", mode, "--samples", str(samples)]
    if workload:
        command += ["--workload", workload]
    print(f"measuring {APPS[app]}; leave the keyboard, mouse and screen alone", file=sys.stderr)
    with screen_lock():
        probe = subprocess.run(command, stdout=subprocess.PIPE, text=True, check=False)
    if probe.returncode:
        raise TaskError("the pixel probe stopped before it finished")
    return [json.loads(line) for line in probe.stdout.splitlines() if line.strip()]


def shell_directory(directory=None):
    """Quote a checkout for a shell on any host, defaulting to this one's path from `~`.

    A leading `~/` stays unquoted so the shell on the other host expands it to its own home.
    """
    if directory is None:
        try:
            directory = "~/" + str(ROOT.relative_to(Path.home()))
        except ValueError:
            directory = str(ROOT)
    if directory.startswith("~/"):
        return "~/" + shlex.quote(directory[2:])
    return shlex.quote(directory)
