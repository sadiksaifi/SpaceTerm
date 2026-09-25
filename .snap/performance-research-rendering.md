# Rendering performance research

Status: research inventory, 2026-09-25. Source baseline:
`134d7027b2014f89d29d0f9c4d087e894f33cfbc`. No implementation or runtime
measurement is claimed here. This record covers GPUI scene work, animations,
visibility, GPU resources, and accessibility. The [measurement record](performance-measurements.md)
still has no runtime baseline.

The first geometry candidate was subsequently implemented and measured in
[rendering results](performance-rendering-results.md). The inventory and
unmeasured claims below remain the pre-implementation assessment.

## Current behavior to preserve

- A visible Pane continues presenting output when its Operating-System Window is
  unfocused. [`SurfaceVisibility::presentable`](../src/ui/render_lifecycle.rs)
  excludes minimization, occlusion, inactive Workspace, and hidden Pane, while
  `animations_active` additionally requires an active application and key window.
  Focus must not become a condition for receiving or displaying terminal output.
- Hidden Panes continue accepting Terminal Session screens and accessibility
  updates, coalesce to the latest generation, and draw when restored.
  [`RenderLifecycle`](../src/ui/render_lifecycle.rs) and the
  [visibility test](../src/ui/terminal_pane/tests.rs) exercise this path.
  [`TerminalPane::set_product_focus`](../src/ui/terminal_pane.rs) evicts presentation
  resources when a Pane leaves the render tree. The native visibility source is
  scoped to the exact window in
  [`macos_render_lifecycle.rs`](../src/platform/macos_render_lifecycle.rs).
- The grid already reuses prepared row inputs, shaped text, geometry, and symbol
  plans; retains only visible row text; and caches a GPUI grid scene during eligible
  opaque block-cursor blinking. See
  [`TerminalGridCache`](../src/ui/terminal_element.rs),
  [`TerminalGridPresentation`](../src/ui/terminal_element/presentation.rs), and
  [`PreparedGraphics`](../src/ui/terminal_graphics.rs). Replacing these with a
  separate renderer would need evidence that the current seam is the bottleneck.
- The Terminal Accessibility model retains row identities and revisions, and the
  native element avoids cloning a shared snapshot. Hidden accessibility
  notifications are retained for restoration. See
  [`accessibility.rs`](../src/terminal/accessibility.rs),
  [`macos_accessibility.rs`](../src/platform/macos_accessibility.rs), and
  [`TerminalPane::update_runtime_visibility`](../src/ui/terminal_pane.rs).

## Candidate inventory

Priority indicates expected value and implementation risk, not a measured speedup.

| Priority | Evidence and present cost | Potential solution | Proof and preservation conditions |
| --- | --- | --- | --- |
| 1, low risk | [`prepare_visible_geometry`](../src/ui/terminal_element.rs) takes and rebuilds `prepared_text`, allocates `previous_text`, `prepared_text`, and `prepared_rows`, calls `find_row_alignment`, and tests row shape even if the same visible `Arc<RowPaintInput>` values and layout recur. The preceding `prepare` already has a pointer identity fast path. This repeats on a GPUI grid draw, including unrelated window invalidation. | Add a same source identity and layout fast path for stable visible geometry. Retain the prepared row vector or slice, and use pointer identity before deep `row_text_shape_eq` for individual rows. Keep scroll alignment for shifted rows. | Count calls, allocation bytes, row comparisons, and shape calls during idle, cursor blinking, unrelated chrome updates, one changed row, scroll, resize, font/color changes. Require byte-identical rendered output and existing row cache, find, cursor, and reflow tests. A `Vec` reuse alone may reduce allocation without reducing the O(rows) scan; measure both. |
| 2, low to medium risk | [`FrameSpinner`](../crates/spaceterm-ui/src/progress.rs) has 10 discrete frames at 80 ms each, but GPUI's [`AnimationElement`](../third_party/gpui/src/elements/animation.rs) calls `request_animation_frame` on every layout. That schedules entity notification on the next display frame in [`Window::request_animation_frame`](../third_party/gpui/src/window.rs). A 60/120 Hz display can therefore rebuild many frames whose spinner index is unchanged. | Schedule one wake at the next 80 ms frame boundary while the spinner is present, preserving the current `Instant` phase and 10-frame artwork. Keep reduced motion static. Scope timer ownership to the indicator so removal cancels wakes. | Compare request, draw, and present counts for one and many spinners at 60/120 Hz, including loading completion, reappearance, and reduced motion. Verify frame index and phase at sampled times, no stale timer after dismissal, and no changed visible timing. A continuous indeterminate bar at [`bar_fill`](../crates/spaceterm-ui/src/progress.rs) changes position every display frame and must keep its smooth animation. |
| 3, medium risk | GPUI macOS starts a [`CVDisplayLink`](../third_party/gpui/src/platform/mac/window.rs) for every nonoccluded window. Each tick reaches `Window`'s frame callback, which skips drawing when clean but still completes the frame. The clean tick path can cause wakeups without useful CPU/GPU work. Occluded windows already stop the link. | Investigate an on-demand frame source that sleeps when the scene is clean and no animation or presentation callback is pending, and wakes for invalidation, input, animation deadline, visibility restoration, and AppKit display requests. | Instrument display-link callbacks, main-thread wakeups, draw calls, and Metal presents in idle visible, unfocused visible, active output, cursor blink, resize, and restoration. This is a GPUI framework change with lost-wakeup and input-latency risk; do not stop the display link until every invalidation source has a reliable wake route. GPUI deliberately presents for one second after input to avoid display underclocking in [`window.rs`](../third_party/gpui/src/window.rs); retain or measure that policy. |
| 4, medium risk | Vendored GPUI allocates full drawable-size path resolve and 4-sample MSAA textures at [`update_drawable_size`](../third_party/gpui/src/platform/mac/metal_renderer.rs), even before a path batch. The textures use about 20 bytes per device pixel combined, or about 99 MiB for a 2880 x 1800 drawable, excluding the layer drawables and other allocations. Path rendering is used by progress rings and may appear later. | Allocate path textures on first actual path batch, retain and resize only when needed, and release on window closure. Consider reclaiming path textures after sustained path-free activity only if GPU residency measurements justify churn. | Record Metal resource residency, allocations, first path render latency, and GPU frame time for ordinary terminal windows, a progress ring, resize, and multiple windows. Keep 4x MSAA and compare edge pixels, color, alpha, transparency, and appearance. Merely lowering sample count changes appearance and is outside scope. |
| 5, medium to high risk | Every dirty GPUI draw submits a full-window scene to Metal: [`Window::present`](../third_party/gpui/src/window.rs) calls the renderer, and [`draw_primitives`](../third_party/gpui/src/platform/mac/metal_renderer.rs) clears the drawable and visits scene batches. The app's cached cursor layer prevents grid preparation in one common blink case, but Metal still receives a complete scene. The effect on GPU time is unmeasured. | Profile primitive counts and GPU passes for a one-cell edit, cursor blink, scroll, transparent background, and graphics. Explore retained scene or damage rendering only if full-scene work dominates. | Use Xcode Metal System Trace / GPU counters and identical screenshots. A partial renderer must handle transparent compositing, backdrop filters, selection, images, cursor restoration, and overlap correctly. It is a larger design change, not an assumed win. |
| 6, medium risk | The path pass clears the full intermediate texture and builds a new vertex `Vec` per path batch in [`draw_paths_to_intermediate`](../third_party/gpui/src/platform/mac/metal_renderer.rs). This may cost CPU and GPU bandwidth for progress rings or other paths. The magnitude and frequency are unknown. | Reuse scratch vertex storage and restrict path pass work to correct batch bounds if profiling finds it material. | Measure allocations and GPU bandwidth with path-heavy controls at fixed geometry. Keep antialiasing and clipping pixels equal; avoid texture load actions that expose stale pixels. |
| 7, contingent | [`TerminalGridElement::prepaint`](../src/ui/terminal_element.rs) constructs `PreparedFrameRow` entries for every visible row on each grid draw, plus find and hyperlink overlays; [`paint_terminal_text`](../src/ui/terminal_element.rs) linearly finds the color run for each glyph. The stable row caches do not eliminate all per-frame assembly or run searches. | Profile candidate assembly and glyph submission after the first cache improvement. If hot, cache immutable row-level paint segments keyed to source and overlay state, or advance through sorted paint runs while iterating glyphs. | Require mixed font/color, combining glyph, emoji, wide cell, blink, selection, find, hyperlink, preedit, and block-cursor correctness. Do not add a larger retained cache before measuring memory growth with 1/4/16 Panes. |
| 8, contingent | Native accessibility and graphics hold independent retained models. The accessibility model updates by row revision and the graphics paint plan already caches geometry. Cost may appear only for large Scrollback, graphics churn, or many Panes. | Measure retained owners, native accessibility update work, image GPU residency, hide/restore and replacement. Make ownership or eviction changes at these seams only after identifying retained bytes and lifetime. | Preserve accessible text, selection, notification order, graphics animation and immediate restore. Do not make accessibility opt-in or discard hidden Terminal Session output. |

## Upstream comparison

The vendored framework is the published GPUI 0.2.2 crate with two local patches
documented in [`SPACETERM-PATCHES.md`](../third_party/gpui/SPACETERM-PATCHES.md).
Current Zed at revision
[`e52ab15eac51e5644da6a3c9e1fac9bb2330b000`](https://github.com/zed-industries/zed/tree/e52ab15eac51e5644da6a3c9e1fac9bb2330b000)
still gates its macOS frame source by NSWindow occlusion in
[`gpui_macos/src/window.rs`](https://github.com/zed-industries/zed/blob/e52ab15eac51e5644da6a3c9e1fac9bb2330b000/crates/gpui_macos/src/window.rs).
It has moved frame-source ownership into a reusable `WindowFrameSource`, but the
source still calls the frame callback on display ticks while running. This supports
investigating frame scheduling; it does not establish a ready upstream on-demand fix.

Ghostty at revision
[`56dbc4a768778753737a3b9cbe0a3f9b4e434553`](https://github.com/ghostty-org/ghostty/tree/56dbc4a768778753737a3b9cbe0a3f9b4e434553)
uses a different renderer/thread architecture. Its
[`generic.zig`](https://github.com/ghostty-org/ghostty/blob/56dbc4a768778753737a3b9cbe0a3f9b4e434553/src/renderer/generic.zig)
starts vsync only while visible with changed cells or an animation wake, and releases
the swap chain when hidden while retaining images to avoid reupload. Its
[`Thread.zig`](https://github.com/ghostty-org/ghostty/blob/56dbc4a768778753737a3b9cbe0a3f9b4e434553/src/renderer/Thread.zig)
skips hidden rendering, rebuilds on visibility return, and uses a lower QoS when
occluded. These are evidence that demand-driven frames and GPU resource release
can work in a terminal. Their implementation cannot be copied into GPUI without
accounting for GPUI invalidation, scene composition, and window lifetimes.

## Measurements before implementation

Use the existing [`docs/performance.md`](../docs/performance.md) protocol and
[`performance-measurements.md`](performance-measurements.md). First record optimized
source-build baselines for a visible idle Pane, unfocused visible Pane, hidden
window, 1/4/16 Panes, partial output, scrolling, spinner, indeterminate bar,
graphics, resize, and accessibility activity. Capture CPU samples, wakeups,
process footprint, GPUI draw/present counts, and native GPU time and residency.
Process footprint cannot stand in for GPU residency. Equal output volume,
appearance, settings, geometry, display refresh, and screen scale are required.

The first implementation candidate should be the unchanged-row geometry fast
path after its call counts are measured. The discrete spinner timer is next if
its indicator is present often enough to matter. GPU texture allocation and
display-link changes need native evidence and visual regression checks before
their risk is justified.
