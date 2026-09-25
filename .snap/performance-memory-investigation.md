# Native memory investigation

Status: active research on 2026-09-26. The footprint difference remains
unattributed. This audit read source, existing native logs, local SDK declarations,
and tool help. It ran no native capture, build, or production edit. The coordinating
agent owns the running profile pipeline.

## Observed evidence

Continuation correction, 2026-09-26: the coordinator's isolated lazy-path-target
comparison reports zero candidate startup allocation sets versus three baseline
sets, with zero path scenes in either variant. Scrolling physical footprint
remains about 229-231 MiB in both, then about 131 MiB in both later. This does not
support attributing the prior roughly 96 MiB graphics-ledger transition to the
unused path texture pair. Deferred driver commitment is a possible explanation,
not an established cause. Record avoided allocations separately; do not claim
100 MiB of resident RAM savings from this candidate. These measurements were
reported by the coordinator, not captured by this reviewer.

Consumption barrier added for the coordinator's final capture gate: the workload
can opt into `SPACETERM_BENCH_ACK=1`. After its timed output, it sends the fixed
four-byte DSR cursor-position query and waits for a bounded standard response.
Receiving it confirms the terminal parser reached the query after the prior
output. It does not establish that a visible frame presented those pixels.
The same check applies while focused, unfocused, or hidden.

`consumption_ack` reports only `received`, `skipped_smoke`, numeric cursor row and
column, separate `query_bytes`, and `elapsed_ms`. Existing output byte/line/update
counts and timed `elapsed_s` exclude this protocol exchange. Smoke mode skips it
explicitly; ordinary manual workloads remain unchanged unless opted in. The
query write and reply read share a 1.5-second deadline; input is capped at 4,096
bytes and partial report storage at 14 bytes. The workload restores raw terminal
and blocking settings in `finally`, and emits no unrelated input or native error
text. The coordinator owns main-harness acceptance and execution of the new
`scripts/test-terminal-resource-workload.py` parser/restoration tests.

All completed scrolling trials below report 1,200 updates, 9,640 lines, and
1,050,487 emitted bytes over about 20 seconds. Each resource capture contains
20 intervals over about 10 seconds after a five-second warmup. These are
time-weighted footprint means, not live allocation counts or settled endpoints.

| Log suffix | Geometry in points | Original footprint MiB | Combined footprint MiB | Interpretation |
| --- | --- | --- | --- | --- |
| `scroll` | 1472 x 937 | 226.76, 229.94 | 230.92, 230.81 | Consistent geometry; candidate higher in two runs |
| `scroll-repeat` | Changes from 1472 x 937 to 900 x 580 | 230.73, 175.11 | 234.07, 176.61 | Reject whole-batch comparison; original guard missed cross-trial geometry |
| `scroll-final` | 900 x 580 | 172.01, 169.95 | 177.53, 176.25 | Candidate higher in two runs; CPU lower in this batch |
| `scroll-swapped` | 900 x 580 | 174.49, 175.52 | 177.04, 175.48 | Binary roles reversed under bundle labels; gap shrinks and ranges nearly meet |

Logs are `target/performance/round2-application-<suffix>.jsonl`. Original and
combined binary SHA-256 prefixes are `f9b6f72e316c` and `1ff80256b1ed` respectively.
The swapped log records the mapping explicitly; never interpret its `baseline`
label as original source. The separate `scroll-isolated` log uses different hashes
(`3f1a0e91a09e`, `b826a5d7e06d`), so it cannot be pooled into this table without its
source-variant manifest. Its respective footprint ranges are 176.31-176.99 and
176.63-177.56 MiB.

Focused idle also varies within one binary: combined samples were 124.98 and
114.56 MiB, with original samples 121.25 and 124.78 MiB. That roughly 10 MiB spread
precludes treating every few-MiB difference as a code-caused retained allocation.
It does not invalidate the repeated scrolling difference; that difference still
needs attribution.

## Source-level retention audit

No color-Arc identity dependency was found. `TerminalGridCache` compares color
values by `Eq`. Reprojection and cursor scene eligibility also compare configured
color values. Pointer comparisons concern screen snapshots, rows, or prepared
geometry. Sharing color Arcs changes none of those identities. Color payloads
contain scalar values and arrays, with no references to snapshots, GPU resources,
or owners. The glyph lookup helper adds no retained fields and returns the same
color. These facts rule out a direct ownership cycle in the two changes; they do
not rule out allocator layout or changed submission timing.

A concrete timing-sensitive pool exists in
[GPUI's Metal renderer](../third_party/gpui/src/platform/mac/metal_renderer.rs).
`InstanceBufferPool` begins with 2 MiB managed Metal buffers. `acquire` allocates
when its free vector is empty. Command completion returns the buffer, and
`release` retains every buffer matching the current size, with no count limit.
The pool is retained by `MacPlatformState` and shared by windows. A high-water
burst can therefore leave reusable GPU buffers resident after it finishes.
Faster CPU submission could change the number simultaneously outstanding. This
is a testable mechanism, not evidence that it caused the observed difference.

Scene overflow doubles the shared buffer size up to the existing 256 MiB stop
condition. Old-size free buffers are cleared and late old-size returns are
dropped. Distinguish a change in pool count from a change in buffer size before
choosing a fix. Metal drawables are separately capped at three per renderer;
texture dimensions and display backing scale also affect memory.

## Sampling recommendation

Keep the existing `proc_pid_rusage` sampler during the CPU comparison. Its
`rusage_info_v0` ABI matches the installed SDK, and PID start-time checks prevent
PID reuse from silently changing the target. One external query per interval
already provides physical footprint and resident size. Retain first, last,
minimum, maximum and time-weighted means from those same samples, with monotonic
timestamps. The harness currently discards the interval rows and retains only
means, so old logs cannot reveal whether a footprint difference was a plateau,
a single step, or continued growth.

After the CPU interval, use one `/usr/bin/vmmap -summary <owned-pid>` snapshot.
The installed tool supports `-summary`; it omits individual region listings.
This is a one-shot attribution step, not a low-overhead continuous sampler.
Record its elapsed time and workload phase. Keep it outside the CPU interval and
do not compare its overhead to the unprofiled CPU numbers. The coordinator has
implemented an optional post-capture profile; its first native results and parser
validation remain in progress.

Retain only numeric columns from allowlisted categories, including `MALLOC`
families, `VM_ALLOCATE`, `IOAccelerator`, `IOSurface`, `CoreAnimation`, `Stack`,
and total. Preserve column names and units rather than positional numbers alone.
Group unknown categories into a numeric `other` total only when the format permits
an exact sum. Do not print raw stdout/stderr, headers, process identifiers,
addresses, paths, region labels, mapped filenames, allocation stacks, or terminal
contents. Capture tool output in memory and emit sanitized JSON. `-wide` adds
detail that is not needed for category attribution; the current parser must
continue rejecting everything outside its allowlist if it remains enabled.

If `vmmap` cannot attach, record a typed unavailable result. Do not assume ad-hoc
signing grants task inspection. `/usr/bin/footprint --pid <owned-pid> --format
bytes --swapped --wired` is an available alternative category summary; its JSON
or text output also requires sanitization. Do not use `--all`, `--unmapped`,
verbose region output, corpse forks, heap dumps, or allocation stack logging for
the first attribution pass. Those collect more or perturb the workload more.

Apple explains that Metal allocations appear in `VM: IOAccelerator` and drawables
in `VM: IOSurface`, and that private Metal storage is absent from ordinary heap
allocation tracking. Its VM Tracker reports dirty and compressed/swapped memory
separately from resident memory. Therefore RSS alone cannot establish the GPU or
heap contribution. See Apple's
[Metal memory analysis guide](https://developer.apple.com/documentation/xcode/analyzing-the-memory-usage-of-your-metal-app).

For a later narrow diagnostic build, numeric pool counters are more decisive than
heap stacks: current buffer size, free count, outstanding count, allocations and
peak outstanding count. `TASK_VM_INFO` also exposes numeric graphics-footprint,
compressed, reusable and internal ledgers in the installed SDK. Reading another
process requires task access; calling it in an explicit diagnostic build avoids
assuming that access. Neither extension is implemented by this audit.

## Attribution controls

1. Run an A/A comparison with the exact same binary under both benchmark bundle
   identities, then repeat the original/combined pair with the label mapping
   reversed. Store source hashes and staged hashes separately: `stage_binary`
   copies and ad-hoc signs the bundle after the current manifest hashes source
   binaries. Bundle identifiers depend on labels and can affect AppKit settings.
2. Keep one geometry, position, backing scale, display refresh rate, material,
   system appearance and active accessibility clients. The current harness
   records bounds but not backing scale or refresh rate. Cross-trial focused
   bounds validation now rejects the mixed batch above.
3. Compare original, palette-only, glyph-only and combined from the same source
   snapshot and build settings. Existing source-comparison baseline files match
   `1e25ad9` byte-for-byte for both changed production files. That does not prove
   all dependencies, native archives, compiler flags or remaining worktree files
   matched each binary; retain the complete build/source manifest.
4. Compare the same phase: active scrolling, then a fixed settled interval with
   the process and screen still alive. Emitted byte totals establish producer
   work, not consumed bytes at every sampling instant or number of GPU submissions.
   The current producer deadline can end while a slow post-capture profile runs;
   timestamp the profile and preserve that distinction.
5. If excess is `IOAccelerator`, inspect the instance pool before changing Rust
   allocation policy. If `IOSurface`, inspect live drawable count and dimensions.
   If malloc categories, separate used bytes from reserved arena capacity before
   calling it a leak. If compressed/reusable categories differ, retain both the
   ledger footprint and category accounting instead of treating RSS as equivalent.

## Bounded buffer reuse proposal

Do not impose a global fixed free-buffer count while the pool serves multiple
windows. A safer candidate, if attribution confirms the pool, ties retained free
capacity to registered live renderers and their proven maximum simultaneous frame
demand. When a renderer closes, trim only free buffers; apply the reduced budget
again as its outstanding command completions return. Completion handlers already
own in-flight buffers, which must remain untouched.

With a verified three-slot demand per renderer, retaining the combined live
renderer demand can preserve the next ordinary burst while releasing historical
capacity from closed windows. The drawable limit alone is not yet proof of that
bound for every submission/retirement path. Validate multiwindow bursts, closing
with pending commands, resize, failed submission, pool growth, and device identity.
Removing active-scene high-water capacity or trimming merely because a window is
temporarily hidden necessarily risks allocation on a later burst. Measure that
latency tradeoff; do not claim a policy that both releases needed storage and
guarantees no future allocation.

## Hidden capture failure

The recorded hidden attempt failed during the external hide request, before any
resource samples. The owned baseline app was alive, launched, focused and visible.
`NSRunningApplication.hide` returned false. Apple's contract says the return value
reports whether the request was sent; it does not demonstrate a SpaceTerm hidden
state failure. The current script aborts at this point and correctly emits no
hidden resource result. See Apple's
[hide API](https://developer.apple.com/documentation/appkit/nsrunningapplication/hide%28%29).

The script invokes AppKit from a short-lived Python helper without a running
AppKit event loop. Apple also documents that time-varying `NSRunningApplication`
properties refresh with the run loop. This is a diagnostic lead, not a proven
explanation for the failed request. The observation helper uses a fresh process,
so indefinitely cached properties are less likely than in one polling instance.

The smallest reliable measurement fallback is to focus the exact owned app,
invoke its existing Hide Application action (`cmd-h`) through targeted native UI
automation, then re-observe `hidden=true`, `frontmost=false`, and zero visible
owned windows before sampling. GPUI already routes this action to
`NSApplication.hide:`. Do not replace hiding with minimizing or ignore the false
return. A helper correction should initialize an AppKit context, use a bounded
run-loop-aware wait, re-fetch the owned application, and report bounded activation
policy/state booleans on failure; it still needs an actual successful hide/restore
trial. No hidden-state correction or capture was performed by this audit.

## First category capture

The coordinator's first `continuation-memory-regions.jsonl` capture succeeded,
but `vmmap` paused the running application. The producer skipped catch-up updates
by design: three trials emitted 1,176 updates and the fourth emitted 1,175,
instead of 1,200. The coordinator is moving the profile after producer completion
and a fixed settle interval. These perturbed captures cannot establish equal-work
resource improvements.

The available sanitized category data still guides the next measurement:

- All four runs show 64 KiB dirty `IOAccelerator` and 64.4 MiB dirty `IOSurface`.
  Combined has nine IOSurface regions versus seven for original, but the extra
  region count does not correspond to extra dirty bytes in this summary.
- `Malloc Small` dirty memory overlaps: original 31.8/32.7 MiB and combined
  32.7/32.3 MiB. Its empty-region dirty memory also varies across launches.
- VM-summary total dirty memory is 227.0/227.5 MiB for original and
  225.7/225.3 MiB for combined. VM-summary totals include sharing/accounting that
  differs from `proc_pid_rusage` physical footprint; these are not replacements
  for the earlier footprint means.

This does not support reducing the instance pool yet. No raw summary was retained
for this audit, so omitted category names cannot be verified from these logs.
Possible fixed additional buckets include `IOKit`, known malloc reusable/reused
variants, and Mach-O segment labels `__DATA`, `__DATA_CONST`, `__DATA_DIRTY`,
`__AUTH`, `__AUTH_CONST`, `__LINKEDIT`, and `__TEXT`. Match only exact known labels.
For other valid category rows, aggregate numeric columns under one fixed
`unlisted_regions` key without retaining the original name. Exclude footer and
total rows from that aggregation to avoid counting them twice. This preserves
coverage without emitting custom Metal labels or mapped paths. Numeric pool
counters or graphics ledgers remain the next step if category summaries do not
account for a reproducible delta.
