# Remaining performance work

Updated: 2026-09-26. This is the continuation queue after the
[second measured pass](performance-round2.md). These are hypotheses and profiling
requirements, not promised gains. Preserve all terminal behavior and appearance.

## Recommendation

Measure and address clean-window frame-source wakeups next. The existing native
idle captures still show about 120 interrupt wakeups per second. Source inspection
identifies recurring GPUI display callbacks, but their causal contribution needs
profiling. Ghostty's demand-driven renderer is relevant evidence for this work.
Do not stop callbacks without a complete restart protocol.

## Ordered queue

| Priority | Work | Evidence required before implementation | Required parity |
| --- | --- | --- | --- |
| 1 | Demand-driven idle frame source | Attribute wakeups to callbacks; count draws and presents at known refresh/scale, focused and unfocused-visible | Restart for output, input, invalidation, animation, next-frame callbacks, resize, display change, and restoration; preserve latency |
| 2 | Event-driven Control Connection exit observation | Measure 1/4/16 ready Remote Workspaces; current source polls every 10 ms per connection | Prompt failure and authority revocation, single reaping owner, cancellation and cleanup; no longer polling interval as a substitute |
| 3 | GPU path texture and hidden-resource lifetime | Native Metal allocation/residency capture with ordinary terminal, paths, blur, resize, scale, hide/restore | Identical pixels, in-flight command safety, first-use latency, graphics retention and restore |
| 4 | Find corpus allocation and native incremental search | Profile Find open during fixed output, actual retained history and compression; compare native API with current search | Same matching, order, screen switching, counts, navigation, reflow, pruning and complete publication |
| 5 | Output and cell allocation | Allocation profile for ASCII, Unicode, escape-heavy output; include real deterministic terminal traces | OSC 52 denial/order, Unicode/wide cells, bounded memory, input fairness and equal throughput |
| 6 | Pane and graphics lifetime | 1/4/16 Panes, repeated close/open, image replacement, inactive Tabs, settled footprint | Deterministic teardown, full retained history and graphics, exact restoration |
| 7 | Launch and build configuration | First native frame, first terminal frame, normal shell-ready milestones; measure SSH probe tail; compare ThinLTO builds | Initial font/menu/appearance fidelity, unwinding across callbacks, full feature parity |

## Measurement debt

- Resolve the second-pass scrolling footprint observation with repeated controlled
  allocation/residency profiles. The operation benchmarks improve, while some
  native captures show higher footprint. Identity swaps and isolated variants
  do not establish the cause; preserve every batch in the evidence record.

- Repair and validate native hidden-state capture. The inherited harness's hide
  operation failed its acceptance check; do not treat unfocused-visible as hidden.
- Capture unfocused-visible and inactive-Tab workloads separately, preserving
  output consumption and terminal replies.
- Record exact terminal grid, display scale/refresh, settings, completed output,
  input latency, CPU, footprint, and GPU metrics appropriate to the hypothesis.
- Keep native builds outside measurement windows. Use `.mise.toml` tasks and
  source-built, isolated benchmark applications. Do not measure a stale installed app.
- Require a repeatable benefit or an explicit small measured tradeoff. Reject
  optimizations that hide work by dropping output, freezing visible content,
  reducing history, changing fonts, or disabling graphics/accessibility.

## Research entry points

- [Rendering and visibility](performance-round2-rendering.md): pinned Ghostty/Zed
  sources, scheduling contracts, spinner cadence, and path textures.
- [Terminal engine](performance-round2-terminal.md): pinned Ghostty/libghostty-vt
  and WezTerm sources, search differences, cell text, output filters, and buffers.
- [Launch and lifecycle](performance-round2-launch.md): rejected font queries,
  process observation, build settings, and Pane ownership.
- [Accepted measurements](performance-round2-results.md) and
  [independent review](performance-round2-review.md).
