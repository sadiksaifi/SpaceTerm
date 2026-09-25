# Launch, Pane scaling, and remote lifetime research

Audited on 2026-09-26 at `1e25ad960d27fd7f526d083901c4e6ae10a21a60`.
The font enumeration candidates were rejected. The final controlled bulk-query
candidate was slower, and its production changes were reverted. Other launch
and lifetime opportunities remain evidence-gated.

## Ranked opportunities

| Rank | Opportunity | Evidence and selection gate |
| --- | --- | --- |
| Rejected | Copy macOS font-family attributes in bulk | Final exact-option measurements regressed enumeration. The initially faster option set lacks proven catalog equivalence. |
| 1 | Replace ready Control Connection process polling with a cancellable exit observation | Each ready Remote Workspace polls every 10 ms. Capture actual remote-idle wakeups and preserve immediate failure, authority revocation, and cleanup. |
| 2 | Measure ThinLTO and fewer Rust codegen units | No release profile override exists. Build-only experiment has low product-policy risk; runtime gains remain unknown. |
| 3 | Remove synchronous SSH capability probing from the local launch critical path | Probe precedes GPUI and has a five-second deadline. Existing raw SSH median is only about 5 ms; first measure full supervision and its slow tail. |
| 4 | Reduce first Settings opening font work without retaining all loaded faces | Current deferral moves classification and its retained font state to first Settings opening. Requires actual opening latency and settled-footprint measurements. |
| 5 | Consolidate Pane termination supervision if scaling shows a material cost | Each Pane owns three threads, two of which block. Reservations are not physical footprint. No evidence yet justifies changing isolation or cleanup. |

## Native family enumeration experiment

[`MacTextSystem::all_font_names`](../third_party/gpui/src/platform/mac/text_system.rs:100)
constructs the all-font collection, gets every descriptor, extracts its family
attribute, and appends the in-memory families. The public
[`TextSystem::all_font_names`](../third_party/gpui/src/text_system.rs:90) adds its
fallback families and `.SystemUIFont`, then sorts and deduplicates. Startup still
calls this entire path in
[`capture_initial_fonts`](../src/ui/appearance_runtime.rs:354).

Apple's
[direct family query](https://developer.apple.com/documentation/coretext/ctfontmanagercopyavailablefontfamilynames%28%29)
returns a retained array of visible family names. Its
[font collection constructor](https://developer.apple.com/documentation/coretext/ctfontcollectioncreatefromavailablefonts%28_%3A%29)
returns all fonts available to the application. Those descriptions do not
establish equivalent sets. Hidden families, process-registered fonts, font
activation, language, and fallback identities need verification. The existing
[font measurements](performance-launch-results.md) provide a reason to test the
query, not proof that it is faster or equivalent.

The isolated fixture in [`performance_fonts.rs`](../examples/performance_fonts.rs)
now has four modes:

- `families-compare`: compares the existing GPUI result with the direct query and
  the bulk descriptor-attribute query, each with GPUI's same fixed fallback
  additions. It prints only counts and equality. The bulk attribute comparison
  rejects missing or added families; direct-query equality is informational.
- `families-descriptors`: independently runs the original per-descriptor
  enumeration in a fresh process. It does not depend on production implementation.
- `families-direct`: times the direct query plus fallback additions, sorting, and
  deduplication in a fresh process.
- `families-attributes`: times the bulk family-attribute query on the same
  available-font collection, with native deduplication and the same additions.

The `listing` and `selected` modes continue to use the actual GPUI implementation.
The comparison also asserts that production's catalog equals the independent
legacy catalog. Both collection fixtures now copy the original CoreText
collection options, including duplicate-descriptor filtering.

The fixture registers no in-memory fonts. A production candidate must preserve
the existing `memory_source.all_families()` extension and the public wrapper.
The copied fallback list is explicit fixture scaffolding, not application policy.
The fixture uses a retained CFArray owner, checks null and conversion failures,
and reports no font names or native errors.

Run `mise run bench:macos:fonts families-compare 1` before timing. If equality
passes, alternate `mise run bench:macos:fonts families-descriptors 10` and
`mise run bench:macos:fonts families-direct 10` batches with no other builds or
captures running. Record binary hash, hardware, OS, font count, ordering, median,
and range. Separate query cost from first native frame and usable-shell time.
One host's equality is insufficient to delete descriptor behavior without
additional registered-font and fallback checks.

The coordinating agent observed direct-query parity on this host: 260 names on
each side, zero missing and zero added. Its first fresh-process timing batches
showed approximately 119 ms for descriptors and 13 ms for the direct query.
Exact samples and methodology belong in the measurement results. This is query
timing, not application launch timing.

The direct-query replacement was not selected. Chromium's
[font-list implementation](https://chromium.googlesource.com/chromium/src/+/refs/tags/107.0.5285.0/content/common/font_list_mac.mm)
documents hidden-family filtering on macOS 10.15. Font-kit uses the same direct
query for its own family enumeration, and Skia historically used descriptor
enumeration to emulate this query where unavailable. Neither establishes exact
equivalence for SpaceTerm's existing catalog. A null-result fallback cannot
detect a successful but incomplete list. No production edit was made.

A second experiment uses
[CTFontCollectionCopyFontAttribute](https://developer.apple.com/documentation/coretext/ctfontcollectioncopyfontattribute%28_%3A_%3A_%3A%29)
with `kCTFontFamilyNameAttribute` and `kCTFontCollectionCopyUnique`. Apple defines
its result as attribute values for the collection's descriptors, with missing
attributes represented by `kCFNull`. This retains the existing font universe and
attribute meaning. Native deduplication removes repeated family values, which
the public GPUI wrapper already removes.

The first attribute fixture compared equal across 260 families. Ten fresh
process samples measured 24,546, 21,015, 21,836, 21,648, 22,357, 22,567, 22,150,
21,484, 21,671, and 22,771 microseconds: median 21.993 ms and range 21.015-24.546
ms. These initial samples used null collection options. That changed the native
operation compared with GPUI's original duplicate-descriptor filtering, so the
22 ms result did not establish the proposed production optimization.

The final controlled fixture used the original collection options for both
paths. Actual production `listing` measured the candidate; independent raw
descriptor enumeration measured the legacy algorithm. Each row contains ten
fresh-process samples. These are native enumeration results, not application
first-frame measurements.

| Path | Batch | Median | Range |
| --- | --- | ---: | ---: |
| Legacy descriptors | First | 116.111 ms | 115.131-120.812 ms |
| Bulk candidate, actual GPUI | First | 127.389 ms | 126.335-131.091 ms |
| Legacy descriptors | Repeat | 116.023 ms | 114.895-118.462 ms |
| Bulk candidate, actual GPUI | Repeat | 128.075 ms | 126.211-130.256 ms |

The candidate's median was 11.3-12.1 ms slower across these batches. Its selected
font classification median was 135.027 ms, with a 132.392-151.128 ms range. The
legacy fixture and actual GPUI wrapper have different Rust/FFI conversion paths,
so this does not precisely isolate bulk-call overhead. The controlled results
do establish that the initial large saving does not survive preserving the
original collection options. Raw logs are
`target/performance/round2-font-final-*.log`.

Removing only descriptor duplicate filtering was considered and rejected for
this pass. Apple's
[duplicate option](https://developer.apple.com/documentation/coretext/kctfontcollectionremoveduplicatesoption)
does not specify the equality key. Its
[font priority documentation](https://developer.apple.com/documentation/coretext/ctfontpriority)
describes priority when resolving descriptor duplicates. These sources do not
guarantee that duplicates have identical family attributes. Thus unique family
values cannot be assumed to preserve the original catalog after removing the
filter, including user and process registrations. The native parity fixture on
one host does not settle that question.

The production bulk helper and its native test were reverted. GPUI's font file
has no remaining changes from this experiment. The controlled fixture and this
record remain for future work. A future candidate must resolve duplicate
semantics or provide another behavior-preserving path before acceptance.

## Remote idle wakeups

[`spawn_supervisor`](../src/ssh/control_connection.rs:735) sleeps for the
[`10 ms interval`](../src/ssh/control_connection.rs:37), locks the owned child,
calls `try_wait`, and transitions live authority to Failed when the child exits.
This is a durable loop, unlike the short-lived readiness and authentication
watchers. Its nominal 100 checks/second/Remote Workspace is source arithmetic,
not an observed interrupt-wakeup rate.

Ghostty's current
[`Exec.zig`](https://github.com/ghostty-org/ghostty/blob/main/src/termio/Exec.zig#L98-L150)
installs an `xev.Process` watcher and delivers exit through a completion callback.
Its same implementation retains a blocking reader thread and a cancellable pipe.
The useful transfer is event-driven exit observation with explicit teardown,
not a claim that all terminal work should share one thread.

SpaceTerm's
[`SshProcessBackend`](../src/ssh/process.rs:119) exposes mutable `try_wait` and
exclusive child ownership. A watcher must fit that ownership, avoid a blocking
wait while holding the child mutex, avoid racing reapers, and support stop/drop
without joining indefinitely. Introduce native exit notification at the process
adapter if measurements justify it. Increasing the polling interval alone
changes disconnection latency and does not meet the user's constraint.

Measure 1 and 4 ready idle Control Connections, separately from SSH children,
then exercise unexpected exit, exit-before-watcher-registration, close racing
exit, observer failure, repeated reconnect, and descendant cleanup. Retain
OpenSSH authentication policy from [ADR 0003](../docs/adr/0003-let-openssh-own-remote-authentication.md).

## Build profile experiment

[`Cargo.toml`](../Cargo.toml:89) overrides dev/test profiles only.
[Cargo's documented defaults](https://doc.rust-lang.org/cargo/reference/profiles.html)
use optimization level 3, 16 release codegen units, and local ThinLTO when `lto`
is false. Therefore this is an experiment in cross-crate ThinLTO, not turning all
optimization on for the first time.

Zed's current
[release profile](https://github.com/zed-industries/zed/blob/main/Cargo.toml#L1094-L1106)
uses `lto = "thin"`, one codegen unit, and a 16-unit override for its main crate.
Measure SpaceTerm's default against ThinLTO at 16 and one codegen unit. Capture
release build time, binary size, launch milestones, focused output CPU and
throughput, terminal-input latency, and idle footprint. Keep the pinned native
Ghostty build settings fixed. Do not extrapolate Rust LTO effects across the
separately compiled native archive.

Keep panic unwinding unchanged. SpaceTerm catches unwinds in native Services and
application-quit callbacks, and libghostty-vt catches Rust callback unwinds.
`panic = "abort"` would alter recoverable callback behavior, contradicting the
functionality constraint. See
[`macos_services.rs`](../src/platform/macos_services.rs:191) and
[`terminal.rs`](../third_party/libghostty-vt/src/terminal.rs:1669).

## Other launch and lifetime gates

The SSH probe remains synchronous in
[`StartupDependencies::capture`](../src/app.rs:78).
The captured-process supervisor checks child status with a 10 ms sleep in
[`process.rs`](../src/ssh/process.rs:1040); the raw SSH duration excludes this
overhead. An async probe requires a typed pending state and updates to command
availability and the Remote Workspace backend after completion. Eager backend
construction itself mostly retains fields and does not establish a launch
hotspot: [`new`](../src/ui/native_remote_workspace_flow_backend.rs:80).

First Settings construction calls
[`complete_font_catalog`](../src/ui/settings_window.rs:327) synchronously.
GPUI's [`load_family`](../third_party/gpui/src/platform/mac/text_system.rs:211)
loads the family's faces and retains them in its text system. Measure Settings
open latency and settled footprint before claiming the earlier launch memory
savings remain after using Settings. An isolated font-metadata scan may avoid
polluting rendering caches, but equivalence must include existing four-glyph
classification, rejected malformed fonts, selected-face identity, and fallback
behavior. Do not substitute a native monospace trait without parity evidence.

Pane scaling remains three threads per Pane: the
[`worker`](../src/terminal/session/launch.rs:327),
[`PTY reader`](../src/platform/native_pty.rs:390), and
[`termination supervisor`](../src/platform/native_pty.rs:206).
WezTerm's current
[`mux reader`](https://github.com/wezterm/wezterm/blob/main/mux/src/lib.rs#L294-L328)
also feeds a separate per-Pane parser thread. Thread count by itself does not
show waste. Capture 1, 4, and 16 Panes and repeated open/close cycles, including
thread count, physical footprint, descriptors, and surviving children after
cleanup. Blocking idle threads should not be conflated with polling wakeups.

## Status

Research and the bounded native font experiment are complete. The production
font candidate was rejected and reverted after final measurements. No launch
performance gain is claimed from this experiment. The independent fixture is
retained, and the reviewer was notified of the rejection.
All other candidates remain evidence-gated. External source links reference
moving upstream branches inspected on the audit date; pin revisions before any
implementation derived from them.
