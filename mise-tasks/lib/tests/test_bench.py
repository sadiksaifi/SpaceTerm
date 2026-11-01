"""Check the bench:desktop:macos and bench:server measurement, budget and receipt code."""

import json
import subprocess
import tempfile
import unittest
from pathlib import Path

from spaceterm_tasks import TaskError, bench

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
        summary = bench.summarize(
            [
                measurement("S6", "yes", "off", 3.0),
                measurement("S6", "yes", "off", 1.0),
                measurement("S6", "yes", "off", 2.0, values={"rows": 10}),
                measurement("S6", "yes", "on", 9.0),
            ]
        )
        self.assertEqual(summary["S6/yes/off"]["runs"], 3)
        self.assertEqual(summary["S6/yes/off"]["roles"]["client"]["cpu_s"], 2.0)
        self.assertEqual(summary["S6/yes/off"]["values"]["rows"], 10)
        self.assertEqual(summary["S6/yes/on"]["roles"]["product"]["cpu_s"], 9.0)


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
        [verdict] = bench.evaluate(summary, budgets, hardware="Chip")
        self.assertFalse(verdict["pass"])
        self.assertEqual(bench.evaluate(summary, budgets, hardware="Other"), [])

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

    def test_checked_in_budgets_name_measurements_the_desktop_task_records(self):
        budgets = bench.load_budgets()
        for budget in budgets["desktop"]["budget"]:
            with self.subTest(budget=budget["name"]):
                scenario = budget["measurement"].split("/")[0]
                self.assertIn(scenario, [f"S{number}" for number in range(1, 11)])
                self.assertIn(budget["role"], ("client", "server", "product"))
                self.assertIn(budget["metric"], bench.METRICS)
        self.assertEqual(budgets["server"]["regression"], 0.1)

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
            shell = subprocess.Popen(command, shell=True, stdout=subprocess.PIPE, text=True)
            jobs.wait(number, ".ready", 30)
            self.assertFalse(jobs.path(number, ".done.json").exists())
            jobs.start(number)
            receipt = jobs.receipt(number, 30)
            output, _ = shell.communicate(timeout=30)
            self.assertEqual(receipt["kind"], "receipt")
            self.assertEqual(receipt["returncode"], 0)
            self.assertIn(f"{bench.MARKER} {number}", output)
            self.assertNotIn(bench.MARKER, command)
        finally:
            jobs.remove()

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


class BaselineFileTests(unittest.TestCase):
    def test_checked_in_server_baselines_carry_every_metric(self):
        for path in sorted((bench.BASELINES / "server").glob("*.json")):
            with self.subTest(host=path.stem):
                data = json.loads(path.read_text())
                for values in data["summary"].values():
                    self.assertEqual(
                        set(values),
                        {"cpu_s", "peak_footprint_bytes", "after_footprint_bytes", "bytes_sent"},
                    )


if __name__ == "__main__":
    unittest.main()
