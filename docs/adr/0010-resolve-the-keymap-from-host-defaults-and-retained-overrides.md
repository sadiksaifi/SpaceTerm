# Resolve the Keymap from host defaults and retained overrides

SpaceTerm lets a person change the Shortcut of every Command in the Keybindings Settings Section. The Keymap is the host's default Shortcuts with the overrides retained in `settings.json` applied. The design splits ownership four ways.

- The host composition supplies the default Shortcut of each Command and the System Reserved Shortcuts, because both follow platform convention. The macOS table lives in `src/platform/macos_reserved_shortcuts.rs`.
- The portable policy in `src/keybindings/` owns the Terminal Reserved rules, because they describe terminal input rather than any one Operating System.
- `settings.json` retains only overrides. An absent Command uses its default, `null` means Unassigned, and an override that restates its default is removed. A future change to a default reaches everyone who did not override it.
- Standard application commands such as Copy, Paste, and Quit, and the bindings internal to controls, stay fixed and outside the Keymap.

A Shortcut belongs to at most one Command. Commands differ in scope: some bind at the application level and some only inside a Workspace. The Workspace key context wraps the whole Workspace window, so a Shortcut shared by two scopes would still collide there. Ownership is therefore checked across both scopes. When a person records a Shortcut another Command owns, the recording Command takes it and the previous owner becomes Unassigned. A hand-edited document can still name one Shortcut twice or name a System Reserved Shortcut. Validation rejects the duplicate. Resolution marks a System Reserved override as blocked and leaves it inactive, because the System Reserved table is host policy and document validation is portable.

A layout-independent Terminal Reserved override is invalid in the document itself, which makes the file Malformed Settings. Settings Recovery covers a later change to the Terminal Reserved rules that invalidates a previously valid override.

GPUI resolves ties between bindings by order, and SpaceTerm's controls bind their own keys after the desktop profile installs. A rebind therefore replaces the tagged SpaceTerm segment of the keymap in place and keeps every other binding at its position. Shortcut hints in tooltips, menus, and captions read the installed keymap rather than a separate table. Native menus capture key equivalents when they are installed, so the menu bar is reinstalled after each rebind that changes the resolved Keymap.

Settings edits apply to the keymap as they happen, through the same live preview that appearance edits use, so every window shows the new Shortcut before the write completes.

Settings retain the written modifiers and key. Resolving Shift during parsing would permanently bind a hand-edited override to the layout active when the document was read. An injected host keyboard-layout adapter supplies the dispatch spellings used by defaults, overrides, and System Reserved checks. Input-source changes refresh the Keymap without rewriting Settings; recorded symbols remain literal. Overrides that collide only after layout resolution have one owner, and an override that becomes Reserved stays retained but inactive until the layout permits it again.
