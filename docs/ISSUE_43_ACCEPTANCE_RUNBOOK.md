# Issue 43 Native and Release Acceptance Runbook

This runbook defines the reproducible native compatibility and release-performance acceptance
campaign required by [issue #43](https://github.com/sadiksaifi/SpaceTerm/issues/43). It covers
US-45, US-47, and US-48. The issue remains the acceptance authority; this document turns its
matrices into an executable recording procedure without weakening them.

The deterministic conventional terminal corpus in `src/terminal/conformance.rs` and
[`CONFORMANCE.md`](CONFORMANCE.md) is a prerequisite, not a substitute. Source inspection, a
passing reducer, a passing test Adapter, or structural package verification does not prove the
AppKit, GPUI, PTY, Shell Process, packaged-app, native-service, or accessibility behavior required
here.

## Scope and authority

- Run conventional acceptance through the packaged release application and its production GPUI,
  AppKit, PTY, Terminal Emulator, and Shell Process path.
- Use the product and domain contracts in [`CONTEXT.md`](../CONTEXT.md) and
  [`UBIQUITOUS_LANGUAGE.md`](UBIQUITOUS_LANGUAGE.md), including the distinction between a Window's
  Focused Pane and Terminal Input Focus.
- Published protocols and Apple platform behavior are authoritative. Ghostty is a behavioral and
  performance reference, not protocol authority and not an implementation dependency.
- Conventional acceptance excludes image behavior. Static Kitty graphics is a separately owned,
  supplementary row under issue #89 and `scripts/kitty-graphics-smoke.sh`. Sixel, iTerm inline
  images, Kitty host-file media, and Kitty animation remain excluded.
- Do not check an item from source inspection alone. A process merely launching is not a pass.

## Acceptance campaign identity

An **acceptance campaign** is the complete set of required scenario runs supporting one final
PASS or FAIL result. A **scenario run** is one execution of a matrix row or performance scenario.

### Frozen-artifact rule

Freeze the following tuple before executing any result intended for the final campaign:

1. SpaceTerm commit SHA.
2. SHA-256 of `Cargo.lock` at that SHA.
3. Packaged app marketing version and build version.
4. Executable architecture and code-signing verification result.
5. SHA-256 of the exact `SpaceTerm.app` archive or canonical bundle digest recorded by the package
   verifier.
6. SHA-256 of the exact DMG.

All required final rows must use that exact tuple. Do not combine results across commits,
`Cargo.lock` contents, rebuilt app bundles, or rebuilt DMGs. A source change, dependency change, or
rebuild that changes either package hash starts a new campaign with a new run ID. After a fix, rerun
the previously failing scenario on the new packaged artifact; because the artifact identity
changed, rerun every other required row before declaring the new campaign PASS.

The final packaged smoke must launch the app from the mounted, verified DMG as a new process.
Source-build and unpacked-app runs may be diagnostic evidence but cannot supply a final result.

### Run ID

Use this stable, UTC-based form:

```text
i43-<YYYYMMDDTHHMMSSZ>-<commit12>-<dmg-sha12>
```

Every execution record belongs to the same campaign run ID. A matrix `case_id` remains the stable
inventory identity; it never encodes a subject or rerun. Each actual SpaceTerm or Ghostty execution
gets a unique subject-scoped `record_id` containing its attempt number. A rerun creates a new record
that explicitly supersedes the prior same-subject record without renaming or overwriting it.
SpaceTerm/Ghostty pairs link their exact opposite-subject record IDs rather than linking only a
shared case ID.

## Identity capture and privacy

Record the following before every campaign and verify that it has not changed before every
scenario run:

- SpaceTerm commit SHA and `Cargo.lock` SHA-256.
- Packaged app marketing/build version, executable architecture, code-signing result, and app/DMG
  SHA-256.
- macOS product version and build, machine model and model identifier, CPU, memory, display logical
  resolution, backing resolution, refresh rate, and backing scale.
- Exact executable, version output, and executable SHA-256 for every shell and TUI in the native
  matrix.
- Terminal font actually selected and whether `JetBrainsMono Nerd Font` was available.
- Initial rows/columns and logical/backing-pixel dimensions.
- Launch source. Final evidence must say `mounted verified DMG`.
- Clean Workspace root, temporary configuration locations, input source/IME used, accessibility
  permissions, notification permission, and relevant peripheral/display availability.

Identity capture must be reproducible and privacy-safe:

- Replace the local home-directory prefix in published paths with `$HOME` and the temporary root
  with `$TMPDIR`; retain the remaining path, executable hash, and version so another person can
  identify the same binary.
- Do not publish serial numbers, hardware UUIDs, device UDIDs, account names, host names, Apple IDs,
  IP or MAC addresses, SSIDs, notification contents, shell history, access tokens, cookies, or
  signing identities beyond the non-secret verification result.
- Do not capture or publish terminal, clipboard, environment, path, secret, logical-key, or typed
  key content in Local Diagnostics. Use deterministic non-sensitive fixture text for screenshots
  and recordings.
- Inspect screenshots, recordings, Instruments traces, logs, and generated manifests before
  upload. Redaction must not obscure the behavior being judged. If safe redaction would do so,
  repeat the scenario with safe fixture data.
- Record commands with secrets replaced by named placeholders. Never place credentials in command
  lines, environment dumps, filenames, or artifacts.

Use a new temporary Workspace root and temporary shell/TUI configuration for the campaign. Do not
modify or depend on permanent user shell, editor, tmux, Yazi, or coding-agent configuration for a
pass result. Preserve the temporary configuration files as scrubbed evidence.

## Artifact contract

Use the schema and templates in
[`ISSUE_43_ACCEPTANCE_EVIDENCE.md`](ISSUE_43_ACCEPTANCE_EVIDENCE.md). Store artifacts under one
directory named exactly as the run ID. Use ASCII lowercase case IDs and deterministic payload
artifact names:

```text
<case-id>--<subject>--<attempt-2digit>--<artifact-kind>.<extension>
```

Examples:

```text
native-bash--spaceterm--01--interaction.png
focus-sidebar--spaceterm--01--dec1004-bytes.txt
perf-sustained-ascii--spaceterm--01--time-profiler.trace
package-build--spaceterm--01--verification.log
```

Every immutable payload artifact must appear in `artifacts.tsv` with its owning `record_id`,
subject, stable `case_id`, relative path, SHA-256, byte count, media type, producing tool/version,
capture time in UTC, and privacy review result. Never overwrite an artifact; increment the
subject-scoped attempt number.

The payload manifest explicitly excludes `campaign.yaml`, `artifacts.tsv`, and `control.sha256`.
After payload bytes and URLs are frozen, generate the payload manifest, generate `campaign.yaml`
with the payload-manifest digest, then generate `control.sha256` over those two control files. The
control file never lists itself; its digest and the direct URLs of all three control files are
anchored in the final GitHub comment. The evidence schema defines the exact acyclic generation and
verification order.

For every matrix row record:

- exact command and sanitized environment/configuration inputs;
- unique `record_id`, subject, stable inventory `case_id`, attempt, exact comparison record when
  paired, and exact superseded record when rerun;
- ordered interactions, including timing where it affects the result;
- expected result and the authority used to derive it;
- observed result;
- PASS, FAIL, NOT-RUN, SKIPPED-UNAVAILABLE, or NOT-APPLICABLE status;
- at least the evidence required by that matrix, with payload-manifest-backed artifact links;
- the smallest reproduction and Ghostty comparison when a failure occurs.

## Status and skip rules

Use only these scenario statuses:

| Status | Meaning |
| --- | --- |
| `PASS` | Every required interaction was executed through the production packaged path and the recorded observations satisfy the stated result. |
| `FAIL` | A required interaction was executed and any expected result was absent, incorrect, non-reproducible, or contradicted by evidence. |
| `NOT-RUN` | A required interaction was not completed or its evidence is missing. This is not a pass. |
| `SKIPPED-UNAVAILABLE` | Only an explicitly conditional subcase was unavailable on the recorded machine, as allowed below. |
| `NOT-APPLICABLE` | Only an explicitly conditional step had a false precondition, as allowed below. |

`SKIPPED-UNAVAILABLE` is allowed only for language that is conditional in issue #43:

- numpad input **where available**;
- making the Operating-System Window non-key while SpaceTerm remains active **where possible**;
- backing-scale/display movement **when a second display is available**.

`NOT-APPLICABLE` is allowed only when a coding agent presents no detected/OSC 8 link for the
**if presented** subcase, or when packaging tool availability is already known and `just doctor`
is therefore not required. Record how the precondition was evaluated. These statuses normally
belong to a conditional subcase while the containing matrix row remains PASS after all of its
unconditional requirements pass.

Record why the condition was unavailable and the hardware/software fact proving it. Skipping one of
these conditional subcases does not excuse the rest of its row. A missing named shell/TUI,
Ghostty comparison build, IME, non-US input source, Accessibility Inspector, VoiceOver,
Instruments trace, package artifact, or permission is `NOT-RUN`, not `SKIPPED-UNAVAILABLE`, and
prevents a final PASS. Configure or install the prerequisite and rerun.

The final issue result has only two values: `PASS` or `FAIL`. Any required `FAIL` or `NOT-RUN`, any
unapproved skip, any missing payload/control file or digest, or any identity mismatch makes the
campaign FAIL. A failure remains a failure until the same recorded scenario passes in a new
packaged run. Never average, majority-vote, or replace a failed observation with source reasoning.

## Ghostty reference identity and ambiguity

Issue #5 names Ghostty revision `46767b521358200bfe3f268f365ccd2f218db558` as its original
audited behavioral reference. The current conformance contract records the embedded Ghostty core
revision `a887df42c56f6de86c0fe6da9c4eeca37931e083`. Neither identifier by itself identifies a runnable
Ghostty application bundle for the release-performance comparison, and the embedded core revision
must not be represented as the comparison app's version.

Before performance work, choose one runnable Ghostty build and freeze its complete identity:

- release channel/source and public version string;
- commit SHA when the build exposes one;
- app marketing/build version, executable architecture, and code-signing result;
- app bundle or distribution SHA-256;
- executable path with private prefixes normalized;
- configuration file contents/checksum, selected font, grid size, and all behavior-affecting
  settings.

Use that exact build for every SpaceTerm/Ghostty pair in the campaign. Record both original
revision identifiers above and explain which, if either, the runnable build corresponds to. If no
matching historical app bundle is reproducibly obtainable, do not silently substitute a current
Ghostty release: identify the chosen current build as a separate behavioral reference and record
the ambiguity. Published specifications and Apple behavior still decide correctness. A Ghostty
difference is an observation, not an automatic SpaceTerm failure; an unavailable required Ghostty
performance run is `NOT-RUN` and prevents a final PASS.

## Execution drivers

After lower stack layers are rebased, use their repository-provided evidence and workload drivers
when available. This runbook depends on capabilities rather than provisional filenames. The
drivers are expected to provide, at minimum:

- privacy-safe run-identity capture, payload-manifest generation, and detached control hashing;
- deterministic styled, Unicode, wide-grapheme, link, drawing-symbol, sustained-output, and resize
  workloads with byte counts;
- periodic RSS sampling at ten-second intervals;
- exact focus-report byte capture and held-key release probes;
- controlled failure injection through production Seams where practical;
- package verification output and artifact collation.

Record each driver's exact path, commit, help/version output, and full invocation. If a capability
is not automated, execute it manually and record the same inputs and observations. Automation
never changes the pass criteria and does not replace the required native visual, interaction,
accessibility, or profiling evidence.

For authenticated failure runs, create an owner-private directory, choose an absent absolute FIFO
path within it, and launch the mounted-DMG collector:

```sh
just acceptance-mounted-dmg-failure-identity <new-run-dir> <absolute-control-fifo>
```

The collector prints `Live acceptance staging root: <path>` before it blocks. Use that exact
owner-private root for live probe inputs/outputs; it is atomically renamed to the requested run
directory after the app quits. Wait until verifier stderr reports
`authenticated failure control is ready: <path>; status: <path>.status`. Do not create
or open the FIFO yourself. Arm one case at a time, and do not send the next case until the current
action reaches its required completion:

```sh
scripts/acceptance/failure-action-driver.sh \
  --control <absolute-control-fifo> \
  --case <fixed-case-id>
```

The driver sends only the fixed case name and an opaque one-request correlation nonce, waits for
the verifier's fixed `accepted` status echoing that nonce, and returns only after the authoritative
completed receipt produces fixed `completed` with the same nonce. The verifier owns launch authentication, request IDs,
monotonic sequence numbers, and the exact app peer; replay, app-bundle, source-build, and ordinary
launches cannot enable this path. Preserve `identity/failure-actions.tsv`, its metadata hash, and
the native recording for every action. A rejected command, replay, out-of-order result, unknown
schema, second pending action, incomplete phase sequence, or missing final result makes collection
`NOT-RUN`.

Trust boundary: the official verifier is the trusted same-UID campaign controller, not a
cryptographically authenticated server. SpaceTerm independently requires its exact canonical
packaged executable vnode on a read-only mount and rejects source/writable instances. A different
same-UID controller could trigger only an exact mounted instance it launches itself, but cannot
produce evidence authenticated and published by this verifier, affect another instance, or leave
a persistent/global injection setting.

The same verifier publishes `identity/native-observation-live.tsv` and the exact owner-private
`identity/ax-subject.tsv` before reporting that the mounted app is ready. The AX subject hashes
the provisional launch observation and binds the package application-tree hash, process start
time, executable vnode/filesystem, read-only mount, and live signature; pass this file directly to
the native AX probe and never reconstruct it from a PID or process-name scan.

### Native accessibility probe

`scripts/acceptance/native-ax-probe.sh` provides privacy-safe, run-owned structured evidence for
the targeted Pane's macOS accessibility contract. It never launches or discovers an application.
Before running it, the authenticated mounted-DMG launch controller must freeze an owner-private
`spaceterm.acceptance.ax-subject/v1` record for the live process. The record binds the run and
launch nonce to the exact PID/start time, mounted bundle, executable vnode/filesystem, bundle
identifier, application digest, and live signing identity. The probe independently revalidates
those facts before querying, before changing Selection, and after observing notifications.

The launch controller's live-subject record is canonical tab-separated UTF-8, uses the same `%25`,
`%09`, `%0d`, and `%0a` value encoding as `run-identity.tsv`, has mode `0600` in an owner-private
real directory, and contains exactly these records (the controller supplies the values; do not
reconstruct them by process-name lookup):

The final `native-observation.tsv` is published only after SpaceTerm exits and is therefore not
this live input. The mounted-DMG launch controller instead publishes the owner-private siblings
`identity/native-observation-live.tsv` and `identity/ax-subject.tsv` after authenticating its Unix
peer and before the campaign begins. The subject's `launch.observation.sha256` hashes the exact
provisional observation bytes. Never derive either record with `pgrep`, a bundle-name scan, or a
caller-supplied PID alone.

```text
schema	spaceterm.acceptance.ax-subject/v1
run.id	<run ID>
launch.nonce	<64 lowercase hex>
package.app.sha256	<bundle-tree SHA-256>
package.app.path	<canonical mounted SpaceTerm.app path>
package.app.bundle.identifier	io.github.sadiksaifi.spaceterm
package.app.executable.path	<canonical mounted SpaceTerm executable path>
process.pid	<positive PID>
process.start.tv-sec	<proc start seconds>
process.start.tv-usec	<proc start microseconds>
process.executable.device	<decimal st_dev>
process.executable.inode	<decimal st_ino>
process.executable.fsid	<signed decimal fsid0>:<signed decimal fsid1>
process.signature.cdhash	<lowercase live CDHash>
process.signature.identifier	<live signing identifier>
process.signature.team-identifier	<live Team ID or empty>
process.mount.read-only	true
launch.controller	acceptance-launch-verifier
launch.source	mounted-dmg
launch.observation.sha256	<SHA-256 of the authenticated provisional launch observation>
launch.observation.complete	true
```

That provisional observation has exactly 27 records. Its authenticated failure-action section is
`failure.action.schema` followed by `failure.action.enabled`; the latter is exactly `true` when the
controller configured failure control and `false` for an ordinary campaign or replay.

Compile once into the owner-private run staging directory, then use explicit Pane count/order and
visible UTF-16 coordinates. Selection mutation additionally requires expected before/after ranges,
a bounded observation interval, and at least one Pane-scoped Selection notification:

```text
scripts/acceptance/native-ax-probe.sh compile "$RUN_STAGING"
scripts/acceptance/native-ax-probe.sh run "$RUN_STAGING" \
  --identity "$RUN_STAGING/identity/ax-subject.tsv" \
  --output "$RUN_STAGING/capability/capability-accessibility--spaceterm--01--ax.tsv" \
  --expected-run-id "$RUN_ID" \
  --expected-failure-action-enabled <true|false matching controller invocation> \
  --privacy fixture-sentinel \
  --fixture-file "$RUN_STAGING/identity/ax-fixture-before.txt" \
  --fixture-sha256 <lowercase SHA-256> \
  --expected-pane-count 1 --pane-order 0 \
  --probe-line 0 --probe-index 0 --probe-range 0:1 \
  --expected-before-selected 0:0 --set-selected 0:1 \
  --expected-after-selected 0:1 --observe-ms 2000 --expect-selection 1
```

Use `metadata-only` privacy unless the Pane contains only a deterministic fixture. Text-bearing
queries require the explicit `fixture-sentinel` mode, an owner-private exact fixture file, and its
precomputed SHA-256. The probe compares values in memory and emits only approved hashes, UTF-16
lengths, ranges, geometry, booleans, notification counts, and monotonic timestamps. A mismatch
produces no acceptance artifact. Do not use shell history, ordinary terminal content, clipboard
content, credentials, paths, or other user data as the fixture.

`--expect-focus` requires the exact target Pane to regain focus. Focus changes to other elements
owned by the same authenticated process are retained as informational focus-away observations but
do not satisfy that minimum.

For Selection mutation, the probe re-queries and hashes the exact approved fixture immediately
before setting `AXSelectedTextRange`, confirms that Pane identity/order and every observable
generation guard still match, drains pre-dispatch AX events, resets a `mach_continuous_time`
baseline, installs the Pane-scoped Selection subscription only after that guard, and counts only
a notification delivered between Selection dispatch and the continuous deadline. It then
re-queries the expected range. This proves the public native path did not act on a stale observed Pane. It does
not manufacture an internally stale Presentation Generation: that rejection remains covered by
the deterministic Terminal Session tests because AppKit stamps a new public request from the
current model. Do not describe ordinary numeric-range reuse as native stale-generation proof.

This helper supplements but cannot replace the `capability-accessibility` row's manual inspection
with Accessibility Inspector and VoiceOver. Accessibility Inspector remains the visual hierarchy
and property cross-check. VoiceOver remains the end-to-end spoken-navigation, editing, Selection,
Cursor, soft-wrap, Scrollback, and Pane-boundary check; neither result may be inferred from a green
probe. Record the manual artifacts separately.

## Campaign procedure

1. Confirm issue #42's conformance prerequisite is satisfied at the candidate SHA.
2. Create the clean temporary Workspace root and temporary program configurations.
3. Record the complete SpaceTerm, host, display, font, shell/TUI, and Ghostty identities.
4. Run `just doctor` if packaging tool availability is uncertain.
5. Run `just package` and preserve the complete verification output.
6. Freeze the commit, `Cargo.lock`, app, and DMG tuple; create the campaign run ID and campaign
   record skeleton.
7. Mount that DMG and launch its `SpaceTerm.app` as a new process. Do not fall back to a source
   process after this point.
8. Execute packaged-app smoke, the native shell/TUI matrix, every Terminal Input Focus route, the
   capability/native-service matrix, and every failure-recovery row.
9. Execute the sustained-output, resize, and render-path protocols in optimized builds. Pair each
   SpaceTerm scenario with the frozen Ghostty reference using the same machine, font, grid,
   Shell Process, input, and duration.
10. Run final `just validate` at the frozen SHA and attach the complete output.
11. Freeze and upload the privacy-reviewed payload artifacts, generate `artifacts.tsv`, generate
    `campaign.yaml`, generate and verify `control.sha256`, then produce the final GitHub issue
    comment with the detached control digest from the evidence template.

## Native shell and TUI matrix

For every row, attach at least one screenshot and record the exact command, interactions, expected
result, observed result, and PASS/FAIL status. A process merely launching is not a pass.

| Case ID | Program | Required acceptance |
| --- | --- | --- |
| `native-bash` | Bash | Start an interactive shell; enter/edit ordinary and Unicode text; exercise Control and Option input; run styled and hyperlink output; paste single-line and multiline text; create Scrollback; resize; interrupt a foreground command; exit cleanly. |
| `native-zsh` | Zsh | Repeat the Bash checks and verify temporary Shell Integration updates directory, prompt/command, completion, and title metadata without modifying user configuration. |
| `native-vim` | Vim | Open a real text file; enter/leave alternate screen; insert Unicode; use navigation, function, Control, and mouse input; resize repeatedly; copy/paste; exit and verify Primary Screen restoration. |
| `native-neovim` | Neovim | Repeat the Vim checks, including mouse tracking, bracketed paste, cursor-shape changes, and clean alternate-screen exit. |
| `native-tmux` | tmux | Create a client, split panes, move focus, use the prefix, resize, enable mouse interaction, detach/exit, and verify the outer SpaceTerm Pane remains usable. |
| `native-less` | less | Page, search, follow links/text where applicable, resize, reach top/bottom, quit, and verify Primary Screen and Scrollback restoration. |
| `native-fzf` | fzf | Filter Unicode input, navigate, select, cancel, resize, and verify cursor/application-key behavior. |
| `native-btop` | btop | Exercise keyboard and mouse controls, colors/styles/drawing symbols, live updates, resize, suspend/quit, and input responsiveness during updates. |
| `native-yazi-no-previews` | Yazi without previews | Use a temporary configuration that disables previews; navigate, select, scroll, resize, open/return, and quit. Record the exact configuration and invocation. |
| `native-claude-code` | Claude Code | Start the conventional text UI; submit and edit a prompt; scroll; follow a detected/OSC 8 link if presented; resize; interrupt generation; exit cleanly. Image behavior is not part of this matrix. |
| `native-pi-coding-agent` | pi-coding-agent | Repeat the Claude Code conventional text-UI checks, including prompt editing, scrolling, resize, interruption, and clean exit. Image behavior is not part of this matrix. |

A failure in any named program must include the smallest reproducible command/input sequence and
whether the frozen Ghostty reference behaves differently under the same steps.

## Terminal Input Focus matrix

For each route, record the negotiated Cursor before the transition, the hollow Cursor while
blocked, and the restored negotiated Cursor afterward. The hollow transition must be visible on
the **next presented frame** after the authoritative focus transition. A hidden emulator Cursor
remains hidden.

When DEC 1004 is enabled, also record exact PTY bytes: one focus-out per loss, one focus-in per
gain, no duplicate reports, and held terminal-routed keys released before focus-out. Evidence must
show that the Window-owned Focused Pane identity remains distinct from Terminal Input Focus.

| Case ID | Required route |
| --- | --- |
| `focus-pane-switch` | Focus another Pane and restore the original Focused Pane. |
| `focus-sidebar` | Focus the Workspace sidebar and return to the terminal responder. |
| `focus-workspace-rename` | Start and finish/cancel Workspace rename. |
| `focus-workspace-context-menu` | Open and dismiss a Workspace context menu. |
| `focus-pane-menu` | Open and dismiss a Pane menu. |
| `focus-window-menu` | Open and dismiss a Window menu and Window context menu. |
| `focus-top-chrome` | Interact with top chrome/window dragging. |
| `focus-window-selector` | Interact with the Window selector, including switching Windows. |
| `focus-terminal-find` | Open Terminal Find, restore terminal responder focus while Find remains open, and close Find. |
| `focus-native-panels` | Present and dismiss every native modal/save panel used by the terminal. |
| `focus-non-key-os-window` | Make the Operating-System Window non-key while SpaceTerm remains active where possible. |
| `focus-app-activation` | Deactivate and reactivate SpaceTerm. |
| `focus-hierarchy-switch` | Switch Active Workspace and Active Window while multiple Panes remain visible. |

## Capability and native-service matrix

Exercise every row through the production packaged path, not only test reducers.

| Case ID | Required acceptance |
| --- | --- |
| `capability-keyboard` | Keyboard: ordinary text, navigation, function keys, numpad where available, Control/Option/Command routing, repeat/release, non-US layout, dead key, and at least one native IME composition/commit. |
| `capability-mouse` | Mouse: local Selection, application tracking, captured drag/release, Shift override, vertical/horizontal precision wheel, momentum, and alternate-screen scrolling. |
| `capability-paste` | Paste: ordinary text, bracketed multiline paste, unsafe confirmation approval/cancellation, embedded closing fence, file URL paste, and file drag/drop. |
| `capability-focus-bytes` | Focus bytes: DEC 1004 enable-current-state behavior, exact transition bytes, duplicate suppression, and held-key cleanup. |
| `capability-styles` | Styles: semantic default/ANSI/indexed/RGB colors, reverse, bold, faint, italic, blink, invisible, underline variants/colors, strikethrough, overline, drawing symbols, wide text, combining text, emoji, and fallback fonts. |
| `capability-links` | Links: OSC 8 and detected URL activation, validated local path activation, stale-generation rejection, and inert malformed/missing targets. |
| `capability-resize-scrollback` | Resize and Scrollback: cell/pixel resize, rapid live resize, reflow, viewport anchoring while output continues, Selection anchoring, Primary/Alternate Screen restoration, and backing-scale/display change. |
| `capability-accessibility` | Accessibility: inspect the actual Pane with Accessibility Inspector and VoiceOver; verify editable text-area role/value, UTF-16 ranges, visible range, Selection, Cursor, wide/combining/emoji text, soft wraps, retained Scrollback, range bounds, hit testing, and Pane-scoped notifications. |
| `capability-attention` | Attention: BEL audio/visual behavior, Pane/Window unread state, Dock attention rate limiting/cancellation, inactive-only native notification delivery, aggregation, and no focus stealing. |
| `capability-macos-services` | macOS Services: selection export and text insertion through the Services menu, with insertion entering the Paste Payload sanitizer. |
| `capability-context-actions` | Context actions: enablement follows current Selection and Terminal Hyperlink state and stale state is inert. |
| `capability-quick-look` | Quick Look: available only for an existing validated local regular-file Terminal Hyperlink; web, missing, remote, and stale targets are unavailable. |
| `capability-local-diagnostics` | Local Diagnostics: trigger bounded typed diagnostics, confirm no terminal/clipboard/environment/path/secret/logical-key content, export only after an explicit save-panel choice, and verify no automatic network connection or upload. |

## Failure-recovery matrix

Inject or reproduce each failure through the production Seam where practical. Record the trigger,
visible state, retained Presentation Generation, recovery action, and post-recovery result. The
authenticated driver exposes these fixed, content-free cases:

| Driver case | Production Seam and required action result |
| --- | --- |
| `presentation-invalid-scale` | Invalid backing-scale update; visible recoverable presentation failure retains the last valid generation, Retry completes as recovered. |
| `presentation-glyph` | Glyph paint preflight; requires a visible glyph, then the same recoverable presentation/retry sequence. |
| `renderer-image-preflight` | Image paint preflight; requires an actual visible Terminal Image, then a recoverable renderer-resource failure and successful Retry. |
| `renderer-resource-before-sync` | Resource synchronization fails before sync; retained visible generation and Retry must recover. |
| `renderer-resource-after-staging` | Display a new or replaced Terminal Image after arming. The Seam remains armed across empty/reused syncs and fails only after a real nonempty stage; the receipt must show positive staged keys/bytes and exactly equal rollback counts before Retry recovers. |
| `pasteboard-write` | Arms the next real Selection copy; perform Copy, confirm terminal input/session remain usable, then Retry succeeds. |
| `pty-fatal` | Emits the real typed fatal PTY failure; close the Pane to complete the authenticated action as closed. |
| `emulator-fatal` | Emits the real typed fatal Terminal Emulator/Session failure; close the Pane to complete the authenticated action as closed. |
| `normal-exit-control` | Arms observation only; enter real `exit 0` in the terminal and require completion as exited. |

Recoverable result order is `armed/accepted`, `injected/failed-state`,
`retry-requested/accepted`, `completed/recovered`. Fatal result order is `armed/accepted`,
`injected/failed-state`, `completed/closed`. Normal exit is `armed/accepted`,
`completed/exited`. Run fatal cases last or in separate launches because closing the claimed Pane
ends its action controller. The authenticated fatal result proves failure presentation and close
receipt only: separately capture PID/PGID identity and reap after close, then create a replacement
Pane and run a new command as native campaign evidence.

| Case ID | Required acceptance |
| --- | --- |
| `failure-presentation-recoverable` | Recoverable presentation failure preserves the last valid Presentation and succeeds after retry. |
| `failure-renderer-resource-recoverable` | Recoverable renderer-resource failure preserves the last valid Presentation and succeeds after retry. |
| `failure-platform-action-recoverable` | Recoverable native platform action failure leaves terminal input/session usable and clears or replaces its transient failure after successful retry. |
| `failure-pty-fatal` | Fatal PTY failure requires closing the Pane; closing remains responsive and leaves no owned process behind. |
| `failure-emulator-session-fatal` | Fatal Terminal Emulator/Session failure requires closing and recreating the Pane; the replacement starts and runs a new command. |
| `failure-normal-exit` | Normal exit remains distinct from every failure class. |
| `failure-diagnostics-bounded` | Exported Local Diagnostics remain bounded and content-free. |

## Release-performance protocol

Use an optimized packaged build. Debug or test-profile results do not satisfy this section. Attach
Time Profiler and Allocations traces, sampled RSS data, workload commands, and screenshots or a
screen recording.

Run each scenario once in SpaceTerm and once in the frozen Ghostty reference build using the same
machine, font, grid size, Shell Process, input, and duration. Ghostty is a behavioral/performance
reference, not a requirement for identical implementation or identical absolute numbers.

### Sustained-output run

1. Warm the app for 60 seconds.
2. Run deterministic high-rate ASCII output for 10 minutes while periodically entering terminal
   input.
3. Repeat with Unicode, wide graphemes, ANSI styles, links, and drawing symbols.
4. Repeat while the Terminal Viewport is scrolled away from the bottom.
5. Repeat while the Workspace/Window/Pane is hidden or occluded, then restore it.
6. Record bytes processed, RSS every 10 seconds, input responsiveness, frame behavior, final
   Presentation, and Shell Process exit.

Pass requires:

- UI Session events never exceed the designed bounded latest-state behavior and no output-sized UI
  backlog appears.
- After warm-up, RSS reaches a plateau: the final five-minute RSS range stays within 10% or 64 MiB,
  whichever is larger, of the first post-warm-up five-minute range and does not grow with total
  bytes processed.
- Input remains responsive and no main-thread stall longer than 250 ms appears in the trace.
- Restoring a hidden surface presents the newest generation without replaying intermediate frames.
- Final visible content and lifecycle event are correct after the producer exits.

Record the first and final five-minute minimum, maximum, and range, the computed threshold
`max(10% of the first range, 64 MiB)`, and the raw ten-second samples. Do not replace the issue's
range test with a trend line or average.

### Resize run

1. Populate Primary Scrollback with at least 10,000 deterministic lines containing short,
   soft-wrapped, styled, blank, and wide-character rows.
2. Repeatedly resize columns, rows, and both dimensions for at least five minutes while output
   continues.
3. Include backing-scale/display movement when a second display is available.
4. Record reflow time, input responsiveness, RSS, PTY cell/pixel dimensions, final grid state, and
   Selection/viewport anchoring.

Pass requires no unbounded resize command backlog, no lost final geometry, no input starvation, no
content corruption, and no memory growth correlated with resize count.

### Render-path proof

Profile idle Cursor blink, text blink, sustained output, Selection, Marked Text, Kitty static
graphics where separately accepted, and live resize.

Pass requires:

- `TerminalGridElement::paint` has no text-shaping call stack.
- `paint` constructs no paths, symbol plans, row plans, or image-placement geometry.
- No normal-frame heap allocation stack is rooted in `TerminalGridElement::paint`; exceptional
  error reporting is recorded separately.
- Cursor-only or blink-only frames do not reshape unchanged terminal rows.
- Shaping and geometry work remain proportional to visible changed rows and genuine overlay
  changes.

Record the Instruments version, trace duration, sampling settings, process identity, inspected
call-tree filters, representative stack screenshots, and trace artifact. Absence must be verified
against the captured call tree; a source search alone is insufficient.

## Packaged-app smoke

The final run must use the app from the verified package artifact.

| Case ID | Required acceptance |
| --- | --- |
| `package-doctor` | Run `just doctor` if packaging tool availability is uncertain. |
| `package-build` | Run `just package` and preserve its verification output. |
| `package-launch-dmg` | Mount the DMG and launch that `SpaceTerm.app` as a new process. |
| `package-window-shell` | Observe an Operating-System Window and a ready interactive Shell Process. |
| `package-command-output` | Enter a deterministic command and verify exact visible output. |
| `package-resize` | Resize and verify new PTY cell/pixel dimensions and intact presentation. |
| `package-process-reap` | Close the Pane/app and confirm the owned Shell Process is terminated and reaped. |
| `package-identity` | Record app/DMG hashes, versions, architecture, signature result, launch command, screenshot, and logs. |
| `package-final-validate` | Run final `just validate` and attach the complete result. |

Structural bundle, signature, resource, and DMG verification alone does not satisfy this smoke
test.

## Final acceptance gate

The campaign is PASS only when all of the following are true:

- Every named shell/TUI row passes its required interactions in the packaged release app.
- Every Terminal Input Focus route switches to the hollow Cursor on the next presented frame,
  restores the negotiated Cursor correctly, and emits exact ordered DEC 1004 bytes when enabled.
- Every capability/native-service row is exercised through the production GPUI/AppKit/PTY path
  with reproducible evidence.
- Every recoverable and fatal failure scenario has a successful, observable recovery path
  consistent with its type.
- Sustained output and resize meet the bounded-memory, bounded-backlog, responsiveness,
  final-state, and trace requirements above.
- The render-path profile proves no shaping, path/plan geometry construction, or normal-frame heap
  allocation rooted in `paint`, and no unchanged-row reshaping on Cursor/blink-only frames.
- Local Diagnostics remain bounded, content-free, explicitly exported, and local.
- Packaged-app smoke and final `just validate` pass, with all required artifacts attached to issue
  #43.

## Supplementary Kitty graphics row

If static Kitty graphics release smoke is run, record it as `perf-render-kitty-static` and link
the issue #89 evidence and `scripts/kitty-graphics-smoke.sh` invocation. Its status is supplementary
and cannot replace, weaken, or repair a conventional matrix result. Do not add Sixel, iTerm inline
images, Kitty host-file media, or Kitty animation to this campaign.
