# ComboBox behavior

`spaceterm-ui::ComboBox<I>` is a controlled selector with a searchable popup.
The caller supplies the committed identity; navigating results only changes the
provisional highlight. Acceptance closes the popup before calling the owner.
The top-chrome New Workspace icon uses an ephemeral selection and routes the
chosen Workspace Source through the Workspace Manager. The bottom sidebar
button retains the full New Workspace Panel.

## Desktop interaction

- Enter, Space, Up, Down, or printable input opens the focused trigger.
- Up/Down, Home/End, and Page Up/Page Down navigate enabled options. Navigation
  does not commit a value. Enter or a primary press and release on the same
  current row accepts it once.
- Escape cancels. Tab and Shift-Tab dismiss and continue focus traversal.
  Outside presses dismiss without activating the underlying content.
- Secondary clicks, including Ctrl+click, never accept an option.
- The existing TextInput owns text editing, clipboard commands, undo, grapheme
  boundaries, and input-method composition. The popup preserves its context menu.
- Disabled, empty, and busy results do not accept stale values. Item replacement
  invalidates an outstanding pointer press and repairs provisional selection.
- Replacing the popup with a Menu, CommandPalette, or Modal transfers transient
  ownership and preserves the original focus destination.

## Geometry

The preferred side is a preference. Placement checks the opposite side when
there is insufficient room: left to right, right to left, top to bottom, and
bottom to top. Cross-axis alignment follows logical text direction and changes
alignment before shifting inside the viewport margin. When neither side can
hold the requested size, placement uses the side with more room and returns the
actual available bounds. If neither side has any room, it tries the perpendicular
axis. The result list scrolls within the available bounds.

Placement uses the current trigger bounds and viewport on each render, including
after a window resize. Resizing keeps the active option visible without pinning
ordinary wheel scrolling to it. The popup stays in the GPUI window viewport. It
does not depend on native operating-system popover windows.

Menu shares this placement policy. Its panels retain independent scroll offsets,
reveal keyboard highlights after resizing, and anchor submenus to the scrolled
parent row.

The result container owns its padding once, equally on all four sides. Rows do
not add another outer margin. The popup explicitly sets its text size and line
height so the search editor and caret do not inherit the surrounding app size.
The Workspace theme uses 12px primary text with a 16px line, a 28px search row,
30px single-line options, and 4px result padding. An icon-only trigger uses a
theme-owned 28px square, with the same editing, focus, and popup behavior.

## References and limits

- [Apple combo boxes](https://developer.apple.com/design/human-interface-guidelines/combo-boxes)
  informs control legibility. Its editable-value model differs from this
  searchable chooser.
- [Apple popover positioning](https://developer.apple.com/documentation/appkit/nspopover/show(relativeto:of:preferrededge:))
  treats the anchor and preferred edge as live positioning inputs.
- [Microsoft ComboBox guidance](https://learn.microsoft.com/en-us/windows/apps/develop/ui/controls/combo-box)
  recommends concise, single-line choices and discusses keyboard selection.
- [W3C combobox pattern](https://www.w3.org/WAI/ARIA/apg/patterns/combobox/)
  describes provisional navigation, cancellation, and preservation of standard
  text editing. Its DOM and ARIA mechanisms are web-specific.
- [Floating UI flip](https://floating-ui.com/docs/flip) and
  [Radix Popover](https://www.radix-ui.com/primitives/docs/components/popover)
  describe collision-aware placement and sizing to the available viewport.

GPUI 0.2.2 does not expose portable custom accessibility roles and active-option
relationships for ordinary elements. Logical names and state are retained, but
this component does not claim VoiceOver, Narrator, or Orca conformance. Optional
platform keyboard equivalents such as F4 are not a universal control contract.

## Visual verification

Inspect the source build with the popup open and with a single filtered result.
The selected fill should have the same surrounding inset on the left, right,
top, and bottom. Search text and its caret should have the same visual scale as
the option labels. Check keyboard selection, searching, Escape, and a source's
handoff to its picker. Rendered GPUI tests exercise edge placement, viewport
resize, and reaching the final result in a constrained popup.
