"""Run one benchmark workload inside a SpaceTerm Pane and write a receipt when it finishes.

bench:desktop:macos and bench:server type `sh DIRECTORY/go NUMBER` into a Pane, which runs this script
with NUMBER and DIRECTORY. It reads `NUMBER.json`, clears the screen and Scrollback, writes `NUMBER.ready` and waits for the benchmark to create
`NUMBER.go`. The benchmark creates it after SpaceTerm has presented the clear and gone idle and
after its first measurement. Then it runs the workload on the Pane's terminal,
writes the receipt `NUMBER.done.json` and prints a completion marker. The typed command never
contains the marker, so the marker on the screen also means the workload finished.

Standard library only: the Pane's shell may run on another host.
"""

import json
import os
import resource
import subprocess
import sys
import time
from pathlib import Path

MARKER = "SPACETERM-BENCH-DONE"
# A full-screen program redraws this file, alternating between its first and last rows.
VIM_SCRIPT = """set nomore noswapfile nobackup noundofile
set t_RV= t_u7=
set number cursorline
let duration=str2float($SPACETERM_BENCH_SECONDS)
let started=reltime()
let frames=0
while reltimefloat(reltime(started)) < duration
  if frames % 2 == 0
    normal! gg
  else
    normal! G
  endif
  redraw!
  let frames+=1
  sleep 16m
endwhile
qa!
"""
CLEAR = b"\x1b[H\x1b[2J\x1b[3J"
FILL_LINE = b"0123456789 abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ\r\n"


def write_all(data):
    view = memoryview(data)
    while view:
        view = view[os.write(1, view) :]


def pane():
    """The Pane this runs in: the Pane the current app names in `SPACETERM_PANE`, which
    bench:desktop:macos asks the CLI about, and the session its shell leads, which also
    identifies a v0.6.8 Pane."""
    return {"pane": os.environ.get("SPACETERM_PANE"), "session": os.getsid(0)}


def run(job, directory):
    kind = job["kind"]
    result = {"kind": kind}
    if kind == "cat":
        result["returncode"] = subprocess.run(["cat", job["file"]]).returncode
    elif kind == "yes":
        producer = subprocess.Popen(["yes"], stdout=subprocess.PIPE)
        consumer = subprocess.run(
            ["head", "-n", str(job.get("lines", 20_000_000))], stdin=producer.stdout
        )
        if producer.stdout is not None:
            producer.stdout.close()
        # yes ends by SIGPIPE when head closes the pipe; the benchmark judges each code.
        result["producer_returncode"] = producer.wait()
        result["returncode"] = consumer.returncode
    elif kind in ("scroll", "partial"):
        with open(directory / f"{job['number']}.workload.json", "w") as log:
            command = [sys.executable, job["workload"], "--mode", kind, "--rate", "60"]
            command += ["--duration", str(job["seconds"])]
            result["returncode"] = subprocess.run(command, stderr=log).returncode
    elif kind == "vim":
        script = directory / "redraw.vim"
        text = directory / "tui.txt"
        if not script.exists():
            script.write_text(VIM_SCRIPT)
            rows = (f"{row:06d} benchmark full-screen TUI redraw content\n" for row in range(1000))
            text.write_text("".join(rows))
        environment = dict(os.environ, SPACETERM_BENCH_SECONDS=str(job["seconds"]))
        command = ["vim", "-Nu", "NONE", "-i", "NONE", "-n", "-S", str(script), str(text)]
        result["returncode"] = subprocess.run(command, env=environment).returncode
    elif kind == "fill":
        for _ in range(job.get("lines", 100_000) // 1000):
            write_all(FILL_LINE * 1000)
        result["lines"] = job.get("lines", 100_000)
        result.update(pane())
    elif kind == "receipt":
        result["returncode"] = 0
        result.update(pane())
    else:
        raise SystemExit(f"unknown workload {kind}")
    return result


def main():
    number, directory = sys.argv[1], Path(sys.argv[2])
    job = json.loads((directory / f"{number}.json").read_text())
    job["number"] = number
    # Each workload starts from an empty screen and Scrollback, so earlier workloads' history
    # never changes what this one costs.
    write_all(CLEAR)
    (directory / f"{number}.ready").write_text(str(os.getpid()))
    go = directory / f"{number}.go"
    deadline = time.monotonic() + job.get("start_timeout", 120)
    while not go.exists():
        if time.monotonic() > deadline:
            raise SystemExit("the benchmark did not start the workload")
        time.sleep(0.01)
    started = time.monotonic()
    result = run(job, directory)
    usage = resource.getrusage(resource.RUSAGE_CHILDREN)
    result.update(wall_s=time.monotonic() - started, child_cpu_s=usage.ru_utime + usage.ru_stime)
    try:
        size = os.get_terminal_size()
        result.update(columns=size.columns, rows=size.lines)
    except OSError:
        pass
    partial = directory / f"{number}.done.partial"
    partial.write_text(json.dumps(result))
    partial.replace(directory / f"{number}.done.json")
    print(f"\r\n{MARKER} {number}", flush=True)


if __name__ == "__main__":
    main()
