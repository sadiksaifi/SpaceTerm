# Appearance files

SpaceTerm keeps application chrome colors, chrome typography, terminal colors, and terminal
typography independent. A color-scheme import never selects the scheme, changes a font, or carries
Workspace, Pane, Terminal Session, filesystem, or terminal-program state.

The native schemas are [`appearance-settings.schema.json`](schema/appearance-settings.schema.json)
and [`color-schemes.schema.json`](schema/color-schemes.schema.json). Both formats use
`schema_version: 1`. Implementations also enforce a 4 MiB byte limit, nesting depth 32, at most 32
schemes per import, and at most 128 installed custom schemes. Unknown and duplicate object keys are
rejected. Errors are typed and do not include rejected contents, paths, or native errors.

## Definitions and completion

Scheme IDs use `^[a-z][a-z0-9]*([._-][a-z0-9]+)*$` and are limited to 128 ASCII bytes.
The `builtin.` namespace is reserved. Display names do not identify storage. Chrome definitions
have no implicit parent. Every source passes through the same compiler: exact user overrides,
then authored roles, then source-local dependency completion. Missing background uses neutral
`#202020` for dark or `#fafafa` for light; missing text chooses a contrasting neutral. Missing
accent uses text. A partial Terminal definition retains the existing Terminal built-in fallback.
Removing a Chrome override restores compilation from its authored definition, not a frozen snapshot.

Colors accept `#RGB`, `#RGBA`, `#RRGGBB` and `#RRGGBBAA`; output uses lowercase `#RRGGBBAA`.
Chrome roles accept straight alpha except `border_transparent`, which must have zero alpha and
means no paint. Native backdrop appearance is separate; effective foundation presentation remains
opaque. Terminal protocol foreground/background, ANSI arrays, cursor and optional interaction
foregrounds remain opaque. Optional terminal foreground roles alone accept null.

Chrome family dependencies are intentionally small:

- Background, text and accent complete structural surfaces and readable secondary content.
- Neutral and ghost interaction states complete from their own surfaces and corresponding text.
- Primary actions derive from accent, Destructive actions from error; their state tuples are
  independently authorable and do not use persistent-selection roles.
- Persistent selection derives from selection inputs; selected hover, pressed and disabled paints
  remain independently authorable. Navigation rows pair primary, secondary, matching text and icons
  with each actual state surface.
- Toggle off/on families complete surface, mark, label and border for every interaction state.
- Field placeholder and caret follow field content/surface; focus and invalid borders remain
  separate. Shared field presentation composes them without replacing invalidity with focus.
- Status backgrounds follow their corresponding status foreground with multiplied opacity;
  scrim follows background. Explicit status surfaces or scrims remain unchanged by seed overrides.
- Badges and hyperlink previews use static surfaces. Scrollbar idle, hover and dragging are distinct.

The canonical role registry is published in the schema. All supplied roles remain explicit;
missing dependent foregrounds may choose a contrasting neutral. Ordinary text targets 4.5:1 and
necessary indicators 3:1. Disabled and decorative treatment has narrower obligations.

Pane Captions share the actual Terminal surface, including program changes. A contextual paint
operation resolves their content, controls, attention and focus against that surface. Inactive
selected Tabs retain visible identity. Window background appearance retains source intent without
silently enabling native transparency. See [ADR 0006](adr/0006-compile-chrome-themes-before-presentation.md).

## Terminal roles

`foreground`, `background`, and `cursor` are host defaults. `normal`, `bright`, and `dim` each have
exactly eight entries ordered black, red, green, yellow, blue, magenta, cyan, white.
`bright_foreground` and `dim_foreground` retain the explicit default choices. `cursor_text`,
`selection_foreground`, `find_match_foreground`, and `find_active_match_foreground` accept `null`;
null means the documented effective underlying color. `selection_background`,
`find_match_background`, `find_active_match_background`, `hyperlink`, and `visual_bell` are
terminal-owned interaction paint.

Zed JSON is an explicit Adapter. Callers list candidates, choose a zero-based candidate index and
one or both kinds, then explicitly install the translated schemes. Unrelated Zed fields are
ignored. IDs are deterministic `import.<sha256>.<index>.<kind>`. Import is atomic and never selects
or downloads anything.

## Preferences and defaults

Chrome and terminal scheme selection are independent. Each accepts either a fixed scheme with an
explicit expected appearance, or separate light and dark IDs selected from the current System
Appearance. An unavailable request remains visible as the requested ID while rendering uses the
matching built-in fallback and reports a bounded diagnostic.

Chrome defaults to 13 px system UI text with weights 400/600/600 and compact density. Its size
range is 10 through 24 px. Terminal defaults to 18 px monospace text, line height `20/18`, weights
400/700, italic enabled, bold-as-bright enabled, and explicit ligature-disable features. Terminal
size ranges from 8 through 32 px and line height from 1 through 2. Unavailable font requests retain
their requested family while resolution supplies a suitable system fallback and Apple Color Emoji.

Reset is typed and independently targets every scheme-selection, per-domain scheme choice, font,
size, weight, line-height, italic, bold-as-bright, and density field; both domains' scheme
selections at once; one color override role for an exact scheme ID; each
color, typography, density, or rendering group; or all appearance preferences. Resetting a role
removes that override so it inherits again. No reset removes installed custom schemes.

## Settings Window

The Settings Window is the interface for everything above, across four sections: Appearance,
Interface, Terminal, and Color Schemes. It opens from the application menu and its key equivalent,
presents one navigation list beside a detail pane showing one section at a time, groups each
section's rows under a title, and searches Settings Row labels, group titles, and keywords.

One layout rule covers every row on every page: the label starts at the content's left edge, the
control ends at its right edge, and guidance stacks under the label rather than taking a line of
its own, so it stays with the setting it explains and stops where the control begins. A group is a title and a run of
rows, separated from the next group by space alone. Nothing is framed or ruled off: a scheme may
resolve the window, panel, and elevated surfaces to one color, as the built-in dark scheme does, so
a card could only ever be drawn as an outline, and a hairline between every pair of rows adds a
line for a reading the gap already gives. Rows within a group therefore sit closer together than
one group sits to the next, which the suite asserts.

Chrome and Terminal Appearance Modes each present Light, Dark and Auto independently. Editing
one preserves the other domain's policy and scheme choice. Changes preview live and commit shortly
after the last change, so there is no save action.
See [ADR 0005](adr/0005-present-settings-in-a-separate-operating-system-window.md).

Per-role color overrides are not editable from the Settings Window. They remain supported by the
document and reachable through import and the settings file.

## Native boundary and development exerciser

Portable resolution accepts only an optional light/dark System Appearance fact and installed-font
facts. Native observation, retained settings storage, GPUI font preparation, and renderer updates
live outside this module. Resolved values are immutable and carry a monotonic runtime generation;
settings documents carry a separate `u64` revision.

Run `mise run dev:appearance` for the development-only harness with an isolated retained Config
root, or `mise run dev:appearance:macos` for the separately identifiable macOS bundle. The harness
uses the production settings owner and supports preview, cancel, direct save, preview save, reload,
native and Zed import, complete export, system selection, independent color/font/density toggles,
typed resets, font refresh, and requested/effective diagnostics. `cmd-alt-a` returns to the harness
and `cmd-alt-c` toggles its Chrome preview without activating it, so an open menu, focused masked
input, or modal remains the active acceptance surface. `Reset Next Field` and `Reset Next Group`
cycle the typed reset surface for manual checks. The terminal and fixture buttons activate a
live terminal window or open Alert, Dialog,
ProgressDialog, menu, ComboBox, plain-input, and obscured-input acceptance fixtures. The harness can
open only when `SPACETERM_APPEARANCE_EXERCISER=1`; its startup path validation prevents access to the
ordinary settings root.
