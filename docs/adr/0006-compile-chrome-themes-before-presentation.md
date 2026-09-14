# Compile Chrome themes before presentation

Chrome and Terminal Color Schemes and Appearance Modes are independent. Terminal colors retain
protocol ownership; Chrome changes do not reinterpret terminal palettes or OSC values. Settings
presents each Appearance Mode independently. Inactive selected Tabs remain identifiable.

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
The foundation retains requested opaque/transparent/blurred intent with an effective opaque
presentation. Public transparency requires separate native and rendered acceptance. Window owners
apply native effects once per window. A root paints its backing once; panels and fields paint their
own surfaces once; floating and critical surfaces retain deliberate backing. Transparency must not
fade text or change terminal protocol colors. Alpha replacement, opacity multiplication and
source-over are distinct operations.

Exports distinguish authored definitions from current effective portable copies. Effective exports
include user overrides and fresh install identities, including copies of built-ins. Zed remains an
external Adapter into SpaceTerm semantics. Source identity and attribution remain separate from
installed identity and content fingerprints; source backdrop intent does not silently enable native
transparency. Backward compatibility with the old development contract is not retained.
