# Appearance files

SpaceTerm authors its interface appearance. The application chrome wears the built-in SpaceTerm
Light or Dark colors that Appearance Mode selects, with system UI text at 13 px and weights
400/600/600. Density is the only interface preference. Terminal colors and terminal typography are
user preferences. A color-scheme import never selects the scheme, changes a font, or carries
Workspace, Pane, Terminal Session, filesystem, or terminal-program state.

The native schemas are [`appearance-settings.schema.json`](schema/appearance-settings.schema.json)
and [`color-schemes.schema.json`](schema/color-schemes.schema.json). Retained Settings use
`schema_version: 2`; Color Scheme packages use `schema_version: 1`. Implementations also enforce a
4 MiB byte limit, nesting depth 32, at most 32 schemes per import, and at most 128 installed
imported schemes. Unknown and duplicate object keys are rejected. Errors are typed and do not
include rejected contents, paths, or native errors.
Both formats resolve their shared Color Scheme shape through the immutable
[`color-scheme-definitions-v1.schema.json`](schema/color-scheme-definitions-v1.schema.json)
resource.

## Definitions

Scheme IDs use `^[a-z][a-z0-9]*([._-][a-z0-9]+)*$` and are limited to 128 ASCII bytes.
The `builtin.` namespace is reserved; the built-in schemes are `builtin.spaceterm.light` and
`builtin.spaceterm.dark`. Display names do not identify storage. A partial definition retains the
built-in fallback of its appearance for every role it omits.

Colors accept `#RGB`, `#RGBA`, `#RRGGBB` and `#RRGGBBAA`; output uses lowercase `#RRGGBBAA`.
Scheme names and attribution bounds count Unicode characters and reject control characters; the
overall document limit remains bytes. Authored Terminal colors remain opaque.

## Terminal roles

`foreground`, `background`, and `cursor` are host defaults. `normal`, `bright`, and `dim` each have
exactly eight slots ordered black, red, green, yellow, blue, magenta, cyan, white. A color authors
that slot; `null` retains its built-in fallback.
`bright_foreground` and `dim_foreground` retain the explicit default choices. `cursor_text`,
`selection_foreground`, `find_match_foreground`, and `find_active_match_foreground` accept `null`;
null means the documented effective underlying color. `selection_background`,
`find_match_background`, `find_active_match_background`, `hyperlink`, and `visual_bell` are
terminal-owned interaction paint.

## Import and export

Zed JSON is an explicit Adapter that translates a Zed theme's terminal colors. Callers list
candidates, choose a zero-based candidate index or the whole family, then explicitly install the
translated schemes. Unrelated Zed fields are ignored. Installed IDs are deterministic
`import.<source-identity-sha256>`, based on package ID when supplied, family name, author,
candidate name and appearance. Formatting, candidate ordering and color edits preserve identity.
Family/author/name changes create a new identity; source metadata never authorizes replacement.
Collisions and repeat imports require the catalog's explicit replacement intent. Source descriptors
and a canonical candidate-content fingerprint are retained separately. Missing author/family data
does not prove common ownership. Import is atomic and never selects or downloads.

Definition export preserves sparse authored intent. Effective export includes current color
overrides as complete portable copies. Built-in copies receive nonreserved identities. Installing
an effective export into a fresh catalog reproduces its colors; importing a copy repeatedly still
requires explicit replacement. The Settings buttons distinguish effective schemes, definitions
and the entire Settings document.

## Preferences and defaults

One Appearance Mode selects Light, Dark or Auto for the whole application. It selects the built-in
chrome of that appearance and the matching Terminal scheme slot. The Terminal persists one Light
and one Dark scheme ID, so changing the mode never replaces either choice. Auto resolves one System
Appearance fact. An unavailable request remains visible as the requested ID while rendering uses
the matching built-in fallback and reports a bounded diagnostic.

The `window` group holds Density (compact by default), Transparency (0.35 by default, from 0 to 1)
and Blur (on by default). Terminal defaults to 18 px monospace text, line height `20/18`, weights
400/700, italic enabled, bold-as-bright enabled, and explicit ligature-disable features. Terminal
size ranges from 8 through 32 px and line height from 1 through 2. An unavailable font request
retains its requested family while resolution supplies a suitable system fallback and Apple Color
Emoji.

Reset is typed and independently targets Appearance Mode, Density, Transparency, Blur, each
Terminal Light or Dark scheme slot, font, size, weight, line-height, italic, and bold-as-bright
field; one Terminal color override role for an exact scheme ID; each Terminal color, typography,
or rendering group; or all preferences. Resetting a role removes that override so it inherits
again. No reset removes installed schemes.

## Settings Window

The Settings Window is the interface for everything above, across four sections: Appearance,
Terminal, Color Schemes, and Privacy. It opens from the application menu and its key equivalent,
presents one navigation list beside a detail pane showing one section at a time, groups each
section's rows under a title, and searches Settings Row labels, group titles, and keywords.

One layout rule covers every row on every page: the label starts at the content's left edge, the
control ends at its right edge, and guidance stacks under the label rather than taking a line of
its own, so it stays with the setting it explains and stops where the control begins. A group is a
title and a run of rows, separated from the next group by space alone. Nothing is framed or ruled
off: the built-in dark chrome resolves the window, panel, and elevated surfaces to one color, so a
card could only ever be drawn as an outline, and a hairline between every pair of rows adds a line
for a reading the gap already gives. Rows within a group therefore sit closer together than one
group sits to the next, which the suite asserts.

Appearance presents the mode, Density, Transparency, and Blur. Terminal presents the Terminal
scheme for the current mode, or both slots under Auto, followed by type and rendering. Changes
preview live and commit shortly after the last change, so there is no save action.
See [ADR 0005](adr/0005-present-settings-in-a-separate-operating-system-window.md).

Per-role Terminal color overrides are not editable from the Settings Window. They remain supported
by the document and reachable through import and the settings file.

## Native boundary and development exerciser

Portable resolution accepts only an optional light/dark System Appearance fact and installed-font
facts. Native observation, retained settings storage, GPUI font preparation, and renderer updates
live outside this module. Resolved values are immutable and carry a monotonic runtime generation;
settings documents carry a separate `u64` revision.

Run `mise run dev:appearance` for the development-only harness in its separately identifiable
macOS application bundle. `mise run dev:appearance:macos` is the explicit platform task. The harness
uses the production settings owner and supports preview, cancel, direct save, preview save, reload,
native and Zed import, complete export, shared system selection, Terminal color, font and Density
toggles, typed resets, font refresh, and requested/effective diagnostics. `cmd-alt-a` returns to
the harness and `cmd-alt-c` toggles the shared Appearance Mode without activating it, so an open
menu, focused masked input, or modal remains the active acceptance surface. `Reset Next Field` and
`Reset Next Group` cycle the typed reset surface for manual checks. The terminal and fixture
buttons activate a live terminal window or open Alert, Dialog, ProgressDialog, menu, ComboBox,
plain-input, and obscured-input acceptance fixtures. The harness can open only when
`SPACETERM_APPEARANCE_EXERCISER=1`; its application identity gives every retained application
directory a namespace separate from ordinary and development builds.
