# SpaceTerm domain language

The SpaceTerm hierarchy, terminology, and invariants are defined in `AGENTS.md`.
This file records additional concepts that name deep modules.

## Overlay Scrollbar

A compact vertical control that presents scroll position without reserving layout space. It owns
thumb geometry, transient visibility, hover retention, drag capture, and Vague-themed rendering.
Terminal scrollback and the Workspace list adapt their native offset units at its interface.

## Identity Ownership

Each hierarchy Module allocates the identities it owns. `WorkspaceCollection` allocates Workspace
IDs, `WindowCollection` allocates Window IDs, and `TerminalWindow` allocates Pane and split IDs.
Creation factories receive the generated identity so infrastructure can bind events without
manufacturing domain state. Identities are monotonic and are not reused after deletion.

## Native PTY Owner

`SpawnedPty` exclusively owns the macOS PTY master, reader, writer, and child-process cleanup.
The terminal worker uses its narrow read acquisition, resize, write, wait, and termination
Interfaces without reaching into platform handles.
