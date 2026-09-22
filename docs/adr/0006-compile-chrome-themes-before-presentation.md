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
from the effective same-definition dependencies. Missing background and text use neutral fallbacks.
Missing success, warning, and error use portable semantic seeds adjusted for contrast against the
definition's own background, so a link accent cannot turn destructive feedback into an informational
color. Explicitly authored status colors and overrides remain exact. Light/Dark selects these
fallback seeds, never a hidden parent Color Scheme. Parent inheritance is unsupported. Authored
inputs remain stored separately from resolved paints, whose role provenance records authored,
overridden, derived or neutral fallback origin. This preserves intent when dependency inputs change.

Actions, persistent selection, static surfaces and statuses have separate meanings even where
initial colors match. Complete interactive paints own foreground, icon/mark, surface and border.
Disabled suppresses interaction; pressed precedes hover; selected/checked value selects its own
state family. Keyboard focus is orthogonal and never erases selected or invalid meaning. Shared
controls and application presentation own state composition, while framework Adapters convert the
result without inventing colors. Product-owned preparation supplies immutable active and inactive
variants of the complete control catalog. Each window selects its variant without changing a
process-global theme, so activating one window cannot restyle another window's retained controls.

Pane Captions retain their Terminal surface, including program changes. One contextual presentation
operation resolves caption content, controls, focus and attention against that actual surface. It
may adapt foreground contrast without modifying the authored theme or terminal protocol colors.
Compilation retains explicitly authored colors; completion only fills missing roles. Prepared
presentation may adapt those resolved paints for the actual material host, window activity, and
accessibility requirements. These adjustments leave authored definitions, role provenance, and
effective exports unchanged. Keeping the two stages separate permits readable custom controls
without turning a rendering fallback into a saved theme edit.

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
panels nor collapse onto a shared ceiling. Floating surfaces preserve the window's transmitted
material instead of adding another dense sheet. Zero keeps the opaque presentation, while one
clears the window tint and retains elevation on resting and floating surfaces. The separate blur
Setting controls native and in-window spatial filtering without changing material transmission.
Surface composition derives material fills from the opaque presentation,
which stays the contrast reference. A Pane is an ordinary resting surface in both appearances: it
paints only the accepted Terminal background's difference from the window sheet, which Light
authors above the window root and Dark below it. A Pane therefore transmits exactly what the
Transparency Setting asks of every other resting surface, and the largest surface in the window
answers that Setting instead of holding its own backing against it. A persistent selection is a
resting surface on the same terms: it paints its authored step from the chip or row beneath it
and lets the window transmit the rest, because a fill that holds its step against the opaque host
keeps its ink while the shell under it fades, and a step authored at 1.21 would then render at
twice that and more as the Setting rose. Hover is the exception, since it answers a pointer
rather than describing hierarchy and its authored step is the smallest in the scheme. An
unfocused selection is asked for the separation the focused one actually has, never for a fixed
floor above it, which is what once drove a bright unfocused chip into a dark recess. Readability
at high transmission is the reader's own choice, taken once for the whole window: a Pane is not
singled out for protection that the Chrome around it does not get. Every Pane keeps the selected
chip's neutral hairline at any transparency. The Pane rim is a boundary, since it separates the
reading surface from the Chrome. A chip rim is neither a boundary nor an indicator but a lift: it
catches the light a raised edge would so a Tab or a selected row reads as sitting above the strip
behind it, and it stays well under the Pane's rim, which is what keeps it from reading as a drawn
line. A dark scheme states the lift most quietly, because light ink on a dark strip becomes a
frame at a step a bright scheme still carries as an edge. Application preparation leaves both
built-in lifts as authored rather than raising them to the boundary floor that interactive edges
answer to. A custom definition keeps that floor, since its rim is the only edge the application
can count on, and Increase Contrast keeps it everywhere.

The desktop behind an Operating-System Window may use its platform's native effect. Everything
inside the window follows one portable GPUI floating-surface contract. Apple design is a quality
reference, while platform Adapters own only native window capabilities. Separate native popup views
would split interaction, accessibility, and lifecycle ownership across platforms, so menus, palettes,
tooltips, modals, and Pane-local overlays remain GPUI-owned. Their shared shell filters already-painted
GPUI content and bounds its color without adding framebuffer opacity. A small host-relative wash,
outer shadow and edge provide elevation. When the native window is translucent, the shell also caps
the retained framebuffer alpha according to Transparency. Preserving sampled alpha alone left dense
Terminal backings and modal scrims hiding the native material. The cap scales premultiplied color
with alpha, retaining a faint trace of filtered application content while revealing the existing
window backdrop. It does not affect the scrim outside the shell or background input blocking.
Opaque native windows keep captured coverage because they have no translucent backing to reveal.
At full coverage the alpha cap is idempotent. Repeating the color treatment leaves already-admitted
colors unchanged, so nested surfaces do not repeatedly tint the same content. Blur off skips
spatial filtering but retains the same color treatment and alpha limit. Reduce Transparency makes
both native and in-window materials opaque without erasing the user's Settings. Increase Contrast
strengthens prepared content, boundaries, and floating tones while retaining eligible transmission
and blur. Show Borders adds interactive-control edges independently. These capabilities remain
separate because a request for stronger contrast or boundaries is not a request to remove
transparency. A platform's
lack of native desktop transparency does not disable GPUI floating-surface translucency or blur.
The compiled floating tone bounds opaque GPUI content to a range with readable foregrounds. If a
custom tone admits no readable neutral foreground, its floating-only RGB moves minimally toward
the appearance endpoint; authored and resting-surface colors remain unchanged. The native material
is composited outside GPUI and cannot be sampled by this filter. Limiting framebuffer coverage
lets that same native material show through; it does not establish a contrast guarantee
against arbitrary final desktop pixels. Opaque accessibility presentation remains the deterministic
fallback. A full source-over floating tint would restore that guarantee by obscuring the native
material, which conflicts with the shared-material presentation.

The window root owns the continuous window tint. Each built-in Terminal background is authored one
small step from that tint, below it in Dark and above it in Light, so reproducing it costs little
ink and a Pane reads as a distinct region of the same window rather than as an inset. A bright
scheme spends the wider overlay, since white ink over a near-white base covers less distance.
Containers and resting controls paint only their color difference from that reference, and nested
list rows paint only their own fill. Grouped content rises from the page it sits on in both
appearances, since the ladder runs one way: a bright scheme that sank its groups instead, to keep
them off the tone a raised control takes, cut recesses into its own page. A control whose fill
then matches the group under it is carried by its edge and shadow rather than by another rung.
Explicit Terminal cell backgrounds remain opaque even when their RGB matches the default. Text and
terminal protocol colors retain their own semantics.

Controls inherit their containing surface's material. Window, Panel, Card and Floating hosts select
prepared control themes from the same catalog. Their normal, hover, pressed, selected and disabled
fills remain host-relative overlays; they do not introduce separate backdrop filters. Segmented
options resolve against their track, and selection chips resolve against their actual panel or
titlebar. Floating field and control content resolves against the composed state, while semantic
colors and focus indicators retain their meaning. A custom trigger that owns its surface also owns
its hover fill, so the wrapper cannot add another highlight underneath it.

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
