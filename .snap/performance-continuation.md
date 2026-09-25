# Continuing performance execution

Status: implementation, validation, and publication complete, 2026-09-26.
Baseline for this continuation: `3831cd1`.

This is a chronological work log. Earlier pending work is superseded by
[final results](performance-continuation-results.md) and
[merge readiness](performance-merge-readiness.md).
The user directed continued work after the coordinator stopped prematurely.
The second-pass commits remain local and validated; its footprint limitation is
unresolved. The broader goal remains active until required work is handled.

## Current assignments

| Owner | Work | Record | State |
| --- | --- | --- | --- |
| Primary | Controlled VM-region footprint attribution, measurement harness, integration | This file | Active |
| Rendering agent | Complete idle frame wake protocol and implementation design | performance-frame-source.md | Active |
| Terminal agent | Independent memory/harness attribution and hidden-state analysis | performance-memory-investigation.md | Active |
| Launch agent | Event-driven Control Connection exit observation | performance-remote-exit.md | Active |

Builds and native captures are coordinated serially by the primary agent.
Agents may research and edit their assigned records concurrently. Production
changes require measured evidence and relevant behavior/appearance validation.

## Ordered work

1. Attribute the native scrolling footprint difference using controlled VM-region
   profiles and repeat the original/candidate comparison.
2. Attribute idle wakeups and implement a complete demand-driven wake protocol
   where evidence supports it. Preserve visible output and input latency.
3. Repair hidden and unfocused-visible capture and measure them separately.
4. Continue the remaining queue in performance-next.md, including remote polling,
   GPU lifetime, launch milestones, Find and Pane scaling.

No global completion is inferred from finishing one batch. Update this record
with evidence, rejected experiments, accepted changes, and exact outstanding work.

## Measurement corrections

The initial VM parser did not match macOS 27's region-table format. It now
selects the REGION TYPE table, ignores later malloc-zone totals, retains known
numeric category summaries only, and rejects a missing total. Parser tests pass.
`vmmap` pauses the target. Capturing it before paced output ended reduced the
producer's completed output; those runs are excluded. Capturing after producer
exit instead observed renderer teardown; those runs are also excluded.
The corrected workload reports completion then keeps the synthetic shell alive
for the memory profile. Cleanup terminates the owned producer before the app.
The harness also checks final terminal grid and exact output counts across trials.

## Test gates

Remote exit-observation red gate: `mise run test:macos` failed exactly the new
`macos_collected_process_exit_should_remain_observable` assertion, with 202 other
tests passing. The native observer implementation proceeds from this gate.
The display-link lifecycle candidate compiles through the application native-test
build; registry tests and real native lifecycle verification are still pending.

## Confirmed attribution and current candidates

The optional probe build hash is
`afabcc69bc2fe524ba11751e7b074348dda157adf29d6910f35e6b7d5caad7dc`, retained at
`target/performance/spaceterm-continuation-probes-before`.
Two same-binary focused idle trials each show approximately 120 native and logical
frame callbacks/second. In one nine-second interval all 1,080 callbacks were clean:
zero draws, presents, queued callbacks, or dirty states. Lifetime counters show
one native link retained, three source wrappers created/two released, and three
path-texture allocation sets despite zero scenes containing paths. The 1 Hz
benchmark sampler accounts for one additional process wake per second.
Log: `continuation-frame-idle-before.jsonl`.

The no-drain structured memory capture retains the same 113 x 42 terminal grid,
1,200 updates and 1,050,487 output bytes in each original/combined trial.
Running footprint means overlap earlier same-binary control variation. Settled
captures expose large graphics-ledger transitions (roughly 96 MiB) independent
of code variant, and overlapping malloc sizes. No new palette/glyph ownership
leak is established. The ordinary eager path targets are a separate measured
allocation opportunity: approximately 100 MiB of unmapped graphics footprint
and zero path scenes in the ordinary terminal fixture. Native resource tests
and path-pixel parity tests are being added before lazy allocation.
Logs: `continuation-memory-control.jsonl` (default profiler reclamation) and
`continuation-memory-no-drain.jsonl` (reclamation explicitly disabled).
Do not add footprint's swapped bytes to dirty bytes; that tool includes swapped
in dirty. Its auxiliary ledger and category sum are retained separately.

## Latest validation

- 41 Control Connection tests pass; 206 native macOS tests pass.
- Four shared display-link registry tests pass.
- Real CoreVideo/GCD example passes 32 restart/close cycles and 66 contexts, with
  zero callbacks during close drain. The final probe-enabled task exits 0 and
  asserts one native link, 66 sources created/released, 67 subscriptions and
  unsubscriptions, and 65 native starts/stops. Log: continuation-display-native-probes.log.
- Independent production lifecycle review found no material race/ownership issue.
  The missing late-callback assertion was added and passes in the final native run.
- Eight measurement harness tests pass, covering numeric-only report retention,
  process/unit/error validation, equal output/grid, and bounded counter deltas.

The native path-texture tests now pass, including fractional-edge pixels and
resize reuse. Its isolated optimized candidate is archived and paired scrolling
captures are running. See performance-path-textures.md.

The process-exit observation resource fixture passes. For 16 idle owned local
children, polling consumed about 1.72% CPU and 1,334 interrupt wakes/second;
native observation consumed about 0.0007% CPU and 0.33 wakes/second. This measures
the observation mechanism, not complete connected Remote Workspaces. No retained
memory reduction is claimed. See performance-remote-exit.md.

The real native demand fixture reproduces clean-window callback waste with an
explicit failing assertion and nonzero task exit. Two earlier fixture compile
errors and an AppKit exit-status issue were corrected before accepting the red
gate. Demand scheduling implementation is active.

Independent review found hidden captures discarded pre-hide window geometry.
The harness now records and validates focused geometry against the batch reference
before hiding, then separately verifies hidden state throughout measurement.

Next: measure lazy targets, implement and verify complete frame wake behavior,
verify hidden capture, and continue launch, Find, and Pane scaling work. The
overall goal remains active.

## Current measured demand candidate

Optimized binary `spaceterm-continuation-demand`, SHA-256
`657fbd6fc1dd2e8eceda4b870d044a92d4f128ade308831efdfc193d7d7e1c61`,
passed real native scheduling checks. The accepted focused-idle ABBA capture
at 900 x 580 points reduces CPU from 0.765-0.775% to 0.113-0.135% and interrupt
wakes from 121.5-121.6/second to 1.5-1.6/second. Candidate intervals contain zero
native/logical frame callbacks. The sampler contributes one wake/second.
No physical-footprint improvement is established. Log: continuation-demand-idle.jsonl.

Native latency measurements expose a first-wake tradeoff. In eight alternating
samples per mode, continuous-source notify-to-presentation observation had a
4.148 ms median versus 11.219 ms for an idle source; input medians were 5.325 ms
and 9.820 ms. These are submission observation proxies with 1 ms polling, not
display-photon measurements. The candidate is not accepted until that avoidable
startup delay is investigated. The rendering agent is implementing asynchronous
first-wake dispatch while retaining vsync pacing for sustained work.
Log: continuation-frame-demand-latency.log.

Hidden-state capture now passes at equal geometry using the optional AeroSpace
owned-window floating control. It modifies only the benchmark PID's sole window.
Both variants have zero hidden frame callbacks. A small owned AppKit helper
window and strict non-overlap checks support unfocused-visible captures. The
windowless helper did not become frontmost on this host and was replaced before
accepting a capture. No window-manager configuration is changed by the harness.

Find's direct UTF-8 encoding prototype improves operation time but its first
version increased Unicode corpus capacity. Version two reserves a complete
grapheme first and checks exact capacity parity before measurement. No Find
production change has been accepted yet. See performance-find-continuation.md.

Metal construction takes only 1.74-2.82 ms in observed warm launches. Prewarming
is not selected on that evidence. Additional startup stages now isolate the
roughly 350 ms between run-loop entry and the initial window opening.
