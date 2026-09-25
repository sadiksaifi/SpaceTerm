# Second performance pass

Started: 2026-09-26. Baseline: `1e25ad9`. Status: measured implementation pass complete; further profiling remains.

The user renewed the goal of minimum launch latency and foreground and unfocused
RAM, CPU, and GPU use, preserving all functionality and appearance. This record
is task documentation explicitly requested by the user.

## Acceptance

- Preserve input latency, output, terminal protocols, graphics, accessibility,
  remote work, appearance, and deterministic cleanup.
- Measure optimized builds with identical workloads and separate visible
  unfocused windows, hidden windows, and inactive Tabs.
- Accept production changes only with relevant correctness checks and repeatable
  evidence. Label source observations, hypotheses, and measured effects separately.
- Complete a bounded pass: research the remaining inventory, investigate the
  strongest candidates, implement justified improvements, validate and review
  the changes, and record unresolved profiling requirements. No finite pass can
  prove a global performance optimum.
- Preserve the current branch. Do not merge or publish without authorization.

## Assignments

| Owner | Work | Record | State |
| --- | --- | --- | --- |
| Rendering research agent | Zed/GPUI and Ghostty rendering, visibility, GPU ownership | [Rendering](performance-round2-rendering.md) | Complete |
| Terminal research agent | Ghostty, libghostty-vt, WezTerm, parser/snapshot/memory costs | [Terminal](performance-round2-terminal.md) | Complete |
| Launch research agent | Startup, native dependencies, Pane scaling and lifetime | [Launch](performance-round2-launch.md) | Complete |
| Primary agent | Select experiments, implement, measure, validate, integrate findings | This record and results | Complete |

## Evidence inherited from the first pass

The existing optimized measurements establish reduced snapshot work, stable row
preparation, and startup font classification. Accepted native captures show lower
footprint for fresh default Settings. They do not establish GPU savings or cover
unfocused-visible windows. Focused idle still recorded about 120 interrupt
wakeups per second. See [application results](performance-application-results.md).

## Decisions and results

Existing records remain historical evidence; this pass will not relabel
unmeasured first-frame, GPU, or hidden-state effects as improvements.

Selected experiments:

1. Remove repeated allocation of unchanged snapshot palette, override, and
   configured-color data. Accepted after repeated release measurements: clean
   checks improve 13-15%, with native queries and damage checks preserved.
2. Replace repeated linear glyph color-run lookup with ordered lookup. Validate
   byte offsets and nonmonotonic glyph order. Accepted with an explicit measured
   tradeoff: 120-run lookup improves 78%; eight-run rows cost about 17 ns more.
3. Compare Core Text family enumeration with full descriptor enumeration. Require
   exact family-set parity, custom font handling, and optimized native timings.
   Rejected: the complete equivalent query was slower, while the faster variants
   could not guarantee the same catalog. Production font code is unchanged.

See [experiment results](performance-round2-results.md) for timings and limitations,
and [independent review](performance-round2-review.md) for behavior and ownership
verification. The full validation gate passed. Native CPU and footprint evidence is mixed;
no general application resource improvement is claimed. See the
[continuation queue](performance-next.md) for unresolved measurements.

Builds and benchmarks run serially under the primary agent. Research agents own
separate files. The pre-change source binary is retained locally at
`target/performance/spaceterm-round2-baseline`; raw experiment logs use the
`target/performance/round2-` prefix. These generated artifacts are not committed.

## Delivery

The measured pass is complete on the existing branch. Local commits:

- `40b965f`: reuse unchanged snapshot colors, including the measurement fixture.
- `28a8bc6`: index fragmented glyph color runs, with behavior and timing fixtures.
- `956fd2d`: retain controlled native font comparisons; no font production change.
- `9745b1a`: reject cross-trial window geometry changes in native captures.

`mise run validate` passed for the combined source. Final native script/diff lint
passed, and `mise run build:release` reproduced the measured combined binary's
SHA-256. The working production files were verified against the exact validated
source after isolated comparison builds. The commits remain local; PR #352 has
not been updated or merged by this pass. Remaining profiling is recorded in
[the continuation queue](performance-next.md), including the unresolved footprint
observation. Completing this pass does not establish a global performance optimum.
