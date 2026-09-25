# Performance progress

## Current state

The performance goal is active on `perf/application-resources`. The initial
inventory and three research reports are complete. GPT-6 Sol agents at high
reasoning own the font, geometry, compression, and native process experiments.
The parent owns cell UTF-8 construction and integration.

| Work item | Evidence | State |
| --- | --- | --- |
| Cell UTF-8 construction | Seven-sample optimized benchmark improves all three workloads; explicit Unicode fidelity test passes | Implemented |
| Stable row geometry | Optimized cache fixture saves about 1.035 microseconds/call; 67 terminal element tests pass | Implemented |
| Startup font work | Native full/selected classification fixture differs by about 262 ms; 19 appearance tests pass | Implemented |
| Idle compression | Four native fixtures save 0.375-0.391 MiB with exact history copy; bounded scheduling, worker fairness, Find and selection checks pass | Implemented |
| Complete application resources | Valid ABBA comparisons for focused idle and scrolling; hidden and focused-history attempts excluded | Captured with limits |
| Combined validation and PR | 3,112 tests pass; format, Rust lint and release build pass | PR pending |
| Three adversarial PR reviews | Run after PR creation; resolve material findings | Pending |

The optimized baseline build passed on retry. Baseline portable test binaries
reported 2,893 passing tests and zero failures, but that task exited with status 2
because the artifact supervisor failed to measure a changing build directory.
Repeat the command successfully before claiming a passing validation gate.

Optimized ignored fixtures run through `mise run bench:one`; its explicit
`gpui/inspector` feature is necessary because existing application inspector tests
reference interfaces that release GPUI otherwise omits. Native font measurements
need a real GPUI application on the main thread: GPUI's normal test text system
does not enumerate native fonts.

## Next actions

1. Publish the validated implementation and measurement records as a PR.
2. Run three independent GPT-6 Sol high adversarial reviews against that PR.
3. Resolve material findings, revalidate affected behavior, and update the PR.

## Remaining profiling work

Native hidden-state and history trials did not meet the harness acceptance checks
on this host. Do not claim gains from those trials. Unfocused-visible windows,
first-frame timing, first Settings opening, Pane scaling, graphics lifetime, GPU
execution/residency, and remote idle polling remain explicit follow-up measurements
in the inventory. No production change was selected for those unmeasured areas.
