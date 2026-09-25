# Performance progress

## Current state

The measured implementation batch is complete on `perf/application-resources`.
PR [#352](https://github.com/sadiksaifi/SpaceTerm/pull/352) is open and unmerged.
The inventory, three research reports, four production improvements, measurements
and three independent GPT-6 Sol high adversarial reviews are recorded here.

| Work item | Evidence | State |
| --- | --- | --- |
| Cell UTF-8 construction | Seven-sample optimized benchmark improves all three workloads; explicit Unicode fidelity test passes | Implemented |
| Stable row geometry | Optimized cache fixture saves about 1.035 microseconds/call; 67 terminal element tests pass | Implemented |
| Startup font work | Native full/selected classification fixture differs by about 262 ms; 19 appearance tests pass | Implemented |
| Idle compression | Four native fixtures save 0.375-0.391 MiB with exact history copy; bounded scheduling, worker fairness, Find and selection checks pass | Implemented |
| Complete application resources | Valid ABBA comparisons for focused idle and scrolling; hidden and focused-history attempts excluded | Captured with limits |
| Combined validation and PR | 3,112 tests pass; format, Rust/script lint and release build pass; PR #352 published | Complete |
| Three adversarial PR reviews | Two initial reviews found no material issue; third found one P2 test gap, corrected and rechecked by all three reviewers | Complete |

The optimized baseline build passed on retry. Baseline portable test binaries
reported 2,893 passing tests and zero failures, but that task exited with status 2
because the artifact supervisor failed to measure a changing build directory.
Repeat the command successfully before claiming a passing validation gate.

Optimized ignored fixtures run through `mise run bench:one`; its explicit
`gpui/inspector` feature is necessary because existing application inspector tests
reference interfaces that release GPUI otherwise omits. Native font measurements
need a real GPUI application on the main thread: GPUI's normal test text system
does not enumerate native fonts.

## Review outcome

The reviews covered all 31 initial changed paths. One test copied the selection
before testing Find over compressed history. Commit `9596103` moved the marker to
older retained history, runs Find first after compression, and recompresses before
checking selection copying. The focused test and Rust lint passed again. All
three reviewers verified the correction, with no new material finding.

Production code and the measured application binary are unchanged by that fix.
The PR remains unmerged for the user's review.

## Remaining profiling work

Native hidden-state and history trials did not meet the harness acceptance checks
on this host. Do not claim gains from those trials. Unfocused-visible windows,
first-frame timing, first Settings opening, Pane scaling, graphics lifetime, GPU
execution/residency, and remote idle polling remain explicit follow-up measurements
in the inventory. No production change was selected for those unmeasured areas.

## Menu startup follow-up

The user approved requesting application activation before native window creation.
The change passed 21 application tests, lint, format and an optimized build. Two
additional GPT-6 Sol high reviews found no material issue. The source development
app opened and its File menu worked. Exact first-frame menu ordering remains
unverified; see [menu startup timing](performance-menu-startup.md).
