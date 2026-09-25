# Performance goal

Status: active. Started 2026-09-25 at baseline commit
`134d7027b2014f89d29d0f9c4d087e894f33cfbc`.

Improve SpaceTerm launch latency, CPU, GPU, memory, wakeups, and sustained terminal
throughput while preserving every feature and the existing appearance. The user
explicitly requested this repository record and parallel research.

Branch: `perf/application-resources`. Complete the problem inventory and assess
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

## Workstreams

| Area | Completion evidence | Status |
| --- | --- | --- |
| Launch | Selected font work measured and reduced; first-frame and ordinary-shell milestones remain unmeasured | Implemented; focused captures complete |
| Foreground | Snapshot and unchanged-row costs reduced; native process comparisons distinguish CPU, footprint and throughput | Implemented; focused captures complete |
| Background | Existing visibility gates audited; hidden process comparison selected; unfocused-visible and inactive-Tab scaling remain profiling work | Hidden capture unavailable; limitation recorded |
| Memory and lifetime | Bounded idle compression preserves history and reduces native footprint; Pane and graphics lifetime profiling remains | Compression implemented |
| GPU | Source audit complete; no native execution/residency measurement, so no GPU allocation or scheduling changes selected | Deferred with evidence requirements |
| Regression protection | 3,112 tests, Rust lint, format and optimized build pass; three PR reviews follow publication | Reviews pending |

"Best possible" has no provable global endpoint. Keep this goal active until the
measured candidates are resolved and remaining limits are explicitly documented.
Do not infer overall completion from one faster benchmark.

## Records

- [Problem inventory](performance-inventory.md): costs, candidate fixes, selection.
- [Measurement protocol](performance-measurements.md): experiments, results, limitations.
- [Engine research](performance-research-engines.md): Ghostty, libghostty-vt, WezTerm.
- [Rendering research](performance-research-rendering.md): GPUI, Zed, rendering and visibility.
- [Launch research](performance-research-launch.md): launch and resource ownership.
- [Progress](performance-progress.md): implemented changes and next actions.

Update this record and the relevant evidence file after each completed work item.
