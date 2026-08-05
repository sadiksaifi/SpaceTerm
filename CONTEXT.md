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

`SpawnedPty` exclusively owns the macOS PTY master, reader, writer, and Shell Process. After startup,
its terminal worker is the only owner that may wait, perform full termination escalation, or reap.
Terminal Session shutdown uses a cloned one-shot signaller synchronized with worker reaping, so
close callers request prompt termination without waiting for full process cleanup.

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

### Unified Terminal Geometry

`TerminalGeometry` is the sole conversion contract between GPUI logical viewport and cell metrics,
the native window backing scale, Terminal Emulator dimensions, mouse positions, and PTY pixel
dimensions. Full logical grid extents are scaled before rounding so fractional cells cannot
accumulate row or column drift. A Pane observes native window metric changes, invalidates
scale-dependent rendering resources, and sends resize updates through a latest-only mailbox so a
resize burst cannot create an unbounded control backlog.

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
