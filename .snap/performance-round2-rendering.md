# Rendering follow-up research

Date: 2026-09-26. Local source inspected: `1e25ad960d27fd7f526d083901c4e6ae10a21a60`.
This extends [the rendering inventory](performance-research-rendering.md).
The unchanged-row geometry improvement is already implemented and measured;
it is not a remaining candidate. This report contains code evidence and a
measurement plan. It makes no new application CPU, GPU, or energy claim.

## Recommendation

Measure and replace repeated color-run scans during glyph painting first. This
removes work inside an existing rendering operation without adding retained
state, changing frame scheduling, or modifying the renderer. Native frame-source
profiling remains the larger potential idle improvement, but its correctness
surface is substantially larger.

## States that must remain distinct

| State | Existing contract | Safe optimization boundary |
| --- | --- | --- |
| Focused visible Operating-System Window | Active Panes display output and permitted cursor/text animations | Remove repeated preparation and paint lookup work; retain frame pacing and latency |
| Unfocused but visible Operating-System Window | Output still displays; Terminal Input Focus ends; terminal surface animations stop under current policy | Never make presentation depend on application or key-window focus; measure separately from hidden state |
| Minimized or fully occluded Operating-System Window | Presentation stops; Terminal Sessions keep consuming output; latest state is presented on restoration | Suppress presentation work and reclaim reconstructible resources while retaining session state and accessibility obligations |
| Inactive Tab, inactive Workspace, or Pane excluded by zoom | Pane leaves the render tree and immediately evicts presentation resources | Preserve PTY processing, terminal replies, metadata, history, and exact restoration; do not wait for another render to clean up |

Owners: `src/ui/render_lifecycle.rs:15` (`presentable`) and `:19`
(`animations_active`); `src/ui/terminal_pane.rs:815` (`set_product_focus`),
`:1872` (`update_runtime_visibility`), and `:1910`
(`evict_presentation_resources`). Those locations refer to the inspected baseline
and can move as the follow-up is implemented.

## Ranked candidates

### 1. Resolve a glyph's paint color without restarting a linear scan

At baseline `src/ui/terminal_element.rs:876`, every non-emoji glyph scans
`PreparedText.paint_runs` from its beginning. `FragmentBuilder::push` at `:1883`
appends UTF-8 text and coalesces adjacent identical colors, so run ends are
strictly increasing. Shaping and color runs deliberately differ: selection and
Terminal Find change paint colors without changing glyph positioning.

For 120 ASCII cells with a different color per cell, the existing lookup performs
7,260 end comparisons per fragment paint. The same fragment can be painted again
for cursor clipping or recoloring. Replacing the scan with
`partition_point(|run| run.end <= glyph_index)` and then `get` preserves the first
run whose exclusive end exceeds the glyph index, including the transparent
fallback when none exists. It requires no assumption about the order of shaped
glyph indices. A forward-only iterator would require that extra assumption and
is not the proposed implementation. The macOS shaper explicitly handles backward
glyph indices in `third_party/gpui/src/platform/mac/text_system.rs:526`.

Measure 1, 4, and 120 runs, since binary lookup may cost more for the common
single-color case. The fixture must exercise the actual lookup used by painting,
with prepared text produced through the real row owner. Measure seven optimized
samples with constant 120-column input and retain both medians and individual
samples. A helper benchmark is CPU lookup evidence, not a frame-rate claim.

Parity checks cover UTF-8 byte boundaries, repeated and decreasing glyph indices,
the last run, absent runs, and a missing match. Existing owner tests cover
selection and Find color precedence, font runs, combining text, emoji, fractional
cell widths, and block-cursor clipping/recoloring. No text, glyph, origin, font,
or scene submission change is authorized by this candidate.

Implementation fixture: `performance_glyph_colors` in
`src/ui/terminal_element.rs`. Run `mise run bench:one performance_glyph_colors`.
The baseline extraction retained the linear lookup in `PreparedText::color_at`.
Run the owner tests through
`mise run test:one terminal_element::tests` after the change.

The first benchmark measured the existing lookup at 0.114575, 0.124738, and
2.335492 microseconds per 120-glyph row for 1, 4, and 120 runs. Initial binary
candidates improved 120-run rows but regressed common short run lists, even with
a small-run linear path and an inline hint. Those candidates were rejected.
A same-binary control alternates the candidate and the original lookup, reversing
their order between seven samples; it includes 1, 4, 8, 16, and 120 runs.

Inspection of the optimized arm64 fixture with `nm` and `otool` established that
the benchmark still called `PreparedText::color_at` for each glyph. That helper
included a large stack prologue and the full `rgba(0).into()` fallback conversion
because `rgba` is an external crate call. The next candidate uses the existing
`Hsla::transparent_black()` constant for the exact same fallback, shrinking the
helper. `third_party/gpui/src/color.rs:356` and `:589` establish the equivalence.
The parity test verifies fallback output against the old conversion, actual row
preparation with three and ten color runs, non-ASCII byte boundaries, and
reordered indices. This compiler observation explains the experiment; it does
not replace measured acceptance.

The final candidate keeps linear lookup through eight runs because the measured
crossover is between eight and sixteen in this fixture. The final same-binary
comparison used 20,000 rows of 120 glyph lookups per sample, seven samples for
each implementation, an optimized macOS arm64 test binary, and fixed prepared
text from the GPUI test text system. The fixture excludes shaping, scene
submission, GPU execution, and complete frame cost.

| Color runs | Original median, microseconds/row | Final median, microseconds/row | Absolute change |
| --- | ---: | ---: | ---: |
| 1 | 0.137777 | 0.126977 | 0.010800 microseconds lower; sample ranges overlap |
| 4 | 0.124675 | 0.123519 | 0.001156 microseconds lower; sample ranges overlap |
| 8 | 0.165254 | 0.181831 | 0.016577 microseconds higher |
| 16 | 0.357538 | 0.296890 | 0.060648 microseconds lower |
| 120 | 2.525910 | 0.548890 | 1.977021 microseconds lower, about 4.6 times faster |

The eight-run overhead is about 17 nanoseconds per 120-glyph row. The final
candidate does not make every isolated workload faster. The large-run result is
a glyph-color lookup improvement and must not be reported as an application CPU
or GPU speedup. The one- and four-run overlapping ranges establish no material
change in those cases in this experiment. Correctness and broader application
checks remain separate gates owned by the coordinating agent.

Logs: `target/performance/round2-glyph-baseline.log`,
`target/performance/round2-glyph-candidate.log`, and
`target/performance/round2-glyph-controlled.log`, and
`target/performance/round2-glyph-constant.log`, and
`target/performance/round2-glyph-final.log`. Each log retains individual sample
values. The final optimized fixture passed on 2026-09-26.

### 2. Schedule discrete spinner frames at their actual deadlines

`crates/spaceterm-ui/src/progress.rs:430` uses GPUI's continuous animation wrapper
for ten frames at 80 ms each. `third_party/gpui/src/window.rs:1766` requests an
entity notification at the next display frame. A spinner displayed at 120 Hz can
request about 9.6 redraws per artwork change. Production call sites include
`src/ui/terminal_status.rs:357` and
`crates/spaceterm-ui/src/command_palette.rs:3460`.

Use an element-owned deadline timer with the same original phase and frame
sequence, if profiling shows these indicators are present often enough. Preserve
Reduced Motion, phase on redraw, and removal cancellation. Do not slow the
continuous indeterminate progress bar: its artwork changes each display frame.
Do not silently freeze a spinner in an unfocused visible window. Its current
component has no focus gate, so that would change the existing appearance.

Measure application draw/present counts, CPU, and timer wakes with 1 and 16
spinners, 60 and 120 Hz, and repeated dismissal/reappearance. A timer that keeps
running after the element disappears is a regression. This remains unimplemented.

### 3. Stop clean-window display ticks through a complete wake protocol

`third_party/gpui/src/platform/mac/window.rs:476` starts the display link for
nonoccluded windows. The callback at `third_party/gpui/src/window.rs:1032` can skip
drawing but still enters the window and completes the frame. The previous valid
idle trials measured approximately 120.6 interrupt wakeups/s, but display refresh
was not recorded and interrupt wakeups do not identify their source. See
[the application measurements](performance-application-results.md).

Instrument callbacks, main-thread wakes, scene rebuilds, and Metal presents before
changing this framework behavior. A valid demand-driven source must wake for
entity invalidation, terminal output, input, animation deadlines, pending
`on_next_frame` callbacks, resize, display changes, and visibility restoration.
GPUI presents for one second after input to resist display underclocking
(`third_party/gpui/src/window.rs:1051`); retain this behavior until latency
measurements justify changing it. Disabling ticks solely because a scene is clean
can strand the callbacks that make it dirty.

This is a potentially material foreground and unfocused-visible idle improvement,
but not a low-risk patch. No current upstream drop-in fix was established.

### 4. Allocate unused Metal path targets lazily, then measure hidden residency

`third_party/gpui/src/platform/mac/metal_renderer.rs:304` allocates path
intermediate textures on every drawable resize, even without a path batch.
`:322` allocates one BGRA8 target plus a 4-sample target. Their nominal combined
size is 20 bytes/device pixel, about 99 MiB for 2880 x 1800 pixels; actual resource
residency and driver allocation must be measured. `:582` uses them for paths.

First investigate allocation on the first path batch and size-aware reuse. Keep
the sample count and all blend/clip behavior. Measure ordinary terminal windows,
progress rings, first path appearance, resize, scale changes, and transparency.
Record Metal resources separately from process footprint. Texture allocation
failure and zero dimensions must retain existing handling. A second experiment
could release reconstructible window GPU resources during sustained occlusion,
but must account for in-flight commands and restoration latency. Pane cache
eviction alone does not establish that window-level Metal resources were freed.

## Refreshed primary-source evidence

Sources were inspected on 2026-09-26; links pin the revisions retrieved from
each repository's HEAD rather than relying on moving branch URLs.

- [Ghostty renderer, `c959af63`](https://github.com/ghostty-org/ghostty/blob/c959af63d11b524a84c21900372990dbc024b059/src/renderer/generic.zig#L1153-L1255):
  visibility updates resynchronize the display link. Vsync runs while visible
  with rebuilt cells or an animation wake. Occlusion releases the swap chain
  after synchronizing rendering, and the next draw recreates it. Images remain
  retained to avoid upload work on every restoration. The portable lesson is to
  distinguish replaceable presentation resources from retained terminal content.
- [Ghostty renderer thread, `c959af63`](https://github.com/ghostty-org/ghostty/blob/c959af63d11b524a84c21900372990dbc024b059/src/renderer/Thread.zig#L259-L281):
  macOS QoS distinguishes focused visible, unfocused visible, and occluded
  surfaces. Its hidden render callback skips rebuilding cells and catches up on
  visibility return. These policies support SpaceTerm's existing distinction
  between focus and presentation. They do not justify pausing a hidden Terminal
  Session or lowering the entire GPUI application thread's QoS.
- [Zed macOS window, `e91b82c1`](https://github.com/zed-industries/zed/blob/e91b82c106817f2419207ebf81f1da766698ac95/crates/gpui_macos/src/window.rs#L834-L860):
  `start_display_link` still checks native occlusion before starting a
  `WindowFrameSource`; `stop_display_link` stops that source. This ownership is
  newer than SpaceTerm's vendored GPUI, but this code does not establish that a
  clean visible window sleeps. An upstream update requires measuring and
  preserving SpaceTerm's local rendering patches.

## Native measurement gate

Use the source build and `.mise.toml` tasks. Record display scale and refresh,
exact terminal geometry, Settings, output bytes and rate, frontmost status, and
occlusion before and after every accepted capture. Use paired ABBA trials and
separate producer costs. The existing hidden-state trials failed their native
visibility checks, so repair that harness before claiming hidden-state gains.

Capture focused idle, unfocused visible idle, focused output, unfocused visible
output, hidden output, and output in an inactive Tab with 1/4/16 Panes. Measure
CPU, wakeups, process footprint, GPU time, Metal resource residency, and time to
first restored frame. Verify final terminal text, metadata and replies after
restoration. Add graphics, selection, Find, accessibility demand, and resize to
the parity scenarios when the candidate can affect their owners.
