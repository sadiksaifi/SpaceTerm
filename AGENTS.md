# Product goal

Build a functional prototype of a modern native macOS terminal named:

**SpaceTerm**

Technology choices:

- Rust
- GPUI from the Zed team for native UI and GPU rendering
- libghostty-vt for terminal emulation
- A macOS PTY implementation for launching and communicating with shells
- Native macOS SF Symbols through `gpui-symbols`
- Theme: Vague is the only application and terminal theme and the source of all brand colors. SpaceTerm is dark-mode only. The complete color palette is defined in `src/theme.rs`; reuse those tokens and do not hardcode or redefine color values in UI, terminal, platform, or domain modules.
- Font: `JetBrainsMono Nerd Font` for terminal text when available
- A sensible system monospace fallback when `JetBrainsMono Nerd Font` is unavailable

## UI direction

Build the interface directly with GPUI as a compact, Zed-like desktop experience.

Use `gpui-symbols` for icons and native macOS menus and system dialogs where appropriate.

Keep reusable SpaceTerm controls in a small internal UI module.

This is a macOS-only application for now. Do not spend time creating Linux or Windows abstractions.

## Mental model and terminology

SpaceTerm is inspired by the useful layout hierarchy of tmux, but it is not a tmux client and does not have a tmux-style server/client model.

The hierarchy is:

SpaceTerm
└── Workspace
    └── Window
        └── Pane

The exact state terminology is:

- Active Workspace
- Active Window
- Focused Pane

Do not use these tmux terms for the corresponding product concepts:

- Session
- Attached session
- Client
- Tab

A Window always belongs to exactly one Workspace.

A Window cannot be linked, shared, or attached to multiple Workspaces.

Use “Window” in the user interface and domain language even though it may visually resemble a browser tab.

Use a name such as `TerminalWindow` internally where necessary to avoid conflict with GPUI’s operating-system `Window` type.

## Required invariants

Maintain all of these invariants:

- The application always has at least one Workspace.
- Every Workspace always has at least one Window.
- Every Window always has at least one Pane.
- Every Window belongs to exactly one Workspace.
- Every Pane belongs to exactly one Window.
- The Active Workspace ID is always valid.
- Every Workspace’s Active Window ID is always valid.
- Every Window’s Focused Pane ID is always valid.
- Pane layouts support arbitrarily nested splits.
- Split ratios are constrained by sensible minimum Pane dimensions.
- Deleting an entity cleans up all terminal processes owned by it.
- No operation leaves an orphaned PTY or shell process.
- No Window can appear in multiple Workspaces.
- No active or focused identifier can reference a deleted entity.


## Architecture principles

Follow these engineering principles throughout the implementation.

### Deep modules

Prefer modules with:

Small public interfaces.
Significant internal capability.
Hidden implementation complexity.
Strong invariants.

Do not expose internal terminal, PTY, or tree-management details throughout the UI.

### Narrow interfaces

Keep interfaces focused on what their callers actually need.

Avoid large service traits with unrelated methods.

Avoid passing application-wide state into every component.

### Dependency injection

Inject replaceable dependencies at meaningful boundaries, including:

Terminal backend creation.
PTY/session creation.
ID generation where deterministic tests need it.
Clock or scheduling behaviour only if actually needed.
Platform-specific services.

Dependency injection should improve testability and replaceability without creating unnecessary factories or abstraction layers.

Prefer constructor injection and explicit ownership.

Avoid global mutable singletons.

### Separation of policy and mechanism

Keep product rules such as minimum entity counts and focus fallback inside testable domain modules.

Keep mechanism-specific concerns such as GPUI rendering, PTY syscalls, and Ghostty integration outside the domain model.

### Encapsulation

Domain operations should be expressed through intentional methods such as:

create_workspace
delete_workspace
create_window
delete_window
split_pane
close_pane
focus_pane
resize_split

Do not allow arbitrary external mutation of collections and active IDs.

### Error handling

Use typed errors where failures require caller handling.

Add actionable context to infrastructure errors.

Do not use unwrap() or expect() in ordinary runtime paths unless an invariant makes failure impossible and the reason is documented.

Do not silently ignore PTY, process, or terminal-backend errors.

### Simplicity

Do not create an elaborate framework before it is needed.

One application crate with well-designed modules is preferred over a large workspace of tiny crates.
