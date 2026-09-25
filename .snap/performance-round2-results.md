# Second-pass experiments

Status: measured implementation pass completed on 2026-09-26. Baseline source: `1e25ad9`.
All timings below use optimized builds through `mise run`; builds and measurements
run serially. These are isolated operation timings, not total application gains.

## Snapshot color reuse

The candidate compares current configured colors, palette values, and override
flags against the last published values. Equal data shares the existing immutable
Arcs. All native queries, damage detection, and publication conditions remain.
No new cache or retained owner is introduced.

`mise run bench:one performance_snapshot` measures seven samples per workload.
Clean and cursor-only checks use 20,000 iterations after 100 warmups. Existing
output fixtures use 300 frames after 20 warmups on a 120 x 40 grid.

| Workload | Baseline median ns | Candidate median ns | Candidate repeat median ns | Baseline repeat median ns |
| --- | ---: | ---: | ---: | ---: |
| Clean snapshot check | 633 | 549 | 542 | 637 |
| Cursor feed and snapshot | 6,204 | 6,152 | 6,258 | 6,139 |
| ASCII one-row feed and snapshot | 11,488 | 11,832 | 11,588 | 11,606 |
| ASCII full feed and snapshot | 224,776 | 223,732 | 220,590 | 230,810 |
| Unicode full feed and snapshot | 467,261 | 471,692 | 468,023 | 469,786 |

The clean check improves consistently by 13-15% across the baseline/candidate/
candidate/baseline captures. The baseline ranges are 630-648 and 633-646 ns;
candidate ranges are 543-560 and 537-574 ns. Other workload ranges overlap and
do not establish an improvement. Source inspection establishes
three fewer allocation requests when those values are unchanged. Idle Terminal
Sessions do not continuously request snapshots, so this does not imply an idle
CPU reduction. An independent terminal research agent found no material cache
invalidation or benchmark issue. All 145 emulator tests passed through
`mise run test:one terminal::emulator::tests`.

## Glyph color lookup

The accepted candidate uses linear lookup for at most eight color runs and binary
lookup above eight. A constant transparent fallback removes a conversion whose
generated code prevented helper inlining. It preserves byte-index semantics,
including nonmonotonic shaped glyph order. The behavior fixture covers real
prepared fragments with three and ten runs, Unicode boundaries, reversed index
order, and empty/out-of-range fallback.

`mise run bench:one performance_glyph_colors` alternates candidate and original
lookup in the same optimized executable, reversing their order across seven
samples. Each sample resolves 120 glyphs for 20,000 rows. It measures only color
lookup, excluding shaping, painting, GPU execution, and complete frame costs.

| Color runs | Original median us/row | Candidate median us/row |
| --- | ---: | ---: |
| 1 | 0.138 | 0.127 |
| 4 | 0.125 | 0.124 |
| 8 | 0.165 | 0.182 |
| 16 | 0.358 | 0.297 |
| 120 | 2.526 | 0.549 |

The fragmented 120-run case improves about 78%, or 4.6 times. Its original range
is 2.506-2.547 us and candidate range is 0.547-0.555 us. Eight-run rows cost about
0.017 us more, a 10% lookup regression in this very small operation. This is an
explicit tradeoff, not a claim that every row becomes faster. Native application
comparisons below do not establish a general resource improvement. The first two candidates regressed common short
rows materially and were revised; their logs remain local experiment evidence.

## Native font enumeration

Ten fresh processes per mode compared the current descriptor walk with two native
queries. Every mode returned the same 260 family names on this host. The direct
visible-family query took about 13 ms versus about 119 ms for the descriptor walk,
but it can exclude hidden fonts on supported macOS versions. It is rejected as
a complete catalog replacement despite local equality.

The bulk family-attribute query initially measured 21.015-24.546 ms with a
collection that omitted the old descriptor duplicate-filter option. Exact set
equality passed on this host. The real production candidate preserved that option
and reversed the result: two ten-process baseline batches measured 116.111 and
116.023 ms median, while candidate batches measured 127.389 and 128.075 ms.
Baseline ranges were 115.131-120.812 and 114.895-118.462 ms; candidate ranges were
126.335-131.091 and 126.211-130.256 ms.

The bulk production change and its native test were removed. The faster variant
without descriptor filtering lacks a documented guarantee that duplicate removal
cannot affect family attributes for custom/registered fonts. The direct visible
list is also rejected. The independent comparison fixture remains available for
future investigation. Neither a launch improvement nor a font behavior change is
part of the accepted work.

The discarded native test passed on retry. Its first task invocation returned
status 2 because the artifact supervisor could not measure a changing target
directory, even though its child test later passed. The retry completed normally.

## Validation

`mise run validate` completed successfully. It passed formatting, all-target
checks, 3,114 application/workspace tests, seven GPUI scene tests, five native
backdrop pixel tests, the alternate Blade renderer check, Rust/script/diff lint,
both owned Ghostty feature-matrix test runs, and dependency formatting.
The seven ignored application tests are explicit/manual fixtures, including the
optimized benchmarks executed separately for this work.

Earlier attempts found one test-fixture formatting issue and one Clippy style
issue, both corrected. A GPUI check was interrupted by the artifact supervisor's
directory-measurement race; its retry and the complete final gate passed. These
failures were not hidden or treated as passing gates.

The independent source review found no material issue in the retained changes.
No production font or vendored GPUI modification remains. Raw logs are in
`target/performance/round2-*.log`. The successful complete gate is
`round2-validation-passed.log`.

## Complete application comparison

The two production changes are retained for their repeatable isolated CPU-work
reductions and passing behavior checks. Complete application captures do not
establish a general CPU or RAM improvement. Some scrolling comparisons show
higher candidate footprint. This remains unresolved; the changes must not be
reported as a measured application memory saving or a universal resource win.

Both binaries were built from source with only the two production changes
differing. The baseline uses the original files at `1e25ad9`; the exact validated
candidate files were restored after each comparison build. Native captures run
without concurrent builds. Host: macOS 27.0, Mac16,8, M4 Pro, 24 GiB RAM.

Each batch uses four fresh processes in ABBA order, fresh isolated settings,
five seconds of warmup, and ten seconds of capture at 0.5-second intervals.
Producer cost is measured separately. Each scrolling process emits 1,200 updates,
9,640 lines and 1,050,487 bytes over approximately 20 seconds; each idle process
emits 40 lines and 4,087 bytes. Equal production does not establish final rendered
pixel equality; behavior and rendering tests provide separate correctness evidence.

Values below are the two trial means for each binary. CPU uses one-core percent;
footprint uses MiB. Rows with different window bounds are separate cases.

| Case and window bounds | Original CPU % | Combined CPU % | Original footprint MiB | Combined footprint MiB |
| --- | --- | --- | --- | --- |
| Idle, 1472 x 937 | 1.000, 0.957 | 0.977, 1.020 | 121.254, 124.782 | 124.977, 114.557 |
| Scroll, 1472 x 937 | 34.116, 34.259 | 34.041, 34.014 | 226.765, 229.942 | 230.924, 230.807 |
| Scroll, 900 x 580 | 25.408, 24.251 | 23.991, 23.533 | 172.010, 169.946 | 177.531, 176.248 |
| Scroll, 900 x 580, exchanged application identities | 26.831, 25.250 | 22.308, 25.432 | 174.491, 175.521 | 177.035, 175.477 |

Idle CPU and footprint ranges overlap. Idle interrupt wakeups remain roughly
120.6-121.0 per second. The small-window scrolling result suggests less CPU work
but higher footprint; exchanging the harness's application identities produces
overlapping ranges and does not settle attribution. Resident-memory ranges also
overlap in the exchanged batch. Do not pool these cases or select only favorable
trials. A repeated controlled allocation/residency profile is needed to resolve
the footprint observation.

Separate palette-only and glyph-only builds check whether one change clearly
accounts for the footprint difference. This comparison is between variants, not
a measurement of either variant's improvement over the original. Source review
found no added heap owner, identity-sensitive invalidation, or backreference that
explains the MiB-scale footprint difference. Absence of an identified ownership
issue does not disprove a resource regression.

A repeated batch changed from 1472 x 937 to 900 x 580 points between trials. It
is excluded from aggregate comparisons. The harness previously checked geometry
only within a trial. It now retains the first valid window observation and
rejects any later focused trial with different bounds before starting samplers.
Replaying the recorded observations rejects the two mismatched captures and
accepts all four observations from the original fixed-size batch. Native script
lint and independent cleanup/reference review pass.

Hidden scrolling failed before capture because macOS refused the harness's hide
request. That trial establishes no hidden-resource result. Unfocused-visible,
inactive-Tab, native GPU, exact grid/refresh, and normal-shell launch remain
unmeasured.

## Isolated variants and binary provenance

The final ABBA variant capture completed with all windows at 900 x 580 points.
Both variants emit the same scrolling workload stated above.

| Variant | CPU % | Footprint MiB | Resident MiB |
| --- | --- | --- | --- |
| Palette only | 23.895, 24.743 | 176.311, 176.991 | 113.774, 115.467 |
| Glyph lookup only | 24.112, 24.411 | 177.561, 176.634 | 115.751, 115.692 |

CPU and footprint ranges overlap between variants. This does not isolate a
specific change as the cause of the earlier footprint difference. Both retained
optimizations have operation-level evidence; application memory acceptance
remains a profiling limitation, not a demonstrated improvement.

Local binary SHA-256 values:

| Binary | SHA-256 |
| --- | --- |
| Original baseline | `f9b6f72e316cc359f88dc83e55ff2c9988c74582a7b6106e981912bb9e8472db` |
| Combined candidate | `1ff80256b1ed5a4506c3e3480c1d3cf10c4d4576a5a2ab273b6ef88af24ff923` |
| Palette only | `3f1a0e91a09ef41539e802dcdb51ad42dc2015bd71212f17102b450e48e35bc5` |
| Glyph lookup only | `b826a5d7e06d01dd53ef690062d5f00ab5fa8ad76c4720b219c0547b0974bb5f` |

Raw native logs are `target/performance/round2-application-*.jsonl` with matching
stderr `.log` files. The `scroll-swapped` header labels are deliberately inverted:
its `baseline` role runs the combined candidate and its `candidate` role runs the
original. The `scroll-isolated` baseline role is palette-only; candidate is
glyph-only. The results tables above attribute values to actual code variants.
`scroll-repeat` is excluded for mixed geometry; `hidden` failed before capture.
The original and combined binaries and the two isolated variants remain under
`target/performance/spaceterm-round2-*`. Generated binaries and raw logs are
local artifacts; this committed record retains the findings and limitations.
