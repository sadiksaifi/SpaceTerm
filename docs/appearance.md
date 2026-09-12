# Appearance files

SpaceTerm keeps application chrome colors, chrome typography, terminal colors, and terminal
typography independent. A color-scheme import never selects the scheme, changes a font, or carries
Workspace, Pane, Terminal Session, filesystem, or terminal-program state.

The native schemas are [`appearance-settings.schema.json`](schema/appearance-settings.schema.json)
and [`color-schemes.schema.json`](schema/color-schemes.schema.json). Both formats use
`schema_version: 1`. Implementations also enforce a 4 MiB byte limit, nesting depth 32, at most 32
schemes per import, and at most 128 installed custom schemes. Unknown and duplicate object keys are
rejected. Errors are typed and do not include rejected contents, paths, or native errors.

## Identities and inheritance

Scheme IDs use `^[a-z][a-z0-9]*([._-][a-z0-9]+)*$` and are limited to 128 ASCII bytes. The
`builtin.` namespace is reserved. Display names may repeat and never identify storage. A partial
custom definition inherits the complete built-in scheme with the same kind and light/dark
classification. User overrides are stored separately by exact scheme ID. Removing an override key
means inherit; `null` is accepted only for the optional terminal foreground roles.

Built-ins are:

- `builtin.vague-pro.chrome.dark` and `builtin.vague-pro.terminal.dark`, extracted from the pinned
  Vague Pro source under its MIT license.
- `builtin.spaceterm.chrome.light` and `builtin.spaceterm.terminal.light`, owned by SpaceTerm.

Colors accept `#RGB`, `#RGBA`, `#RRGGBB`, and `#RRGGBBAA`, with red, green, blue, then alpha byte
ordering. Canonical output is lowercase `#RRGGBBAA`. Terminal foreground, background, cursor,
normal/bright/dim ANSI arrays, default bright/dim foregrounds, and optional interaction
foregrounds must be opaque. Chrome root, panel, popup, title, Tab, and text-input
surfaces must be opaque. List hover/selection, text selection, Find, scrim, shadow,
and visual-bell overlays may use alpha.

Pane Captions share their terminal's displayed background, including terminal-program changes.
Their text, typography, controls, and symmetric density-scaled padding remain Chrome-owned.

Chrome lists use separate `ghost_element_hover` and `ghost_element_selected` roles across the
sidebar, pickers, context menus, and ComboBox rows. Hover takes precedence while a selected row
is hovered. The built-ins assign matching values (`#252530` in Vague Pro Dark and `#e4e5ea` in
SpaceTerm Light), but native overrides and imported themes may distinguish them. There is no
forced equality rule or shared `list_item_background` role. Active Tabs retain the independent
`tab_active_background` role; inactive-window Tabs retain their uniform inactive band.

`element_selected_hover` serves both a selected control, such as a segmented option, and an
emphasized action button, so a scheme that equalized it with `element_selected` would leave every
primary button without hover feedback. The built-ins author it one step beyond `raised` in their
own surface family (`#2f2f3b` in Vague Pro Dark and `#dadbe2` in SpaceTerm Light). A built-in
chrome interaction fill comes from that surface family: a role Zed does not carry is authored
here, never borrowed from a terminal or selection color.

Zed imports preserve `ghost_element.hover`, `ghost_element.selected`, and `tab.active_background`
independently. This follows [Zed's list-state roles](https://github.com/zed-industries/zed/blob/main/crates/ui/src/components/list/list_item.rs),
not an assumption that all themes use equal colors. Terminal text selection remains independent.

## Chrome roles

The schema accepts only the following roles. Reusable controls derive shared menu/modal roles from
the same surface, text, border, element, status, and input roles rather than accepting duplicate
component-specific colors.

- Surfaces: `background`, `panel_background`, `elevated_surface_background`,
  `title_bar_background`, `title_bar_inactive_background`, `tab_active_background`,
  `tab_inactive_background`.
- Text and icons: `text`, `text_secondary`, `text_muted`, `text_placeholder`, `text_disabled`,
  `text_accent`, `link_text`, `link_text_hover`, `icon`, `icon_muted`, `icon_disabled`,
  `icon_accent`.
- Borders: `border`, `border_variant`, `border_focused`, `border_selected`, `border_disabled`,
  `border_transparent`.
- Filled controls: `element_background`, `element_hover`, `element_active`, `element_selected`,
  `element_selected_hover`, `element_disabled` and the corresponding `element_foreground`,
  `element_hover_foreground`, `element_active_foreground`, `element_selected_foreground`,
  `element_selected_hover_foreground`, `element_disabled_foreground`.
- Ghost controls: `ghost_element_background`, `ghost_element_hover`, `ghost_element_active`,
  `ghost_element_selected`, `ghost_element_disabled`, with corresponding foreground roles.
- Navigation: `navigation_selection`, `sidebar_focus`. Active/inactive text and icons reuse the
  primary/muted roles; list-row backgrounds use the separate ghost hover/selected roles.
- Status: `info`, `info_background`, `success`, `warning`, `warning_background`,
  `warning_border`, `error`, `error_background`, and `error_border`.
- Inputs: `input_text`, `input_placeholder`, `input_disabled_text`, `input_caret`,
  `input_selection_background`, `input_background`, `input_disabled_background`, `input_border`,
  `input_focused_border`, `input_invalid_border`.
- Modal-only semantics: `modal_scrim`, `modal_checkbox`, `modal_checkbox_selected`,
  `modal_checkbox_focused`, `modal_checkbox_disabled`. Modal surface/border/title/body/detail reuse
  elevated surface, border, primary text, and muted text.
- Scroll and resize: `scrollbar_track`, `scrollbar_track_border`,
  `scrollbar_thumb_background`, `scrollbar_thumb_border`, `scrollbar_thumb_hover_background`,
  `resize_idle`, `resize_focused`, `resize_hovered`, `resize_dragged`, `resize_disabled`.
- Elevation: `shadow`.

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

Appearance Mode is presented once, not per domain. The document keeps a separate selection for each
domain, and the one control writes Light, Dark, or Auto to both in a single edit, mapping onto a
fixed selection with that appearance or a system light/dark pair. Each domain still chooses its own
scheme within that mode, so an interface scheme and a terminal scheme remain independent; the scheme
pickers are restricted to the appearance the mode selects. A domain's scheme reset restores that
scheme and leaves the mode alone, because the mode belongs to the control that spans both. A
hand-edited document whose two selections disagree is presented using the chrome selection, and the
next change writes both back into agreement. Changes preview live and commit shortly after the last
change, so there is no save action.
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
