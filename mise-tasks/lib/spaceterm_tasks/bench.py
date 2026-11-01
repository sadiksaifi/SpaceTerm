"""Shared code for bench:desktop:macos and bench:server.

It reads per-process CPU time, memory and wakeups, coordinates workloads that run inside a Pane
through receipt files, summarizes repeated runs, and checks results against the budgets and
per-host baselines in mise-tasks/bench.
"""

import ctypes
import ctypes.util
import hashlib
import json
import os
import platform
import shlex
import socket
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError

BENCH = ROOT / "mise-tasks" / "bench"
BUDGETS = BENCH / "budgets.json"
BASELINES = BENCH / "baselines"
WORKLOAD = BENCH / "workload.py"
DRIVER = Path(__file__).with_name("bench_driver.py")
MARKER = "SPACETERM-BENCH-DONE"
MIB = 1024 * 1024
SHELLS = frozenset(("zsh", "bash", "sh", "fish", "dash", "-zsh", "-bash", "-sh", "-fish"))
# The fixed `cat` input: this line repeated to 200,000,000 bytes.
INPUT_LINE = b"0123456789 abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ\n"
INPUT_BYTES = 200_000_000
INPUT = Path.home() / ".cache" / "spaceterm-bench" / "fixed-200MB.txt"


# Process usage


class _RusageInfoV4(ctypes.Structure):
    """`struct rusage_info_v4` from <sys/resource.h>."""

    _fields_ = [("uuid", ctypes.c_ubyte * 16)] + [
        (name, ctypes.c_uint64)
        for name in (
            "user_time system_time pkg_idle_wkups interrupt_wkups pageins wired_size "
            "resident_size phys_footprint proc_start_abstime proc_exit_abstime child_user_time "
            "child_system_time child_pkg_idle_wkups child_interrupt_wkups child_pageins "
            "child_elapsed_abstime diskio_bytesread diskio_byteswritten cpu_time_qos_default "
            "cpu_time_qos_maintenance cpu_time_qos_background cpu_time_qos_utility "
            "cpu_time_qos_legacy cpu_time_qos_user_initiated cpu_time_qos_user_interactive "
            "billed_system_time serviced_system_time logical_writes lifetime_max_phys_footprint "
            "instructions cycles billed_energy serviced_energy interval_max_phys_footprint "
            "runnable_time"
        ).split()
    ]


RUSAGE_INFO_V4 = 4
_system = None
_seconds_per_tick = None


def _darwin():
    """libSystem and the Mach tick length, which is 125/3 ns on Apple silicon and 1 ns on Intel."""
    global _system, _seconds_per_tick
    if _system is None:
        system = ctypes.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True)
        system.proc_pid_rusage.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]
        system.proc_reset_footprint_interval.argtypes = [ctypes.c_int]
        timebase = (ctypes.c_uint32 * 2)()
        if system.mach_timebase_info(timebase) != 0 or not timebase[1]:
            raise TaskError("mach_timebase_info failed")
        _seconds_per_tick = timebase[0] / timebase[1] / 1e9
        _system = system
    return _system


def usage(pid):
    """CPU seconds, RSS, physical footprint and wakeup counts of one process, or None if it ended.

    On Linux, footprint is anonymous resident memory plus swap, the closest procfs value to
    macOS physical footprint, and wakeups are voluntary context switches.
    """
    if sys.platform == "darwin":
        system = _darwin()
        info = _RusageInfoV4()
        if system.proc_pid_rusage(pid, RUSAGE_INFO_V4, ctypes.byref(info)) != 0:
            return None
        return {
            "cpu_s": (info.user_time + info.system_time) * _seconds_per_tick,
            "rss_bytes": info.resident_size,
            "footprint_bytes": info.phys_footprint,
            "interval_max_footprint_bytes": info.interval_max_phys_footprint,
            "idle_wakeups": info.pkg_idle_wkups,
            "interrupt_wakeups": info.interrupt_wkups,
        }
    return linux_usage(Path("/proc") / str(pid))


def linux_usage(directory):
    try:
        stat = (directory / "stat").read_text()
        status = (directory / "status").read_text()
    except OSError:
        return None
    # The command name may contain spaces and parentheses; fields follow the last parenthesis.
    fields = stat.rpartition(")")[2].split()
    ticks = os.sysconf("SC_CLK_TCK")
    values = parse_status(status)
    try:
        written = parse_io((directory / "io").read_text()).get("wchar", 0)
    except OSError:
        written = 0
    return {
        "cpu_s": (int(fields[11]) + int(fields[12])) / ticks,
        "rss_bytes": values.get("VmRSS", 0),
        "footprint_bytes": values.get("RssAnon", 0) + values.get("VmSwap", 0),
        "idle_wakeups": values.get("voluntary_ctxt_switches", 0),
        "interrupt_wakeups": values.get("nonvoluntary_ctxt_switches", 0),
        "written_bytes": written,
    }


def parse_status(text):
    """Integer fields of /proc/PID/status, with kB values in bytes."""
    values = {}
    for line in text.splitlines():
        key, _, value = line.partition(":")
        parts = value.split()
        if len(parts) == 2 and parts[1] == "kB" and parts[0].isdigit():
            values[key] = int(parts[0]) * 1024
        elif len(parts) == 1 and parts[0].isdigit():
            values[key] = int(parts[0])
    return values


def parse_io(text):
    values = {}
    for line in text.splitlines():
        key, _, value = line.partition(":")
        if value.strip().isdigit():
            values[key] = int(value)
    return values


def reset_peak(pid):
    """Start a new peak footprint interval for `pid` where the system keeps one."""
    if sys.platform == "darwin":
        _darwin().proc_reset_footprint_interval(pid)


def processes():
    """Every process as `{"pid", "ppid", "command"}`, where command is the executable path."""
    if sys.platform == "darwin":
        listing = subprocess.run(
            ["ps", "-axo", "pid=,ppid=,comm="], capture_output=True, text=True, check=True
        ).stdout
        return parse_ps(listing)
    rows = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            stat = (entry / "stat").read_text()
            command = os.readlink(entry / "exe")
        except OSError:
            continue
        rows.append(
            {
                "pid": int(entry.name),
                "ppid": int(stat.rpartition(")")[2].split()[1]),
                "command": command,
            }
        )
    return rows


def parse_ps(listing):
    rows = []
    for line in listing.splitlines():
        parts = line.strip().split(None, 2)
        if len(parts) == 3 and parts[0].isdigit() and parts[1].isdigit():
            rows.append({"pid": int(parts[0]), "ppid": int(parts[1]), "command": parts[2]})
    return rows


def classify(rows, roots):
    """Roles for the processes in `roots` (pid to role) and their descendants.

    A descendant is a `shell` when its executable is a shell, and otherwise a `program`.
    """
    children = {}
    for row in rows:
        children.setdefault(row["ppid"], []).append(row)
    by_pid = {row["pid"]: row for row in rows}
    roles = {}
    pending = [pid for pid in roots if pid in by_pid]
    for pid in pending:
        roles[pid] = roots[pid]
    while pending:
        pid = pending.pop()
        for child in children.get(pid, []):
            if child["pid"] in roles:
                continue
            roles[child["pid"]] = "shell" if Path(child["command"]).name in SHELLS else "program"
            pending.append(child["pid"])
    return [dict(by_pid[pid], role=role) for pid, role in roles.items()]


def snapshot(members):
    """Usage of each `{"pid", "role", "command"}` member that is still running."""
    result = []
    for member in members:
        current = usage(member["pid"])
        if current is not None:
            result.append(dict(member, usage=current))
    return {"monotonic": time.monotonic(), "processes": result}


PRODUCT_ROLES = ("client", "server")


def delta(before, after, *, new_from_zero=False, peaks=None):
    """Per-process and per-role differences between two snapshots.

    CPU and wakeups are differences, memory is the value at `after`. Processes that started
    between the snapshots count from zero with `new_from_zero` and are skipped otherwise.
    `peaks` maps pids to the highest footprint a Sampler saw.
    """
    elapsed = after["monotonic"] - before["monotonic"]
    earlier = {process["pid"]: process["usage"] for process in before["processes"]}
    zero = {"cpu_s": 0, "idle_wakeups": 0, "interrupt_wakeups": 0, "written_bytes": 0}
    rows = []
    for process in after["processes"]:
        now = process["usage"]
        old = earlier.get(process["pid"])
        if old is None:
            if not new_from_zero:
                continue
            old = zero
        peak = max(now["footprint_bytes"], (peaks or {}).get(process["pid"], 0))
        row = {
            "pid": process["pid"],
            "role": process["role"],
            "cpu_s": now["cpu_s"] - old["cpu_s"],
            "rss_bytes": now["rss_bytes"],
            "footprint_bytes": now["footprint_bytes"],
            "peak_footprint_bytes": peak,
            "wakeups_per_s": (
                now["idle_wakeups"]
                + now["interrupt_wakeups"]
                - old["idle_wakeups"]
                - old["interrupt_wakeups"]
            )
            / elapsed
            if elapsed > 0
            else 0.0,
        }
        if "written_bytes" in now:
            row["written_bytes"] = now["written_bytes"] - old.get("written_bytes", 0)
        rows.append(row)
    return {"elapsed_s": elapsed, "processes": rows, "roles": totals(rows, elapsed)}


METRICS = ("cpu_s", "rss_bytes", "footprint_bytes", "peak_footprint_bytes", "wakeups_per_s")


def totals(rows, elapsed):
    """Sums per role, plus `product`: the client and server, excluding shells and programs."""
    result = {}
    for role in ("client", "server", "shell", "program", "product"):
        members = [
            row
            for row in rows
            if row["role"] == role or (role == "product" and row["role"] in PRODUCT_ROLES)
        ]
        if not members:
            continue
        total = {metric: sum(row[metric] for row in members) for metric in METRICS}
        total["cpu_percent"] = 100 * total["cpu_s"] / elapsed if elapsed > 0 else 0.0
        total["processes"] = len(members)
        result[role] = total
    return result


class Sampler:
    """Polls the footprint of a set of processes on a thread and keeps each one's maximum."""

    def __init__(self, pids, interval=0.05):
        self.pids = list(pids)
        self.interval = interval
        self.peaks = {}
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._run, daemon=True)

    def __enter__(self):
        for pid in self.pids:
            reset_peak(pid)
        self._thread.start()
        return self

    def __exit__(self, *_):
        self._stop.set()
        self._thread.join()
        self._sample()
        return False

    def _sample(self):
        for pid in self.pids:
            current = usage(pid)
            if current is not None:
                peak = max(
                    current["footprint_bytes"], current.get("interval_max_footprint_bytes", 0)
                )
                self.peaks[pid] = max(self.peaks.get(pid, 0), peak)

    def _run(self):
        while not self._stop.wait(self.interval):
            self._sample()


def settle(members, *, quiet_cpu_s=0.015, window_s=0.25, calm_windows=2, limit_s=30):
    """Wait until the members' total CPU stays under `quiet_cpu_s` per window, so the
    presentation work that follows a finished producer is measured too. Returns seconds waited."""
    started = time.monotonic()
    calm = 0
    previous = snapshot(members)
    while calm < calm_windows and time.monotonic() - started < limit_s:
        time.sleep(window_s)
        current = snapshot(members)
        spent = delta(previous, current)["roles"].get("product", {}).get("cpu_s", 0)
        calm = calm + 1 if spent < quiet_cpu_s else 0
        previous = current
    return time.monotonic() - started


# Workloads inside a Pane


class Jobs:
    """Numbered workloads in a private directory. A Pane runs `sh DIRECTORY/go NUMBER`."""

    def __init__(self, python=sys.executable):
        self.directory = Path(tempfile.mkdtemp(prefix="stb-", dir="/tmp"))
        self.number = 0
        go = self.directory / "go"
        go.write_text(
            f'exec {shlex.quote(python)} {shlex.quote(str(DRIVER))} "$1" '
            f"{shlex.quote(str(self.directory))}\n"
        )

    def prepare(self, kind, **parameters):
        """Write job `kind` and return its number and the command that runs it.

        The command starts with a space so shells that ignore such lines keep it out of history.
        """
        self.number += 1
        job = dict(parameters, kind=kind, workload=str(WORKLOAD))
        (self.directory / f"{self.number}.json").write_text(json.dumps(job))
        return self.number, f" sh {self.directory}/go {self.number}"

    def path(self, number, suffix):
        return self.directory / f"{number}{suffix}"

    def wait(self, number, suffix, timeout):
        path = self.path(number, suffix)
        deadline = time.monotonic() + timeout
        while not path.exists():
            if time.monotonic() > deadline:
                raise TaskError(f"workload {number} did not write its {suffix} receipt in time")
            time.sleep(0.02)
        return path

    def start(self, number):
        self.path(number, ".go").touch()

    def receipt(self, number, timeout):
        return json.loads(self.wait(number, ".done.json", timeout).read_text())

    def remove(self):
        for entry in self.directory.iterdir():
            entry.unlink()
        self.directory.rmdir()


def fixed_input(path=INPUT, size=INPUT_BYTES):
    """Create the fixed `cat` input, 200 MB by default, once and return its path and SHA-256."""
    if not path.exists() or path.stat().st_size != size:
        path.parent.mkdir(parents=True, exist_ok=True)
        partial = path.with_suffix(".partial")
        chunk = INPUT_LINE * 16384
        with partial.open("wb") as output:
            written = 0
            while written < size:
                part = chunk[: size - written]
                output.write(part)
                written += len(part)
        partial.replace(path)
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1 << 20), b""):
            digest.update(block)
    return path, digest.hexdigest()


# Summaries, budgets and baselines


def median(values):
    return statistics.median(values) if values else None


def summarize(measurements, key=("scenario", "phase", "accessibility")):
    """Medians of every role metric across runs that share `key`.

    Returns `{key: {role: {metric: median}}}` with keys joined by `/`.
    """
    groups = {}
    for measurement in measurements:
        name = "/".join(str(measurement[part]) for part in key)
        groups.setdefault(name, []).append(measurement)
    summary = {}
    for name, runs in groups.items():
        roles = {}
        for role in sorted({role for run in runs for role in run["metrics"]["roles"]}):
            rows = [
                run["metrics"]["roles"][role] for run in runs if role in run["metrics"]["roles"]
            ]
            roles[role] = {
                metric: median([row[metric] for row in rows if metric in row])
                for metric in sorted({metric for row in rows for metric in row})
            }
        extras = {}
        for metric in sorted({metric for run in runs for metric in run.get("values", {})}):
            extras[metric] = median(
                [run["values"][metric] for run in runs if metric in run.get("values", {})]
            )
        summary[name] = {"runs": len(runs), "roles": roles, "values": extras}
    return summary


def load_budgets(path=BUDGETS):
    """The budgets from #773. `evaluate` describes desktop budgets, `regressions` server ones."""
    return json.loads(path.read_text())


def lookup(summary, name, role, metric):
    """A median from `summarize`: a role metric, or with role `value` a scenario value."""
    entry = summary.get(name)
    if entry is None:
        return None
    if role == "value":
        return entry["values"].get(metric)
    return entry["roles"].get(role, {}).get(metric)


def evaluate(summary, budgets, *, reference=None, hardware=None):
    """Check a desktop summary against `budgets["desktop"]`.

    Each budget names a measurement (`scenario/phase`), a role and a metric and holds at least
    one bound: `max`, an absolute limit for the budget hardware; `ratio`, a limit against the
    same measurement in `reference` under the same Accessibility state; `accessibility_ratio`,
    a limit for Accessibility on against off; `per_10k_rows`, a limit for what Accessibility on
    adds per 10,000 rows the measurement's `rows` value counts; or `after_ratio`, a limit for
    the measurement against `before`. Returns one verdict per bound that has the values it needs.

    Ratio budgets compare with v0.6.8, so `reference` is the summary of a v0.6.8 run. Roles are
    `client`, `server` and `product`, their sum without shells and the programs they run.
    Memory is physical footprint in bytes; CPU is seconds of one core.
    """
    desktop = budgets["desktop"]
    absolute = hardware == desktop["hardware"]
    verdicts = []
    for budget in desktop["budget"]:
        measurement, role, metric = budget["measurement"], budget["role"], budget["metric"]
        for accessibility in ("off", "on"):
            name = f"{measurement}/{accessibility}"
            value = lookup(summary, name, role, metric)

            def verdict(kind, limit, actual, against=None):
                verdicts.append(
                    {
                        "budget": budget["name"],
                        "measurement": name,
                        "role": role,
                        "metric": metric,
                        "bound": kind,
                        "limit": limit,
                        "value": actual,
                        "against": against,
                        "pass": actual is not None and actual <= limit,
                    }
                )

            if value is None:
                continue
            if "max" in budget and absolute:
                verdict("max", budget["max"], value)
            if "ratio" in budget and reference is not None:
                base = lookup(reference, name, role, metric)
                if base:
                    verdict("ratio", budget["ratio"], value / base, base)
            if "accessibility_ratio" in budget and accessibility == "on":
                off = lookup(summary, f"{measurement}/off", role, metric)
                if off:
                    verdict("accessibility_ratio", budget["accessibility_ratio"], value / off, off)
            if "per_10k_rows" in budget and accessibility == "on":
                off = lookup(summary, f"{measurement}/off", role, metric)
                rows = lookup(summary, name, "value", "rows")
                if off is not None and rows:
                    added = (value - off) / (rows / 10_000)
                    verdict("per_10k_rows", budget["per_10k_rows"], added, off)
            if "after_ratio" in budget:
                before = lookup(summary, f"{budget['before']}/{accessibility}", role, metric)
                if before:
                    verdict("after_ratio", budget["after_ratio"], value / before, before)
    return verdicts


def regressions(baseline, current, server_budgets):
    """Server medians in `current` that exceed `baseline` by more than the allowed fraction.

    A difference under the metric's noise floor never counts. Returns one entry per comparison.
    """
    allowed = server_budgets["regression"]
    floors = server_budgets["floor"]
    results = []
    for name, metrics in sorted(current.items()):
        old = baseline.get(name)
        if old is None:
            continue
        for metric, value in sorted(metrics.items()):
            before = old.get(metric)
            if before is None or value is None:
                continue
            limit = before * (1 + allowed)
            regressed = value > limit and value - before > floors.get(metric, 0)
            results.append(
                {
                    "measurement": name,
                    "metric": metric,
                    "baseline": before,
                    "value": value,
                    "regressed": regressed,
                }
            )
    return results


def host_name():
    """This machine's short host name, which names its baseline file."""
    return socket.gethostname().split(".")[0].lower()


def environment():
    """Facts that make results comparable: host, hardware, OS and checkout."""

    def output(command):
        try:
            return subprocess.run(
                command, capture_output=True, text=True, check=False
            ).stdout.strip()
        except OSError:
            return ""

    if sys.platform == "darwin":
        hardware = output(["sysctl", "-n", "machdep.cpu.brand_string"])
        memory = int(output(["sysctl", "-n", "hw.memsize"]) or 0)
        system = "macOS " + output(["sw_vers", "-productVersion"])
    else:
        hardware = next(
            (
                line.partition(":")[2].strip()
                for line in Path("/proc/cpuinfo").read_text().splitlines()
                if line.startswith("model name")
            ),
            platform.machine(),
        )
        memory = parse_status(Path("/proc/meminfo").read_text()).get("MemTotal", 0)
        system = platform.system() + " " + platform.release()
    return {
        "host": host_name(),
        "hardware": hardware,
        "cpus": os.cpu_count(),
        "memory_bytes": memory,
        "system": system,
        "commit": output(["git", "-C", str(ROOT), "rev-parse", "HEAD"]),
        "checkout_clean": output(["git", "-C", str(ROOT), "status", "--porcelain"]) == "",
        "time": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
    }


def write_json(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    partial = path.with_suffix(".partial")
    partial.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n")
    partial.replace(path)
