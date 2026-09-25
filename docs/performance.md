# Terminal resource measurements

Use optimized source builds (`mise run build:release`). Record the commit, macOS version,
hardware, display scale and refresh rate, terminal grid dimensions, font, and
application versions. Keep these conditions fixed across SpaceTerm, Ghostty, and
Muxy. Run one workload at a time and avoid builds or other heavy background work.

## Reproducible workloads

Run in a dedicated Pane, since the workload clears and fills its terminal screen:

```sh
mise run bench:workload scroll 60 60
```

Use at least 120 columns so the text workloads do not wrap. Geometry and wrapping
must match in every application. Backpressure can reduce delivered rate; a lower
resource number alone does not prove equivalent throughput.

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
mise run bench:macos:resources 12345 20 1
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
mise run bench:workload image 120
```

Start this fixture in two dedicated Tabs, switch between them to hide each Pane,
then return to verify image restoration without new output. Compare settled
memory with matching visible geometry and Tab counts. The fixed image contains
16 MiB of decoded pixels; process footprint can include additional native,
snapshot, upload, and GPU representations, and should be measured rather than
estimated by multiplying that size.

Use the same number of graphics-enabled Sessions when comparing runs. Keep the
fixture below the graphics admission limits when measuring rendering rather than
quota behavior; the current limits are defined in `src/terminal/graphics.rs`.

Optional native power captures require the user's available privileges and should
use matching workload and sampling windows. Whole-system CPU/GPU power includes
WindowServer, the producer, profiling overhead, and unrelated applications.
Report it separately from application CPU and wakeups. Activity Monitor Energy
Impact is a relative score, not watts. Raw billed-energy counters are not watts
either. Record unavailable power or GPU metrics explicitly rather than inferring
them from CPU or memory. The process sampler and workload do not invoke sudo or
control application UI.

## Source application comparison on macOS

After building both optimized source binaries, compare fresh processes with one
fixture at a time:

```sh
mise run bench:macos:application target/performance/spaceterm-baseline target/release/spaceterm idle 2
mise run bench:macos:application target/performance/spaceterm-baseline target/release/spaceterm scroll 2
mise run bench:macos:application target/performance/spaceterm-baseline target/release/spaceterm scroll 2 hidden
mise run bench:macos:application target/performance/spaceterm-baseline target/release/spaceterm scroll 2 unfocused
mise run bench:macos:application target/performance/spaceterm-baseline target/release/spaceterm history 2
```

The task stages temporary signed application bundles with distinct benchmark
bundle identifiers. Each trial uses separate
XDG configuration, data, state, cache, and runtime directories, preserving the
account's `HOME` and existing SpaceTerm settings. The workload becomes that
launch's shell, and writes its process identifier and completed output counts
to private files. Two ten-second captures begin after five seconds of warm-up.
The task samples the application and workload process separately, records
on-screen window bounds and frontmost status before and after capture, and
alternates binary order across repetitions. The `hidden` state hides only the
owned application and verifies that it has no on-screen window. If a native hide
request fails and event posting is already available, the harness sends the Hide
shortcut only to that PID. State verification remains mandatory. `unfocused`
activates a small owned helper window and verifies that SpaceTerm stays visible
and that the helper's bounds do not overlap it.
Window observations use a fresh AppKit helper process because
[NSRunningApplication properties](https://developer.apple.com/documentation/appkit/nsrunningapplication?language=objc)
can remain cached until the next main-runloop turn.
The `history` fixture emits exactly 10,000 fixed ASCII lines, reports readiness
after output completes, and holds the terminal idle during warm-up and capture.
The task terminates only processes it started. It checks matching output totals,
terminal grids, and window geometry across trials. A fixed cursor-position query
after completed output must receive a valid terminal reply, including while
hidden. The query's four bytes are accounted separately from workload output.
The reported shell-start interval is neither first-frame time nor shell-ready
time for a normal login shell.

On AeroSpace hosts, `--float-owned-window` excludes only the benchmark PID's sole
window from changing tiled geometry. No window-manager configuration is changed.
Geometry checks still apply. `--profile-memory` captures sanitized numeric native
footprint categories after output and CPU sampling finish, while the synthetic
shell remains alive. Logical Metal allocation sizes and physical footprint are
different measures; neither establishes GPU power or execution time.

`mise run build:performance` enables optional numeric frame and startup probes.
Compare two such builds with `--frame-counters`. The sampler adds one wake per
second to both processes; ordinary builds omit it. Native frame wake, lifecycle,
and pixel checks are available through the `test:gpui:*:macos` tasks. The separate
`bench:gpui:path-targets:macos` fixture measures allocation accounting and resize
call cost without presenting drawables, so it does not measure resident RAM or
complete application resize latency.
