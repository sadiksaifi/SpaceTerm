#!/usr/bin/env python
# MISE description="Measure a temporary Development server under fixed workloads and fail on a regression against this host's baseline"
# USAGE flag "--runs <count>" help="Runs of each workload and condition" default="3"
# USAGE flag "--seconds <seconds>" help="Duration of the scroll, partial and vim workloads" default="20"
# USAGE flag "--workloads <list>" help="Comma-separated workloads: yes, cat, scroll, partial, vim" default="yes,cat,scroll,partial,vim"
# USAGE flag "--conditions <list>" help="Comma-separated conditions: plain, accessibility" default="plain,accessibility"
# USAGE flag "--host <name>" help="Baseline name; defaults to this machine's short host name"
# USAGE flag "--record" help="Store the medians as this host's baseline instead of comparing"
# USAGE flag "--output <directory>" help="Directory for results.json; defaults to target/bench/server/TIME"
"""Measure a temporary Development server under fixed workloads.

The task builds the server and a presentation client from this checkout in release mode, starts
a foreground Development server and creates one Workspace through the client. The client claims
its Pane at 120 x 40 cells, presents it at 60 Hz and acknowledges every frame. In the
`accessibility` condition it also fetches every accessibility revision the server announces, as
the desktop does while an assistive application reads the Pane; in `plain` it fetches none.

Each workload runs in that Pane: `yes | head -n 20000000`, `cat` of a fixed 200 MB file, and
bench:workload's scroll and partial modes and a vim redraw loop for `--seconds`. The Pane's shell
writes a receipt when a workload finishes, after which the task waits for presentation work to
settle. The task records the server's CPU seconds, peak footprint, footprint 10 s after the
workload and the bytes the server sent the client, then confirms with `spaceterm pane read` that
the Pane shows the workload's completion marker.

Results go to `--output`. The medians are compared with mise-tasks/bench/baselines/server/HOST.json;
the task fails when one exceeds the baseline by more than mise-tasks/bench/budgets.json allows.
`--record` replaces the baseline instead.

On macOS the server uses the account's Development state, so no other Development server may run.
On Linux with bwrap, the server, client and CLI run with private, temporary Development state, so
another Development server keeps running.
"""

import argparse
import contextlib
import json
import os
import queue
import select
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, developer_environment, main
from spaceterm_tasks.bench import (
    BASELINES,
    MARKER,
    MIB,
    Jobs,
    Sampler,
    delta,
    environment,
    fixed_input,
    host_name,
    load_budgets,
    median,
    regressions,
    settle,
    snapshot,
    write_json,
)
from spaceterm_tasks.cargo import build_executable
from spaceterm_tasks.development_supervision import READY, development_run, source_environment

IDENTITY = "spaceterm-development"
BUILD = ("--package", "spaceterm-cli", "--release", "--locked")
WORKLOADS = ("yes", "cat", "scroll", "partial", "vim")
CONDITIONS = ("plain", "accessibility")
AFTER_SECONDS = 10
METRICS = ("cpu_s", "peak_footprint_bytes", "after_footprint_bytes", "bytes_sent")


class Jail:
    """Private Development server state for every process the task starts, through bwrap."""

    def __init__(self):
        self.directory = None
        if sys.platform.startswith("linux") and shutil.which("bwrap"):
            self.directory = Path(
                tempfile.mkdtemp(prefix="spaceterm-bench-server-", dir="/var/tmp")
            )
            for name in ("share", "runtime"):
                (self.directory / name).mkdir(mode=0o700)
            self.share = Path.home() / ".local" / "share" / IDENTITY
            self.runtime = Path("/tmp") / f"{IDENTITY}-server-{os.geteuid()}"
            for mount in (self.share, self.runtime):
                mount.mkdir(mode=0o700, parents=True, exist_ok=True)

    def wrap(self, command):
        if self.directory is None:
            return [str(part) for part in command]
        return [
            "bwrap", "--dev-bind", "/", "/",
            "--bind", str(self.directory / "share"), str(self.share),
            "--bind", str(self.directory / "runtime"), str(self.runtime),
            "--", *[str(part) for part in command],
        ]  # fmt: skip

    def remove(self):
        if self.directory is not None:
            shutil.rmtree(self.directory, ignore_errors=True)


def lines(stream, sink):
    for line in stream:
        sink.put(line)
    sink.put(None)


class Server:
    """A foreground Development server that ends, with its Terminal Sessions, when its input closes."""

    def __init__(self, executable, jail, env):
        self.jailed = jail.directory is not None
        self.process = subprocess.Popen(
            jail.wrap([executable, "server", "run", "--owned"]),
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
        )
        assert self.process.stdout is not None
        ready, _, _ = select.select([self.process.stdout], [], [], 30)
        line = self.process.stdout.readline() if ready else b""
        if line != READY:
            self.stop()
            raise TaskError(
                "the Development server did not start: another Development server runs, or this account's Development service state forbids a foreground server, as it does for mise run development"
            )
        self.pid = self.find(executable)

    def find(self, executable):
        """The server's own pid: the child itself, or bwrap's descendant running `executable`."""
        if not self.jailed:
            return self.process.pid
        from spaceterm_tasks.bench import classify, processes

        target = str(Path(executable).resolve())
        for row in classify(processes(), {self.process.pid: "jail"}):
            if row["role"] != "jail" and row["command"] == target:
                return row["pid"]
        raise TaskError("the Development server process was not found")

    def stop(self):
        if self.process.stdin:
            self.process.stdin.close()
        try:
            self.process.wait(timeout=60)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            self.process.wait()


class Client:
    """The presentation client: it owns the bench Workspace and types workload commands."""

    def __init__(self, executable, jail, env, accessibility):
        command = [executable, IDENTITY] + (["--accessibility"] if accessibility else [])
        self.process = subprocess.Popen(
            jail.wrap(command), env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True
        )
        self.output = queue.Queue()
        threading.Thread(target=lines, args=(self.process.stdout, self.output), daemon=True).start()
        ready = self.next(30)
        if ready.get("event") != "ready":
            raise TaskError("the presentation client did not attach its Pane")
        self.workspace = ready["workspace"]
        self.pane = ready["pane"]

    def next(self, timeout):
        try:
            line = self.output.get(timeout=timeout)
        except queue.Empty:
            raise TaskError("the presentation client did not answer") from None
        if line is None:
            raise TaskError("the presentation client exited")
        return json.loads(line)

    def send(self, command):
        assert self.process.stdin is not None
        self.process.stdin.write(command + "\n")
        self.process.stdin.flush()

    def type(self, text):
        self.send(f"type {text}")

    def stats(self):
        self.send("stats")
        return self.next(30)

    def close(self):
        """Ends the client, which kills its Workspace first."""
        if self.process.stdin:
            self.process.stdin.close()
        try:
            self.process.wait(timeout=60)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            self.process.wait()
        if self.process.returncode:
            raise TaskError("the presentation client could not close its Workspace")


def confirm(cli, jail, env, client, number):
    """Observes the Pane through the CLI until it shows the workload's completion marker."""
    command = [cli, "--server", "local", "--quiet", "--timeout", "30s", "pane", "read"]
    command += ["--pane", f"id:{client.pane}", "--rows", "1", "--max-bytes", "4096"]
    command += ["--wait-for", f"{MARKER} {number}"]
    # The output is terminal content; only the exit status matters.
    result = subprocess.run(
        jail.wrap(command), env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
    )
    if result.returncode:
        raise TaskError(f"spaceterm pane read did not see workload {number} finish")


def measure(context, client, workload, args):
    jobs, server = context["jobs"], context["server"]
    members = [{"pid": server.pid, "role": "server", "command": "spaceterm"}]
    number, command = jobs.prepare(
        workload, file=str(context["input"]), seconds=args.seconds, lines=20_000_000
    )
    client.type(command)
    jobs.wait(number, ".ready", 60)
    sent = client.stats()
    before = snapshot(members)
    with Sampler([server.pid]) as sampler:
        jobs.start(number)
        receipt = jobs.receipt(number, 900)
        settled = settle(members)
        after = snapshot(members)
    received = client.stats()
    time.sleep(AFTER_SECONDS)
    later = snapshot(members)
    confirm(context["cli"], context["jail"], context["env"], client, number)
    window = delta(before, after, peaks=sampler.peaks)["processes"]
    if not window or not later["processes"]:
        raise TaskError("the Development server exited during the workload")
    row = window[0]
    values = {
        "cpu_s": row["cpu_s"],
        "peak_footprint_bytes": row["peak_footprint_bytes"],
        "after_footprint_bytes": later["processes"][0]["usage"]["footprint_bytes"],
        "bytes_sent": received["bytes"] - sent["bytes"],
    }
    details = {
        "elapsed_s": after["monotonic"] - before["monotonic"],
        "settle_s": settled,
        "frames": received["frames"] - sent["frames"],
        "accessibility_revisions": received["accessibility_revisions"]
        - sent["accessibility_revisions"],
        "accessibility_bytes": received["accessibility_bytes"] - sent["accessibility_bytes"],
        "receipt": receipt,
    }
    if "written_bytes" in row:
        details["written_bytes"] = row["written_bytes"]
    return values, details


def summarize(runs):
    groups = {}
    for run in runs:
        groups.setdefault(f"{run['workload']}/{run['condition']}", []).append(run["values"])
    return {
        name: {metric: median([values[metric] for values in group]) for metric in METRICS}
        for name, group in groups.items()
    }


def table(summary):
    lines = [
        "| Workload | Condition | CPU s | Peak MiB | After 10 s MiB | Sent MiB |",
        "|---|---|---|---|---|---|",
    ]
    for name, values in sorted(summary.items()):
        workload, condition = name.split("/")
        lines.append(
            f"| {workload} | {condition} | {values['cpu_s']:.3f} | "
            f"{values['peak_footprint_bytes'] / MIB:.1f} | {values['after_footprint_bytes'] / MIB:.1f} | "
            f"{values['bytes_sent'] / MIB:.2f} |"
        )
    return "\n".join(lines)


def choices(text, allowed, option):
    chosen = [item.strip() for item in text.split(",") if item.strip()]
    if not chosen or any(item not in allowed for item in chosen):
        raise TaskError(f"{option} accepts {', '.join(allowed)}")
    return chosen


def run(args):
    workloads = choices(args.workloads, WORKLOADS, "--workloads")
    conditions = choices(args.conditions, CONDITIONS, "--conditions")
    if args.runs < 1:
        raise TaskError("--runs needs at least 1")
    host = args.host or host_name()
    output = (
        Path(args.output)
        if args.output
        else ROOT / "target" / "bench" / "server" / time.strftime("%Y%m%d-%H%M%S")
    )
    budgets = load_budgets()["server"]
    executable = build_executable(*BUILD, binary="spaceterm")
    presenter = build_executable(*BUILD, binary="server_bench", example=True)
    input_path, input_digest = fixed_input()
    env = developer_environment(source_environment(os.environ))
    jail = Jail()
    runs = []
    with contextlib.ExitStack() as cleanup:
        cleanup.callback(jail.remove)
        jobs = Jobs()
        cleanup.callback(jobs.remove)
        # Without a jail, the server uses the account's Development state, as a source run does.
        if jail.directory is None:
            cleanup.enter_context(development_run())
        server = Server(executable, jail, env)
        cleanup.callback(server.stop)
        context = {"jobs": jobs, "server": server, "cli": executable, "jail": jail, "env": env}
        context["input"] = input_path
        for condition in conditions:
            client = Client(presenter, jail, env, condition == "accessibility")
            with contextlib.ExitStack() as presentation:
                presentation.callback(client.close)
                number, command = jobs.prepare("receipt")
                client.type(command)
                jobs.wait(number, ".ready", 60)
                jobs.start(number)
                jobs.receipt(number, 30)
                for workload in workloads:
                    for repeat in range(1, args.runs + 1):
                        print(
                            f"bench:server: {workload}, {condition}, run {repeat}", file=sys.stderr
                        )
                        values, details = measure(context, client, workload, args)
                        if condition == "accessibility" and not details["accessibility_revisions"]:
                            raise TaskError("the client fetched no accessibility revisions")
                        runs.append(
                            {"workload": workload, "condition": condition, "run": repeat}
                            | {"values": values, "details": details}
                        )
    summary = summarize(runs)
    results = {
        "environment": dict(
            environment(), jailed=jail.directory is not None, input_sha256=input_digest
        ),
        "settings": {
            "runs": args.runs,
            "seconds": args.seconds,
            "columns": 120,
            "rows": 40,
            "max_hz": 60,
        },
        "runs": runs,
        "summary": summary,
    }
    print(table(summary))
    baseline_path = BASELINES / "server" / f"{host}.json"
    if args.record:
        write_json(
            baseline_path,
            {
                "environment": results["environment"],
                "settings": results["settings"],
                "summary": summary,
            },
        )
        write_json(output / "results.json", results)
        print(f"bench:server: recorded the baseline for {host}")
        return 0
    if not baseline_path.exists():
        write_json(output / "results.json", results)
        print(f"bench:server: no baseline for {host}; run with --record to store one")
        return 0
    baseline = json.loads(baseline_path.read_text())
    comparisons = regressions(baseline["summary"], summary, budgets)
    results["baseline"] = {
        "host": host,
        "commit": baseline["environment"].get("commit"),
        "comparisons": comparisons,
    }
    write_json(output / "results.json", results)
    regressed = [entry for entry in comparisons if entry["regressed"]]
    for entry in regressed:
        print(
            f"regression: {entry['measurement']} {entry['metric']} {entry['value']:.6g} "
            f"against baseline {entry['baseline']:.6g}"
        )
    if regressed:
        raise TaskError(
            f"{len(regressed)} medians exceed {host}'s baseline by more than {budgets['regression']:.0%}"
        )
    print(f"bench:server: within {budgets['regression']:.0%} of {host}'s baseline")
    return 0


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--seconds", type=float, default=20)
    parser.add_argument("--workloads", default=",".join(WORKLOADS))
    parser.add_argument("--conditions", default=",".join(CONDITIONS))
    parser.add_argument("--host")
    parser.add_argument("--record", action="store_true")
    parser.add_argument("--output")
    arguments = parser.parse_args()
    main(lambda: run(arguments))
