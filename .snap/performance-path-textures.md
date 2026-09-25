# Lazy Metal path targets

Status: implemented; native pixels, allocation accounting, and full validation pass, 2026-09-26.

## Evidence

An optimized probe build observed three eager path-target allocation sets during
ordinary terminal startup and zero scenes containing paths. The same nine-second
idle capture contained 1,080 completely clean frame callbacks, no draws/presents.
Unmapped graphics-ledger categories reach roughly 100 MiB at 1472 x 937 points.
The four-sample full-window path target and resolved target are allocated on every
drawable resize in the old implementation, even for scenes with no paths.

## Candidate

Drawable resize invalidates mismatched existing path textures without allocating
replacements. The real path rasterization operation allocates the pair at first
use for its viewport and reuses it while dimensions match. The texture format,
sample count, pipelines, clear/resolve operations, clipping and drawing are
unchanged. Zero dimensions still release targets and avoid invalid native
allocations. Commands retain in-flight native resources under the existing Metal
ownership contract; allocation moves within the existing command-encoding path.

## Validation

`test:gpui:path-textures:macos` first failed because resize allocated unused
targets. An initial pixel fixture used an incorrectly inferred integer vector;
that fixture was corrected to bytes before accepting the red gate. The corrected
red run had one intended allocation failure and passing original path pixels.
Both tests pass after lazy allocation: no unused targets across resize/zero-size
transitions; real Metal translucent path pixels and 4x MSAA after first use and
resizing; same-size target reuse. The fractional-edge coverage rerun also passes
(`continuation-path-textures-fractional.log`). Independent allocation and native
command-buffer ownership review found no material defect. Full renderer validation and optimized native resource comparisons now pass.
See performance-continuation-results.md for the final native state captures.

The reference binary with counters and eager targets is retained at
`target/performance/spaceterm-continuation-probes-before`, SHA-256
`afabcc69bc2fe524ba11751e7b074348dda157adf29d6910f35e6b7d5caad7dc`.
No global performance completion is inferred from this candidate.

The isolated lazy-target probe build is retained at
`target/performance/spaceterm-continuation-lazy-paths`, SHA-256
`140afe7cfd325bbd2e7c2efb6aa8e7684a33e212e713e464f6c1f74d0c300b6a`.
It includes no demand-gating changes. The optimized build completed successfully.

## Isolated scrolling comparison

`continuation-lazy-paths-scroll.jsonl` contains an accepted ABBA comparison with
equal 113 x 42 grids, 1,200 updates and 1,050,487 bytes per trial. Cumulative
path-target allocation sets fall from three to zero, with zero path scenes for
both binaries. Active footprint remains approximately 229-233 MiB for both.
This does not establish a large physical-memory reduction. The earlier roughly
96 MiB graphics-ledger transition also occurs with zero path targets, so it
cannot be attributed to these unused targets. Allocation avoidance is confirmed;
startup and first-path latency evidence still determines final acceptance.

The first hidden trial verifies hidden state and zero frame callbacks. Its
paired candidate was rejected by the pre-hide geometry guard. AeroSpace is
running on this host. The rejected pair is not a performance comparison.

## Final native allocation measurement

`mise run bench:gpui:path-targets:macos` exits 0. Log:
`target/performance/continuation-path-target-resources.log`. Four alternating
repetitions each perform 60 drawable-size updates among 1800 x 1160 and
2944 x 1874 device pixels, with no path drawing. The eager control allocates
path targets after resize and reuses matching dimensions; this is conservative
relative to the old implementation, which also reallocated matching sizes.

Median component cost per resize is 24.144 microseconds with eager allocation
and 0.207 microseconds with lazy allocation. Peak path `allocated_size` is
112,852,992 bytes for eager allocation and zero for lazy allocation. Device
`current_allocated_size` is 113,459,200 versus 606,208 bytes. These are Metal
allocation-accounting values. They are not physical resident memory, complete
application resize latency, or a first-path performance measurement. The fixture
does not acquire drawables, submit frames, or write path pixels.

The application comparison confirms zero unused path allocations and no physical
footprint improvement under the measured workload. Real first-use and resized
path pixels, transparency, fractional MSAA edges, and same-size reuse pass the
native correctness fixture. This change moves the same necessary allocation to
first path use; it does not reduce required allocations for a path-containing scene.
