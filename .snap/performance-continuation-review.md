# Continuation performance review

Reviewed 2026-09-26. Scope: native SSH exit observation, Control Connection
supervisor cancellation, shared GPUI display-link ownership, native lifecycle
fixture, lazy Metal path targets, and opt-in measurement instrumentation. This is an independent source
review. The reviewer did not build, run application captures, or edit production
code. The broader performance goal remains active.

## Resolved findings

P2: The native display-link lifecycle fixture previously reported success even
when a callback reached a closed context during its drain interval. The source
now requires `late_ticks == 0` before reporting success. The reviewer verified
the assertion; rerunning the corrected fixture remains with the coordinator.

P2: Hidden application comparisons previously discarded the focused window's
bounds and compared empty hidden-window lists. Equal terminal rows and columns
do not establish equal window dimensions. The harness now retains
`window_focused` as the shared batch reference and validates each focused window
against it before hiding. Hidden state is checked separately before and after
capture. The reviewer verified the corrected reference wiring.

No unresolved material finding remains in this source review.

## Production review

No material production correctness defect found in the reviewed changes.

- Exit registration precedes the owned status check. Cached exits return an
  immediate observation. The waiter never reaps. Interrupted or failed native
  observation, and an exit hint before collectible status, fall back to the
  existing polling path.
- The supervisor stop flag is set before interruption. Shutdown interrupts
  before joining. Connection drop transfers the join to owned process cleanup.
  Native queue and socket endpoints have RAII owners; dropping the waiter also
  closes the endpoint retained by its external interruption handle. Cancellation
  uses socket shutdown, so inherited duplicate endpoints cannot prevent wakeup.
- Shared native links remain in the static display registry. Their callback
  context contains only a display identifier. Subscriber removal is serialized
  with the callback before the final source owner cancels and releases it.
  Native start/stop calls occur outside the registry lock. The dispatch source
  is resumed exactly once, including the never-started window case.
- Frame counters and the sampler are excluded unless `performance-probes` is
  explicitly enabled. Counter records contain fixed numeric fields only. The
  native resource fixture retains ordinary signaling and reaping ownership and
  excludes setup and teardown from its timed comparison.

## Lazy path targets and resource harness

No material production defect found in lazy path-target allocation. Nonpositive
dimensions cannot allocate textures. Resize removes mismatched targets; the
first path batch allocates matching targets before rasterization. Matching-size
targets are reused. Target format, four-sample coverage, clear/resolve operations,
path clipping, and final path compositing remain unchanged.

The renderer uses Metal's ordinary `commandBuffer` method through the Rust
binding. This method [retains resources used by encoded commands](https://developer.apple.com/documentation/metal/mtlcommandbuffer/retainedreferences),
so clearing the renderer's texture owners during resize does not release
textures still needed by in-flight commands. The pixel fixture waits for command
completion before CPU readback and synchronizes managed storage on discrete
devices. Its assertions cover transparent pixels, premultiplied translucent
interior pixels, fractional-edge coverage, first path use, resized targets, and
same-size reuse. Tests were inspected, not run by this reviewer.

The hidden-state fallback sends Command-H using `CGEventPostToPid` for the owned
application, checks existing event-posting permission without requesting it,
and releases both created events. The harness verifies the focused window before
this action and rejects captures unless the application becomes hidden and has
no visible windows. Failed delivery cannot silently become an accepted hidden
sample.

The resource harness retains strict numeric memory fields, rejects native
warnings and errors, and checks report PID and byte units. It preserves the
auxiliary ledger separately from the category total. Native memory inspection
runs after completed output with the synthetic shell held alive, and explicitly
disables deferred-reclaim draining. Output counts and terminal grid are checked
across trials. Counter fields remain fixed and numeric. No new material cleanup
or measurement-acceptance defect was found after the geometry correction.

The shared-link design follows [Zed's pinned implementation](https://github.com/zed-industries/zed/blob/e91b82c106817f2419207ebf81f1da766698ac95/crates/gpui_macos/src/display_link.rs),
checked on the review date. The local Apple SDK `dispatch/source.h`, lines
511-525, states that cancellation prevents further event-handler invocations;
an already executing handler is allowed to finish. Window lifecycle and dispatch
handlers run on the same main queue, which is the relevant context-lifetime
constraint here.

## Validation limits

The coordinator reports 41 Control Connection tests, 206 native tests, four
registry tests, and a 32-cycle native lifecycle run passing. These results were
not rerun by this reviewer. The registry tests cover shared displays and display
migration, but native validation used the currently connected main display.
Physical display unplug/replug and refresh-mode changes have not been exercised
in this review. The harness observes window dimensions in points, not the
drawable's backing scale; captures assume the same display and scale throughout
the batch. Lazy allocation moves the first path-target allocation into the first
path draw, so its interaction latency and native resource savings still require
the coordinator's measurements. Full integration gates and performance
acceptance remain with the coordinator.

## Independent demand-scheduling review

The launch agent independently reviewed the later demand-scheduling changes on
2026-09-26 using the snap-review behavior, lifecycle, architecture and test
lenses. Scope was `PlatformWindow`, logical `Window` invalidation/completion,
macOS frame request/driver/activation/occlusion/display/Drop paths, the Wayland
completion signature, and the real native `macos_frame_demand` fixture. No
material source finding was identified. No production edit, build or native
capture was performed by this reviewer.

- Logical demand remains with existing owners: dirty state, queued frame
  callbacks, pending presentation and the active-window one-second input grace.
  Dirty notifications, explicit refresh, direct draw, input and standalone
  `on_next_frame` callbacks all reach the platform requester. A callback added
  during a callback run remains in the new queue and requests another frame.
- `run_frame` consumes the pending bit before invoking the logical callback,
  releases the native mutex around that callback, and restores the callback
  before reconciling. `completed_frame` ORs its result into the pending bit.
  Completion therefore cannot clear demand posted while the frame runs. A
  synchronous reentrant frame request sets demand before the running guard.
- The requester captures weak native state and no logical App borrow. On native
  mutex contention it coalesces one deferred retry; a nested AppKit loop can
  retry after a 1 ms timer rather than blocking the main thread. The retry exits
  when state disappears. This timer runs only after actual contention, not
  during ordinary clean-window idle. Native pending state remains until delivered.
- Hidden windows stop their source while retaining demand. Restoration requests
  a frame regardless of entity changes. A missing native screen preserves the
  pending bit for restoration/screen-change retry. Retargeting drops the old
  source before requesting the new display's source.
- Closing during a callback marks state closed, removes demand and releases the
  source before queued native-window teardown. The returning frame driver checks
  closed state before restoring a callback or accessing its destroyed renderer.
  Native ivars retain state through the callback. The existing first-activation
  focus guard remains. Later activation and layer display retain synchronous
  transaction presentation; normal CoreVideo frames do not enable that mode.
- The default requester leaves continuously ticking platforms unchanged. The
  Wayland signature adjustment retains its unconditional surface commit.

The public native fixture exercises idle stop, notified rendering, standalone
and chained callbacks, finite animation, direct-draw presentation, full input
grace, hidden restoration, unfocused-visible updates, source release and close
inside a real callback. It does not use TestAppContext's automatic drawing.
The coordinator's `continuation-frame-demand-diagnostic.log` reports every stage
passing. This reviewer inspected that log and fixture; final task status and
broader acceptance remain with the coordinator.

Native display migration/removal, changes in refresh mode, deliberately induced
AppKit mutex contention and nested synchronous layer callbacks remain explicit
validation limits. Source tracing does not replace those native cases. Start
failure retains requested demand but needs a later wake to retry, matching the
adapter's current recovery boundary; no automatic retry guarantee is claimed.
Launch, first prompt, GPU duration and pixel equivalence are outside the demand
fixture's counters and are not established by its success.
