# SpaceTerm Context

This document is canonical for SpaceTerm's product intent, technology decisions, UI constraints,
and architectural principles. Domain terminology, relationships, and invariants are canonical in
`docs/UBIQUITOUS_LANGUAGE.md`.

## Product intent

SpaceTerm is a modern native macOS terminal. Its layout hierarchy takes useful inspiration from
tmux, but SpaceTerm is not a tmux client and has no tmux-style server/client model.

## Technology decisions

- Use Rust 2024 and GPUI for native UI and GPU rendering.
- Use `libghostty-vt` for terminal emulation.
- Use a macOS PTY to launch and communicate with shells.
- Use `gpui-symbols` for native macOS SF Symbols.
- Support macOS only; do not create Linux or Windows abstractions.
- Treat Vague as the only application and terminal theme and as the source of all brand colors.
  Reuse the tokens in `src/theme.rs`; never hardcode or redefine colors in UI, terminal, platform,
  or domain code.
- Prefer `JetBrainsMono Nerd Font` for terminal text and use a sensible system monospace fallback
  when it is unavailable.

## UI direction

- Build the interface directly with GPUI as a compact, Zed-like desktop experience.
- Use `gpui-symbols` for icons and native macOS menus and system dialogs where appropriate.
- Keep reusable SpaceTerm controls in a small internal UI Module.

## Architecture principles

### Deep Modules

Prefer Modules with a small Interface, significant hidden Implementation, and strong invariants.
Do not expose terminal emulation, PTY ownership, or Pane Layout mechanics throughout the UI.

### Narrow Interfaces

Keep Interfaces focused on what their callers need. Avoid broad traits with unrelated methods,
and do not pass application-wide state into every Module.

### Dependency injection

Use constructor injection and explicit ownership at meaningful replaceability Seams such as
Terminal Session creation, PTY creation, and platform integration. Inject clocks or scheduling only
when needed. Domain Modules own hierarchy identity allocation; do not recreate identity counters in
the UI. Avoid global mutable singletons and unnecessary factory layers.

### Policy and mechanism

Keep product policy, including minimum entity counts and focus fallback, inside testable domain
Modules. Keep GPUI rendering, PTY syscalls, and Ghostty integration outside the domain model.

### Encapsulation

Express domain changes through intentional operations such as `create_workspace`,
`close_workspace`, `create_window`, `close_window`, `split_pane`, `close_pane`, `focus_pane`, and
`resize_split`. Do not permit arbitrary external mutation of collections or active and focused
identities.

### Error handling

Use typed errors when callers must handle failure and attach actionable context to infrastructure
errors. Do not use `unwrap()` or `expect()` in ordinary runtime paths unless a documented invariant
makes failure impossible. Never silently ignore PTY, process, or terminal-emulation errors.

### Simplicity

Prefer one application crate with well-designed Modules over a workspace of small crates. Add
abstractions only when they create a meaningful Seam, Leverage, or Locality.

## Current architectural decisions

### Overlay Scrollbar

The Overlay Scrollbar is a compact vertical control that presents scroll position without reserving
layout space. It owns thumb geometry, transient visibility, hover retention, drag capture, and
Vague-themed rendering. Terminal Scrollback and the Workspace list adapt their native offset units
at its Interface.

### Identity Ownership

Each hierarchy Module allocates the identities it owns. `WorkspaceCollection` allocates Workspace
IDs, `WindowCollection` allocates Window IDs, and `TerminalWindow` allocates Pane and Split IDs.
Creation factories receive the generated identity so infrastructure can bind events without
manufacturing domain state. Identities are monotonic and are not reused after deletion.

### Native PTY Owner

`SpawnedPty` exclusively owns the macOS PTY master, reader, writer, and Shell Process. PTY
configuration, Shell Process launch, and Terminal Emulator initialization occur on the terminal
worker; Terminal Session creation returns after starting that worker, and startup errors arrive as
typed terminal events. The worker installs a deferred one-shot signaller so a close racing startup
still requests prompt termination. Once launched, only the worker may wait, perform full
termination escalation, or reap, so close callers never wait for process cleanup. Shutdown sends
one SIGHUP to the complete owned process group, allows a bounded grace period, then sends SIGKILL to
that group when any member remains. The worker reaps the Shell Process exactly once after reader
output and publishes typed normal, signal, graceful-shutdown, or forced-shutdown exit state.

### Bounded PTY Output Transport

Terminal Session control Commands, including Shutdown, use a reliable latency-sensitive lane. PTY
reader events use a separate bounded queue with backpressure and ordered worker notifications.
Workers coalesce only consecutive output notifications up to a control boundary; reader completion
remains reliable and ordered after its output. Closing the receiver wakes a blocked reader producer
so terminal cleanup can continue off-thread.

### Immutable Terminal Presentation Snapshots

Each Terminal Session worker exclusively mutates its Terminal Emulator and publishes owned semantic
snapshots to GPUI. Snapshots retain shared identity for unchanged rows and metadata, carry cursor,
viewport, active-screen, size, and precise damage independently of cell content, and cross the
worker boundary through a bounded latest-screen channel. GPUI renders only these immutable values
and never borrows Terminal Emulator state.

### Semantic Terminal Colors

Terminal Presentation snapshots preserve default, palette-indexed, and RGB color sources instead
of flattening them into display colors. Each snapshot also owns the active defaults, complete
palette, and terminal-wide reverse state needed to resolve those sources immutably. GPUI applies
bold-as-bright only to normal ANSI foreground indices, then composes per-cell inverse with
terminal-wide reverse. Selection and Cursor colors remain separate presentation inputs, and a
color-state change invalidates prepared paint rows without changing unchanged snapshot row
identity.

### Negotiated Cursor Presentation

The Terminal Emulator snapshot normalizes every visible cursor to its viewport row, wide-grapheme
head, and occupied cell width while preserving negotiated shape, blink request, visibility, cursor
color, and cursor-text color independently from cell and selection colors. Cursor-only changes
damage only the previously and currently occupied viewport rows. Terminal Presentation renders a
filled block above backgrounds and selections and recolors its covered glyph, while bar and
underline cursors add thin geometry without replacing text.

### Terminal Input Focus

Terminal Input Focus is a pure derived fact and is distinct from the Window-owned Focused Pane
identity. The Terminal Focus Coordinator combines Active Workspace, Active Window, Focused Pane,
terminal responder ownership, key Operating-System Window, active application, and temporary UI
blockers. Only the complete true state admits terminal key input and presents the negotiated
cursor. Any false fact preserves emulator visibility but presents a steady, outline-only hollow
block; restoring every fact restores the negotiated shape and blink request.

### Terminal Grapheme Shaping

Terminal Presentation shapes the complete text stored in each terminal cell and anchors the
result at that cell's grid position. Wide heads, combining sequences, variation-selector forms,
emoji ZWJ sequences, and bidirectional-sensitive cells use an unconstrained whole-cell shaping
fragment; spacer tails never produce text. Compatible simple narrow cells may share one
fixed-width fragment, but fragments never cross a row, Selection, or Cursor boundary. Terminal
fonts disable ligatures and carry an explicit emoji and system-monospace fallback cascade, while
CoreText appends its language-aware system cascade. Cursor recoloring uses the same whole-grapheme
width policy as ordinary text. Prepared rows retain immutable snapshot identity, and a Cursor move
rebuilds only its previous and current rows, so shaping work remains bounded by visible cells and
changed rows.

### Ordered Terminal Focus Reporting

Every authoritative Terminal Input Focus transition crosses the reliable Terminal Session command
lane. The terminal worker owns the last reported focus state, DEC 1004 mode state, and the set of
terminal-routed held keys. Enabling DEC 1004 emits the current state immediately; later duplicate
transitions emit nothing. On focus loss, the worker synthesizes releases for its held keys before
emitting focus-out, while application Actions remain outside that held-key set.

### Unified Terminal Geometry

`TerminalGeometry` is the sole conversion contract between GPUI logical viewport and cell metrics,
the native window backing scale, Terminal Emulator dimensions, mouse positions, and PTY pixel
dimensions. Full logical grid extents are scaled before rounding so fractional cells cannot
accumulate row or column drift. A Pane observes native window metric changes, invalidates
scale-dependent rendering resources, and sends resize updates through a latest-only mailbox so a
resize burst cannot create an unbounded control backlog.

### Typed Terminal Key Input

Terminal key input crosses the Terminal Session command lane as one ordered, typed event carrying
press, repeat, or release action; physical and native identity; logical identity; UTF-8 text and an
unshifted codepoint; and full and consumed side-aware modifiers. Application Actions resolve before
the raw terminal handler. Unsupported physical identities produce a typed error and never degrade
to guessed terminal bytes. A worker-owned keyboard protocol Module reads Terminal Emulator modes
for every event and delegates cursor, keypad, DECBKM, modifyOtherKeys, fixterms, and negotiated
Kitty keyboard encoding to `libghostty-vt`; UI code never constructs terminal escape sequences.

### Native macOS Keyboard Bridge

The focused Pane reads the current AppKit keyboard event synchronously from GPUI's raw keyboard
callbacks, after application Actions have had the first opportunity to consume Command bindings.
The bridge maps native macOS keycodes to stable physical identities while deriving logical text,
unshifted codepoints, and consumed modifiers from each event, so input-source changes require no
cached layout state or Terminal Session restart. It balances left/right modifier transitions and
carries the injected Option-as-Alt policy with every ordered key event. Key releases use the same
Terminal Session command lane as presses and repeats.

### Workspace-Bound Terminal Creation

`WorkspaceCollection` owns default Workspace naming and passes the exact stored Workspace root into
payload construction. `WorkspaceTerminalSessionFactory` binds the existing dynamic Terminal Session
factory Seam to that root, and Windows, Pane hosts, and terminal Panes carry only this scoped Module.
Terminal creation therefore cannot select a working directory independently of its owning
Workspace, and no additional provider trait or global state is introduced.

### Cross-Hierarchy Close Escalation

Closing a final child escalates to its owning hierarchy Module without first destroying the child.
Closing the final Pane requests its Window close; closing the final Window removes its Workspace
when another Workspace remains, or closes the Operating-System Window when globally final. Explicit
Close Workspace remains distinct and replaces the final Workspace. The Module that resolves each
close synchronously removes the entity and initiates one-shot shutdown of its Terminal Sessions.
Shell termination and PTY ownership cleanup continue on terminal worker threads so GPUI callers do
not wait for reader or Shell Process joins.
