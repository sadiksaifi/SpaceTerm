# GPUI frame-source lifecycle and idle demand

Date: 2026-09-26. Status: lifecycle checks passed; demand candidate implemented,
native behavior validation in progress. This work
continues [the rendering follow-up](performance-round2-rendering.md). It does not
close the application's overall performance goal.

## Conclusion

Repair native display-link ownership before adding idle gating. The previous
`MacWindowState::start_display_link` destroys the old wrapper and creates another
on activation, occlusion changes, display changes, and synchronous layer draws.
The wrapper's destructor intentionally forgets its native CVDisplayLink to avoid
an upstream teardown crash. Reusing that sequence whenever a clean window sleeps
would accumulate native links at the rate of idle/wake cycles.

The first candidate shares one process-lifetime native link per observed display
identifier. Each live wrapper owns a resumed dispatch source and registers it
while running. The last subscriber stops that display's link. Unsubscription
removes the source from the registry before cancellation and release. This fixes
the ownership prerequisite without changing which native events request frames.
It does not claim lower steady-state idle wakeups.

## Lifecycle candidate

Changed files:

- `third_party/gpui/src/platform/mac/display_link.rs`: shared display registry,
  source ownership, idempotent start/stop, deterministic registry lifecycle tests.
- `third_party/gpui/build.rs`: allowlist the existing system `dispatch_release`
  function in generated bindings and remove the unused `dispatch_suspend`
  binding. No dependency is added.
- `third_party/gpui/examples/macos_display_link_lifecycle.rs` and its explicit
  `Cargo.toml` example entry: native main-thread adapter fixture.
- The coordinating agent owns `third_party/gpui/SPACETERM-PATCHES.md` attribution
  and scope update.

The constructor and `start`/`stop` signatures remain unchanged. Existing
`MacWindow` callers can still destroy and reconstruct wrappers: the sources are
released and the native display link is reused. Creation failures can now surface
from `start` when the first subscriber creates the native link; both calls already
return `Result` to their existing caller. A failed start removes its subscriber
and leaves the retained link available for a later attempt.

The registry lock protects subscriber lookup/removal against the CoreVideo
output callback. Creation, start, and stop calls into CoreVideo happen outside
that lock. The output callback receives a numeric display identifier, not a
pointer to a source or window. A late callback sees current subscribers under
the registry lock. It cannot dereference a removed window's source. Dispatch
sources are resumed exactly once and never suspended, including sources dropped
before their first successful start. Their last Rust owner cancels and releases
the dispatch object. The public wrapper remains confined to the main thread.

The registry state uses generic link and source handles so its real transition
and retention rules can be tested independently of CoreVideo and GCD. Those are
the uncontrolled capabilities. The tests exercise production subscribe,
unsubscribe, wake traversal, and failed-start cleanup operations.

Deterministic tests currently cover:

1. Two windows share one running display link; closing the first preserves ticks
   for the second; closing the last stops the link; duplicate removal is harmless.
2. Moving between displays preserves another window's subscription and reuses the
   original display entry when the window returns.
3. A failed start removes its source reference and a subsequent attempt starts
   the retained link.
4. Ten thousand wrapper lifetimes release every source reference while retaining
   one native link entry; late traversal reaches no closed window.

The four registry tests passed in the coordinating agent's first run. The test
filter is `platform::mac::display_link::tests` in the vendored GPUI crate.
It cannot be run by the application-only `test:one` task. The coordinator should
add a dedicated mise task using the existing `test:gpui:scene` command shape,
then run the lifecycle tests, app owner checks, formatting/lints, and optimized
source build in sequence. This agent has not run builds or native benchmarks.

The native example target is `macos_display_link_lifecycle`. It compiles the real
adapter with `#[path]` and the existing generated bindings, without exporting a
test-only GPUI API. Main-thread CoreFoundation run-loop pumping delivers real
CoreVideo/GCD callbacks to stable counter contexts. It checks two simultaneous
sources, idempotent start/stop, continued ticks for one source after another stops
or closes, restart after stop, 32 repeated lifetimes, 16 sources dropped
without starting, and 16 sources started then canceled before their first queued
callback can run. Contexts stay allocated through final draining so an unexpected
late callback becomes a counter observation rather than a fixture use-after-free.
After bounded draining, stopped/closed sources must receive no further callbacks.
The fixture reports numeric counts and static failure classifications. It is a
lifecycle check, not an idle CPU benchmark.

With `performance-probes` enabled, the example also checks the real adapter's
counters: one native link created across all lifetimes, 66 sources created and
released, and balanced successful native starts/stops and window subscriptions.
The first uninstrumented native run reported 32 cycles, 66 contexts, and zero
callbacks during close draining, but its command wrapper exited with status 2.
The retry in `target/performance/continuation-display-native-retry.log` passed
with task exit 0, confirmed by the coordinator. A final native run with the
explicit late-callback assertion and enabled resource counters remains queued.

The lifetime bound is one link per distinct display identifier observed during
the process, not one link per activation or window. Entries intentionally survive
disconnect/reconnect because releasing a previously running CoreVideo link is
the unsafe operation being avoided. Test native display removal, reconnection,
refresh-rate changes, and display migration. Reuse of an identifier relies on
CoreVideo updating its timing; current Zed documents the same assumption. Do not
delete/release old entries to address a timing failure without solving teardown.

## Source evidence

[Zed `display_link.rs`, revision `e91b82c1`](https://github.com/zed-industries/zed/blob/e91b82c106817f2419207ebf81f1da766698ac95/crates/gpui_macos/src/display_link.rs)
was inspected on 2026-09-26. Its shared registry addresses both release of a link
while its IO thread still runs and release of a dispatch source referenced by a
late output callback. The lifecycle candidate adapts that design to the vendored
GPUI's generated dispatch bindings and preserves its Apache-2.0 attribution.
It also retains the existing lower-level CoreVideo binding and its attribution.

[Zed's window integration](https://github.com/zed-industries/zed/blob/e91b82c106817f2419207ebf81f1da766698ac95/crates/gpui_macos/src/window.rs#L834-L860)
still starts its source for nonoccluded windows. The registry repair alone does
not establish demand-driven clean-window sleep.

[Ghostty renderer](https://github.com/ghostty-org/ghostty/blob/c959af63d11b524a84c21900372990dbc024b059/src/renderer/generic.zig#L1238-L1255)
starts vsync when a visible surface has rebuilt cells or an animation wake. This
supports the next experiment's objective, but GPUI must account for whole-window
entities, callbacks, and scene presentation rather than only terminal cells.

## Existing wake pathways

Locations below describe the pre-gating owner structure; line numbers may move.

| Origin | Current path | Required demand hook |
| --- | --- | --- |
| Terminal output, timers, Settings, entity changes | `App::notify` (`app.rs:2034`) reaches every tracked `WindowInvalidator::invalidate_view` (`window.rs:116`) | Wake the native source when dirty is set outside a draw phase; include every tracked window, not only the active window |
| Explicit window refresh | `Window::refresh` (`window.rs:1479`) calls `set_dirty(true)` | Centralize wake with `WindowInvalidator::set_dirty(true)` so callers cannot forget it |
| All-window refresh | `App::apply_refresh_effect` (`app.rs:1321`) directly calls each invalidator's `set_dirty(true)` | The same centralized dirty hook covers this path without scanning windows on every frame |
| Continuous animation | `AnimationElement::request_layout` calls `request_animation_frame`, which uses `on_next_frame` (`window.rs:1756`) | Appending the first callback must wake a sleeping clean window, even before the callback causes an entity notification |
| Callback chaining and IME | `on_next_frame` is used directly, including `invalidate_character_coordinates` (`window.rs:4511`) | Every queued callback is demand. Callbacks installed while a frame runs must retain another frame |
| Input without visible model change | `dispatch_event` (`window.rs:3965`) updates `last_input_timestamp` before dispatch | Wake even if no listener calls notify: the existing one-second presentation grace is independent of dirty state |
| A scene drawn outside the frame callback | `App::open_window` draws before publishing the window; `dispatch_key_event` (`window.rs:4114`) may draw a dirty dispatch tree | `Window::draw` sets `needs_present = true` at `:2097`; this transition must wake presentation even when the draw cleared dirty state |
| Resize and backing scale | Native `set_frame_size`/`view_did_change_backing_properties` invoke the resize callback, then `bounds_changed` refreshes | Dirty hook wakes; preserve drawable resize and synchronous AppKit transaction behavior |
| Window movement | Native moved callback invokes `bounds_changed` | Dirty hook wakes; retain display/scale updates |
| Activation and hover | GPUI callbacks update state and refresh; native activation also performs an immediate frame on later activations | Dirty hook plus native synchronous-frame path. Preserve the existing first-activation focus guard |
| Application appearance | `appearance_changed` notifies observers; it does not itself always call `refresh` | Preserve observer semantics and wake when observers notify; count AppKit's native layer requests separately rather than assuming every appearance callback is dirty |
| Occlusion/minimization/restoration | `window_did_change_occlusion_state` currently stops/starts the source | Hidden means physically stop ticks while retaining demand. Visibility return must request a frame even if no entity changed |
| Display changes | `window_did_change_screen` restarts for the new display | Resubscribe to the new display and request a frame; preserve demand if AppKit temporarily has no screen |
| AppKit layer display and native Tab activation | `display_layer` (`mac/window.rs:2142`) and key-status handler synchronously invoke the frame callback between source stop/start and transaction toggles | Route through one native frame driver; retain synchronous presentation and avoid unconditionally restarting a clean source afterward |
| Window close | `MacWindow::drop` stops the source before clearing native handlers and scheduling close | Wake closures must hold a weak native owner; cancellation precedes native view release; pending work cannot recreate the source |

`WindowInvalidator::invalidate_view` intentionally does not set dirty during a
draw phase. `refresh` also ignores requests made while drawing. The demand change
must preserve that existing invalidation policy rather than introduce an endless
self-redraw loop. Callback registration is a separate demand signal and must
remain effective during layout and paint.

## Minimal demand protocol after attribution

Keep product visibility in `RenderLifecycle`; the framework only decides whether
its whole window needs another native frame. Unfocused visible windows continue
to receive and present output. Hidden windows retain demand without ticking. An
inactive Tab continues processing its Terminal Sessions and contributes no grid
redraw until its existing product visibility restores it.

Suggested narrow platform interface:

- Obtain a clonable frame-request callback from `PlatformWindow` when creating a
  `WindowInvalidator`. macOS captures a weak `MacWindowState`, not the `Window`
  or an owning `Arc`, so the native frame callback cannot form a retention cycle.
  Other backends may preserve their existing continuously driven behavior with a
  default no-op requester during this isolated macOS change.
- Release `WindowInvalidator`'s `RefCell` borrow before invoking its requester.
  The current explicit native GPUI callbacks release the `MacWindowState` guard
  before invocation, including input, IME, frame, resize, appearance, and native
  Tab callbacks. A requester should still avoid blocking on a reentrant native
  mutex: use a weak-owner `try_lock` and coalesced foreground-executor retry if
  unavailable. The queued retry must retain demand even if completion sleeps
  before it runs; it must hold no strong window reference.
- Extend `PlatformWindow::completed_frame` with a `needs_another_frame` value.
  At the end of the logical frame, calculate it from current dirty state,
  `needs_present`, nonempty `next_frame_callbacks`, and the existing active-window
  one-second input grace. Compute after callbacks, draw/present, and effects have
  run. Wayland's existing completion hook must retain its behavior while accepting
  the new argument; it must not accidentally stop compositor acknowledgements.
- macOS owns `pending_request`, `callback_running`, visibility, and a retained
  frame source. A wake sets `pending_request`. A frame driver consumes that bit
  before invoking the logical callback. Completion ORs in further demand rather
  than overwriting requests that arrived while the callback ran. The driver
  restores its callback, then subscribes only if visible and work remains.
- Use the same driver for display-link ticks and synchronous AppKit frame calls.
  A nested native frame request while the callback is taken records demand and
  returns; it must not disappear merely because the callback is temporarily
  unavailable. Preserve the existing transaction toggles around synchronous
  frames. No CoreVideo callback invokes GPUI directly from the IO thread.
- Keep a consumed request distinct from new work. A stale queued tick after
  unsubscription can be discarded when there is no demand. A request during a
  frame can cause one extra clean callback, but it cannot be lost. Prefer this
  bounded redundancy over guessing that the in-progress frame handled new work.

The input grace requires continued ticks until its existing deadline. No extra
timer is needed while those ticks run. Once expired and otherwise idle, stop the
source. A new event or deadline timer calls the requester and restores pacing.
Keep the initial launch grace and first presentation; do not use focus as a
condition for presenting terminal output.

The minimal production files for this later step are `platform.rs`, `window.rs`,
`platform/mac/window.rs`, and the Wayland `completed_frame` implementation.
The retained source from the lifecycle fix remains the native capability owner.
`app.rs`'s dirty sites are covered by the invalidator, but its test-only automatic
draw loop needs careful treatment in the tests described below. No renderer or
Terminal Session logic should be changed for frame-demand scheduling.

## Attribution counters before gating

Use compile-time-enabled, content-free counters in the optimized source harness.
Do not write per-frame logs or allocate per-event records. Snapshot counters at
explicit measurement boundaries; keep process PID and window token ownership
inside the harness. The production default should incur no instrumentation cost.

The selected seam is an optional GPUI `performance-probes` feature with
`FramePerformanceSnapshot::capture()`. `src/frame_performance.rs` defines typed
cumulative `u64` fields backed by relaxed atomic counters. The snapshot never
resets or logs values and contains no window or terminal content. Each field is
atomic, but a snapshot across IO and main threads is not one transaction. The
coordinator owns explicit benchmark opt-in, monotonic timestamps, and bounded
numeric sampling output at 1 Hz; that sampling cost belongs to both compared
processes. All counter hooks are removed by `cfg` in default production builds.
The counter module, feature, serializable snapshot API, and native/logical hooks
are implemented. They do not change frame scheduling. Compilation and native
attribution capture are coordinated with the application's other measurements.

Counters required:

| Counter | Hook | What it distinguishes |
| --- | --- | --- |
| Native vsync callbacks | CoreVideo callback in `display_link.rs` | Display pacing frequency before main-queue coalescing |
| Delivered window ticks | `step` in `platform/mac/window.rs` | Actual main-thread frame wake frequency |
| Synchronous layer/key frames | `display_layer` and native activation branch | AppKit requests that are not display-link ticks |
| Callback queue executions | Logical `on_request_frame` after taking callbacks | Animation/IME obligations that exist without dirty state |
| Scene rebuilds | `Window::draw` | UI CPU preparation rather than mere tick delivery |
| Scene presents | `Window::present` | Renderer submissions, including one-second input grace |
| Clean callback completions | Logical callback with no draw or present | Proven redundant main-thread work eligible for suppression |
| Source/link create, subscribe, stop, release | Lifecycle boundaries in `display_link.rs` | Native ownership bound and cancellation behavior over repeated transitions |
| Demand reasons | Dirty, queued callback, input grace, pending presentation, native restoration | Why the proposed scheduler would remain awake |

The lifecycle fix can be measured before adding demand counters, but idle gating
must wait for attribution. Current interrupt wakeup measurements alone cannot
identify native display ticks, actual draws, or Metal presents.

## Tests and native acceptance for demand gating

`platform/test/window.rs:241` currently discards `on_request_frame`, and
`App::flush_effects` automatically draws dirty test windows (`app.rs:1234`). A test
that only calls `cx.notify` and observes a changed scene can therefore pass even
when a real sleeping native source would never wake. Add an explicit frame-driver
fixture that stores the callback and advances frames only when demand schedules
one. Verify the real requester and completion operations, not only a duplicated
Boolean truth table. Preserve the convenience behavior of unrelated GPUI tests.

Required behavior tests:

1. A clean window stops; entity notification wakes and presents its new scene.
2. Queuing `on_next_frame` on a clean window runs it, including a callback that
   queues another callback while executing.
3. Continuous animation keeps scheduling frames; completion/removal lets it stop.
4. Input with no model change restarts the one-second presentation grace, which
   ends without requiring another input event.
5. A direct `draw` that clears dirty state still receives a presentation.
6. Hidden notifications coalesce without ticks; restoration presents the newest
   scene; unfocused visible output continues to render.
7. A wake immediately before, during, and after completion is never stranded;
   duplicate wakes coalesce; a stale queued tick after sleeping is harmless.
8. A synchronous layer request or native activation while a callback runs is
   retained; transaction mode and first-activation focus behavior remain intact.
9. Display switching, missing-screen transitions, and closure do not leak,
   resurrect a window, or cancel another window's subscription.

After deterministic checks, use the existing source-bundle harness for idle and
output in focused visible, unfocused visible, hidden, and inactive-Tab states.
Record native scene counters with CPU, wakeups, and footprint. Verify cursor
blink, indeterminate controls, IME, pointer hover, resize, native Tabs, display
scale/refresh changes, and repeated hide/restore. Compare first input-to-frame and
restoration latency. A lower wake count with delayed output, a stuck callback, or
different artwork is a failed optimization.

## Native attribution and demand red gate

The 2026-09-26 optimized source capture in
`target/performance/continuation-frame-idle-before.jsonl` resolves the attribution
question. Both A/A processes used the same retained binary. Each nine-second
counter interval contained 1,080 native vsync callbacks, 1,080 window callbacks,
and 1,080 logical frames. Every logical frame was clean. There were zero scene
draws, scene presents, queued callbacks, or demand reasons during either
interval. The source therefore delivered 120 unnecessary logical callbacks per
second. Interrupt wakeups were about 121.7/s, including the benchmark's 1 Hz
sampler. These counters justify stopping the idle source; the wakeup total alone
would not have done so.

Cumulative startup counters also recorded eight scene draws, zero scenes with
paths, and three path texture allocation sets. The coordinator owns the separate
Metal allocation candidate and its memory validation. Frame scheduling must not
be credited with that memory change.

The new `macos_frame_demand` native example uses the real GPUI Application,
Window, renderer, and public notification/callback APIs. It requires
`performance-probes`. Its first gate waits for launch to settle and then requires
zero native ticks, logical frames, and presents during a 250 ms idle interval.
That gate is expected to fail before scheduling changes. Later gates verify
notification, chained callbacks, animation completion, direct-draw presentation,
input grace, and hidden demand followed by restoration. It does not use
TestAppContext's automatic dirty-window drawing. The coordinator owns the serial
red/green runs and their task records.

The lifecycle fixture now additionally requires zero callbacks into a context
marked closed before reporting success. It keeps all contexts alive through
bounded draining and checks quiescence after draining as well. A native late
CoreVideo callback remains permissible; a canceled GCD source invoking a closed
window context is not.

The native requester must remain inert after `MacWindow` closes. A weak pointer
alone is insufficient: Objective-C window/view ivars can retain `MacWindowState`
until the queued native close finishes. The driver therefore needs an explicit
closed state and must not restore callbacks or touch the renderer after a
callback closes its Window. Missing `NSScreen` transitions retain pending work
and pause the source until the screen-change/restoration callback retries.

`display_id_for_screen` currently owns the temporary `NSScreenNumber` key created
with `NSString::alloc(...).init_str(...)`. The narrow fix releases that key after
reading the dictionary's numeric value. It must not release the borrowed screen,
dictionary, or returned number. This follows Apple's
[Objective-C ownership rules](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/MemoryMgmt/Articles/mmRules.html),
checked 2026-09-26. Ordinary wake requests should reuse the retained source and
avoid allocating this key at all; display-change callbacks retarget the source.


## Implemented demand candidate

The red run `target/performance/continuation-frame-demand-red-exit.log` compiled
and failed on the intended idle logical-frame assertion with nonzero status.
AppKit's `terminate:` exits 0 rather than returning from `Application::run`, so
the fixture explicitly exits 1 on an assertion error before requesting quit.

The candidate changes these owners:

- `platform.rs`: a clonable, no-argument frame requester and a completion demand
  boolean. Continuously paced platforms retain the default no-op requester.
- `window.rs`: WindowInvalidator owns the requester. Entity invalidation and
  refresh wake after releasing its RefCell borrow. `on_next_frame`, input, and
  direct draw request frames. Completion includes remaining dirty state, pending
  presentation, queued callbacks, and the unchanged one-second active input grace.
- `platform/mac/window.rs`: one driver handles native ticks and synchronous
  AppKit frames. It consumes old demand before the callback and preserves new
  demand through completion. The source remains allocated while idle or hidden,
  but unsubscribes when no visible demand remains. Display changes retarget it.
  Close prevents pending work from restoring callbacks or touching the renderer.
- `platform/linux/wayland/window.rs`: accepts the completion argument and retains
  its unconditional surface commit. Its compositor acknowledgment is unchanged.

The weak macOS requester never borrows the App. It first tries the native mutex.
If AppKit reenters while a native operation holds that mutex, one coalesced
foreground task retries. A nested native run loop can execute that task before
the mutex is released, so it also uses try_lock and waits 1 ms only on continued
contention. It holds no strong Window state across that wait. Closed or released
windows end the retry without restarting their source.

A draw during the logical callback can retain one additional clean frame request.
That bounded extra tick avoids discarding a request that may have arrived during
the frame. Idle measurements must show quiescence after that tail, and output
measurements must check whether repeated native source start/stop affects pacing.

The native fixture now also checks unfocused visible notification with a second
non-overlapping window, continued first-window behavior after closing the second,
and closure from inside the actual frame callback. On idle assertion failures it
prints the stage and bounded numeric before/after snapshots. Its first candidate
run failed an idle assertion; classification and correction are in progress.
No demand performance improvement is accepted yet.

## Native green and latency comparison

The diagnostic native run passed every behavior with task exit 0: idle,
notification, chained callbacks, animation, direct draw, input grace, hidden
restoration, unfocused visible updates, and closure inside a frame callback.
No production correction separated the first failed candidate run and this
success. The first failure did not identify its stage. Host window-manager
activity is a possible explanation, not an established cause. Repeat the strict
fixture and retain stage counters if it fails again.

The final display-link native probe also passed with task exit 0: 65 native
starts/stops, 67 subscriptions/unsubscriptions, 66 sources created/released,
and zero callbacks to closed contexts. The independent review in
[the continuation review](performance-continuation-review.md) found no material
source issue. Forced AppKit contention/nested layer callbacks and actual display
migration/refresh changes remain native validation limits.

The optional `GPUI_FRAME_DEMAND_LATENCY=1` mode of `macos_frame_demand` compares
16 alternating samples, eight per source state. The continuous control queues
`on_next_frame` recursively without dirtying the scene. The idle condition has
no callback chain. Each notification and native-input phase waits at least
1.5 seconds, with a fixed small phase variation, and checks the observed native
callback count to confirm the source is actually awake or stopped.

The notification measurement starts before changing a fixed numeric revision
and notifying its Entity. Completion requires both that revision to have rendered
and the scene-presentation counter to advance. The input measurement constructs
and sends a key event only to the fixture's native view, then observes the first
presentation. It does not post keys to another application. Both measurements
use monotonic elapsed microseconds and a 1 ms observation timer. The fixture
reports samples plus median/minimum/maximum for each condition.

This is a same-candidate mechanism comparison. The continuous control adds a
frame callback and is not the archived old implementation. Timings include API
and observation scheduling costs and identify submission observation, not physical
screen photons. The comparison can expose a restart-delay tradeoff, but cannot
support claims about end-to-end terminal input latency without application-level
measurements. Latency results and the final performance acceptance are pending.

The first latency run in `target/performance/continuation-frame-demand-latency.log`
found a restart cost. With eight samples per condition, notification-to-present
medians were 4.148 ms continuously paced and 11.219 ms from idle. Native-input
medians were 5.325 ms and 9.820 ms respectively. The largest idle sample was
14.656 ms. These numbers justify a first-wake correction rather than accepting
lower idle activity at the expense of that delay.

`DisplayLink::start` now merges one event into its existing main-queue dispatch
source after a successful new subscription. It does so only on the stopped-to-
started transition; repeated starts do not add events. The first frame no longer
waits for CoreVideo's restarted phase, and subsequent frames retain native pacing.
A racing vsync can coalesce into that source event. The Window driver's pending,
visibility, and closed gates still decide whether a delivered event may run.
The native lifecycle fixture now requires start to return without invoking the
callback synchronously. The correction and latency comparison are awaiting the
coordinator's next runs.

The existing `window_vsync_callbacks` probe field now counts dispatch-source
deliveries, including the initial wake signal. `native_vsync_callbacks` continues
to count actual CoreVideo callbacks. Their difference is therefore expected at
source restarts. Do not interpret both as physical refresh counts.

A 60 Hz output producer may repeatedly cross the sleep boundary. Measure equal
output, draw/present counts, native starts/stops, and CPU before considering any
short source-idle grace. No additional idle grace or presentation policy has been
added speculatively.

The first-wake latency rerun passed with task exit 0 in
`target/performance/continuation-frame-demand-first-wake-latency.log`. With eight
samples per source state, continuously paced medians were 4.664 ms for notify
and 5.554 ms for native input. Idle-source medians were 1.293 ms and 1.377 ms.
The largest idle values were 1.411 ms for notify and 1.817 ms for input. The
initial source signal removed the measured restart delay in this mechanism test.
These remain submission-observation timings, not physical-display latency.
Full native behavior/lifecycle repeat and sustained-output resource comparison
remain required before final acceptance.

## Final acceptance

The full native behavior fixture passes after the immediate-wake correction,
including idle sleep, notification, callbacks, animation, direct draw, input
grace, hide/restore, unfocused-visible output, and close inside a callback.
The final lifecycle fixture reports one native link, 66 sources created/released,
83 subscriptions/unsubscriptions, 81 native starts/stops, and zero callbacks
during close drain. Sixteen companion sources are started and dropped before
run-loop delivery, proving queued first-wake cancellation. Start does not invoke
the callback synchronously. Both tasks exit 0.

Final equal-output captures verify lower foreground wakeups and idle CPU, visible
unfocused rendering, and continued hidden terminal processing. Full validation
and independent review pass. See performance-continuation-results.md. Earlier
pending statements in this chronological record are superseded by these results.
