# Performance goal

Status: second measured optimization pass completed on 2026-09-26.
The continuing performance objective and remaining profiling are recorded below.
The first measured implementation and PR reviews completed on 2026-09-25.
PR [#352](https://github.com/sadiksaifi/SpaceTerm/pull/352) is open and unmerged.
Started at baseline commit
`134d7027b2014f89d29d0f9c4d087e894f33cfbc`.

Improve SpaceTerm launch latency, CPU, GPU, memory, wakeups, and sustained terminal
throughput while preserving every feature and the existing appearance. The user
explicitly requested this repository record and parallel research.

Branch: `perf/application-resources`. The second pass starts at `1e25ad9` and
continues in this checkout. Its assignments, acceptance criteria, and outcomes
are tracked in [second-pass work](performance-round2.md).

Original first-pass scope: complete the problem inventory and assess
potential fixes before implementation. Research and review agents use GPT-6 Sol
with high reasoning; a large implementation may use GPT-6 Astra with high
reasoning. After implementation and validation, open a PR and run three
independent adversarial reviews. Resolve material findings and revalidate before
reporting the PR ready. Do not merge it without a user request.

## Guardrails

- Preserve terminal protocol behavior, input latency, accessibility, graphics,
  appearance, remote work, and cleanup.
- Keep unfocused-visible windows distinct from hidden windows. Unfocused windows
  still present output. Hidden Terminal Sessions still process output and replies.
- Keep diagnostics content-free and local filesystem authority explicit.
- Accept changes against repeatable optimized-build measurements and relevant
  correctness checks. Report unmeasured effects as hypotheses.
- Compare equal output volume, geometry, settings, and display conditions. Lower
  resource use caused by lost output or reduced functionality is a regression.

## Second-pass outcome

Three research agents revisited Ghostty/libghostty-vt, WezTerm, Zed, startup,
rendering, and terminal ownership. Two measured operation optimizations are
implemented and validated: unchanged snapshot color reuse and fragmented glyph
color lookup. The equivalent native font query was slower and was reverted.
The application benchmark now rejects geometry changes between trials.

The full validation gate passes. Native application measurements remain mixed,
including a footprint concern that the captures did not resolve. No new launch,
GPU, hidden-state, or general RAM improvement is claimed. See
[second-pass results](performance-round2-results.md) for exact measurements and
[remaining work](performance-next.md) for the ordered continuation queue.
The next recommendation is to attribute the approximately 120 idle wakeups per
second before changing the frame-source wake protocol.

## First-pass workstreams

| Area | Completion evidence | Status |
| --- | --- | --- |
| Launch | Selected font work measured and reduced; first-frame and ordinary-shell milestones remain unmeasured | Implemented; focused captures complete |
| Foreground | Snapshot and unchanged-row costs reduced; native process comparisons distinguish CPU, footprint and throughput | Implemented; focused captures complete |
| Background | Existing visibility gates audited; hidden process comparison selected; unfocused-visible and inactive-Tab scaling remain profiling work | Hidden capture unavailable; limitation recorded |
| Memory and lifetime | Bounded idle compression preserves history and reduces native footprint; Pane and graphics lifetime profiling remains | Compression implemented |
| GPU | Source audit complete; no native execution/residency measurement, so no GPU allocation or scheduling changes selected | Deferred with evidence requirements |
| Regression protection | 3,113 tests, Rust lint, format and optimized build pass; three independent reviews complete; one test-coverage finding corrected and verified | Complete |

"Best possible" has no provable global endpoint. The first measured implementation
batch is complete: all four selected candidates are implemented, validated and
reviewed. The unmeasured areas above remain follow-up profiling work. Completion
of this batch is not a claim that every possible performance cost is eliminated.

## Records

- [Problem inventory](performance-inventory.md): costs, candidate fixes, selection.
- [Measurement protocol](performance-measurements.md): experiments, results, limitations.
- [Engine research](performance-research-engines.md): Ghostty, libghostty-vt, WezTerm.
- [Rendering research](performance-research-rendering.md): GPUI, Zed, rendering and visibility.
- [Launch research](performance-research-launch.md): launch and resource ownership.
- [Progress](performance-progress.md): implemented changes and remaining profiling.
- [Validation and reviews](performance-validation.md): checks and three independent reviews.
- [Application results](performance-application-results.md): accepted comparisons and limits.

Update this record and the relevant evidence file after each completed work item.
