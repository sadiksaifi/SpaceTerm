# PR merge readiness

Status: active, 2026-09-26. PR #352 remains open and unmerged.
The user asked for the total time to merge, not a batch estimate. The coordinator
estimated 45-60 minutes and froze implementation scope. Further performance
experiments belong in the documented follow-up queue. No merge is authorized.

## Changes to finish

- Native process-exit observation with cancellation and polling fallback.
- Shared display-link ownership, demand-driven scheduling, and immediate
  asynchronous first wake. Preserve continuous vsync pacing and input grace.
- Lazy Metal path targets with identical native path pixels.
- Find corpus encoding without temporary grapheme Strings or increased capacity.
- Explicit performance instrumentation and reliable focused, unfocused-visible,
  hidden, geometry, output-consumption, and resource measurement.
- Repair artifact-supervisor failure handling exposed by final validation.

## Required before publishing

- [x] Complete `mise run validate:macos` successfully. The final task exits 0.
  It includes 3,123 workspace tests, 206 native macOS tests, renderer/Blade,
  vendor, formatting, lint, and 20 benchmark-parser/ownership checks. The first attempt's
  artifact monitor failed while measuring changing build files. Its orphaned
  test command finished with 3,123 tests passing and no failures; that does not
  count as a complete validation gate.
- [x] Recheck native display-link lifecycle after immediate first wake, including
  cancellation before the queued wake is delivered. Final fixture exits 0:
  one native link, 66 sources created/released, 83 subscriptions/unsubscriptions,
  81 native starts/stops, and zero callbacks during close drain.
- [x] Native frame behavior after immediate first wake: idle, notification,
  callbacks, animation, direct draw, input grace, hidden restoration,
  unfocused-visible rendering, and close inside a callback all pass.
- [x] Native latency mechanism comparison: the revised idle source has 1.293 ms
  notify and 1.377 ms input median presentation-observation latency. Its earlier
  4-7 ms restart penalty is removed. This is not a photon-latency measurement.
- [x] Validate the actual optimized final binary in focused, unfocused-visible,
  and hidden workloads with equal output and terminal consumption acknowledgment.
- [x] Measure the actual production Find encoder against the retained legacy
  implementation and record native Metal allocation accounting with its limits.
- [x] Review final production and measurement changes independently, resolve
  material findings, and finish source records.
- [ ] Make atomic commits, push the existing branch, update the full PR body,
  and verify its remote head and check state.

## Research that does not block this PR

Pane scaling and image pressure require their own multi-producer benchmark with
layout barriers and explicit child ownership. Native graphics residency needs
allocation categories sampled alongside actual drawable dimensions. The current
data does not justify a large physical-memory claim for lazy path targets.
Additional startup optimization is evidence-gated; measured Metal construction
is only 1.74-2.82 ms, so speculative prewarming is not selected.

These limits must remain visible in the PR. Merge readiness does not prove a
global performance optimum or erase the research queue.
