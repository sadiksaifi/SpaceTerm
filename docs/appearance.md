# Appearance files

SpaceTerm keeps application chrome colors, chrome typography, terminal colors, and terminal
typography independent. A color-scheme import never selects the scheme, changes a font, or carries
Workspace, Pane, Terminal Session, filesystem, or terminal-program state.

The native schemas are [`appearance-settings.schema.json`](schema/appearance-settings.schema.json)
and [`color-schemes.schema.json`](schema/color-schemes.schema.json). Retained Settings use
`schema_version: 2`; Color Scheme packages use `schema_version: 1`. Implementations also enforce a
4 MiB byte limit, nesting depth 32, at most 32 schemes per import, and at most 128 installed custom
schemes. Unknown and duplicate object keys are rejected. Errors are typed and do not include
rejected contents, paths, or native errors.
Both formats resolve their shared Color Scheme shapes through the immutable
[`color-scheme-definitions-v1.schema.json`](schema/color-scheme-definitions-v1.schema.json)
resource.
The unreleased retained Settings v1 development format is not migrated.

## Definitions and completion

Scheme IDs use `^[a-z][a-z0-9]*([._-][a-z0-9]+)*$` and are limited to 128 ASCII bytes.
The `builtin.` namespace is reserved. Display names do not identify storage. Chrome definitions
have no implicit parent. Every source passes through the same compiler: exact user overrides,
then authored roles, then source-local dependency completion. Missing background uses neutral
`#202020` for dark or `#fafafa` for light; missing text chooses a contrasting neutral. Missing
accent uses text. A partial Terminal definition retains the existing Terminal built-in fallback.
Removing a Chrome override restores compilation from its authored definition, not a frozen snapshot.

Colors accept `#RGB`, `#RGBA`, `#RRGGBB` and `#RRGGBBAA`; output uses lowercase `#RRGGBBAA`.
Scheme names and attribution bounds count Unicode characters and reject control characters; the overall document limit remains bytes.
Chrome roles accept straight alpha except `border_transparent`, which must have zero alpha and
means no paint. Native backdrop appearance is separate; effective foundation presentation remains
opaque. Rendering prepares one canonical backing: root uses its own RGB opaque, panel and elevated
surfaces compose on that root, and fields compose on the canonical panel. The prepared field is
fully backed even when hosted on another surface, so derived text sees exactly that paint. Structural
no-paint ghost backgrounds remain transparent. Authored RGBA stays retained for export and later
native transparency delivery. Authored Terminal colors remain opaque. ANSI arrays use eight
nullable slots, where `null` inherits that slot from the built-in fallback. Optional scalar
terminal foreground roles also accept null.

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
exactly eight slots ordered black, red, green, yellow, blue, magenta, cyan, white. A color authors
that slot; `null` retains its built-in fallback.
`bright_foreground` and `dim_foreground` retain the explicit default choices. `cursor_text`,
`selection_foreground`, `find_match_foreground`, and `find_active_match_foreground` accept `null`;
null means the documented effective underlying color. `selection_background`,
`find_match_background`, `find_active_match_background`, `hyperlink`, and `visual_bell` are
terminal-owned interaction paint.

Zed JSON is an explicit Adapter. Callers list candidates, choose a zero-based candidate index and
one or both kinds, then explicitly install the translated schemes. Unrelated Zed fields are
ignored. Installed IDs are deterministic `import.<source-identity-sha256>.<kind>`, based on package ID
when supplied, family name, author, candidate name and appearance. Formatting, candidate ordering
and color edits preserve identity. Family/author/name changes create a new identity; source metadata
never authorizes replacement. Collisions and repeat imports require the catalog's explicit
replacement intent. Source descriptors and a canonical candidate-content fingerprint are retained
separately. Missing author/family data does not prove common ownership.

Zed Chrome import maps structural background/title/Tab colors, text/icon/border variants, neutral
and ghost element states, info/success/warning/error colors and surfaces, scrollbar track/border/
idle/hover/active colors, the first player's selection and `background.appearance`. Missing and null
Zed values stay absent; SpaceTerm-specific action/toggle/field states compile from those authored
inputs. Unsupported syntax/editor roles are ignored. Source backdrop intent is retained while
foundation native presentation remains opaque. Import is atomic and never selects or downloads.

Definition export preserves sparse authored Chrome intent. Effective export includes current color
overrides as complete portable copies. Built-in copies receive nonreserved identities. Installing
an effective export into a fresh catalog reproduces its colors; importing a copy repeatedly still
requires explicit replacement. The Settings buttons distinguish effective schemes, definitions
and the entire Settings document.

## Preferences and defaults

One Appearance Mode selects Light, Dark or Auto for the whole application. Chrome and Terminal each
persist their own Light and Dark scheme IDs, so changing the shared mode never replaces either
surface's scheme choice. Auto resolves one System Appearance fact and selects the matching slot in
both families. An unavailable request remains visible as the requested ID while rendering uses the
matching built-in fallback and reports a bounded diagnostic.

Chrome defaults to 13 px system UI text with weights 400/600/600 and compact density. Its size
range is 10 through 24 px. Terminal defaults to 18 px monospace text, line height `20/18`, weights
400/700, italic enabled, bold-as-bright enabled, and explicit ligature-disable features. Terminal
size ranges from 8 through 32 px and line height from 1 through 2. Unavailable font requests retain
their requested family while resolution supplies a suitable system fallback and Apple Color Emoji.

Reset is typed and independently targets the shared Appearance Mode, each per-domain Light or Dark
scheme slot, font, size, weight, line-height, italic, bold-as-bright, and density field; one color
override role for an exact scheme ID; each
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

One Appearance Mode presents Light, Dark and Auto for the whole application. Chrome and Terminal
retain separate Color Scheme choices for each mode. Changes preview live and commit shortly after
the last change, so there is no save action.
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
native and Zed import, complete export, shared system selection, independent color/font/density toggles,
typed resets, font refresh, and requested/effective diagnostics. `cmd-alt-a` returns to the harness
and `cmd-alt-c` toggles the shared Appearance Mode without activating it, so an open menu, focused
masked input, or modal remains the active acceptance surface. `Reset Next Field` and `Reset Next Group`
cycle the typed reset surface for manual checks. The terminal and fixture buttons activate a
live terminal window or open Alert, Dialog,
ProgressDialog, menu, ComboBox, plain-input, and obscured-input acceptance fixtures. The harness can
open only when `SPACETERM_APPEARANCE_EXERCISER=1`; its startup path validation prevents access to the
ordinary settings root.
