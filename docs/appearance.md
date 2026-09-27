# Appearance files

SpaceTerm authors its interface appearance. Application Chrome wears the built-in SpaceTerm
Light or Dark colors that Appearance Mode selects, with system UI text at 13 px and weights
400/600/600. Density is the only interface preference. Terminal Themes and terminal typography are
user preferences, and a Terminal Theme colors terminal output only. Installing a theme never
selects it, changes a font, or carries Workspace, Pane, Terminal Session, filesystem, or
terminal-program state.

The retained Settings Document follows
[`appearance-settings.schema.json`](schema/appearance-settings.schema.json) at `schema_version: 3`.
Implementations also enforce a 4 MiB byte limit, nesting depth 32, and at most 512 installed
Terminal Themes. Unknown and duplicate object keys are rejected. Errors are typed and do not
include rejected contents, paths, or native errors.

## Definitions

Theme IDs use `^[a-z][a-z0-9]*([._-][a-z0-9]+)*$` and are limited to 128 ASCII bytes.
The `builtin.` namespace is reserved; the built-in themes are `builtin.spaceterm.light` and
`builtin.spaceterm.dark`. Display names do not identify storage. A partial definition retains the
built-in fallback of its appearance for every role it omits.

Colors accept `#RGB`, `#RGBA`, `#RRGGBB` and `#RRGGBBAA`; output uses lowercase `#RRGGBBAA`.
Theme names and attribution bounds count Unicode characters and reject control characters; the
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

## Zed themes

Zed's theme format is the only theme interchange format. A Zed family document is read as Zed
reads it: comments and trailing commas are accepted, and a repeated key takes its last value. A
family document may hold at most 4 MiB and 256 themes. Only terminal roles and the editor roles a
terminal needs are read; everything else in the document is ignored and never reaches Application
Chrome.

Translation is lenient in the way Zed's loader is. A color that does not parse is absent. An absent
role derives from the theme's own colors, then from the built-in theme of its appearance:

| Role | Sources, first present wins |
| --- | --- |
| background | `terminal.background`, `terminal.ansi.background`, `editor.background`, `background` |
| foreground | `terminal.foreground`, `editor.foreground`, `text` |
| normal palette | `terminal.ansi.<color>` |
| bright palette | `terminal.ansi.bright_<color>`, then the normal slot |
| dim palette | `terminal.ansi.dim_<color>`, then the normal slot mixed 35% toward the background |
| bright and dim foreground | `terminal.bright_foreground`; `terminal.dim_foreground`, then foreground mixed 40% toward the background |
| cursor | `players[0].cursor`, `editor.foreground`, foreground |
| selection | `players[0].selection`, then the cursor at alpha `0x40` |
| find match | `search.match_background`, then normal yellow at alpha `0x66` |
| active find match | `search.active_match_background`, then the find match halfway to opaque, then normal yellow at alpha `0x99` |
| hyperlink | `link_text.hover`, `text.accent`, normal blue |

Translucent protocol colors are composited over the background, so every installed theme stores a
complete, opaque protocol palette. A structurally invalid theme in a chosen family document
installs nothing from that document.

Installed IDs are `zed.<sha256>` of a source identity. A theme from a local family document is
identified by family name, author, theme name, and appearance. A theme from a Zed Extension is
identified by extension ID, theme name, and appearance, so it survives family and author renames
between versions. Installing the same identity replaces it in place and keeps every selection that
names it. Installing a Zed Extension also removes every theme an earlier version of it installed,
so an update drops themes the new version no longer ships. The origin records the family, the
theme, the extension ID and version when there is one, and a fingerprint of the source entry.

## Zed extension registry

The Themes section browses theme extensions in the Zed extension registry at `api.zed.dev`.
SpaceTerm contacts the registry only when the person browses it or installs an extension. Requests
send the user agent `SpaceTerm` and no other identifying data. They use HTTPS only, follow at most
four redirects, each HTTPS, and verify certificates against the Operating System's trust store. The listing is limited to
8 MiB and 10,000 extensions. An extension archive is limited to 16 MiB compressed, 32 MiB
unpacked, and 4,096 entries. It is read in memory: only regular `themes/*.json` files are read, at
most 512 of them, and nothing in the archive reaches the filesystem. A family document in an
extension that fails to translate is skipped, as Zed skips it; an extension without any usable
theme installs nothing.

The Settings Window can export the whole Settings Document. There is no separate theme export.

## Preferences and defaults

One Appearance Mode selects Light, Dark or Auto for the whole application. It selects the built-in
Application Chrome of that appearance and the matching Terminal Theme slot. The Terminal persists
one Light and one Dark theme ID, so changing the mode never replaces either choice. Auto resolves one System
Appearance fact. An unavailable request remains visible as the requested ID while rendering uses
the matching built-in fallback and reports a bounded diagnostic.

The `window` group holds Density (compact by default), Transparency (0.35 by default, from 0 to 1)
and Blur (on by default). Terminal defaults to 18 px monospace text, line height `20/18`, weights
400/700, italic enabled, bold-as-bright enabled, and explicit ligature-disable features. Terminal
size ranges from 8 through 32 px and line height from 1 through 2. An unavailable font request
retains its requested family while resolution supplies a suitable system fallback and Apple Color
Emoji.

Reset is typed and independently targets Appearance Mode, Density, Transparency, Blur, each
Terminal Light or Dark theme slot, font, size, weight, line-height, italic, and bold-as-bright
field; one Terminal color override role for an exact theme ID; each Terminal color, typography,
or rendering group; or all preferences. Resetting a role removes that override so it inherits
again. No typed reset removes installed themes. The Settings Window's Reset All restores every
preference and also removes every installed theme, so no selection outlives the theme it names.

## Settings Window

The Settings Window is the interface for everything above, across four sections: Appearance,
Terminal, Themes, and Privacy. It opens from the application menu and its key equivalent,
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

Appearance presents the mode, Density, Transparency, and Blur. Terminal presents type and
rendering. Themes presents the Terminal Theme for the current mode, or both slots under Auto, then
the installed themes with search, the Zed extension registry, and local Zed family import. Changes
preview live and commit shortly after the last change, so there is no save action.
See [ADR 0005](adr/0005-present-settings-in-a-separate-operating-system-window.md).

Per-role Terminal color overrides are not editable from the Settings Window. They remain supported
by the document and reachable through the settings file.

## Native boundary and development exerciser

Portable resolution accepts only an optional light/dark System Appearance fact and installed-font
facts. Native observation, retained settings storage, GPUI font preparation, and renderer updates
live outside this module. Resolved values are immutable and carry a monotonic runtime generation;
settings documents carry a separate `u64` revision.

Run `mise run dev:appearance` for the development-only harness in its separately identifiable
macOS application bundle. `mise run dev:appearance:macos` is the explicit platform task. The harness
uses the production settings owner and supports preview, cancel, direct save, preview save, reload,
Zed family import, shared system selection, Terminal color, font and Density
toggles, typed resets, font refresh, and requested/effective diagnostics. `cmd-alt-a` returns to
the harness and `cmd-alt-c` toggles the shared Appearance Mode without activating it, so an open
menu, focused masked input, or modal remains the active acceptance surface. `Reset Next Field` and
`Reset Next Group` cycle the typed reset surface for manual checks. The terminal and fixture
buttons activate a live terminal window or open Alert, Dialog, ProgressDialog, menu, ComboBox,
plain-input, and obscured-input acceptance fixtures. The harness can open only when
`SPACETERM_APPEARANCE_EXERCISER=1`; its application identity gives every retained application
directory a namespace separate from ordinary and development builds.
