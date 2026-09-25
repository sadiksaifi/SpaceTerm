# SpaceTerm GPUI patch

Source: the published `gpui` 0.2.2 crate. The upstream Apache 2.0 license is retained.

The macOS renderer used additive destination alpha in the main and path-sprite
pipelines while using source-over RGB. Translucent surfaces therefore accumulated
coverage incorrectly, darkening Light surfaces and creating edge artifacts.
Both pipelines now use `OneMinusSourceAlpha` for destination alpha, matching the
path-rasterization pipeline and premultiplied source-over composition.

Path resolve and multisample textures are allocated when a scene first draws a
path, rather than on every drawable resize. Equal-size targets are reused;
resize invalidates targets with the old dimensions. Path-free scenes therefore
avoid these allocations. Texture formats, four-sample coverage, blending, and
path rendering remain unchanged. Native tests verify translucent pixels,
fractional-edge coverage, reuse, and first-path rendering after resize. Allocation
savings do not imply an equivalent reduction in process physical footprint.

The macOS window exposes its stored traffic-light position for live updates through
`PlatformWindow::set_traffic_light_position` and `Window::set_traffic_light_position`.
The setter accepts a concrete position because SpaceTerm's host geometry is immutable for a
window's lifetime; windows without host geometry leave AppKit's default untouched. SpaceTerm
titlebar height follows Chrome density, so the native buttons must move when
density changes without reopening the window. Other platforms keep the default no-op.

The macOS display-link adapter shares one retained native link per observed display
identifier and owns cancellable per-window dispatch sources. This adapts Zed's Apache-2.0
implementation at
[`e91b82c106817f2419207ebf81f1da766698ac95`](https://github.com/zed-industries/zed/blob/e91b82c106817f2419207ebf81f1da766698ac95/crates/gpui_macos/src/display_link.rs).
The original wrapper
retained a new native link on every restart to avoid a CoreVideo teardown crash.
Shared ownership bounds that retention and prevents late callbacks from reaching
released window sources. Native fixtures exercise shared-window subscriptions,
start, stop, restart, and cancellation before queued callback delivery.

macOS windows request display frames only while visible work remains. Entity
invalidation, refresh, frame callbacks, direct draws, and input wake the source.
The existing one-second active-input presentation grace is retained. Hidden work
remains pending until restoration; unfocused visible windows continue rendering.
The native source remains allocated across idle and hidden periods, and retargets
on display changes. Completion preserves requests queued during a frame.

The first successful subscription signals the existing main-queue dispatch
source asynchronously, avoiding CoreVideo restart-phase delay. Repeated starts
do not signal it again; subsequent frames remain display paced. Synchronous
AppKit frames retain their transaction mode and first-activation focus guard.
Weak requesters avoid borrowing the logical Window or blocking on a reentrant
native mutex, and closed windows reject pending requests. Other platforms keep
their existing pacing; Wayland still commits every completed frame.

The temporary Objective-C key used to obtain a screen's display identifier is
released after use.

The `native-test-support` feature supplies cumulative counters used by the native
frame-demand and display-link lifecycle regression examples. Normal builds omit
these counters. The examples assert idle source suspension, wake and rendering
behavior, hidden restoration, callback-driven closure, and balanced source
ownership. This feature does not enable `test-support`, whose automatic drawing
would bypass native frame scheduling. Native Metal tests assert lazy path
allocation and pixel parity.

Keep local changes limited to documented behavior and regression coverage. Remove
each correction when an adopted upstream release provides equivalent behavior.
