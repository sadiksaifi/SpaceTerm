# Remaining performance work

Updated: 2026-09-26. This is the research queue after the
[completed continuation](performance-continuation-results.md). These are hypotheses and profiling
requirements, not promised gains. Preserve all terminal behavior and appearance.

## Completed continuation

- Demand-driven frame scheduling removes the measured clean-window 120 Hz callback
  stream. The native first-wake, lifecycle, animation, and restoration gates pass.
- macOS Control Connection exit observation replaces idle 10 ms polling, with
  cancellation, single reaping ownership, and polling fallback preserved.
- Path textures allocate at first path use. Native pixels and allocation accounting
  pass; no physical-RAM improvement is claimed.
- Find corpus encoding removes temporary per-cell Strings with exact corpus,
  mapping, and capacity parity. The actual production operation is measured.
- Native focused, unfocused-visible, and hidden captures now enforce equal geometry,
  output, and terminal consumption. The final full validation gate passes.

See [final measurements](performance-continuation-results.md). Implementation
scope is frozen for PR #352; the following are research follow-ups, not unfinished
selected changes or merge blockers.

## Research queue

| Priority | Work | Evidence required | Required parity |
| --- | --- | --- | --- |
| 1 | Pane and graphics lifetime | 1/4/16 Panes with layout barriers, owned producers, repeated close/open, image replacement and inactive Tabs | Deterministic teardown, full retained history/graphics, exact restoration |
| 2 | Native graphics residency | Repeatable graphics-ledger categories alongside drawable dimensions, retained resources and image pressure | In-flight command safety, unchanged pixels, restore and first-use behavior |
| 3 | Launch | First native frame, first terminal frame, normal shell-ready milestones; SSH probe tail and build-profile comparison | Initial menu/font/appearance fidelity and callback unwinding |
| 4 | Remaining Find costs | Many-match snapshot timings, storage retention, incremental native API parity | Matching, order, screen switching, counts, navigation, reflow, pruning and complete publication |
| 5 | Output and cell allocation | Allocation profiles for deterministic ASCII, Unicode and escape-heavy traces | OSC 52 denial/order, wide cells, bounded memory, input fairness and equal throughput |

The second-pass footprint concern was investigated with same-binary controls,
isolated lazy-path builds, and numeric native ledgers. The roughly 96 MiB
transition also occurs with zero path targets; the data does not establish that
this PR introduced the transition or that lazy paths reduce physical memory.
Keep those observations in performance-memory-investigation.md and
performance-metal-ledgers.md. Additional attribution remains an experiment,
not an accepted optimization. Startup probes show Metal construction is only
1.74-2.88 ms, so speculative prewarming was rejected.

## Measurement requirements

Compare equal output volume, terminal grid, display scale/refresh, settings, and
window geometry. Keep builds outside native sampling windows. Distinguish
unfocused-visible, hidden, and inactive-Tab states. Preserve terminal replies,
full output consumption, input latency, graphics, and accessibility. Report
component timings and logical allocation accounting separately from application
CPU, physical footprint, GPU residency, and photon latency. Do not infer a RAM
saving from texture dimensions or a global gain from one mechanism fixture.

## Research entry points

- [Rendering and visibility](performance-round2-rendering.md): pinned Ghostty/Zed
  sources, scheduling contracts, spinner cadence, and path textures.
- [Terminal engine](performance-round2-terminal.md): pinned Ghostty/libghostty-vt
  and WezTerm sources, search differences, cell text, output filters, and buffers.
- [Launch and lifecycle](performance-round2-launch.md): rejected font queries,
  process observation, build settings, and Pane ownership.
- [Accepted measurements](performance-round2-results.md) and
  [independent review](performance-round2-review.md).
