# Compile Chrome themes before presentation

Chrome and Terminal Color Schemes remain independent under one application-scoped Appearance Mode.
The shared Light, Dark or Auto choice selects the matching persisted slot from both scheme families.
Terminal colors retain protocol ownership; Chrome changes do not reinterpret terminal palettes or
OSC values. Inactive selected Tabs remain identifiable.

Settings persists the Appearance Mode once and persists Light and Dark Color Scheme slots inside
each appearance family. This makes contradictory Chrome and Terminal modes unrepresentable while
preserving both families' choices across mode changes and restarts. Resetting the shared mode does
not reset either family's scheme slots.

The Chrome Theme Compiler owns one ordered dependency program for built-ins, native definitions,
Zed imports and live overrides. Exact user overrides precede authored values. Missing roles derive
from the effective same-definition dependencies. Only missing background and text use neutral
fallbacks; Light/Dark does not select a hidden parent. Parent inheritance is unsupported. Authored
inputs remain stored separately from resolved paints, whose role provenance records authored,
overridden, derived or neutral fallback origin. This preserves intent when dependency inputs change.

Actions, persistent selection, static surfaces and statuses have separate meanings even where
initial colors match. Complete interactive paints own foreground, icon/mark, surface and border.
Disabled suppresses interaction; pressed precedes hover; selected/checked value selects its own
state family. Keyboard focus is orthogonal and never erases selected or invalid meaning. Shared
controls and application presentation own state composition, while framework Adapters convert the
result without inventing colors.

Pane Captions retain their Terminal surface, including program changes. One contextual presentation
operation resolves caption content, controls, focus and attention against that actual surface. It
may adapt foreground contrast without modifying the authored theme or terminal protocol colors.
Custom theme roles otherwise retain explicitly authored colors; completion is a fallback, not an
unrequested rewrite of author decisions.

Window background appearance is distinct from Light/Dark and from each straight RGBA color.
Application Settings own transparency and blur independently of scheme authorship. Native and
accessibility capabilities determine the effective presentation without discarding those Settings.
Window owners apply native effects once per window, and SpaceTerm owns the blurred backdrop itself
rather than accepting a framework effect that rewrites a native material's private layers. Depth
comes from one neutral ladder in both appearances: the base is the darkest (Dark) or most shaded
(Light) rung, and chips, controls and floating surfaces rest lighter or brighter on it. One
Setting controls transmission through a continuous window tint. Resting Chrome surfaces use minimal-alpha
color overlays against the opaque scheme reference. One neutral elevation ladder is compressed
into the overlay each appearance may spend, so near-white Light surfaces neither recreate opaque
panels nor collapse onto a shared ceiling. Floating surfaces over content use a denser curve;
zero keeps the opaque presentation, while one clears the window tint and retains color on resting
and floating surfaces. The separate blur Setting controls the native material.
Surface composition derives material fills from the opaque presentation,
which stays the contrast reference. A translucent Pane lifts its default backdrop toward the
elevated surface. Dark Panes also retain a translucent Terminal-colored backing beneath that lift:
thin elevation tints alone transmit too much desktop variation behind muted ANSI colors.
Readability takes precedence over keeping a Pane lighter than its surroundings on bright backdrops.
Light keeps its existing material. Every Pane keeps the selected chip's neutral hairline at any transparency. GPUI cannot blur
content inside the window, so floating surfaces tint rather than blur what they cover.

The window root owns the continuous window tint. Containers and resting controls paint only their color
difference from that reference, and nested list rows paint only their own fill. Explicit Terminal cell
backgrounds remain opaque even when their RGB matches the default. Text and terminal protocol
colors retain their own semantics.

GPUI's rectangular descendant clipping requires terminal content to end above the Pane's bottom
corner arcs. A small bottom inset preserves those rounded edges without an opaque overpaint.
The empty corner fillets and Split gaps each own one Chrome fill.
Authored RGBA remains separate. Alpha replacement, opacity multiplication and source-over are
distinct operations. GPUI 0.2.2 is locally patched because its macOS main and path-sprite
pipelines add destination alpha while blending RGB with source-over. Correct destination-alpha
attenuation is required for layered translucent surfaces; theme colors cannot compensate for it.

Exports distinguish authored definitions from current effective portable copies. Effective exports
include user overrides and fresh install identities, including copies of built-ins. Zed remains an
external Adapter into SpaceTerm semantics. Source identity and attribution remain separate from
installed identity and content fingerprints; source backdrop intent does not silently enable native
transparency. Backward compatibility with the old development contract is not retained.
