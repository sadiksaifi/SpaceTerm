# Launch continuation audit

Attribute the interval between GPUI run entry and the first opened window before
selecting another startup change. Initial probe captures place Metal construction
at only 1.74-2.82 ms, so they do not justify prewarm complexity. The first Terminal
Session's wait for measured grid bounds is an intentional correctness boundary
and should remain.

Status: audit and opt-in instrumentation prepared, 2026-09-26, checkout
`3831cd18095ac9fb74d5763da9dee85cd24556e8` plus the current performance work.
The coordinator subsequently authorized the narrow probes described below.
No builds or native captures were performed by this agent. All durations below
are either linked existing measurements or explicit unknowns.

## Existing evidence and exclusions

- [Selected-font classification](performance-launch-results.md) already removes
  about 262 ms of isolated classification work at the median. This does not
  establish first-window or ordinary-shell readiness time.
- [Earlier native menu installation](performance-menu-startup.md) was confirmed
  by the user to fix the visible menu delay. Preserve installation before
  Settings I/O and fonts, with the complete keymap already installed.
- [Native font enumeration](performance-round2-launch.md) is rejected. Exact
  collection options made the bulk query slower; the faster visible-family
  query lacks equivalent hidden/registered-font semantics. Do not reopen it as
  a startup optimization without new semantic evidence.
- The [application fixture](performance-application-results.md) measured an
  earlier producer marker, including Python fixture startup and 50 ms polling.
  It did not measure the first displayed frame or a normal login shell.
- Cargo still has dev/test overrides and no release override. The prior
  [ThinLTO proposal](performance-round2-launch.md#build-profile-experiment) has
  no recorded result in the current `.snap` records. It is already queued,
  not a newly discovered optimization. Preserve panic unwinding.
- [Ready remote exit observation](performance-remote-exit.md) now removes its
  idle 10 ms poll on supported native paths. The separate captured-process loop
  used by the startup SSH probe still polls. These are different lifetimes.

## Remaining critical path

| Stage and source | Work that still occurs | Measurement and decision |
| --- | --- | --- |
| [`StartupDependencies::capture`](../src/app.rs:60), [`probe_blocking`](../src/ssh/command.rs:165) | Runs supervised `ssh -V` before GPUI exists, with a five-second bound. Captured readers, child collection and cleanup surround the command. [`process.rs`](../src/ssh/process.rs:1077) sleeps up to 10 ms after a running status. | Time the entire probe plus spawn, status collection and cleanup. The old roughly 5 ms raw command median excludes this machinery. Separate fast success, unsupported version, absent executable and deadline cases using controlled adapters. |
| [`Application::new`](../src/app.rs:887), [`open`](../src/app.rs:460), [`MetalRenderer::new`](../third_party/gpui/src/platform/mac/metal_renderer.rs:135) | Initializes native application/window machinery, selects a Metal device, loads the compiled shader library, creates eight pipeline states and a command queue. | Time platform construction, device selection, library load, each pipeline and first drawable separately. Record warm and first-process cases. No SpaceTerm timing currently attributes this cost. |
| [`UserSettings::load`](../src/settings.rs:205), [`ConfigSettingsStorage::read`](../src/settings/storage.rs:91) | Reads the authority-checked Settings Document and parses it synchronously. The existing 4 MiB limit bounds accepted document bytes. | Time secure read and parse separately for missing, ordinary, large valid and invalid fixture documents. Keep initial appearance exact; do not show defaults while the selected appearance loads. |
| [`initialize_controls`](../src/ui/mod.rs:152), [`init_scoped_control_theme_catalogs`](../crates/spaceterm-ui/src/lib.rs:574) | Builds four application/Settings active/inactive catalogs, registers the embedded Lucide font once, and initializes control state. | Time catalog construction separately from font registration. Catalogs are structured theme data, not known hotspots. Defer only if a measurable cost justifies another lifecycle. |
| [`WorkspaceManager::new_with_adapters`](../src/ui/workspace_manager.rs:346), [`TerminalPane::new_with_services`](../src/ui/terminal_pane.rs:578) | Validates the home directory, builds one Workspace/Tab/Pane, measures terminal cell width, creates accessibility/lifecycle owners and closed transient controls. | Attribute home identity I/O, text measurement, accessibility creation and remaining entity construction. Keep filesystem identity checks and native accessibility. |
| [`on_children_prepainted`](../src/ui/terminal_pane.rs:3987), [`update_grid_bounds`](../src/ui/terminal_pane.rs:1810) | Starts the first Terminal Session only once actual grid geometry is available. | Record geometry-ready and worker-start separately. Starting with guessed rows/columns would change child-observed geometry and can cause startup resize/reflow. |
| [`start_deferred_with_context`](../src/terminal/session/launch.rs:299), [`NativePtyOwner::start`](../src/platform/native_pty.rs) | Starts the worker, prepares launch, opens/initializes the PTY and starts its reader/termination owners before the emulator loop. The shell then runs its normal startup files. | Distinguish worker scheduling, PTY-ready, emulator-ready, first processed bytes, first snapshot and first prompt/input milestone. Shell configuration is outside the app's optimization authority. |

`DirectoryPicker::new` creates a closed Command Palette and subscriptions; its
directory reads occur later in `refresh_for_input` through a background task.
Remote backend construction also mostly retains dependencies. Neither owner is
a demonstrated startup I/O hotspot merely because it is eagerly constructed.

## Ranked implementation candidates after attribution

1. **Metal prewarm is not selected after initial measurement.** GPUI already
   embeds a compiled metallib when `runtime_shaders` is absent; SpaceTerm does
   not enable that feature. Precompiling shader source is therefore already
   done. Device setup and pipeline creation may still impose first-use driver
   costs. The coordinator's first captures measure only 1.74-2.82 ms for the
   whole constructor. Keep this candidate deferred. If a different supported
   host later demonstrates a material cost, prototype a native-owned preparation task that uses the
   exact selected device and pipeline descriptors and rejoins existing renderer
   construction. Keep AppKit window/layer creation on its required thread.
   Prefer reusing successfully prepared resources over compiling them twice.
   A best-effort driver warmup is a narrower experiment, but may add CPU work
   and transient footprint even when latency improves. Require lower total
   launch latency without greater settled CPU/GPU/RAM or changed rendering.
2. **Overlap the same SSH probe with independent startup stages.** First test
   whether its full supervised cost matters. A retained probe task can overlap
   GPUI construction, but its result must be joined before the first existing
   capability consumer sees it. This preserves the same final availability
   without inventing an initially unavailable command. Fully removing it from
   the first-window path requires a pending-capability state and coordinated
   command/backend updates; that is a larger policy change, not a worker-only
   edit. Preserve timeout, cancellation and cleanup, with no duplicate probes.
3. **Overlap Settings read with work that does not consume Settings.** If I/O is
   material, read and parse through the existing storage owner on a worker,
   then consume the exact result before appearance resolution. Menu installation
   remains early. Do not remove security/identity checks or persist an assumed
   Settings cache. This is lower priority because the ordinary read may be tiny.
4. **Reduce measured control-catalog or closed-transient construction cost.**
   Only pursue if stage/allocation profiles select it. Creating a lazy Settings
   catalog adds invalidation and first-open latency obligations; the existing
   four catalogs deliberately track appearance generations together. Do not
   trade a small launch gain for a delayed first interaction or wrong colors.

## What upstream sources establish

Ghostty's current [Metal warmup implementation](https://github.com/ghostty-org/ghostty/blob/main/src/renderer/Metal.zig#L372-L405)
selects a device, creates/releases a queue and builds/releases pipelines to warm
driver caches before surface initialization. Its comments identify first-use
device, queue and pipeline costs. The same function exists in the local Ghostty
checkout at `b0c421fcd2e290629d4285c181b52fe2f2095f06`,
[`Metal.zig`](../third_party/ghostty/src/renderer/Metal.zig:409). This supports a
specific experiment, not an assumed SpaceTerm speedup. SpaceTerm uses GPUI's
renderer; calling a libghostty-vt function cannot warm matching GPUI pipelines.

Zed [starts login-environment loading on its background executor](https://github.com/zed-industries/zed/blob/main/crates/zed/src/main.rs#L409-L420)
and [passes a completion receiver to NodeRuntime](https://github.com/zed-industries/zed/blob/main/crates/zed/src/main.rs#L518).
The useful pattern is explicit completion ownership for dependent consumers.
It does not establish that every startup dependency can safely become detached.

WezTerm distinguishes [GUI startup](https://wezterm.org/config/lua/gui-events/gui-startup.html)
from GUI attachment and allows program/Pane construction during startup.
That distinction supports separate process, GUI and child milestones rather
than treating one launch callback as a usable terminal. It gives no comparative
SpaceTerm launch timing. Moving-branch sources were inspected on 2026-09-26;
pin the upstream revision before deriving an implementation from them.

## Concrete measurement plan

Extend the existing optional `performance-probes` capability with a fixed,
bounded set of first-launch timestamps. Keep stage names as enum values and
durations as numbers; emit no environment values, paths, font names, child
contents or credentials. Use one monotonic process clock. Record nested stage
durations without adding them twice to a total. Buffer events in memory and
serialize after launch settles so logging I/O is outside the measured path.

Required milestones are process entry, dependencies captured, host composed,
GPUI ready, menus installed, Settings loaded, appearance installed, controls
installed, native window constructed, grid geometry ready, PTY ready, first
snapshot delivered, first terminal-containing drawable submitted and first
presented drawable. Instrument subprocess start from the external owner too:
process-entry timing excludes loader and pre-main work. Do not subtract clocks
from different APIs unless their epoch/unit relationship has been verified.

GPUI's `present_drawable` call records submission, not physical presentation.
Evaluate Apple's [presented handler](https://developer.apple.com/documentation/metal/mtldrawable/addpresentedhandler(_:))
and [presentedTime](https://developer.apple.com/documentation/metal/mtldrawable/presentedtime)
for an optional native presentation marker. Keep submitted and presented times
separate. A menu-install timestamp proves ordering, not the first visible menu
frame; retain native visual observation for that distinction.

For normal integrated shells, record the first existing semantic prompt/input
state transition from [`apply_semantic_prompt`](../src/terminal/metadata.rs:572)
without recording its payload. That milestone means the shell reported its
state, not that an arbitrary prompt's pixels were presented. Non-integrated
shells need a separate controlled child handshake; do not infer readiness from
arbitrary terminal output or inject commands into the user's shell.

The coordinator should add one explicit macOS mise task for the controlled
launch fixture and run optimized, isolated samples with ordinary Settings and
fixed display/geometry. Alternate baseline/candidate order, use at least ten
fresh processes per condition, and report median, range and tail. Label warm
filesystem/driver caches separately from first-launch observations; do not clear
system caches as an unrecorded fixture step. Verify counts, child cleanup,
appearance overrides, menu/keymap readiness, initial PTY geometry and the first
Settings interaction. The existing 50 ms producer-marker sampler remains useful
for application workloads but is insufficient for choosing millisecond launch
optimizations.

## Prepared first-stage probes

The coordinator authorized measurement before selecting prewarm. The existing
`performance-probes` feature and `SPACETERM_BENCH_FRAME_COUNTERS=1` gate now
enable these additional stderr JSON events:

- `metal_startup`: at most eight renderer constructions per process, indexed
  numerically. Nanosecond durations cover device selection, layer setup, library
  load, vertex buffer, eight main pipelines, command queue, backdrop pipeline,
  atlas and CoreVideo cache. `constructor_total_ns` covers the same contiguous
  interval. Checkpoint clock overhead is included; emission after the final
  checkpoint is excluded. Rendering order and resource ownership are unchanged.
- `startup_stage`: one event per fixed stage below. Seconds use the existing
  sampler's process-local `Instant`. `app_run_enter` includes dependency capture
  and GPUI construction. `initial_window_opened` includes initial-window
  construction and activation request, not guaranteed physical presentation.

These probes serialize synchronously after their timestamps. End-to-end probe
launches include serialization/locking costs; compare instrumented variants and
measure that overhead before claiming millisecond launch improvements. The
sampler's existing once-per-second events remain `frame_counters`; consumers
must dispatch by `event` and must not assume every JSON line is a frame sample.
Normal builds omit this instrumentation. Builds with the feature but without
the explicit environment switch emit nothing. Formatting and diff checks pass;
the coordinator owns compile, native capture and result acceptance.

## First capture and attribution follow-up

The coordinator reports Metal construction at 1.74-2.82 ms, including
1.10-2.09 ms for the eight main pipelines. `app_run_enter` was 43-55 ms and
`initial_window_opened` was 399-405 ms. These initial ranges are not a paired
performance comparison or a physical first-frame measurement. They direct
attention to the roughly 350 ms between run entry and window opening.

The next probe revision adds these exact fixed `startup_stage.stage` names:

| Stage | Location and delta meaning |
| --- | --- |
| `app_run_enter` | Immediately before `Application::run`; existing stage |
| `app_run_callback_enter` | First statement in its run callback; delta from run entry isolates native startup dispatch |
| `settings_load_begin` | After menu/action initialization, immediately before `UserSettings::load`; delta from callback entry covers pre-Settings application setup |
| `settings_loaded` | Immediately after `UserSettings::load`; delta isolates Settings read/parse |
| `appearance_installed` | After appearance installation; delta from Settings loaded covers font capture, classification and appearance resolution/install |
| `initial_window_open_enter` | After controls initialize, immediately before `app::open`; delta from appearance installed covers controls |
| `initial_window_opened` | After `app::open` succeeds; delta from open entry covers native/logical initial-window construction and activation request |

The seven stages retain the existing once-only atomic mask and opt-in gate.
Settings stages are emitted only when the host supplies appearance storage, as
the production macOS host does. Failed startup can produce only a prefix; the
parser must reject an incomplete successful-launch comparison rather than
inventing missing durations. No loading sequence, appearance, menu ordering or
resource ownership changes. The coordinator will compile and capture this
revision; no measurement of the new boundaries is claimed yet.
