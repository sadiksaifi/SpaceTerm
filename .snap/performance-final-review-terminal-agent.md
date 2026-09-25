# Final rendering review

Date: 2026-09-26. Verdict: no material findings in the reviewed source.
The rendering changes are acceptable from this independent source review,
subject to the coordinator's remaining native and full validation gates.

Scope: shared macOS display links, demand scheduling and first wake, portable
invalidation hooks, Wayland frame completion, lazy Metal path targets, their
native fixtures, and optional rendering/startup instrumentation. Find and the
workload acknowledgment were excluded because this reviewer authored them.
No source edit, build, native capture, or new upstream research was performed.

## Ownership and wake behavior

- `third_party/gpui/src/platform/mac/display_link.rs:277`: the initial signal is
  queued only after a successful new subscription. Repeated starts do not add
  signals. Late native callbacks access the static registry, and window sources
  are removed under its lock before cancellation and release.
- `third_party/gpui/src/platform/mac/window.rs:479`: requests remain pending while
  hidden, during a running callback, or while no screen is available. Visibility
  and screen changes reconcile that retained demand.
- `third_party/gpui/src/platform/mac/window.rs:1429`: the weak frame requester
  avoids borrowing the App and uses a coalesced foreground retry when the native
  mutex is occupied. It retains no strong window state across its timer wait.
- `third_party/gpui/src/platform/mac/window.rs:2236`: the driver consumes old
  demand before invoking the callback, preserves new demand through completion,
  and restores the callback before reconciling. Closing during a callback cannot
  restore the callback or restart the destroyed renderer.
- `third_party/gpui/src/window.rs:118`: entity invalidation releases its RefCell
  borrow before requesting a frame. Dirty changes, queued callbacks, input, and
  direct draws all have explicit wake paths. Completion includes dirty state,
  pending presentation, remaining callbacks, and the existing input grace.
- `third_party/gpui/src/platform/linux/wayland/window.rs:1025`: accepting the new
  completion argument preserves the unconditional compositor surface commit.

## Rendering and instrumentation

Lazy path allocation changes target timing and same-size reuse. It preserves
format, multisampling, clear/resolve operations, clipping, and path compositing.
The ordinary Metal command buffer retains resources needed by in-flight work
when resize clears the renderer's texture owners. The inspected native test
checks first use, resize, same-size reuse, translucent premultiplied pixels,
transparent pixels, and fractional-edge coverage.

The native display-link fixture now rejects callbacks to closed contexts and
checks that start does not invoke the callback synchronously. The demand fixture
covers notifications, chained callbacks, animation, direct draw, input grace,
hidden restoration, unfocused visible updates, and close during a callback.
The latency fixture identifies presentation submission observed by the probe;
it does not measure physical display latency.

Rendering counters and startup probes remain behind `performance-probes`.
Startup records require the explicit benchmark environment flag and contain
fixed numeric fields. Normal application builds omit these probes. Window-source
delivery counts include the first-wake signal and must not be described as
physical vsync counts. Avoided path-target allocations have not established a
large physical-memory reduction in the native application.

## Remaining validation limits

This source review did not exercise forced nested AppKit run loops or physical
display unplug/replug, migration, and refresh-mode changes. Their absence is a
coverage limit, not a demonstrated defect. Final native lifecycle repetition,
strict application captures in all requested visibility states, and the full
repository validation gate remain owned by the coordinator. See
[merge readiness](performance-merge-readiness.md) for current gate results.

## Artifact-supervisor addition

Independent read-only review of `scripts/cargo-artifact-supervisor.py` and
`scripts/test-cargo-artifacts.sh`: no material finding in the fix.

At `cargo-artifact-supervisor.py:139`, measurement retries are limited to three
attempts, with two possible 100 ms delays. A nonzero `du` exit rejects its output
before parsing, so even an oversized partial count cannot authorize cleanup.
Missing, malformed, or negative measurements fail after the same bounded retry
policy. This bounds retry count; the underlying `du` call retains its existing
lack of a per-command timeout.

At `cargo-artifact-supervisor.py:245`, measurement failure exits through the new
`finally` cleanup while the active command and its process-group identifier are
still retained. The existing termination routine signals that group, escalates
to SIGKILL if required, checks for remaining live processes, and waits for the
leader. Signal exit status takes precedence. The failure path does not call
Cargo clean when size is unknown; repository ownership verification and explicit
target selection still guard actual cleanup.

The added test cases inject two transient failures followed by a successful
measurement, and persistent failure with a real owned leader and child. They
check the command's original exit status, retained target artifacts, rejection
of the oversized failed measurement, and termination after persistent failure.
Their own `finally` cleanup covers a broken supervisor. Existing foreign-target
and signal tests remain in the script. Tests were inspected, not executed by
this reviewer while the coordinator's native capture was active.
