# SpaceTerm Context

This document owns SpaceTerm's durable product and architecture decisions. Code owns implementation
detail. Domain definitions and invariants belong to the
[ubiquitous language](docs/UBIQUITOUS_LANGUAGE.md).

## Product

SpaceTerm is a modern native desktop terminal. It ships on macOS today; Linux is the intentional
next platform. Its layout takes useful inspiration from tmux without adopting a tmux server/client
model. The product hierarchy is `SpaceTerm -> Workspace -> Tab -> Pane Layout -> Pane`.

The interface is a compact, keyboard-first, Zed-like desktop experience. The application-owned
Workspace Picker is the primary Open Local Project path. It is a live, one-level filesystem
navigator presented through the Command Palette; System Directory Selection is an explicit
fallback.

## Technology

- Use Rust 2024 and GPUI for the application and GPU UI.
- Use `libghostty-vt` for terminal emulation.
- Keep the executable in the root application crate and reusable, platform-neutral controls in
  `crates/spaceterm-ui`.
- Use typed Lucide `IconName` values for application icons and Vague Pro tokens from `src/theme.rs`
  for product color. Terminal text prefers JetBrains Mono Nerd Font with a system monospace
  fallback.
- Package macOS artifacts with pinned `cargo-packager` 0.11.8 and the tracked
  `assets/macos/SpaceTerm.icon`. Xcode 26 or newer produces the layered and legacy icon assets.
  Local packages use ad-hoc signing and are not notarized.

## Architecture

- Prefer deep Modules with narrow Interfaces. Keep terminal emulation, PTY ownership, Pane Layout,
  reusable control mechanics, and filesystem identity behind their owners.
- Put product policy, validation, ordering, bounds, lifecycle, and closed failure classification in
  portable Rust. Use GPUI for behavior it preserves and narrow Operating-System Adapters only for
  irreducible host facts or effects.
- Select capabilities through constructor injection. Constructor-wiring values group dependencies
  but define no platform operations. Owners retain cleanup authority, and stale generations or
  retired handles cannot affect successors.
- Domain Modules allocate identities and expose intentional operations instead of mutable
  collections. Active and focused identities always refer to owned entities.
- Keep the root application crate and the internal UI library. Add a crate only for a durable
  replaceability or locality boundary.
- The macOS Adapters are the only production host implementations. Portable GPUI capabilities are
  the starting point for Linux, but the repository makes no complete Linux claim and defines no
  speculative Windows Adapter.

## Security and failures

Use typed failures when callers must recover. Errors and Local Diagnostics exclude terminal and
clipboard contents, environment values, paths, credentials, and raw native errors. SpaceTerm sends
no automatic telemetry or crash reports.

Local filesystem authority is explicit and retained. Remote values never acquire local-file
authority. Authentication remains under OpenSSH policy; SpaceTerm does not store credentials or
weaken host verification.
