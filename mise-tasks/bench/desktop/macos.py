#!/usr/bin/env python
# MISE description="Measure an installed SpaceTerm app's CPU, memory and wakeups in ten scenarios with Accessibility off and on"
# USAGE flag "--app <app>" help="Installed app to measure; reference is v0.6.8, installed as SpaceTerm" default="preflight" {
# USAGE   choices "preflight" "release" "reference"
# USAGE }
# USAGE flag "--scenarios <list>" help="Comma-separated scenarios S1 to S10" default="S1,S2,S3,S4,S5,S6,S7,S8,S9,S10"
# USAGE flag "--runs <count>" help="Runs of each scenario" default="3"
# USAGE flag "--accessibility <list>" help="Comma-separated Accessibility states: off, on" default="off,on"
# USAGE flag "--seconds <seconds>" help="Duration of the S7 workloads" default="60"
# USAGE flag "--idle-seconds <seconds>" help="Duration of the S2 to S5 idle observations" default="60"
# USAGE flag "--reference <file>" help="results.json of an --app reference run on this Mac with the same settings, for the ratio budgets"
# USAGE flag "--no-budgets" help="Skip the budget check"
# USAGE flag "--input-python <path>" help="Python that holds the Accessibility permission and posts input" default="python3"
# USAGE flag "--output <directory>" help="Directory for results.json; defaults to target/bench/desktop/TIME"
"""Measure an installed SpaceTerm app in the ten scenarios of #773.

The task drives the app with posted keyboard and mouse events and reads the window list, so it
never queries the app through the Accessibility API while Accessibility is off. Accessibility
work is demanded per Pane and ends when the Pane is hidden. So with Accessibility on, the task
reads the text of every Pane it creates or focuses through the Accessibility API, as an
assistive application does, and fails unless the read reached that Pane. Each state starts with
a fresh client. Workloads run in the focused Pane and finish with a receipt file that the Pane's
shell writes; a failed workload fails the task. Before each workload the task waits until the
app is idle, so the workload's measurement excludes the typed command and the cleared Pane.

Every observation records each SpaceTerm process's CPU seconds, RSS, physical footprint, peak
footprint and wakeups per second. Roles are `client`, `server`, `shell` and `program`; `product` is
the client and server, and its peak is their highest simultaneous sum. Every observation after S1
also records how much the server's Terminal Diagnostics counters moved, and the client's while it
runs, read from `spaceterm server status` and `spaceterm diagnostics` outside the measured window;
a failed read fails the task. Results go to `--output` after each observation, then the task checks
the medians against mise-tasks/bench/budgets.json and fails when a budget is missed or a requested
budget has no measurement.

`--app reference` measures v0.6.8, the build the ratio budgets compare with, and checks no budgets.
v0.6.8 is one process without a server, a CLI or a Pane identity in the Pane's environment, so this
run identifies a Pane by its shell's session, records no counters, and records nothing in S4, whose
quit ends v0.6.8. `--reference` takes its results.json and refuses one that measured other work:
other durations, workloads, window sizes or `cat` input, another Mac or macOS version, or another
app version, or one that lacks a measurement a requested ratio budget compares with.

The app needs a shell prompt in its focused Pane. The Python given by `--input-python` must hold
the Accessibility permission in System Settings > Privacy & Security. Leave the keyboard, mouse
and screen alone while the task runs. Measure one product at a time: stop the other product's
client and server first.
"""

import argparse
import json
import os
import re
import signal
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, main
from spaceterm_tasks.bench import (
    CLOCK,
    DESKTOP_REFERENCE_VERSION,
    MIB,
    Jobs,
    Sampler,
    check_receipt,
    check_seconds,
    classify,
    cli_json,
    compare_desktop_reference,
    counter_difference,
    delta,
    environment,
    evaluate,
    fixed_input,
    load_budgets,
    processes,
    retained_rows,
    settle,
    snapshot,
    summarize,
    terminal_counters,
    wait_ready,
    write_json,
)
from spaceterm_tasks.latency import screen_lock

INPUT_HELPER = ROOT / "mise-tasks" / "lib" / "spaceterm_tasks" / "desktop_input.py"
SCENARIOS = tuple(f"S{number}" for number in range(1, 11))
WINDOW = (900, 580)
LARGE_WINDOW = (1100, 740)
# Where a window moves when the larger size would not fit on the display.
ORIGIN = (80, 80)
COLD_SETTLE_SECONDS = 10
FILLED_PANES = 10
FILL_LINES = 100_000
YES_LINES = 20_000_000
TYPED_KEYS = 450
TYPING_RATE = 15
RESIZE_SECONDS = 20
RESIZE_INTERVAL_SECONDS = 0.5
# The work each scenario does beyond its durations; a reference must have done the same.
WORKLOADS = {
    "cold_settle_s": COLD_SETTLE_SECONDS,
    "filled_panes": FILLED_PANES,
    "fill_lines": FILL_LINES,
    "yes_lines": YES_LINES,
    "typed_keys": TYPED_KEYS,
    "typing_rate": TYPING_RATE,
    "resize_s": RESIZE_SECONDS,
    "resize_interval_s": RESIZE_INTERVAL_SECONDS,
}
# v0.6.8 keeps at most this many Scrollback rows per Pane (src/terminal/emulator.rs).
REFERENCE_SCROLLBACK_ROWS = 10_000
# The longest a workload's presentation work may continue after it ends.
SETTLE_LIMIT_SECONDS = 120
# The CLI command that reads the desktop's Terminal Diagnostics counters
# (crates/spaceterm-cli/golden/diagnostics.json).
CLIENT_COUNTERS = ("diagnostics",)


@dataclass(frozen=True)
class App:
    name: str
    identity: str

    @property
    def bundle(self):
        return Path("/Applications") / f"{self.name}.app"

    @property
    def executable(self):
        return str(self.bundle / "Contents" / "MacOS" / self.name)

    @property
    def installations(self):
        return Path.home() / ".local" / "share" / self.identity / "server" / "installations"

    def is_server(self, command):
        return command.startswith(str(self.installations) + "/") and command.endswith("/spaceterm")


PREFLIGHT = App("SpaceTerm Preflight", "spaceterm-preflight")
RELEASE = App("SpaceTerm", "spaceterm")


class Input:
    """The input helper: a Python process that holds the Accessibility permission."""

    def __init__(self, python):
        self.process = subprocess.Popen(
            [python, "-I", str(INPUT_HELPER)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
        )
        if not self.call("check")["trusted"]:
            raise TaskError(
                "the input Python lacks the Accessibility permission; pass --input-python with one "
                "that holds it in System Settings > Privacy & Security"
            )

    def call(self, operation, **arguments):
        assert self.process.stdin is not None and self.process.stdout is not None
        self.process.stdin.write(json.dumps({"op": operation, **arguments}) + "\n")
        self.process.stdin.flush()
        line = self.process.stdout.readline()
        if not line:
            raise TaskError("the input helper exited")
        response = json.loads(line)
        if not response.pop("ok"):
            raise TaskError(f"the input helper refused {operation}")
        return response

    def key(self, key, *modifiers):
        self.call("key", key=key, modifiers=list(modifiers))

    def close(self):
        if self.process.stdin:
            self.process.stdin.close()
        self.process.wait(timeout=10)


def wait_until(condition, timeout, failure, interval=0.05):
    deadline = time.monotonic() + timeout
    while True:
        result = condition()
        if result:
            return result
        if time.monotonic() > deadline:
            if failure is None:
                return result
            raise TaskError(failure)
        time.sleep(interval)


class Desktop:
    """One installed app: its processes, window and focused Pane.

    The app's architecture decides how the task finds its processes and Panes, starts its shells
    and cleans up, so `CurrentDesktop` and `ReferenceDesktop` provide those operations.
    """

    def __init__(self, app, keyboard, jobs, accessibility, clock=CLOCK):
        self.app = app
        self.clock = clock
        self.input = keyboard
        self.jobs = jobs
        self.accessibility = accessibility
        self.reads = []
        # The Pane each read text area belongs to, per client process.
        self.elements = {}

    # Operations each architecture provides

    def roots(self, rows):
        """The app's own processes in a process listing, by pid, with their roles."""
        raise NotImplementedError

    def pane(self, receipt):
        """The Pane whose shell wrote `receipt`, or a TaskError when the receipt cannot tell."""
        raise NotImplementedError

    def stop_server(self):
        raise NotImplementedError

    def start_server(self):
        raise NotImplementedError

    def start_shell(self):
        """Waits until the focused Pane runs a shell. Returns whether that took Start Session."""
        raise NotImplementedError

    def close_panes(self, panes):
        """Closes `panes`, which this task created, and leaves the Panes it found."""
        raise NotImplementedError

    def retained_rows(self, receipt):
        """Rows, Scrollback and screen, that the Pane retains after the fill that wrote
        `receipt`."""
        raise NotImplementedError

    def counters(self, client=True):
        """Terminal Diagnostics counters, `{"server": ..., "client": ...}`. `client` False
        reads no client counters, for a run without a client. An app that keeps counters fails
        the task when a read fails; v0.6.8 keeps none."""
        raise NotImplementedError

    # Processes

    def members(self):
        rows = processes()
        return classify(rows, self.roots(rows))

    def pids(self, role):
        return [member["pid"] for member in self.members() if member["role"] == role]

    def client(self):
        clients = self.pids("client")
        return clients[0] if clients else None

    def shells(self):
        return len(self.pids("shell"))

    def wait_shells(self, count, timeout=30):
        wait_until(lambda: self.shells() == count, timeout, f"expected {count} shells")

    # Lifecycle

    def launch(self):
        subprocess.run(["open", "-a", str(self.app.bundle)], check=True)

    def terminate(self):
        """Ends the client with SIGTERM, which leaves the server and its Terminal Sessions."""
        for pid in self.pids("client"):
            os.kill(pid, signal.SIGTERM)
        wait_until(lambda: not self.pids("client"), 20, "the client did not exit")

    # Window and focus

    def window(self):
        pid = self.client()
        if pid is None:
            return None
        for window in self.input.call("windows", pid=pid)["windows"]:
            if window["layer"] == 0 and window["bounds"][3] > 200:
                return window
        return None

    def frontmost(self):
        front = subprocess.run(
            ["lsappinfo", "front"], capture_output=True, text=True
        ).stdout.strip()
        info = subprocess.run(
            ["lsappinfo", "info", "-only", "pid", front], capture_output=True, text=True
        )
        match = re.search(r"=\s*(\d+)", info.stdout)
        return int(match.group(1)) if match else None

    def show_finder(self):
        subprocess.run(["open", "-a", "Finder"], check=True)

    def activate(self):
        self.launch()
        wait_until(self.window, 10, f"{self.app.name} shows no window")
        wait_until(
            lambda: self.frontmost() == self.client(),
            5,
            f"{self.app.name} did not come to the front",
        )
        time.sleep(0.3)

    def bounds(self):
        window = self.window()
        if window is None:
            raise TaskError(f"{self.app.name} shows no window")
        return window["bounds"]

    def resize(self, width, height):
        """Drags the window's lower-right corner, from inside the frame or just outside it."""
        for inset in (2, -2):
            x, y, current_width, current_height = self.bounds()
            if abs(current_width - width) <= 2 and abs(current_height - height) <= 2:
                return
            corner = [x + current_width - inset, y + current_height - inset]
            self.input.call(
                "drag", **{"from": corner, "to": [x + width - inset, y + height - inset]}
            )
            time.sleep(0.2)
        x, y, current_width, current_height = self.bounds()
        if abs(current_width - width) > 2 or abs(current_height - height) > 2:
            raise TaskError("the window did not take the requested size")

    def place(self, width, height):
        """Gives the window this size, first moving it when the larger size would not fit."""
        x, y, current_width, current_height = self.bounds()
        display = self.input.call("display")["bounds"]
        fits = x + LARGE_WINDOW[0] <= display[2] and y + LARGE_WINDOW[1] <= display[3]
        if not fits:
            # A smaller window first, then its upper-left corner, moves the frame's origin.
            self.resize(min(current_width, WINDOW[0]), min(current_height, WINDOW[1]))
            x, y, current_width, current_height = self.bounds()
            for inset in (2, -2):
                self.input.call(
                    "drag",
                    **{
                        "from": [x + inset, y + inset],
                        "to": [ORIGIN[0] + inset, ORIGIN[1] + inset],
                    },
                )
                time.sleep(0.2)
                x, y, current_width, current_height = self.bounds()
                if x + LARGE_WINDOW[0] <= display[2] and y + LARGE_WINDOW[1] <= display[3]:
                    break
            else:
                raise TaskError("move the window toward the display's upper-left corner")
        self.resize(width, height)

    # The focused Pane

    def type(self, text):
        if self.frontmost() != self.client():
            raise TaskError(
                f"{self.app.name} is no longer the frontmost app; leave the screen alone"
            )
        self.input.call("type", text=text)

    def run_job(self, kind, **parameters):
        """Types workload `kind` into the focused Pane and waits until the app is idle after the
        Pane cleared. Returns the workload's number and the seconds spent settling."""
        number, command = self.jobs.prepare(kind, **parameters)
        self.input.key("u", "control")
        self.type(command + "\n")
        settled = wait_ready(self.jobs, number, self.members(), clock=self.clock)
        return number, settled

    def finish_job(self, number, kind, timeout, **parameters):
        return check_receipt(self.jobs.receipt(number, timeout), kind, **parameters)

    def receipt(self):
        """Proves the focused Pane runs typed commands: its shell writes a receipt file. Returns
        the Pane that wrote it."""
        number, _ = self.run_job("receipt")
        self.jobs.start(number)
        return self.pane(self.finish_job(number, "receipt", 30))

    def focused(self, step):
        """Proves the focused Pane runs typed commands and, with Accessibility on, demands its
        accessibility work. Call after every change of the focused or visible Panes, with
        `step` naming the change. Returns the Pane."""
        pane = self.receipt()
        if self.accessibility:
            self.read_text(step, pane)
        return pane

    def read_text(self, step, pane):
        """Demands Pane `pane`'s accessibility work: reads the focused Pane's text through the
        Accessibility API, and fails unless the read reached a text area of that Pane alone."""
        client = self.client()
        result = self.input.call("read_text", pid=client)
        self.reads.append(dict(result, step=step, pane=pane))
        if not result.get("read"):
            raise TaskError(f"the Accessibility read failed ({result.get('reason')})")
        owner = self.elements.setdefault((client, result["element"]), pane)
        if owner != pane:
            raise TaskError("the Accessibility read reached another Pane than the focused one")

    def ready(self):
        """One visible, frontmost window of WINDOW size whose focused Pane runs typed commands."""
        if not self.pids("server"):
            self.start_server()
        if self.client() is None or self.window() is None:
            self.launch()
            if wait_until(self.window, 10, None) is None:
                # A minimized or closed window may not return on reopen; a new client shows one.
                if self.client() is not None:
                    self.terminate()
                self.launch()
        self.activate()
        self.place(*WINDOW)
        self.start_shell()
        self.focused("ready")

    def restart(self):
        if self.client() is not None:
            self.terminate()
        self.ready()


class CurrentDesktop(Desktop):
    """A client and a local server, which the task reaches through the server's CLI.

    The server keeps the Panes when the client ends, and a Pane's shell knows its Pane from
    `SPACETERM_PANE`.
    """

    def roots(self, rows):
        roots = {}
        for row in rows:
            if row["command"] == self.app.executable:
                roots[row["pid"]] = "client"
            elif self.app.is_server(row["command"]):
                roots[row["pid"]] = "server"
        return roots

    def pane(self, receipt):
        if not receipt.get("pane"):
            raise TaskError("the focused Pane's shell does not know its Pane")
        return receipt["pane"]

    def cli(self):
        launcher = Path.home() / ".local" / "bin" / self.app.identity
        if launcher.exists():
            return str(launcher)
        installed = sorted(self.app.installations.glob("*/*.app/Contents/MacOS/spaceterm"))
        if not installed:
            raise TaskError(f"no installed {self.app.name} server")
        return str(installed[-1])

    def has_server(self):
        return self.app.installations.exists()

    def confirmed(self, *arguments):
        """Runs a `spaceterm` command, confirming the preview it may return. Returns the exit
        status and the error code of the final attempt."""
        command = [self.cli(), "--json", "--no-input", *arguments]
        result = subprocess.run(command, capture_output=True, text=True)
        envelope = json.loads(result.stdout) if result.stdout.strip() else {}
        if result.returncode == 3:
            token = envelope["data"]["confirmation"]["token"]
            result = subprocess.run(command + ["--confirm", token], capture_output=True, text=True)
            envelope = json.loads(result.stdout) if result.stdout.strip() else {}
        return result.returncode, (envelope.get("error") or {}).get("code")

    def service(self, action):
        """Runs `spaceterm server ACTION`."""
        status, code = self.confirmed("server", action)
        if status and not (action == "stop" and code == "stopped"):
            raise TaskError(f"server {action} failed ({code or status})")
        if action == "stop":
            wait_until(lambda: not self.pids("server"), 30, "the server did not stop")

    def stop_server(self):
        if self.has_server():
            self.service("stop")

    def start_server(self):
        if self.has_server():
            self.service("start")

    def start_session(self):
        """A restored Pane offers Start Session after the server restarts; Tab focuses it."""
        self.input.key("tab")
        time.sleep(0.2)
        self.input.key("return")

    def start_shell(self):
        if wait_until(self.shells, 3, None):
            return False
        wait_until(lambda: self.frontmost() == self.client(), 5, "the client is not frontmost")
        self.start_session()
        wait_until(self.shells, 15, "the focused Pane started no shell")
        return True

    def close_panes(self, panes):
        """Closes each Pane through the CLI, and its Workspace with its last Pane, which the
        desktop would only do after a confirmation dialog."""
        for pane in panes:
            status, code = self.confirmed(
                "--server", "local", "pane", "close", "--pane", f"id:{pane}"
            )
            if status:
                raise TaskError(f"spaceterm pane close failed ({code or status})")

    def retained_rows(self, receipt):
        """The rows the CLI reports for the receipt's Pane."""
        command = [self.cli(), "--server", "local", "--json", "--quiet", "pane", "read"]
        command += ["--pane", f"id:{self.pane(receipt)}", "--rows", "1", "--max-bytes", "4096"]
        envelope = cli_json(command)
        if envelope is None:
            raise TaskError("spaceterm pane read did not report the Pane's history")
        return retained_rows(envelope)

    def server_counters(self):
        """The local server's Terminal Diagnostics counters."""
        command = [self.cli(), "--server", "local", "--json", "server", "status"]
        return terminal_counters(command, "spaceterm server status")

    def client_counters(self):
        """The desktop's Terminal Diagnostics counters."""
        return terminal_counters(
            [self.cli(), "--json", *CLIENT_COUNTERS], f"spaceterm {' '.join(CLIENT_COUNTERS)}"
        )

    def counters(self, client=True):
        return {
            "server": self.server_counters(),
            "client": self.client_counters() if client else None,
        }


class ReferenceDesktop(Desktop):
    """SpaceTerm v0.6.8, the build the ratio budgets compare with.

    v0.6.8 is one process that runs its Panes' shells itself. It has no server and no CLI, and
    its Panes' environment names no Pane. Its Panes end with the process and a launch shows one
    new Pane, which starts its shell at once.
    """

    def roots(self, rows):
        return {row["pid"]: "client" for row in rows if row["command"] == self.app.executable}

    def pane(self, receipt):
        """A Pane's shell leads the session of the Pane's terminal, so the session id names the
        Pane while it lives."""
        if not receipt.get("session"):
            raise TaskError("the focused Pane's shell reported no session")
        return f"session {receipt['session']}"

    def stop_server(self):
        pass

    def start_server(self):
        pass

    def start_shell(self):
        wait_until(self.shells, 15, "the focused Pane started no shell")
        return False

    def close_panes(self, panes):
        """A new process shows one new Pane, which avoids v0.6.8's close confirmations."""
        self.restart()

    def retained_rows(self, receipt):
        """v0.6.8 cannot report its rows; it keeps the fill's lines up to its Scrollback limit,
        and the screen's rows."""
        return min(receipt["lines"], REFERENCE_SCROLLBACK_ROWS) + receipt["rows"]

    def counters(self, client=True):
        return {"server": None, "client": None}


APPS = {
    "preflight": (PREFLIGHT, CurrentDesktop),
    "release": (RELEASE, CurrentDesktop),
    "reference": (RELEASE, ReferenceDesktop),
}


class Bench:
    """Runs scenarios and records each observation."""

    def __init__(self, desktop, args, output, results, clock=CLOCK):
        self.desktop = desktop
        self.args = args
        self.output = output
        self.results = results
        self.clock = clock
        self.run = 0

    def record(self, scenario, phase, metrics, values=None, counters=None):
        """Stores one observation. `values` are numbers the summary takes medians of;
        `counters` are the Terminal Diagnostics counters that moved, kept per run."""
        measurement = {
            "scenario": scenario,
            "phase": phase,
            "accessibility": "on" if self.desktop.accessibility else "off",
            "run": self.run,
            "metrics": metrics,
            "values": values or {},
            "counters": counters,
        }
        self.results["measurements"].append(measurement)
        write_json(self.output, self.results)
        product = metrics["roles"].get("product", {})
        print(
            f"bench:desktop:macos: {scenario}/{phase} Accessibility {measurement['accessibility']} run {self.run}: "
            f"{product.get('cpu_s', 0):.3f} CPU s, {product.get('footprint_bytes', 0) / MIB:.1f} MiB",
            file=sys.stderr,
        )

    def observe(
        self,
        scenario,
        phase,
        seconds=0.0,
        action=None,
        before=None,
        new_from_zero=False,
        values=None,
        client=True,
    ):
        """CPU and wakeups across `action` or `seconds`, and memory at their end.

        The counters that moved are read outside the measured window. A caller's `before`
        starts the window before the app runs, so that observation records no counters, and
        `client` False reads none from the client."""
        members = self.desktop.members()
        counted = None if before else self.desktop.counters(client=client)
        before = before or snapshot(members)
        with Sampler(members) as sampler:
            if action:
                values = dict(values or {}, **action())
            else:
                self.clock.sleep(seconds)
        after = snapshot(self.desktop.members())
        moved = None
        if counted is not None:
            later = self.desktop.counters(client=client)
            moved = {side: counter_difference(counted[side], later[side]) for side in counted}
        metrics = delta(
            before,
            after,
            new_from_zero=new_from_zero,
            peaks=sampler.peaks,
            product_peak=sampler.product_peak,
        )
        self.record(scenario, phase, metrics, values, counters=moved)

    def job(self, scenario, phase, kind, **parameters):
        """A workload from its start to the end of the presentation work that follows it."""
        desktop = self.desktop
        number, ready_s = desktop.run_job(kind, **parameters)
        members = desktop.members()
        counted = desktop.counters()
        before = snapshot(members)
        with Sampler(members) as sampler:
            desktop.jobs.start(number)
            receipt = desktop.finish_job(number, kind, 900, **parameters)
            settled = settle(members, limit_s=SETTLE_LIMIT_SECONDS, clock=self.clock)
            after = snapshot(desktop.members())
        later = desktop.counters()
        metrics = delta(before, after, peaks=sampler.peaks, product_peak=sampler.product_peak)
        values = {"wall_s": receipt["wall_s"], "ready_settle_s": ready_s, "settle_s": settled}
        moved = {side: counter_difference(counted[side], later[side]) for side in counted}
        self.record(scenario, phase, metrics, values, counters=moved)

    # Scenarios

    def s1(self):
        """Cold start: the first 10 s after a usable shell receipt, from no running processes."""
        desktop = self.desktop
        clock = self.clock
        if desktop.client() is not None:
            desktop.terminate()
        desktop.stop_server()
        started = clock.monotonic()
        before = {"monotonic": time.monotonic(), "processes": []}
        desktop.start_server()
        desktop.launch()
        wait_until(desktop.window, 30, "the cold client showed no window")
        window_s = clock.monotonic() - started
        started_session = desktop.start_shell()
        wait_until(
            lambda: desktop.frontmost() == desktop.client(), 5, "the client is not frontmost"
        )
        pane = desktop.receipt()
        usable_s = clock.monotonic() - started
        if desktop.accessibility:
            desktop.read_text("S1 cold start", pane)

        def hold():
            clock.sleep(max(0, started + usable_s + COLD_SETTLE_SECONDS - clock.monotonic()))
            return {"window_s": window_s, "usable_s": usable_s, "started_session": started_session}

        self.observe("S1", "cold-start", action=hold, before=before, new_from_zero=True)

    def s2(self):
        self.desktop.ready()
        self.desktop.input.key("u", "control")
        self.observe("S2", "focused-idle", self.args.idle_seconds)

    def s3(self):
        desktop = self.desktop
        desktop.ready()
        desktop.show_finder()
        wait_until(
            lambda: desktop.frontmost() != desktop.client(), 5, "Finder did not come to the front"
        )
        self.observe("S3", "unfocused", self.args.idle_seconds)
        desktop.activate()
        desktop.input.key("m", "command")
        wait_until(lambda: desktop.window() is None, 5, "the window did not minimize")
        self.clock.sleep(2)
        self.observe("S3", "minimized", self.args.idle_seconds)

    def s4(self):
        """Quit closes every window while the status menu keeps the client running, then the
        client ends and leaves the server. v0.6.8 has neither: its quit ends it."""
        desktop = self.desktop
        desktop.ready()
        desktop.input.key("q", "command")
        wait_until(lambda: desktop.window() is None, 10, "the windows did not close")
        self.clock.sleep(2)
        if desktop.client() is not None:
            self.observe("S4", "windows-closed", self.args.idle_seconds)
            desktop.terminate()
        if desktop.pids("server"):
            self.observe("S4", "client-quit", self.args.idle_seconds, client=False)

    def s5(self):
        """Twelve idle Panes: two splits here, then three new Workspaces with two splits each.

        The observation exercises the three Panes of the last Workspace, which are the visible
        ones; the other Workspaces' Panes are hidden and have no Accessibility demand. With
        Accessibility on, each Pane is read when it is created, so all three visible Panes hold
        demand during the observation.
        """
        desktop = self.desktop
        desktop.ready()
        base = desktop.shells()
        created = []
        for workspace in range(4):
            if workspace:
                desktop.input.key("n", "command")
                desktop.wait_shells(base + 3 * workspace)
                self.clock.sleep(0.3)
                created.append(desktop.focused(f"S5 Workspace {workspace + 1}"))
            desktop.input.key("d", "command")
            desktop.wait_shells(base + 3 * workspace + 1)
            created.append(desktop.focused(f"S5 Workspace {workspace + 1} split 1"))
            desktop.input.key("d", "command", "shift")
            desktop.wait_shells(base + 3 * workspace + 2)
            created.append(desktop.focused(f"S5 Workspace {workspace + 1} split 2"))
        self.clock.sleep(10)
        self.observe("S5", "idle-panes", self.args.idle_seconds)
        desktop.close_panes(list(reversed(created)))
        desktop.wait_shells(base)

    def s6(self):
        self.desktop.ready()
        self.job("S6", "cat", "cat", file=str(self.args.input))
        self.job("S6", "yes", "yes", lines=YES_LINES)

    def s7(self):
        self.desktop.ready()
        for kind in ("scroll", "partial", "vim"):
            self.job("S7", kind, kind, seconds=self.args.seconds)

    def s8(self):
        """Ten Tabs filled with 100,000 lines each, then closed.

        With Accessibility on, each Tab's Pane is read before it fills, so it fills with demand.
        Only the visible Tab keeps demand, so `filled` records as `rows` the rows that Tab's
        Pane retains; the Accessibility memory budget divides by them. Hidden Tabs' Panes keep
        their history, and any accessibility memory they keep counts against those rows too.
        """
        desktop = self.desktop
        desktop.ready()
        base = desktop.shells()
        desktop.input.key("k", "command")
        self.observe("S8", "empty", 10)
        filled = None
        for tab in range(FILLED_PANES):
            if tab:
                desktop.input.key("t", "command")
                desktop.wait_shells(base + tab)
                self.clock.sleep(0.3)
                desktop.focused(f"S8 Tab {tab + 1}")
            number, _ = desktop.run_job("fill", lines=FILL_LINES)
            desktop.jobs.start(number)
            filled = desktop.finish_job(number, "fill", 120, lines=FILL_LINES)
        rows = desktop.retained_rows(filled)
        settle(desktop.members(), limit_s=SETTLE_LIMIT_SECONDS, clock=self.clock)
        self.observe("S8", "filled", 10, values={"rows": rows, "panes": FILLED_PANES})
        # A fresh Tab stays while the first Tab, filled, closes ten times over.
        desktop.input.key("t", "command")
        desktop.wait_shells(base + FILLED_PANES)
        for _ in range(FILLED_PANES):
            remaining = desktop.shells()
            desktop.input.key("1", "command")
            desktop.input.key("w", "command")
            wait_until(lambda: desktop.shells() < remaining, 10, "a filled Tab did not close")
        desktop.focused("S8 remaining Tab")
        self.observe("S8", "closed", 10)

    def s9(self):
        desktop = self.desktop
        desktop.ready()
        desktop.input.key("u", "control")
        self.observe(
            "S9",
            "typing",
            action=lambda: desktop.input.call("paced", key="a", count=TYPED_KEYS, rate=TYPING_RATE),
        )
        desktop.input.key("u", "control")

    def s10(self):
        desktop = self.desktop
        clock = self.clock
        desktop.ready()
        desktop.place(*WINDOW)
        self.observe("S10", "before", 1)

        def resize():
            started = clock.monotonic()
            count = 0
            while clock.monotonic() - started < RESIZE_SECONDS:
                desktop.resize(*(LARGE_WINDOW if count % 2 == 0 else WINDOW))
                desktop.input.key("b", "command")
                count += 1
                clock.sleep(max(0, started + count * RESIZE_INTERVAL_SECONDS - clock.monotonic()))
            # An even count leaves the window and sidebar as they were.
            if count % 2:
                desktop.resize(*WINDOW)
                desktop.input.key("b", "command")
                count += 1
            return {
                "resizes": count,
                "sidebar_toggles": count,
                "wall_s": clock.monotonic() - started,
            }

        self.observe("S10", "resize-sidebar", action=resize)
        clock.sleep(9)
        self.observe("S10", "after", 1)


def choices(text, allowed, option):
    chosen = [item.strip() for item in text.split(",") if item.strip()]
    if not chosen or any(item not in allowed for item in chosen):
        raise TaskError(f"{option} accepts {', '.join(allowed)}")
    return chosen


def check_isolation(app):
    """The other product must not run, so its processes cannot share the machine."""
    other = RELEASE if app == PREFLIGHT else PREFLIGHT
    rows = processes()
    running = [
        row for row in rows if row["command"] == other.executable or other.is_server(row["command"])
    ]
    if running:
        raise TaskError(
            f"{other.name} is running; quit its client and stop its server before measuring"
        )


def app_version(app):
    result = subprocess.run(
        ["defaults", "read", str(app.bundle / "Contents" / "Info"), "CFBundleShortVersionString"],
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()


def settings(args, scenarios, states):
    """What a run measures. A reference must share everything but its scenarios, states and
    runs."""
    return {
        "scenarios": scenarios,
        "runs": args.runs,
        "accessibility": states,
        "seconds": args.seconds,
        "idle_seconds": args.idle_seconds,
        "window": list(WINDOW),
        "large_window": list(LARGE_WINDOW),
        "workloads": dict(WORKLOADS),
    }


def load_reference(path, results, budgets, scenarios, states):
    """The summary of the reference run in `path`, after checking that it measured the work
    `results` describes and every ratio budget's measurement in `scenarios` and `states`."""
    reference = json.loads(Path(path).read_text())
    reasons = compare_desktop_reference(reference, results, budgets, scenarios, states)
    if reasons:
        raise TaskError(f"{path} is no comparable reference: {'; '.join(reasons)}")
    return reference["summary"]


def report(verdicts):
    for verdict in verdicts:
        status = "pass" if verdict["pass"] else "MISS"
        heading = f"{status} {verdict['budget']} ({verdict['measurement']}, {verdict['bound']})"
        if verdict["missing"]:
            print(f"{heading}: no measurement of {verdict['missing']}")
        else:
            print(f"{heading}: {verdict['value']:.4g} against {verdict['limit']:.4g}")


def run(args):
    if sys.platform != "darwin":
        raise TaskError("bench:desktop:macos needs macOS")
    app, adapter = APPS[args.app]
    if not app.bundle.exists():
        raise TaskError(f"{app.name} is not installed in /Applications")
    version = app_version(app)
    measures_reference = adapter is ReferenceDesktop
    if measures_reference != (version == DESKTOP_REFERENCE_VERSION):
        raise TaskError(
            f"{app.name} is version {version}; --app reference measures "
            f"v{DESKTOP_REFERENCE_VERSION} and the other apps measure later versions"
        )
    if measures_reference and args.reference:
        raise TaskError("--reference compares a later version with a reference run")
    scenarios = choices(args.scenarios, SCENARIOS, "--scenarios")
    states = choices(args.accessibility, ("off", "on"), "--accessibility")
    if args.runs < 1:
        raise TaskError("--runs needs at least 1")
    check_seconds(args.seconds, "--seconds")
    check_seconds(args.idle_seconds, "--idle-seconds")
    check_isolation(app)
    output = (
        Path(args.output)
        if args.output
        else ROOT / "target" / "bench" / "desktop" / time.strftime("%Y%m%d-%H%M%S")
    )
    path = output / "results.json"
    reads = {}
    args.input, digest = fixed_input() if "S6" in scenarios else (None, None)
    results = {
        "environment": dict(
            environment(),
            app=app.name,
            app_version=version,
            adapter="reference" if measures_reference else "current",
            input_sha256=digest,
        ),
        "settings": settings(args, scenarios, states),
        "measurements": [],
        # Saved with every observation, so a failed run keeps the reads made so far.
        "accessibility_reads": reads,
    }
    budgets = load_budgets()
    check = args.budgets and not measures_reference
    reference = None
    if check and args.reference:
        # Checked before measuring, so an incomparable reference costs no run.
        reference = load_reference(args.reference, results, budgets, scenarios, states)
        results["reference"] = str(Path(args.reference).resolve())
    keyboard = Input(args.input_python)
    jobs = Jobs()
    try:
        print(
            f"bench:desktop:macos: measuring {app.name} {version}; leave the keyboard, mouse and "
            "screen alone",
            file=sys.stderr,
        )
        with screen_lock():
            for state in states:
                desktop = adapter(app, keyboard, jobs, state == "on")
                reads[state] = desktop.reads
                bench = Bench(desktop, args, path, results)
                # A fresh client starts each state, so Accessibility from an earlier read is off.
                desktop.restart()
                for scenario in scenarios:
                    for repeat in range(1, args.runs + 1):
                        bench.run = repeat
                        getattr(bench, scenario.lower())()
                write_json(path, results)
    finally:
        keyboard.close()
        jobs.remove()
    results["summary"] = summarize(results["measurements"])
    verdicts = []
    if check:
        verdicts = evaluate(
            results["summary"],
            budgets,
            reference=reference,
            hardware=results["environment"]["hardware"],
            scenarios=scenarios,
            states=states,
        )
        results["budgets"] = verdicts
    write_json(path, results)
    print(f"bench:desktop:macos: results in {path}")
    if not check:
        return 0
    report(verdicts)
    for budget in budgets["desktop"]["unmeasured"]:
        print(f"not measured here: {budget['name']} ({budget['measured_by']})")
    missed = [verdict for verdict in verdicts if not verdict["pass"]]
    if missed:
        raise TaskError(f"{len(missed)} budget checks missed")
    return 0


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", choices=sorted(APPS), default="preflight")
    parser.add_argument("--scenarios", default=",".join(SCENARIOS))
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--accessibility", default="off,on")
    parser.add_argument("--seconds", type=float, default=60)
    parser.add_argument("--idle-seconds", type=float, default=60)
    parser.add_argument("--reference")
    parser.add_argument("--no-budgets", dest="budgets", action="store_false")
    parser.add_argument("--input-python", default="python3")
    parser.add_argument("--output")
    arguments = parser.parse_args()
    main(lambda: run(arguments))
