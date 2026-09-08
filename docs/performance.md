# Terminal resource measurements

Use optimized source builds (`mise run build:release`). Record the commit, macOS version,
hardware, display scale and refresh rate, terminal grid dimensions, font, and
application versions. Keep these conditions fixed across SpaceTerm, Ghostty, and
Muxy. Run one workload at a time and avoid builds or other heavy background work.

## Reproducible workloads

Run in a dedicated Pane, since the workload clears and fills its terminal screen:

```sh
python3 scripts/terminal-resource-workload.py --mode scroll --duration 60 --rate 60
```

`idle` fills forty lines and waits. `partial` updates a fixed row. `scroll` writes
eight lines per update, or 480 lines per second at the default rate. Use at least
120 columns so these payloads do not wrap. Geometry and wrapping must match in
every application. A delayed producer does not flood output to catch up, so
backpressure can reduce delivered rate; a lower resource number alone does not
prove equivalent throughput. The maximum requested duration is one hour.

On completion the producer writes one JSON summary to stderr with mode, completed
updates, emitted lines and bytes, and elapsed seconds. Counts include any initial
fill but exclude the summary itself. Compare these totals and elapsed
times to check delivered producer throughput. Keep resource capture windows before
completion so printing the summary does not affect the samples.

For a dedicated launch that accepts a shell path, the executable script also
accepts the shell argument `-l`. Set `SHELL` to its absolute path and set
`SPACETERM_BENCH_MODE`, `SPACETERM_BENCH_DURATION`, and `SPACETERM_BENCH_RATE`
in that launch's environment. CLI arguments override these settings. Do not
change the system login shell. The script exits after its duration, so applications
may close that Pane. A short redirected syntax/output check is available with
`--smoke --duration 0.1`; ordinary runs require a terminal.

## Process samples

Choose the application PID, then sample from a separate terminal:

```sh
python3 scripts/measure-terminal-resources.py 12345 --duration 20 --interval 1
```

This macOS-only sampler needs Python 3 and no third-party packages or administrator
privileges. It reads `proc_pid_rusage` v0 and emits one JSON object per interval.
It does not read terminal contents, process arguments, or environment values.
CPU counters are converted using the machine's `mach_timebase_info`; do not assume
one clock tick is one nanosecond. `cpu_percent` uses one CPU core as 100%, so a
multithreaded process can exceed 100%. Report physical `footprint_mib` separately
from `resident_mib`; neither is an exclusive measurement of GPU memory.
Interrupt wakeups and package idle wakeups are distinct counters, not render
counts. The sampler does not include child processes. Sample the workload producer
and any relevant application helpers separately when comparing total costs.

Allow launch and workload warm-up to settle before sampling. Repeat equal-length
captures and report variation alongside averages. Use interval-weighted averages
for CPU and wakeup rates if intervals differ. Do not compare historical numbers
collected with different tools, timebase conversions, or workloads directly.

## Visibility, lifetime, and power

Capture focused idle, focused partial updates, focused scrolling, unfocused but
visible, hidden or minimized, and restored states. Record exactly which state
was used. Default native accessibility demand can change snapshot work and CPU,
so keep accessibility settings and active assistive clients consistent. A
headless rendering benchmark does not include all native accessibility costs.

For lifecycle checks, repeat Tab switching and Pane creation/closure, then allow
frame retirement and cleanup. Track settled footprint and retained application
resources. Allocators and GPU frameworks may keep reusable storage after an owner
drops, so a single RSS reading does not prove a leak. Verify restoration presents
the latest complete screen and that hidden terminal processing still progresses.

For images, also exercise repeated image replacement, deletion, hide/restore, and
Pane closure. Application image reservation counters describe owned logical
resources; measure GPU residency or execution separately with native profiling
tools when available.

The `image` workload displays one fixed synthetic 2048 x 2048 RGBA PNG through
Kitty graphics, using synchronized output and complete buffered writes:

```sh
python3 scripts/terminal-resource-workload.py --mode image --duration 120
```

Image mode uses a one-second startup delay to let launch geometry settle, emits
one image, then waits for the remainder of the duration. Duration includes startup
and image generation; ordinary image runs require more than one second. Smoke
runs skip startup delay. The rate option does not affect image or idle modes.
The image generator releases its temporary buffers before waiting.

Start this fixture in two dedicated Tabs, switch between them to hide each Pane,
then return to verify image restoration without new output. Compare settled
memory with matching visible geometry and Tab counts. Hidden Panes should release
derived presentation resources; their original native image data and complete
Screen snapshots intentionally remain available for restoration. The fixed image
contains 16 MiB of decoded pixels; process footprint can include additional native,
snapshot, upload, and GPU representations, and should be measured rather than
estimated by multiplying that size.

Keep this fixture at two graphics-enabled Sessions. The current application-wide
decoded-image budget and per-Session reservation admit at most two such Sessions;
a third image being rejected is quota enforcement, not a rendering benchmark.

Optional native power captures require the user's available privileges and should
use matching workload and sampling windows. Whole-system CPU/GPU power includes
WindowServer, the producer, profiling overhead, and unrelated applications.
Report it separately from application CPU and wakeups. Activity Monitor Energy
Impact is a relative score, not watts. Raw billed-energy counters are not watts
either. Record unavailable power or GPU metrics explicitly rather than inferring
them from CPU or memory. These scripts never invoke sudo or control application UI.
