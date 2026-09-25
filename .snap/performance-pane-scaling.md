# Native Pane and image lifetime measurements

Use the real application, one owned Workspace window and the existing Pane
split/close owners. Add a benchmark-only controller and numeric observations,
not a separate mock window or Terminal Session factory. The existing single
producer harness needs a multi-child barrier and ownership ledger before it can
measure 1, 4 and 16 Panes correctly.

Status: design only, 2026-09-26. No production edits, builds or native captures.
The coordinator owns implementation selection and serialized measurements.

## Existing behavior to retain

- [`WorkspaceManager`](../src/ui/workspace_manager.rs:346) creates the first
  local Workspace, Tab and Pane through the normal injected native factory.
  Its action forwarding focuses the active terminal and dispatches the same
  [`SplitRight`/`SplitDown` actions](../src/ui/workspace_manager.rs:2859).
- [`PaneHost::split_focused`](../src/ui/pane_host.rs:915) enters the real
  split operation. The operation requires measured leaf bounds, resolves the
  source directory, prepares the launch, applies minimum-size policy and creates
  the normal Pane. A tight loop of splits before layout can fail or use stale
  bounds. Do not bypass that check or manufacture geometry for the benchmark.
- [`update_grid_bounds`](../src/ui/terminal_pane.rs:1810) starts the native
  Terminal Session only after the Pane's real grid is laid out. One Pane owns
  a worker, PTY reader and termination supervisor. This source-derived count
  predicts scaling but does not measure committed stack memory or idle CPU.
- [`commit_close_target`](../src/ui/workspace_manager.rs:2361) routes an
  authorized close through Tab/Pane owners. [`PaneHost::close_pane`](../src/ui/pane_host.rs:1128)
  removes the leaf, calls `TerminalPane::close`, retires accessibility/focus and
  drops the removed entity. [`TerminalPane::close`](../src/ui/terminal_pane.rs:1325)
  retires event/visibility/animation tasks and releases its session. The Pane's
  existing release callback also clears its graphics cache.
- [`TerminalGraphicsCache::clear`](../src/ui/terminal_graphics.rs:262) drops
  renderer images through GPUI and clears prepared/staged graphics. Shared
  `Arc` references and in-flight renderer work can outlive the initiating close.
  A close acknowledgement is not proof that all resources have settled.

## Smallest owned automation seam

Extend the existing `performance-probes` module with a fixed Pane-scaling
scenario, enabled only when the feature and an explicit benchmark environment
switch are present. Accept only bounded counts 1/4/16 and a small fixed phase
enum. The initial `app::start_application` hands its actual
`WindowHandle<WorkspaceManager>` to the controller after successful opening.
Do not discover or operate on arbitrary existing windows.

The controller uses ordinary `Window::dispatch_action` for splitting and focus
navigation. Add a numeric snapshot operation at the Workspace owner that
aggregates existing Tab/Pane iterators: Workspace/Tab/Pane counts, opaque Pane
identifiers, focused Pane identifier, layout generation/bounds, terminal grid,
session-ready state and terminal failure count. Lower owners supply only facts
that are private today; they do not expose mutable entities or replacement
constructors. `TabManager::aggregate_counts` and `terminal_panes`, plus
`PaneHost::terminal_panes` and `focused_pane_id`, already provide most traversal.
Keep probe-only methods behind `performance-probes`.

Build a balanced grid in rounds. Starting with one leaf, split every existing
leaf horizontally, then every existing leaf vertically to reach four. Repeat
those two rounds to reach sixteen. Before splitting a round, snapshot its leaf
IDs. Navigate to each existing target with the normal focus-next action and
verify the focused ID; perform one split, then wait for the new measured layout
and count before continuing. Bound navigation by the current leaf count and
abort on a missing target or failed split. Never split the newly created leaves
again within the same round. This avoids a narrow chain of sixteen Panes.

Keep one fixed window geometry per experiment, chosen to satisfy the existing
minimum size for a 4 x 4 grid. Report and compare each Pane's resulting grid,
not only the outer window size. Same-count baseline/candidate trials must have
identical grid multisets, scale and visibility. A 1-Pane window and a 16-Pane
window with the same outer bounds are different per-Pane workloads; do not
describe their resource ratio as purely per-Pane overhead. A separate fixed
grid-per-Pane experiment may require a larger display/window and must be labeled.

For lifetime cycles, retain one anchor Pane and close only IDs created by this
controller, reducing N to 1 before recreating N. A narrow probe-only
`close_owned_pane` operation may invoke the existing authorized close route
after validating the captured Workspace/Tab/Pane identities. This deliberately
tests resource cleanup, not the Close Confirmation UI; production confirmation
behavior remains covered by existing tests. It must not authorize an arbitrary
window or user Pane. A synthetic running Python producer otherwise triggers
normal confirmation and stalls unattended measurement. Do not fake idle-shell
metadata or change user Settings to avoid that dialog.

Use a retained parent-to-app control pipe for fixed commands and numeric phase
acknowledgements. A worker blocks on the pipe; one owned GPUI task delivers
commands to the captured window. No input polling or command reader wakes are
needed during a measurement interval. Reject malformed, oversized, out-of-order
or excess commands, close on EOF and retain a hard scenario deadline. The parent
never sends arbitrary shell text or global keyboard events.

## Multi-producer barrier and ownership

The current harness sets one `SPACETERM_BENCH_PID_FILE`, one ready file and one
summary file for every child. With multiple Panes, each workload process would
atomically overwrite the same destinations. The last writer would look like the
only child, earlier summaries would disappear and cleanup would miss children.
This must be fixed before accepting any scaling result.

Use a fresh 0700 directory owned by the harness for the entire trial. Each
synthetic workload registers a unique record under its own process identity,
including PID and creation identity for the parent's private ownership ledger.
Use exclusive creation and atomic publication, with a strict maximum of sixteen
active producers and bounded records per configured cycle. Never print paths,
environment values or producer content in the public report. Records must not
be reused across cycles.

After registration, each producer blocks at a barrier before emitting workload
bytes. The parent waits for both the app's final layout snapshot and exactly N
live, directly owned producer registrations. It verifies each process belongs
to the owned app and retains creation identity to reject PID reuse during later
cleanup. Then it releases every producer for the same fixed phase. Capture the
final PTY grid after this barrier, because early children are resized while the
grid is being constructed.

Prefer private inherited control channels or per-producer FIFOs opened through
the retained trial directory over repeated file polling. Keep all setup waits
outside the measured interval. A child registration alone is not readiness:
idle requires its initial screen processed; image mode requires real accepted
placements; scroll mode needs all producers started and the same pacing/count
contract. No sample is valid if any child exits early or fails to report.

Aggregate summaries by cycle and producer identity. Require exactly N valid
summaries, equal per-child fixture configuration and grid, and expected output
counts. Preserve min/max completion latency, total emitted bytes/lines/updates
and per-child distribution. Summing only totals can hide one stalled child
behind a faster sibling. The existing paced producer skips catch-up bursts when
blocked; unequal counts must reject a throughput/resource comparison.

Sample app resources separately from aggregate producer resources. Prefer one
sampler polling a retained PID set on a common clock over sixteen independent
Python samplers, whose setup and timer noise scale with Pane count. Verify PID
creation identity before each sample or fail the interval. Keep per-process
values internal where needed and report app metrics, producer totals and
distribution with aligned interval boundaries.

Finally, request the real Pane closes and let ordinary app ownership terminate
and reap children. Wait for all expected retired identities to disappear and
for worker/reader/supervisor counts to return to the anchor baseline. An emergency
harness kill must recheck retained child identity, affect only owned children,
and mark graceful cleanup evidence failed. Do not kill all app children before
the close phase and then claim that Pane cleanup worked.

## Image lifetime workload

Start with one image per Pane at 1024 x 1024 RGBA. That is 4 MiB of pixel bytes
per copy, or 64 MiB across sixteen Panes before extra native/snapshot/renderer
copies. The current 2048 x 2048 fixture is 16 MiB per copy and can push sixteen
Panes beyond normal resource admission once native and snapshot storage coexist.

[`APPLICATION_DECODED_LIMIT`](../src/terminal/graphics.rs:12) is 384 MiB and
accounts for native image/frame data and owned RGBA snapshots together.
[`GPU_CACHE_LIMIT`](../src/ui/terminal_graphics.rs:15) is separately 384 MiB.
[`upload_image`](../src/ui/terminal_graphics.rs:347) reserves RGBA length and
creates a BGRA copy for `RenderImage`. Do not count successful producer writes
as accepted images. A 2048 x 2048 pressure test is useful separately, with
expected admission/fallback behavior, but must not silently become the ordinary
sixteen-image comparison.

Add numeric probe facts at existing owners: decoded-budget bytes, graphics-cache
reservation bytes, cached images, accepted placements and successfully presented
graphics generation. These are accounting facts, not driver GPU residency.
An image-ready barrier requires expected placements to be processed and visible
Panes to have presented them. Match count and dimensions between variants.

Run each image trial through: blank anchor, N live image Panes, stable image
presentation, N-to-1 close, settled anchor, recreate N, and final N-to-1 close.
Keep the same native window and renderer throughout so closing the window does
not mask retained Pane images. If the anchor retains one image, its decoded/cache
accounting remains the expected nonzero baseline. A separate all-images-deleted
phase must use the normal terminal graphics protocol and verify deletion before
claiming a zero-image baseline.

Record app physical footprint/resident memory, relevant VM categories, decoded
and cache accounting, native renderer image/texture counts if available, thread
and descriptor counts, and live owned child count after each phase. Sample
settled points after GPU completion and normal deferred cleanup; do not force
cache collection or renderer teardown. Zero reservation bytes alone does not
prove zero driver residency. Growth across repeated cycles is more informative
than one before/after difference because allocators and driver caches may retain
their first high-water allocation.

## Minimal implementation order and acceptance

1. Add the bounded owned controller and numeric hierarchy snapshot. Validate
   1/4/16 construction with ordinary split/focus behavior and measured geometry.
2. Replace shared producer files with per-child ownership and synchronized
   start/completion. Validate missing, duplicate, stale and early-exit records.
3. Capture focused idle at 1/4/16 and N-to-1 cleanup/reopen cycles. Then repeat
   unfocused-visible and hidden as separate states using the owned focus helper.
4. Add accepted/presented image accounting and the smaller image fixture. Measure
   create/close/recreate before any image-cache optimization.

Required checks include normal constructors and directory authority, split
minimums, real native PTY cleanup, no leaked readers/supervisors, rejected
unowned close targets, bounded EOF/cancellation, exact aggregate counts and
content-free reports. The coordinator should expose this through explicit
macOS mise tasks and run builds/captures serially. No current 1/4/16 Pane or image
lifetime resource result is established by this design.
