"""Check the bench:desktop:macos and bench:server measurement, budget and receipt code."""

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from types import SimpleNamespace

from spaceterm_tasks import ROOT, TaskError, bench

PS = """    1     0 /sbin/launchd
  100     1 /Applications/SpaceTerm Preflight.app/Contents/MacOS/SpaceTerm Preflight
  200     1 /Users/me/.local/share/spaceterm-preflight/server/installations/a/SpaceTerm Preflight.app/Contents/MacOS/spaceterm
  201   200 /bin/zsh
  202   201 /bin/cat
  300     1 /usr/bin/yes
"""


def usage(cpu, footprint, wakeups=0):
    return {
        "cpu_s": cpu,
        "rss_bytes": footprint * 2,
        "footprint_bytes": footprint,
        "idle_wakeups": wakeups,
        "interrupt_wakeups": 0,
    }


def measurement(scenario, phase, accessibility, cpu, footprint=100, values=None):
    rows = [{"role": "client", "cpu_s": cpu, "rss_bytes": 0, "footprint_bytes": footprint}]
    rows[0] |= {"peak_footprint_bytes": footprint, "wakeups_per_s": 0.0}
    return {
        "scenario": scenario,
        "phase": phase,
        "accessibility": accessibility,
        "metrics": {"roles": bench.totals(rows, 10.0)},
        "values": values or {},
    }


class ProcessTests(unittest.TestCase):
    def test_classifies_roots_and_their_descendants_by_executable(self):
        rows = bench.parse_ps(PS)
        roles = {
            row["pid"]: row["role"] for row in bench.classify(rows, {100: "client", 200: "server"})
        }
        self.assertEqual(roles, {100: "client", 200: "server", 201: "shell", 202: "program"})

    def test_delta_counts_new_processes_only_when_asked(self):
        before = {
            "monotonic": 0.0,
            "processes": [{"pid": 1, "role": "client", "usage": usage(1.0, 10, 5)}],
        }
        after = {
            "monotonic": 2.0,
            "processes": [
                {"pid": 1, "role": "client", "usage": usage(1.5, 30, 9)},
                {"pid": 2, "role": "server", "usage": usage(0.25, 8)},
            ],
        }
        old = bench.delta(before, after)
        self.assertEqual([row["pid"] for row in old["processes"]], [1])
        self.assertEqual(old["roles"]["client"]["cpu_s"], 0.5)
        self.assertEqual(old["roles"]["client"]["wakeups_per_s"], 2.0)
        fresh = bench.delta(before, after, new_from_zero=True, peaks={2: 50})
        self.assertEqual(fresh["roles"]["product"]["cpu_s"], 0.75)
        self.assertEqual(fresh["roles"]["product"]["footprint_bytes"], 38)
        self.assertEqual(fresh["roles"]["server"]["peak_footprint_bytes"], 50)
        self.assertEqual(fresh["roles"]["product"]["cpu_percent"], 37.5)

    def test_product_peak_is_the_highest_simultaneous_sum(self):
        members = [{"pid": 1, "role": "client"}, {"pid": 2, "role": "server"}]
        mib = bench.MIB
        sampler = bench.Sampler(members)
        sampler.record({1: usage(0, 200 * mib), 2: usage(0, 20 * mib)})
        sampler.record({1: usage(0, 20 * mib), 2: usage(0, 200 * mib)})
        sampler.record({1: usage(0, 30 * mib), 2: None})
        before = {
            "monotonic": 0.0,
            "processes": [dict(member, usage=usage(0, 0)) for member in members],
        }
        after = {
            "monotonic": 1.0,
            "processes": [dict(member, usage=usage(0, 10 * mib)) for member in members],
        }
        roles = bench.delta(before, after, peaks=sampler.peaks, product_peak=sampler.product_peak)[
            "roles"
        ]
        self.assertEqual(roles["client"]["peak_footprint_bytes"], 200 * mib)
        self.assertEqual(roles["server"]["peak_footprint_bytes"], 200 * mib)
        self.assertEqual(roles["product"]["peak_footprint_bytes"], 220 * mib)
        # Without samples, the footprints at the end are one simultaneous observation.
        self.assertEqual(
            bench.delta(before, after)["roles"]["product"]["peak_footprint_bytes"], 20 * mib
        )

    def test_reads_linux_procfs_cpu_memory_and_writes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fields = ["S"] + ["0"] * 10 + ["250", "50"] + ["0"] * 30
            (root / "stat").write_text("42 (server (main)) " + " ".join(fields))
            (root / "status").write_text(
                "VmRSS:\t   2048 kB\nRssAnon:\t   1024 kB\nVmSwap:\t      1 kB\n"
                "voluntary_ctxt_switches:\t7\nnonvoluntary_ctxt_switches:\t3\n"
            )
            (root / "io").write_text("rchar: 1\nwchar: 4096\n")
            result = bench.linux_usage(root)
        assert result is not None
        self.assertAlmostEqual(result["cpu_s"], 300 / bench.os.sysconf("SC_CLK_TCK"))
        self.assertEqual(result["rss_bytes"], 2048 * 1024)
        self.assertEqual(result["footprint_bytes"], 1025 * 1024)
        self.assertEqual((result["idle_wakeups"], result["interrupt_wakeups"]), (7, 3))
        self.assertEqual(result["written_bytes"], 4096)

    def test_a_process_that_ended_has_no_usage(self):
        self.assertIsNone(bench.linux_usage(Path("/nonexistent/process")))


class SummaryTests(unittest.TestCase):
    def test_takes_medians_per_measurement_and_state(self):
        # One slow run must not move the result, as it would move a mean.
        summary = bench.summarize(
            [
                measurement("S6", "yes", "off", 100.0),
                measurement("S6", "yes", "off", 1.0),
                measurement("S6", "yes", "off", 2.0, values={"rows": 10}),
                measurement("S6", "yes", "on", 9.0),
            ]
        )
        self.assertEqual(summary["S6/yes/off"]["runs"], 3)
        self.assertEqual(summary["S6/yes/off"]["roles"]["client"]["cpu_s"], 2.0)
        self.assertEqual(summary["S6/yes/off"]["values"]["rows"], 10)
        self.assertEqual(summary["S6/yes/on"]["roles"]["product"]["cpu_s"], 9.0)

    def test_server_summary_takes_medians_per_workload_and_condition(self):
        def run(cpu, condition="plain"):
            values = dict.fromkeys(bench.SERVER_METRICS, 5) | {"cpu_s": cpu}
            return {"workload": "yes", "condition": condition, "values": values}

        summary = bench.summarize_server(
            [run(100.0), run(1.0), run(2.0), run(7.0, "accessibility")]
        )
        self.assertEqual(summary["yes/plain"]["cpu_s"], 2.0)
        self.assertEqual(summary["yes/plain"]["bytes_sent"], 5)
        self.assertEqual(summary["yes/accessibility"]["cpu_s"], 7.0)


class BudgetTests(unittest.TestCase):
    def budgets(self, *budget):
        return {"desktop": {"hardware": "Chip", "budget": list(budget), "unmeasured": []}}

    def test_absolute_limits_apply_only_on_the_budget_hardware(self):
        summary = bench.summarize([measurement("S1", "cold-start", "off", 1.2)])
        budgets = self.budgets(
            {
                "name": "cpu",
                "measurement": "S1/cold-start",
                "role": "product",
                "metric": "cpu_s",
                "max": 1.1,
            }
        )
        [verdict] = bench.evaluate(summary, budgets, hardware="Chip", states=["off"])
        self.assertFalse(verdict["pass"])
        self.assertEqual(bench.evaluate(summary, budgets, hardware="Other", states=["off"]), [])

    def test_ratios_compare_with_the_reference_and_with_accessibility_off(self):
        summary = bench.summarize(
            [measurement("S6", "yes", "off", 13.0), measurement("S6", "yes", "on", 15.0)]
        )
        reference = bench.summarize(
            [measurement("S6", "yes", "off", 10.0), measurement("S6", "yes", "on", 10.0)]
        )
        budget = {"name": "yes", "measurement": "S6/yes", "role": "product", "metric": "cpu_s"}
        budgets = self.budgets(budget | {"ratio": 1.35, "accessibility_ratio": 1.1})
        verdicts = {
            (v["measurement"], v["bound"]): v
            for v in bench.evaluate(summary, budgets, reference=reference)
        }
        self.assertTrue(verdicts[("S6/yes/off", "ratio")]["pass"])
        self.assertFalse(verdicts[("S6/yes/on", "ratio")]["pass"])
        self.assertAlmostEqual(verdicts[("S6/yes/on", "accessibility_ratio")]["value"], 15 / 13)
        self.assertNotIn(("S6/yes/off", "accessibility_ratio"), verdicts)

    def test_memory_per_rows_and_after_ratio(self):
        summary = bench.summarize(
            [
                measurement("S8", "filled", "off", 1.0, footprint=100 * bench.MIB),
                measurement(
                    "S8", "filled", "on", 1.0, footprint=130 * bench.MIB, values={"rows": 200_000}
                ),
                measurement("S10", "before", "off", 0.0, footprint=100),
                measurement("S10", "after", "off", 0.0, footprint=120),
            ]
        )
        budgets = self.budgets(
            {
                "name": "rows",
                "measurement": "S8/filled",
                "role": "product",
                "metric": "footprint_bytes",
            }
            | {"per_10k_rows": 3 * bench.MIB},
            {
                "name": "after",
                "measurement": "S10/after",
                "role": "product",
                "metric": "footprint_bytes",
            }
            | {"before": "S10/before", "after_ratio": 1.15},
        )
        verdicts = {v["budget"]: v for v in bench.evaluate(summary, budgets)}
        self.assertEqual(verdicts["rows"]["value"], 1.5 * bench.MIB)
        self.assertTrue(verdicts["rows"]["pass"])
        self.assertFalse(verdicts["after"]["pass"])

    def test_a_requested_bound_without_its_measurement_fails(self):
        rows = {"name": "rows", "measurement": "S8/filled", "role": "product"}
        rows = rows | {"metric": "footprint_bytes", "per_10k_rows": 3 * bench.MIB}
        cpu = {"name": "cpu", "measurement": "S6/yes", "role": "product", "metric": "cpu_s"}
        cpu = cpu | {"ratio": 1.35}
        after = {"name": "after", "measurement": "S10/after", "role": "product"}
        after = after | {"metric": "footprint_bytes", "before": "S10/before", "after_ratio": 1.15}
        budgets = self.budgets(rows, cpu, after)
        summary = bench.summarize(
            [
                measurement("S8", "filled", "on", 1.0, values={"rows": 10_000}),
                measurement("S6", "yes", "off", 1.0),
                measurement("S6", "yes", "on", 1.0),
                measurement("S10", "after", "off", 1.0),
                measurement("S10", "after", "on", 1.0),
                measurement("S10", "before", "on", 1.0),
            ]
        )
        reference = bench.summarize([measurement("S6", "yes", "off", 1.0)])
        verdicts = bench.evaluate(summary, budgets, reference=reference)
        missing = {(v["budget"], v["measurement"]): v["missing"] for v in verdicts if v["missing"]}
        self.assertEqual(
            missing,
            {
                ("rows", "S8/filled/on"): "S8/filled/off",
                ("cpu", "S6/yes/on"): "reference S6/yes/on",
                ("after", "S10/after/off"): "S10/before/off",
            },
        )
        self.assertFalse(any(v["pass"] for v in verdicts if v["missing"]))
        # Without its rows, the Accessibility memory bound has nothing to divide by.
        summary = bench.summarize(
            [measurement("S8", "filled", "on", 1.0), measurement("S8", "filled", "off", 1.0)]
        )
        [verdict] = bench.evaluate(summary, self.budgets(rows))
        self.assertEqual(verdict["missing"], "S8/filled/on rows")
        # A requested scenario that recorded nothing fails every bound it requests.
        [verdict] = bench.evaluate({}, self.budgets(rows), scenarios=["S8"])
        self.assertEqual(verdict["missing"], "S8/filled/on")
        self.assertEqual(bench.evaluate({}, self.budgets(rows), scenarios=["S6"]), [])

    def test_compare_server_refuses_incomparable_or_incomplete_baselines(self):
        server = {"regression": 0.1, "floor": {}}
        environment = {key: "same" for key in bench.SERVER_ENVIRONMENT}
        settings = {"seconds": 20, "columns": 120, "rows": 40, "max_hz": 60, "runs": 3}
        values = dict.fromkeys(bench.SERVER_METRICS, 10.0)

        def run(summary, **changes):
            return {
                "environment": environment | changes.get("environment", {}),
                "settings": settings | changes.get("settings", {}),
                "summary": summary,
            }

        baseline = run({"yes/plain": values})
        # Half the CPU in half the time is 60% more CPU per second, not an improvement.
        faster = run({"yes/plain": values | {"cpu_s": 8.0}}, settings={"seconds": 10})
        result = bench.compare_server(baseline, faster, server)
        self.assertEqual(result["status"], "incomparable")
        self.assertEqual(result["reasons"], ["setting seconds differs"])
        other = run({"yes/plain": values}, environment={"hardware": "other"})
        self.assertEqual(bench.compare_server(baseline, other, server)["status"], "incomparable")
        wider = run({"yes/plain": values, "cat/plain": values})
        self.assertEqual(
            bench.compare_server(baseline, wider, server)["reasons"], ["baseline lacks cat/plain"]
        )
        partial = run(
            {"yes/plain": {key: value for key, value in values.items() if key != "cpu_s"}}
        )
        self.assertEqual(
            bench.compare_server(partial, baseline, server)["reasons"],
            ["baseline lacks yes/plain cpu_s"],
        )
        self.assertEqual(bench.compare_server(None, baseline, server)["status"], "incomparable")
        self.assertEqual(
            bench.compare_server(
                baseline, run({"yes/plain": values}, settings={"runs": 1}), server
            )["status"],
            "within",
        )
        slower = run({"yes/plain": values | {"cpu_s": 12.0}})
        self.assertEqual(bench.compare_server(baseline, slower, server)["status"], "regressed")

    def test_a_desktop_reference_must_measure_the_same_work_on_the_same_mac(self):
        scroll = {"name": "scroll", "measurement": "S7/scroll", "role": "product"}
        cold = {"name": "cold", "measurement": "S1/cold-start", "role": "product"}
        budgets = self.budgets(
            scroll | {"metric": "cpu_s", "ratio": 1.35}, cold | {"metric": "cpu_s", "max": 1.1}
        )
        environment = dict.fromkeys(bench.DESKTOP_ENVIRONMENT, "same") | {"input_sha256": "a"}
        settings = dict.fromkeys(bench.DESKTOP_SETTINGS, "same")
        current = {
            "environment": environment | {"app_version": "0.7.0", "adapter": "current"},
            "settings": settings | {"runs": 3, "scenarios": ["S1", "S7"]},
        }
        both = [measurement("S7", "scroll", state, 10.0) for state in ("off", "on")]

        def reference(summary=both, **changes):
            return {
                "environment": environment
                | {"app_version": "0.6.8", "adapter": "reference"}
                | changes.get("environment", {}),
                # Runs and scenarios may differ: only the work each measurement did must match.
                "settings": settings
                | {"runs": 1, "scenarios": ["S7"]}
                | changes.get("settings", {}),
                "summary": bench.summarize(summary),
            }

        def reasons(candidate, scenarios=("S1", "S7"), results=current):
            return bench.compare_desktop_reference(
                candidate, results, budgets, scenarios, ("off", "on")
            )

        self.assertEqual(reasons(reference()), [])
        for key in bench.DESKTOP_SETTINGS:
            with self.subTest(setting=key):
                changed = reference(settings={key: "other"})
                self.assertEqual(reasons(changed), [f"setting {key} differs"])
        for key in (*bench.DESKTOP_ENVIRONMENT, "input_sha256"):
            with self.subTest(environment=key):
                changed = reference(environment={key: "other"})
                self.assertEqual(reasons(changed), [f"environment {key} differs"])
        # A run without S6 has no input to compare.
        no_input = current | {"environment": current["environment"] | {"input_sha256": None}}
        self.assertEqual(
            reasons(reference(environment={"input_sha256": "b"}), results=no_input), []
        )
        self.assertEqual(
            reasons(reference(environment={"app_version": "0.7.0", "adapter": "current"})),
            ["not measured by --app reference", "app version is not 0.6.8"],
        )
        self.assertEqual(
            reasons(reference(both[:1])), ["reference lacks S7/scroll/on product cpu_s"]
        )
        # Only ratio budgets of requested scenarios need the reference.
        self.assertEqual(reasons(reference([]), scenarios=("S1",)), [])

    def test_server_regressions_respect_the_fraction_and_noise_floor(self):
        server = {"regression": 0.1, "floor": {"cpu_s": 0.05}}
        baseline = {"yes/plain": {"cpu_s": 10.0, "bytes_sent": 100}, "cat/plain": {"cpu_s": 0.1}}
        current = {"yes/plain": {"cpu_s": 11.5, "bytes_sent": 109}, "cat/plain": {"cpu_s": 0.14}}
        found = {
            (r["measurement"], r["metric"]): r["regressed"]
            for r in bench.regressions(baseline, current, server)
        }
        self.assertEqual(
            found,
            {
                ("yes/plain", "cpu_s"): True,
                ("yes/plain", "bytes_sent"): False,
                ("cat/plain", "cpu_s"): False,
            },
        )


class JobTests(unittest.TestCase):
    def test_a_job_waits_for_go_and_writes_its_receipt_and_marker(self):
        jobs = bench.Jobs()
        try:
            number, command = jobs.prepare("receipt")
            self.assertTrue(command.startswith(" sh "))
            # A Pane's shell leads its terminal's session; the current app also names the Pane.
            shell = subprocess.Popen(
                command,
                shell=True,
                stdout=subprocess.PIPE,
                text=True,
                start_new_session=True,
                env=dict(os.environ, SPACETERM_PANE="7"),
            )
            jobs.wait(number, ".ready", 30)
            self.assertFalse(jobs.path(number, ".done.json").exists())
            jobs.start(number)
            receipt = jobs.receipt(number, 30)
            output, _ = shell.communicate(timeout=30)
            self.assertEqual(receipt["kind"], "receipt")
            self.assertEqual(receipt["returncode"], 0)
            self.assertEqual((receipt["pane"], receipt["session"]), ("7", shell.pid))
            self.assertIn(f"{bench.MARKER} {number}", output)
            self.assertNotIn(bench.MARKER, command)
        finally:
            jobs.remove()

    def run_job(self, kind, **parameters):
        """Runs workload `kind` through the Pane driver in a shell and returns its receipt."""
        jobs = bench.Jobs()
        try:
            number, command = jobs.prepare(kind, **parameters)
            shell = subprocess.Popen(
                command, shell=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
            )
            jobs.wait(number, ".ready", 30)
            jobs.start(number)
            receipt = jobs.receipt(number, 60)
            shell.wait(timeout=30)
            return receipt
        finally:
            jobs.remove()

    def test_a_failed_workload_is_refused(self):
        receipt = self.run_job("cat", file="/nonexistent/spaceterm-bench-input")
        with self.assertRaises(TaskError):
            bench.check_receipt(receipt, "cat", file="/nonexistent/spaceterm-bench-input")

    def test_yes_ends_by_sigpipe_and_only_head_decides_the_outcome(self):
        receipt = self.run_job("yes", lines=10)
        self.assertNotEqual(receipt["producer_returncode"], 0)
        self.assertIs(bench.check_receipt(receipt, "yes", lines=10), receipt)
        failed = self.run_job("yes", lines="many")
        with self.assertRaises(TaskError):
            bench.check_receipt(failed, "yes", lines="many")

    def test_another_workloads_receipt_is_refused(self):
        receipt = {"kind": "receipt", "returncode": 0}
        with self.assertRaises(TaskError):
            bench.check_receipt(receipt, "cat")
        with self.assertRaises(TaskError):
            bench.check_receipt({"kind": "fill", "lines": 1000}, "fill", lines=100_000)

    def test_durations_outside_the_producers_range_are_refused(self):
        self.assertEqual(bench.check_seconds(20, "--seconds"), 20)
        for seconds in (0, 0.05, 3601):
            with self.subTest(seconds=seconds), self.assertRaises(TaskError):
                bench.check_seconds(seconds, "--seconds")

    def test_measurement_waits_for_setup_work_after_the_ready_receipt(self):
        # A member that keeps working for 0.6 s after the Pane cleared, as a server presenting
        # the clear and a client fetching its accessibility revision would.
        burn = "import time\nend = time.monotonic() + 0.6\nwhile time.monotonic() < end: pass\ntime.sleep(60)"
        jobs = bench.Jobs()
        started = time.monotonic()
        busy = subprocess.Popen([sys.executable, "-c", burn])
        try:
            number, command = jobs.prepare("receipt")
            shell = subprocess.Popen(command, shell=True, stdout=subprocess.DEVNULL)
            members = [{"pid": busy.pid, "role": "server", "command": "busy"}]
            bench.wait_ready(jobs, number, members)
            self.assertGreaterEqual(time.monotonic() - started, 0.6)
            jobs.start(number)
            jobs.receipt(number, 30)
            shell.wait(timeout=30)
        finally:
            busy.kill()
            busy.wait()
            jobs.remove()

    def test_a_member_that_never_goes_idle_fails_the_measurement(self):
        busy = subprocess.Popen([sys.executable, "-c", "while True: pass"])
        try:
            members = [{"pid": busy.pid, "role": "server", "command": "busy"}]
            with self.assertRaises(TaskError):
                bench.settle(members, limit_s=0.6)
        finally:
            busy.kill()
            busy.wait()

    def test_a_missing_receipt_times_out(self):
        jobs = bench.Jobs()
        try:
            number, _ = jobs.prepare("receipt")
            with self.assertRaises(TaskError):
                jobs.wait(number, ".ready", 0.1)
        finally:
            jobs.remove()

    def test_fixed_input_is_deterministic(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "input.txt"
            first = bench.fixed_input(path, 1000)
            path.unlink()
            second = bench.fixed_input(path, 1000)
            self.assertEqual(first[1], second[1])
            self.assertEqual(path.stat().st_size, 1000)
            self.assertTrue(path.read_bytes().startswith(bench.INPUT_LINE))


class CliFactTests(unittest.TestCase):
    def test_retained_rows_count_scrollback_and_screen_rows(self):
        golden = ROOT / "crates" / "spaceterm-cli" / "golden" / "pane_read.json"
        envelope = json.loads(golden.read_text())
        # One Scrollback row, `oldest` 0 to `total` 1, above a one-row screen.
        self.assertEqual(bench.retained_rows(envelope), 2)
        rows = envelope["data"]["rows"]
        rows["history"] |= {"oldest": 90_000, "total": 100_000}
        rows["screen"]["size"]["rows"] = 40
        self.assertEqual(bench.retained_rows(envelope), 10_040)

    def test_counter_differences_include_nested_counters(self):
        before = {"pty_reads": 3, "pane_worker_wakes": {"output": 1, "input": 2}}
        after = {"pty_reads": 10, "pane_worker_wakes": {"output": 5, "input": 2}}
        self.assertEqual(
            bench.counter_difference(before, after),
            {"pty_reads": 7, "pane_worker_wakes": {"output": 4, "input": 0}},
        )
        self.assertIsNone(bench.counter_difference(None, after))


def load_desktop_task():
    specification = importlib.util.spec_from_file_location(
        "bench_desktop_macos", ROOT / "mise-tasks" / "bench" / "desktop" / "macos.py"
    )
    assert specification is not None and specification.loader is not None
    module = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(module)
    return module


class FakeClock:
    """Time that passes only when a scenario waits."""

    def __init__(self):
        self.now = 0.0

    def monotonic(self):
        return self.now

    def sleep(self, seconds):
        self.now += max(0.0, seconds)


class FakeApp:
    """The installed app as the desktop task sees it through keys, windows and Accessibility.

    Workspaces hold Tabs and Tabs hold Panes. A Pane holds Accessibility demand from a read
    while it is visible until it is hidden, as the app's per-Pane demand does. A `legacy` app
    is v0.6.8: one process without a server, whose Panes end with it and whose quit ends it.
    """

    def __init__(self, legacy=False):
        self.legacy = legacy
        self.next_pane = 0
        self.reset()
        self.client = None
        self.window = "closed"
        self.front = False
        self.server = not legacy
        self.demand = set()
        self.reads = []
        self.lag = False

    def reset(self):
        self.workspaces = [[[self.new_pane()]]]
        self.workspace = 0
        self.tabs = [0]
        self.focus = self.workspaces[0][0][0]

    def running(self):
        """Whether the Panes' shells run: in the server, or in a legacy app's own process."""
        return self.client is not None if self.legacy else self.server

    def new_pane(self):
        self.next_pane += 1
        return f"{self.next_pane:064x}"

    def panes(self):
        return [pane for workspace in self.workspaces for tab in workspace for pane in tab]

    def tab(self):
        return self.workspaces[self.workspace][self.tabs[self.workspace]]

    def visible(self):
        return set(self.tab()) if self.client and self.window == "shown" else set()

    def changed(self):
        self.demand &= self.visible()

    def launch(self):
        if self.client is None:
            self.client = 1000 + len(self.reads) + self.next_pane
            self.demand.clear()
        self.window = "shown"
        self.front = True
        self.changed()

    def terminate(self):
        self.client = None
        self.window = "closed"
        if self.legacy:
            self.reset()
        self.changed()

    def key(self, key, *modifiers):
        command = "command" in modifiers
        if command and key == "d":
            tab = self.tab()
            pane = self.new_pane()
            tab.insert(tab.index(self.focus) + 1, pane)
            self.focus = pane
        elif command and key == "n":
            self.workspaces.append([[self.new_pane()]])
            self.tabs.append(0)
            self.workspace = len(self.workspaces) - 1
            self.focus = self.workspaces[-1][0][0]
        elif command and key == "t":
            workspace = self.workspaces[self.workspace]
            workspace.append([self.new_pane()])
            self.tabs[self.workspace] = len(workspace) - 1
            self.focus = workspace[-1][0]
        elif command and key == "1":
            self.tabs[self.workspace] = 0
            self.focus = self.tab()[-1]
        elif command and key == "w":
            self.close()
        elif command and key == "m":
            self.window = "minimized"
        elif command and key == "q" and self.legacy:
            self.terminate()
        elif command and key == "q":
            self.window = "closed"
        self.changed()

    def close(self, pane=None):
        pane = pane or self.focus
        location = next(
            (index, number)
            for index, workspace in enumerate(self.workspaces)
            for number, tab in enumerate(workspace)
            if pane in tab
        )
        self.workspace, self.tabs[location[0]] = location
        tab = self.tab()
        tab.remove(pane)
        workspace = self.workspaces[self.workspace]
        if not tab:
            workspace.remove(tab)
            self.tabs[self.workspace] = min(self.tabs[self.workspace], len(workspace) - 1)
        if not workspace:
            del self.workspaces[self.workspace]
            del self.tabs[self.workspace]
            self.workspace = len(self.workspaces) - 1
        self.focus = self.tab()[-1]

    def read(self):
        pane = self.reads[-1] if self.lag and self.reads else self.focus
        self.reads.append(pane)
        if pane in self.visible():
            self.demand.add(pane)
        return {"read": True, "length": 2, "element": int(pane, 16)}


class FakeInput:
    def __init__(self, app):
        self.app = app

    def key(self, key, *modifiers):
        self.app.key(key, *modifiers)

    def call(self, operation, **arguments):
        if operation == "read_text":
            assert arguments["pid"] == self.app.client
            return self.app.read()
        if operation == "paced":
            return {"posted": arguments["count"]}
        return {}


class FakeJobs:
    """Workloads that finish at once in the focused Pane, noting whether it held demand."""

    def __init__(self, app):
        self.app = app
        self.jobs = {}
        self.started = []

    def prepare(self, kind, **parameters):
        number = len(self.jobs) + 1
        self.jobs[number] = dict(parameters, kind=kind, pane=self.app.focus)
        return number, f" sh go {number}"

    def wait(self, number, suffix, timeout):
        return None

    def start(self, number):
        job = self.jobs[number]
        self.started.append((job["kind"], job["pane"], job["pane"] in self.app.demand))

    def receipt(self, number, timeout):
        job = self.jobs[number]
        receipt = {"kind": job["kind"], "returncode": 0, "wall_s": 1.0, "rows": 40}
        # v0.6.8 puts no Pane in the Pane's environment; every shell leads its own session.
        receipt |= {"pane": None if self.app.legacy else job["pane"]}
        receipt |= {"session": int(job["pane"], 16)}
        if job["kind"] == "yes":
            receipt["producer_returncode"] = -13
        if job["kind"] == "fill":
            receipt["lines"] = job["lines"]
        return receipt


def fake_desktop(task, app, accessibility, clock, reference=False):
    """The task's adapter for `app`, with the app's processes and windows replaced by `app`.

    The current app's CLI is replaced too. v0.6.8 has none, so the reference adapter keeps its
    own operations and any call to a server or CLI fails.
    """
    adapter = task.ReferenceDesktop if reference else task.CurrentDesktop

    class Gui(adapter):
        def members(self):
            return []

        def pids(self, role):
            if role == "client":
                return [app.client] if app.client else []
            if role == "server":
                return [1] if app.server else []
            return list(range(len(app.panes()))) if app.running() else []

        def client(self):
            return app.client

        def shells(self):
            return len(app.panes()) if app.running() else 0

        def wait_shells(self, count, timeout=30):
            assert self.shells() == count, (self.shells(), count)

        def launch(self):
            app.launch()

        def terminate(self):
            app.terminate()

        def window(self):
            return (
                {"bounds": [0, 0, *task.WINDOW]} if app.client and app.window == "shown" else None
            )

        def frontmost(self):
            return app.client if app.front else 0

        def show_finder(self):
            app.front = False

        def activate(self):
            app.launch()

        def place(self, width, height):
            pass

        def resize(self, width, height):
            pass

    class Cli(Gui):
        def has_server(self):
            return True

        def service(self, action):
            app.server = action == "start"

        def close_panes(self, panes):
            for pane in panes:
                app.close(pane)
            app.changed()

        def retained_rows(self, receipt):
            app.rows_asked = receipt["pane"]
            return 10_040

        def counters(self):
            return {"server": None, "client": None}

    return (Gui if reference else Cli)(
        task.RELEASE if reference else task.PREFLIGHT,
        FakeInput(app),
        FakeJobs(app),
        accessibility,
        clock,
    )


class DesktopScenarioTests(unittest.TestCase):
    """Runs every bench:desktop:macos scenario against FakeApp, the app's GUI boundary."""

    def setUp(self):
        self.task = load_desktop_task()
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)

    def run_scenarios(self, accessibility, scenarios=None, app=None):
        app = app or FakeApp()
        clock = FakeClock()
        desktop = fake_desktop(self.task, app, accessibility, clock, reference=app.legacy)
        args = SimpleNamespace(idle_seconds=60, seconds=60, input="/input")
        results = {"measurements": []}
        output = Path(self.directory.name) / "results.json"
        bench_run = self.task.Bench(desktop, args, output, results, clock=clock)
        observations = []
        record = bench_run.record

        def observed(scenario, phase, metrics, values=None, counters=None):
            observations.append((scenario, phase, app.visible(), set(app.demand)))
            record(scenario, phase, metrics, values, counters)

        bench_run.record = observed
        desktop.restart()
        for scenario in scenarios or self.task.SCENARIOS:
            bench_run.run = 1
            getattr(bench_run, scenario.lower())()
        return app, desktop, results, observations

    def test_accessibility_on_demands_every_visible_pane_and_every_workload_pane(self):
        app, desktop, _, observations = self.run_scenarios(True)
        for scenario, phase, visible, demand in observations:
            with self.subTest(observation=f"{scenario}/{phase}"):
                self.assertLessEqual(visible, demand)
        s5 = [visible for scenario, _, visible, _ in observations if scenario == "S5"]
        self.assertEqual([len(visible) for visible in s5], [3])
        workloads = [started for started in desktop.jobs.started if started[0] != "receipt"]
        self.assertEqual(len([kind for kind, _, _ in workloads if kind == "fill"]), 10)
        self.assertTrue(all(demanded for _, _, demanded in workloads), workloads)
        self.assertEqual({read["pane"] for read in desktop.reads}, set(app.reads))

    def test_accessibility_off_never_queries_the_app(self):
        app, desktop, _, observations = self.run_scenarios(False)
        self.assertEqual(app.reads, [])
        self.assertEqual(desktop.reads, [])
        self.assertTrue(observations)

    def test_a_read_that_reaches_another_pane_fails(self):
        app = FakeApp()
        app.lag = True
        with self.assertRaises(TaskError):
            self.run_scenarios(True, ["S5"], app)

    def test_s8_divides_accessibility_memory_by_the_rows_its_visible_pane_retains(self):
        app, desktop, results, _ = self.run_scenarios(True, ["S8"])
        [filled] = [m for m in results["measurements"] if m["phase"] == "filled"]
        self.assertEqual(filled["values"]["rows"], 10_040)
        # The visible Tab is the last one filled, so its Pane is the one asked about.
        fills = [pane for kind, pane, _ in desktop.jobs.started if kind == "fill"]
        self.assertEqual(app.rows_asked, fills[-1])

    def test_the_reference_runs_every_scenario_without_a_server_or_pane_environment(self):
        compared = {
            tuple(budget["measurement"].split("/"))
            for budget in bench.load_budgets()["desktop"]["budget"]
            if "ratio" in budget
        }
        for accessibility in (False, True):
            with self.subTest(accessibility=accessibility):
                app, desktop, results, observations = self.run_scenarios(
                    accessibility, app=FakeApp(legacy=True)
                )
                recorded = {(m["scenario"], m["phase"]) for m in results["measurements"]}
                self.assertLessEqual(compared, recorded)
                # v0.6.8's quit ends it, which leaves S4 nothing to observe.
                self.assertNotIn("S4", {scenario for scenario, _ in recorded})
                s5 = [visible for scenario, _, visible, _ in observations if scenario == "S5"]
                self.assertEqual([len(visible) for visible in s5], [3])
                # v0.6.8 keeps 10,000 Scrollback rows; the fill receipt reports 40 screen rows.
                [filled] = [m for m in results["measurements"] if m["phase"] == "filled"]
                self.assertEqual(filled["values"]["rows"], 10_040)
                if not accessibility:
                    self.assertEqual(app.reads, [])
                    continue
                for scenario, phase, visible, demand in observations:
                    with self.subTest(observation=f"{scenario}/{phase}"):
                        self.assertLessEqual(visible, demand)
                workloads = [started for started in desktop.jobs.started if started[0] != "receipt"]
                self.assertTrue(all(demanded for _, _, demanded in workloads), workloads)

    def test_a_reference_with_another_duration_reaches_no_ratio_verdict(self):
        budgets = bench.load_budgets()
        scenarios, states = ["S7"], ["off"]
        environment = {
            "host": "mac",
            "hardware": "Chip",
            "cpus": 12,
            "memory_bytes": 1 << 34,
            "system": "macOS 26.6",
            "input_sha256": None,
        }

        def results(seconds, version, adapter):
            args = SimpleNamespace(runs=3, seconds=seconds, idle_seconds=60)
            return {
                "environment": environment | {"app_version": version, "adapter": adapter},
                "settings": self.task.settings(args, scenarios, states),
            }

        def summary(cpu):
            phases = ("scroll", "partial", "vim")
            return bench.summarize([measurement("S7", phase, "off", cpu) for phase in phases])

        path = Path(self.directory.name) / "reference.json"
        path.write_text(json.dumps(results(20, "0.6.8", "reference") | {"summary": summary(10.0)}))
        # 8 CPU s in 10 s against 10 in 20 s is 60% more CPU per second; its ratio, 0.8, passes.
        with self.assertRaisesRegex(TaskError, "setting seconds differs"):
            self.task.load_reference(
                path, results(10, "0.7.0", "current"), budgets, scenarios, states
            )
        reference = self.task.load_reference(
            path, results(20, "0.7.0", "current"), budgets, scenarios, states
        )
        verdicts = bench.evaluate(
            summary(8.0), budgets, reference=reference, scenarios=scenarios, states=states
        )
        ratios = {v["measurement"]: v for v in verdicts if v["bound"] == "ratio"}
        self.assertEqual(ratios["S7/scroll/off"]["value"], 0.8)
        self.assertTrue(ratios["S7/scroll/off"]["pass"])

    def test_checked_in_budgets_name_measurements_the_desktop_task_records(self):
        _, _, off, _ = self.run_scenarios(False)
        _, _, on, _ = self.run_scenarios(True)
        recorded = off["measurements"] + on["measurements"]
        identities = {(m["scenario"], m["phase"]) for m in recorded}
        budgets = bench.load_budgets()
        for budget in budgets["desktop"]["budget"]:
            with self.subTest(budget=budget["name"]):
                for name in (budget["measurement"], budget.get("before")):
                    if name:
                        self.assertIn(tuple(name.split("/")), identities)
                self.assertIn(budget["role"], ("client", "server", "product"))
                self.assertIn(budget["metric"], bench.METRICS)
        # Every requested bound of every budget yields a verdict with its values present.
        rows = [
            {"role": role, "cpu_s": 1.0, "rss_bytes": 1, "footprint_bytes": 1}
            for role in ("client", "server")
        ]
        rows = [row | {"peak_footprint_bytes": 1, "wakeups_per_s": 1.0} for row in rows]
        for m in recorded:
            m["metrics"] = {"roles": bench.totals(rows, 10.0)}
        summary = bench.summarize(recorded)
        verdicts = bench.evaluate(
            summary, budgets, reference=summary, hardware=budgets["desktop"]["hardware"]
        )
        self.assertEqual([v for v in verdicts if v["missing"]], [])
        expected = set()
        for budget in budgets["desktop"]["budget"]:
            for bound, states in (
                ("max", ("off", "on")),
                ("ratio", ("off", "on")),
                ("accessibility_ratio", ("on",)),
                ("per_10k_rows", ("on",)),
                ("after_ratio", ("off", "on")),
            ):
                if bound in budget:
                    expected |= {(budget["name"], bound, state) for state in states}
        self.assertEqual(
            {(v["budget"], v["bound"], v["measurement"].rsplit("/", 1)[1]) for v in verdicts},
            expected,
        )
        self.assertEqual(budgets["server"]["regression"], 0.1)


class BaselineFileTests(unittest.TestCase):
    def test_checked_in_server_baselines_carry_every_metric(self):
        for path in sorted((bench.BASELINES / "server").glob("*.json")):
            with self.subTest(host=path.stem):
                data = json.loads(path.read_text())
                for values in data["summary"].values():
                    self.assertEqual(set(values), set(bench.SERVER_METRICS))


if __name__ == "__main__":
    unittest.main()
