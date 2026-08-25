# SpaceTerm Context

This document is canonical for SpaceTerm's product intent, technology decisions, UI constraints,
and architectural principles. Domain terminology, relationships, and invariants are canonical in
`docs/UBIQUITOUS_LANGUAGE.md`.

## Product intent

SpaceTerm is a modern native macOS terminal. Its layout hierarchy takes useful inspiration from
tmux, but SpaceTerm is not a tmux client and has no tmux-style server/client model.

## Technology decisions

- Use Rust 2024 and GPUI for native UI and GPU rendering.
- Keep the executable in the root application crate and reusable GPUI controls in the internal
  `spaceterm-ui` library crate.
- Use `libghostty-vt` for terminal emulation.
- Use a macOS PTY to launch and communicate with shells.
- Use `gpui-symbols` for native macOS SF Symbols.
- Ship the SpaceTerm application on macOS and keep native platform integration macOS-specific.
  Reusable `spaceterm-ui` controls render entirely through GPUI and remain platform-neutral for
  possible future Linux and Windows support; do not create speculative platform adapters.
- Treat Vague Pro as the only application and terminal theme and as the source of all brand colors.
  Reuse the tokens in `src/theme.rs`; never hardcode or redefine colors in UI, terminal, platform,
  or domain code.
- Prefer `JetBrainsMono Nerd Font` for terminal text and use a sensible system monospace fallback
  when it is unavailable.

## UI direction

- Build the interface directly with GPUI as a compact, Zed-like desktop experience.
- Use `gpui-symbols` for icons and native macOS menus and system dialogs where appropriate.
- Keep reusable SpaceTerm controls in the small internal `spaceterm-ui` crate. Application UI
  Modules compose those controls with product behavior and Vague Pro presentation.

## Architecture principles

### Deep Modules

Prefer Modules with a small Interface, significant hidden Implementation, and strong invariants.
Do not expose terminal emulation, PTY ownership, or Pane Layout mechanics throughout the UI.

### Narrow Interfaces

Keep Interfaces focused on what their callers need. Avoid broad traits with unrelated methods,
and do not pass application-wide state into every Module.

### Dependency injection

Use constructor injection and explicit ownership at meaningful replaceability Seams such as
Terminal Session creation, PTY creation, and platform integration. Inject clocks or scheduling only
when needed. Domain Modules own hierarchy identity allocation; do not recreate identity counters in
the UI. Avoid global mutable singletons and unnecessary factory layers.

### Policy and mechanism

Keep product policy, including minimum entity counts and focus fallback, inside testable domain
Modules. Keep GPUI rendering, PTY syscalls, and Ghostty integration outside the domain model.

### Encapsulation

Express domain changes through intentional operations such as `create_workspace`,
`close_workspace`, `create_window`, `close_window`, `split_pane`, `close_pane`, `focus_pane`, and
`resize_split`. Do not permit arbitrary external mutation of collections or active and focused
identities.

### Error handling

Use typed errors when callers must handle failure and attach actionable context to infrastructure
errors. Do not use `unwrap()` or `expect()` in ordinary runtime paths unless a documented invariant
makes failure impossible. Never silently ignore PTY, process, or terminal-emulation errors.

### Simplicity

Prefer the root application crate plus the single internal UI library over a workspace of many
small crates. Add another crate only when it creates a meaningful Seam, Leverage, or Locality.

## Current architectural decisions

### Internal UI Library

`spaceterm-ui` owns reusable GPUI control mechanics behind narrow, application-independent
Interfaces. It may own text editing, focus, keyboard, pointer, accessibility, and rendering
mechanisms, but it never defines product colors or imports application, terminal, platform, or
domain Modules. Reusable controls expose bounded visual variants and native control sizes rather
than accepting one-off call-site paint definitions. The root application installs the Vague Pro
paint and metric catalog, then owns surrounding chrome, product policy, and reactions to control
events. Application-specific composition remains in `src/ui`; controls move into the library only
when their behavior has reusable depth.

The internal library's Resize Handle is a keyed, platform-neutral GPUI divider with a mandatory
logical name and an explicitly named movement axis. It owns the enlarged pointer hitbox, resize
cursor, hover, press and keyboard-focus presentation, pointer capture, cumulative displacement,
keyboard steps, reset activation, and ordered typed interaction and cancellation lifecycle. It
requests logical values while preserving the caller's authoritative value throughout an active
interaction. The application owns Pane Layout ratios, Pane minimum dimensions, Workspace sidebar
bounds and collapse policy, terminal focus coordination, and every other product-specific resize
policy.

The internal library's Window Drag Region is a keyed, platform-neutral interaction control with a
mandatory logical name and a bounded logical movement threshold. It owns primary-pointer gesture
eligibility, software capture outside its bounds, explicit pressed and move-requested lifecycle,
exactly-once move requests, double activation, cancellation, child-control exclusion, and event
propagation. A retained read-only status handle lets application policy derive whether the control
owns an interaction without duplicating its pointer lifecycle. The control adds no presentation.
The application owns top-chrome layout and paint, Terminal Input Focus coordination, actual
Operating-System Window movement, and zoom, maximize, restore, or preference policy for double
activation. One injected macOS platform adapter retains the original primary mouse-down, targets
the exact GPUI-backed Operating-System Window, and hands accepted movement to AppKit. The adapter's
accepted response ends the control-owned active interaction because native movement may consume the
pointer release.

The internal library's Tooltip is a keyed, platform-neutral, Operating-System Window-scoped
transient for bounded primary, secondary, and keyboard-equivalent text. It owns delayed hover,
cancellation generations, single-window presentation ownership, target-anchored placement with
flipping and clamping, pointer-transparent rendering, and suppression beneath menus and the Command
Palette. A layout-transparent target adapter supports arbitrary GPUI elements, while one root layer
observes keyboard input without consuming it. Disabled or removed targets, pointer activation,
window deactivation, dragging, and higher-priority transients cancel pending and visible tooltips.
The application supplies Vague Pro paint and metrics and retains every target's independent logical
accessibility name; interactive help remains a Menu or popover rather than a Tooltip.

The internal library's Text Input is an entity-backed, platform-neutral single-line editor with
stable identity and a mandatory logical name. It exclusively owns bounded text, directional
grapheme Selection, input-method composition, undo and redo, clipboard editing, pointer capture
and horizontal autoscroll, focus, caret scheduling, and cached shaping. Typed content-free events
carry monotonic revisions and semantic edit sources; callers read the current value only when
needed. The application installs its Vague Pro variants and explicitly selects a platform
keybinding profile, while labels, borders, validation, search icons, and composite lifecycle policy
remain outside the editor.

The internal library's Command Palette is an entity-backed, Operating-System Window-owned
transient control. It owns query editing, static semantic matching, stable typed selection,
keyboard and pointer navigation, focus restoration, and lifecycle events while callers own item
identity, asynchronous result production, and product actions. Its rows use fixed leading, text,
and trailing semantic slots rather than caller-painted layouts, and the application installs its
Vague Pro presentation catalog.

The palette presents one continuous surface: a borderless editor above a hairline, a
variable-height virtualized result list, and an optional hint and actions footer. Search-line
controls, footer hints, section headings, and the footer actions menu are typed caller values, not
caller-painted elements, so the palette remains the only owner of their size and paint. Rows are
grouped only where adjacent items report different sections. The palette renders its own anchored
full-window layer without deferring it, because GPUI collects deferred draws once per frame and a
deferred palette could not host the deferred menu overlay its footer owns; its owner therefore
renders it last. While a menu owns the Operating-System Window, palette blur and outside presses
are not dismissals. Activating a Command Palette or Menu first establishes internal closed and
focus state, then delivers the typed activation while caller-owned transient blocking remains
continuous, and finally publishes the activated close lifecycle event; non-activation close
ordering is unchanged.

### Overlay Scrollbar

The Overlay Scrollbar is a compact vertical control that presents scroll position without reserving
layout space. It owns thumb geometry, transient visibility, hover retention, drag capture, and
Vague Pro-themed rendering. Terminal Scrollback and the Workspace list adapt their native offset units
at its Interface.

### Identity Ownership

Each hierarchy Module allocates the identities it owns. `WorkspaceCollection` allocates Workspace
IDs, `WindowCollection` allocates Window IDs, and `TerminalWindow` allocates Pane and Split IDs.
Creation factories receive the generated identity so infrastructure can bind events without
manufacturing domain state. Identities are monotonic and are not reused after deletion.

### Native PTY Owner

`SpawnedPty` exclusively owns the macOS PTY master, reader, writer, and Shell Process. PTY
configuration, Shell Process launch, and Terminal Emulator initialization occur on the terminal
worker; Terminal Session creation returns after starting that worker, and startup errors arrive as
typed terminal events. The worker installs a deferred one-shot signaller so a close racing startup
still requests prompt termination. Once launched, only the worker may wait, perform full
termination escalation, or reap, so close callers never wait for process cleanup. Shutdown sends
one SIGHUP to the complete owned process group, allows a bounded grace period, then sends SIGKILL to
that group when any member remains. The worker reaps the Shell Process exactly once after reader
output and publishes typed normal, signal, graceful-shutdown, or forced-shutdown exit state.

### Temporary Shell Integration

The Shell Launch Plan may inject versioned SpaceTerm resources into supported interactive Bash,
Zsh, Fish, Nushell, and Elvish processes without writing user configuration. Each shell keeps its
normal login/startup ordering: Zsh restores and sources the original `ZDOTDIR`, modern Bash
restores `ENV` after its POSIX bootstrap, and XDG-aware shells prepend then remove the packaged
resource directory. Apple `/bin/bash`, unknown shells, disabled integration, and missing or
mismatched resources fall back to an unchanged login launch. Integration scripts emit only
bounded OSC 7 directory and OSC 133 prompt/command marks consumed by Terminal Metadata. The same
resource version is discovered from `Contents/Resources` in a packaged app and `assets` during
development, and package verification requires every supported shell resource.

### Terminal Capability and Compatibility Identity

One Terminal Capability Identity owns the SpaceTerm program name and version, preferred and
fallback `TERM` values, true-color marker, XTVERSION and device-attribute replies, and the bounded
XTGETTCAP allowlist. A Shell Launch Plan selects `xterm-spaceterm` only when its compiled entry is
discoverable in packaged resources and otherwise keeps `xterm-256color`; the selected terminfo
root is passed explicitly to the Shell Process. The generated entry inherits the established
xterm-256color baseline and adds only direct color, authorized clipboard, and cursor-shape
capabilities already implemented by the runtime. Packaging compiles the source with `tic`, and
verification resolves the entry from both the signed app and mounted DMG.

Until name-gated terminal applications discover Kitty graphics through its protocol query or
recognize SpaceTerm directly, Shell Processes receive the narrow Compatibility Identity
`TERM_PROGRAM=ghostty` by default. `SPACETERM=1`, SpaceTerm's own version, `TERM`, terminfo,
XTVERSION, device attributes, and XTGETTCAP remain authoritative; the compatibility alias neither
changes nor expands any supported terminal protocol. SpaceTerm does not set Ghostty resource,
window, socket, or remote-control variables and removes inherited terminal-emulator and
multiplexer markers before launch, so a Shell Process cannot accidentally bind to the runtime that
launched SpaceTerm. Compatibility Identity is an ecosystem adapter, never protocol authority.

### Bounded PTY Output Transport

Terminal Session control Commands, including Shutdown, use a reliable latency-sensitive lane. PTY
reader events use a separate bounded queue with backpressure and ordered worker notifications.
Workers coalesce only consecutive output notifications up to a control boundary; reader completion
remains reliable and ordered after its output. Closing the receiver wakes a blocked reader producer
so terminal cleanup can continue off-thread.

### Immutable Terminal Presentation Snapshots

Each Terminal Session worker exclusively mutates its Terminal Emulator and publishes owned semantic
snapshots to GPUI. Snapshots retain shared identity for unchanged rows and metadata, carry cursor,
viewport, active-screen, size, static graphics, and precise damage independently of cell content, and cross the
worker boundary through a bounded latest-screen channel. GPUI renders only these immutable values
and never borrows Terminal Emulator state.

Terminal-controlled title, working-directory, prompt, command, and progress facts are sanitized
inside the Terminal Session worker and published as immutable Terminal Metadata in the same
snapshot. OSC 7 accepts only absolute `file://` paths with an empty, `localhost`, or verified local
authority; malformed and remote reports cannot replace the last valid value. OSC 133 semantic
zones and command completion retain bounded command text, exit status, and injected-monotonic-clock
duration, while OSC 9;4 progress is clamped to its protocol range. Every metadata value carries
session provenance and freshness, metadata-only changes reuse row identity, and completion marks
the final metadata stale before the typed lifecycle event. Pane and Window chrome consume only the
owning snapshot's resolved sanitized title and never parse terminal controls or query live emulator
state.

### Static Kitty Graphics

SpaceTerm implements the static direct-media subset of the Kitty graphics protocol: RGB, RGBA, and
PNG transmissions; direct chunking and zlib compression; query, transmit, transmit-and-display,
later display, replacement, and deletion; and normal and Unicode-placeholder placements. The
Terminal Session worker is the sole protocol and image-state mutator. `libghostty-vt` resolves
placement identity, source crop, destination backing pixels, cursor-relative geometry, scrolling,
reflow, active-screen isolation, and U+10EEEE placeholder cells before SpaceTerm copies borrowed
state into immutable snapshots. Unchanged image generations reuse owned RGBA buffers while
placement geometry and image-content damage remain independent.

The runtime starts every Terminal Emulator with Kitty storage disabled and enables it lazily only
when a bounded application reservation is available, so a successful protocol query is a truthful
rendering claim. Each Primary or Alternate Screen is limited to 96 MiB and 8192 pixels per
dimension; one graphics-enabled Terminal Session reserves 192 MiB, and application-wide decoded
storage is limited to 384 MiB. Encoded APC input is limited to 128 MiB. File, temporary-file, and
shared-memory media and all animation actions remain disabled and cannot read host files. Kitty
support remains queryable through the official protocol and is not added to terminfo. The narrow
Compatibility Identity enables legacy name-gated applications to attempt the implemented static
subset; unsupported inputs still receive the protocol-defined rejection behavior.

GPUI caches `RenderImage` values by image ID and content generation, converts RGBA to its BGRA
upload format once, and explicitly removes stale atlas entries on replacement, deletion, quota
eviction, screen ownership changes, and Pane release. The cache is application-bounded to 384 MiB.
Candidate resources and immutable placement geometry are staged transactionally; they become
authoritative only after scene submission succeeds, while a failed attempt retains the last valid
resource set and Presentation Generation. Unchanged content and placement generations, active
screen, grid geometry, and backing scale reuse one Arc-backed placement paint plan without
reconstructing image geometry. Source cropping paints transformed full-image bounds through nested
destination and grid masks.
Paint order is terminal base background; images with `z < -1073741824`; cell, search, Selection,
and Cursor backgrounds; images with `-1073741824 <= z < 0`; glyphs and decorations; images with
`z >= 0`; then Marked Text and its caret. Equal-z placements use increasing image ID; Marked Text
intentionally remains usable above above-text images. Backing-scale changes rebuild placement
geometry without recopying decoded pixels.

### Conventional Terminal Conformance Corpus

The test-only corpus in `src/terminal/conformance.rs` is the release gate for every advertised
conventional terminal capability. Each fixture names its owning issue, covered user stories,
authoritative protocol key, oracle class, and deterministic step/input/output budgets. The runner
executes real Terminal Emulator, Session, platform, and UI reducers without user configuration,
network access, or mutable global application state. Byte goldens preserve protocol output;
semantic goldens preserve row and cell order, Presentation Generation, Cursor, screen, and
Terminal Metadata identity. Failures report the fixture, exact step, field, expected value, and
observed value.

Published specifications are authoritative. The audited Ghostty revision is only a behavioral
reference. Static Kitty graphics is covered by its protocol authority; Sixel, iTerm inline images,
and animated or host-file Kitty media remain outside the corpus. The fixture matrix, authority
catalog, audit revision, and maintenance rules are canonical in `docs/CONFORMANCE.md`. `just test`
and therefore `just validate` run the corpus; `just conformance` provides the focused loop.

### Scoped Terminal Attention

BEL and the transition to finished Command Metadata become typed Terminal Attention events on the
owning Terminal Session lane; they never carry command text. Each Pane reduces only its own events
against explicit Terminal Input Focus, active-surface, key-window, and application facts using an
injected monotonic clock. Repeated bells are suppressed within 100 milliseconds, Dock requests are
limited to one per second, and inactive-only notifications aggregate for five seconds. Vague Pro visual
bells and Pane/Window unread indicators never move focus. Focus gain or accepted key input clears
eligible state and cancels the outstanding native Dock request. AppKit audio, Dock, and notification
effects sit behind one testable platform Seam and native notification policy.

### Transactional Terminal Presentation

The Terminal Emulator owns independent Primary Screen and Alternate Screen presentation caches;
entering the Alternate Screen never transfers Scrollback, and leaving it restores the Primary
Screen's content, Cursor, and Terminal Viewport. Primary Scrollback is bounded to 10,000 retained
rows. New output follows the bottom only when the Terminal Viewport was already following it, while
a scrolled Terminal Viewport and Selection remain anchored to logical content across output and
grid reflow.

Every published Terminal Presentation snapshot carries a monotonic Presentation Generation.
Scrollbar, pointer, and wheel mappings return that generation through the reliable Terminal
Session command lane; the worker rejects mappings that no longer describe its presented grid, and
GPUI rejects older snapshots. Grid resize reflows logical content and publishes full resize damage.
A pixel-only resize still reaches both the PTY and Terminal Emulator, but does not manufacture grid
damage or advance the Presentation Generation when no visible state changed.

DEC 2026 synchronized output is one presentation transaction: intermediate snapshots are
suppressed, then one completed snapshot is published when the transaction ends. A worker-owned
one-second deadline prevents a stalled producer from freezing presentation, and resize or terminal
exit flushes pending synchronized output before later screen or lifecycle events.

### Semantic Terminal Colors

Terminal Presentation snapshots preserve default, palette-indexed, and RGB color sources instead
of flattening them into display colors. Each snapshot also owns the active defaults, complete
palette, and terminal-wide reverse state needed to resolve those sources immutably. GPUI applies
bold-as-bright only to normal ANSI foreground indices, then composes per-cell inverse with
terminal-wide reverse. Selection and Cursor colors remain separate presentation inputs, and a
color-state change invalidates prepared paint rows without changing unchanged snapshot row
identity.

### Negotiated Cursor Presentation

The Terminal Emulator snapshot normalizes every visible cursor to its viewport row, wide-grapheme
head, and occupied cell width while preserving negotiated shape, blink request, visibility, cursor
color, and cursor-text color independently from cell and selection colors. Cursor-only changes
damage only the previously and currently occupied viewport rows. Terminal Presentation renders a
filled block above backgrounds and selections and recolors its covered glyph, while bar and
underline cursors add thin geometry without replacing text.

A focused visible cursor whose negotiated state requests blinking participates in the Pane's
single 600-millisecond presentation clock. Accepted terminal key input and Terminal Input Focus
gain reveal the cursor immediately and restart the clock from a new scheduling generation. Focus
loss removes cursor demand and presents the steady hollow cursor; steady or hidden cursors never
schedule frames. Blink phase changes only cursor submission during prepaint, so immutable snapshots
and prepared row identities remain unchanged, and Pane close cancels its owned task.

### Terminal Text Attributes

Terminal Presentation snapshots preserve bold, faint, italic, blink, inverse, and invisible cell
attributes independently from their text and semantic color sources. GPUI selects bold and italic
font faces, resolves bold-as-bright and inverse before applying faint only to foreground opacity,
and splits prepared shaping fragments at every presentation-state boundary. Invisible cells retain
their graphemes in the immutable snapshot but prepare no foreground content; their background and
Selection presentation remain intact.

Visible blinking text is explicit snapshot demand and shares the Pane's presentation clock with
cursor blink. A Pane owns at most one 600-millisecond GPUI task while its product surface is active
and visible text or a focused cursor demands animation. Losing every demand cancels the task and
restores the visible phase, so static, invisible, and inactive content does not schedule frames.
Blink phase filters already prepared cell-aligned fragments and never mutates Terminal Emulator
state.

### Terminal Text Decorations

Terminal Presentation snapshots preserve single, double, curly, dotted, and dashed underline,
independent underline color, strikethrough, and overline as semantic cell attributes. Preparation
coalesces decoration spans only across adjacent cells with identical kind, resolved color, and
blink state; wide spacer tails extend their head cell's decoration without producing text.
Invisible cells prepare no foreground decorations, while Selection remains an independent overlay.

Decoration positions derive from the selected terminal font's baseline, ascent, and x-height, and
their minimum thickness and positions snap to one backing device pixel. Prepared rows cache flat
scene primitives for every decoration, including GPUI wavy-underlines for curly strokes, and reuse
that geometry until row content or layout invalidates it. GPUI clips submission to terminal grid
bounds. The fixed paint order is background and Selection, underline and overline, glyph or
generated symbol, then strikethrough; marked-text overlays remain above the immutable grid
presentation.

### Terminal Drawing Symbols

GPUI generates cell-local geometry for exact single-scalar box-drawing, block-element, Braille,
Powerline, and legacy sextant cells instead of delegating them to font shaping. Combined graphemes,
variation-selector forms, ZWJ sequences, and unsupported scalars continue through the normal
whole-cell shaping path. Generated symbols use the cell's resolved foreground after inverse and
faint processing, while Selection remains an independent background overlay.

Each terminal grid owner caches immutable symbol plans by scalar, device-pixel dimensions, cell
width, and backing scale. Plans are released when scale-dependent rendering state is invalidated
or the owner is dropped. Prepared rows rasterize vector plan primitives into flat, cell-local quads
only when stable row geometry invalidates. Their geometry reaches shared device-pixel cell edges
without font bearings, paints between under-text decorations and strikethrough, participates in
text blink filtering, and receives the same Cursor block recoloring overlay as shaped glyphs.
Logical origins still derive from Unified Terminal Geometry so fractional cell widths do not
accumulate drift.

### Terminal Input Focus

Terminal Input Focus is a pure derived fact and is distinct from the Window-owned Focused Pane
identity. The Terminal Focus Coordinator combines Active Workspace, Active Window, Focused Pane,
terminal responder ownership, key Operating-System Window, active application, and temporary UI
blockers. Only the complete true state admits terminal key input and presents the negotiated
cursor. Any false fact preserves emulator visibility but presents a steady, outline-only hollow
block; restoring every fact restores the negotiated shape and blink request.

### Terminal Grapheme Shaping

Terminal Presentation shapes the complete text stored in each terminal cell and anchors the
result at that cell's grid position. Wide heads, combining sequences, variation-selector forms,
emoji ZWJ sequences, and bidirectional-sensitive cells use an unconstrained whole-cell shaping
fragment; spacer tails never produce text. Compatible simple narrow cells may share one
fixed-width fragment, but fragments never cross a row, Selection, or Cursor boundary. Terminal
fonts disable ligatures and carry an explicit emoji and system-monospace fallback cascade, while
CoreText appends its language-aware system cascade. Cursor recoloring uses the same whole-grapheme
width policy as ordinary text. Prepared rows retain immutable snapshot identity, and a Cursor move
rebuilds only its previous and current rows, so shaping work remains bounded by visible cells and
changed rows.

### Ordered Terminal Focus Reporting

Every authoritative Terminal Input Focus transition crosses the reliable Terminal Session command
lane. The terminal worker owns the last reported focus state, DEC 1004 mode state, and the set of
terminal-routed held keys. Enabling DEC 1004 emits the current state immediately; later duplicate
transitions emit nothing. On focus loss, the worker synthesizes releases for its held keys before
emitting focus-out, while application Actions remain outside that held-key set.

### Balanced Secure Event Input

The terminal worker polls the worker-owned PTY master at a bounded 200-millisecond interval and
classifies hidden input only when canonical mode is enabled and echo is disabled. An AppKit-thread,
application-scoped coordinator enables Carbon Secure Event Input only when exactly one live Pane
both reports hidden input and owns Terminal Input Focus. It performs only physical state
transitions, treats API and termios failures as non-ownership, and releases on focus loss,
application deactivation, Session completion, or Pane removal. Diagnostics contain state,
identity counts, reasons, and OSStatus values but never terminal input.

### Unified Terminal Geometry

`TerminalGeometry` is the sole conversion contract between GPUI logical viewport and cell metrics,
the native window backing scale, Terminal Emulator dimensions, mouse positions, and PTY pixel
dimensions. Full logical grid extents are scaled before rounding so fractional cells cannot
accumulate row or column drift. A Pane observes native window metric changes, invalidates
scale-dependent rendering resources, and sends resize updates through a latest-only mailbox so a
resize burst cannot create an unbounded control backlog.

### Terminal Mouse Input Routing

The Terminal Emulator owns conventional mouse-mode routing and delegates X10, normal, button,
any-motion, UTF-8, SGR, URXVT, and SGR-pixel byte encoding to `libghostty-vt`. Cell and pixel
reports derive from `TerminalGeometry`, including clamped off-grid drags. A captured press owns its
route and button state until the matching release. Shift selects locally only when the injected
host policy permits that override; otherwise its modifier is reported to the application for
presses, motion, and wheel input. Worker snapshots publish whether application mouse tracking is
active so the Pane's pointer presentation matches the effective route.

Precision wheel input retains independent horizontal and vertical fractional remainders in logical
cell units and carries AppKit gesture and momentum phases across GPUI's event boundary. Routing is
strictly application mouse reporting first, alternate-screen arrow policy second, and Primary
Screen Scrollback last. Mouse reporting uses buttons 4/5 vertically and 6/7 horizontally;
horizontal movement never mutates ordinary Scrollback.

### Terminal Hyperlinks

Immutable Terminal Presentations attach validated link targets to complete cells. OSC 8 targets
come from `libghostty-vt`; configured detection maps UTF-8 byte spans back to whole grapheme cells.
URL and local-path validation are separate, bounded, and control-free, with local paths resolved
only against trusted Session context and required to exist. Hover presentation covers every cell
with the same stable identity, while opening requires a platform-modified press and release on the
same identity and Presentation Generation; drags, stale mappings, malformed schemes, and missing
paths are inert.

### Native File Insertion

Pasteboard, Services, and drag/drop file inputs enter one bounded converter as ordered absolute
paths. Each item uses POSIX single-quote shell syntax with embedded quotes split safely, and items
join with exactly one space. Native pasteboard intake accepts only `public.file-url` values;
non-file URLs, relative paths, excessive items, and oversized output are rejected. Converted text
always enters the existing worker-owned Paste Payload sanitizer and confirmation lifecycle and
never writes directly to the PTY.

### Native Terminal Services

macOS Services, contextual actions, Quick Look eligibility, pasteboard file insertion, and
drag/drop are adapters over existing terminal policies rather than independent mutation paths.
Context actions are derived from the current immutable Selection and Terminal Hyperlink state.
Service text and converted file paths become Paste Payload candidates only while the Pane owns
Terminal Input Focus, so normalization, size limits, unsafe-paste confirmation, cancellation, and
PTY writes remain worker-owned. Quick Look accepts only a validated Terminal Hyperlink whose
canonical target is an existing local regular file; a web URL or stale path is never previewable.
Native interactions may request selection formatting or paste processing, but never mutate the
Terminal Emulator directly.

### Terminal Selection

The Terminal Emulator owns Selection gestures and delegates cell, word, line, repeat-click, drag,
wide-cell, and soft-wrap semantics to `libghostty-vt`. Gesture anchors are tracked terminal grid
references, so completed Selection remains attached to logical content across Scrollback movement,
new output, and resize reflow. Selection-owned presentations may continue the active gesture from
its last accepted mapping, while output, resize, or page movement requires a fresh Presentation
Generation. Off-grid drags request worker-owned autoscroll through the existing deadline loop; its
rate is deterministic, depth-sensitive, bounded, and cancelled by release or stale mapping.

Selection copy is an ordered worker query that formats the terminal-owned range through
`libghostty-vt` into a typed plain-text and optional HTML value. Its explicit options distinguish
soft-wrap unwrapping from hard newlines and make trailing-space trimming deterministic; wide
spacer tails never become duplicated text. Pasteboard ownership stays outside the Terminal
Emulator: the macOS Adapter publishes only public UTF-8 plain-text and HTML representations, and
copy failures report the operation or representation type without logging copied content.
Command-C completes that latency-sensitive query and publishes its representations before the
action returns, so any immediate Paste observes the new Selection rather than the previous
pasteboard value.

### Terminal Find

Terminal Find is transient Pane-owned UI over one Terminal Session's active screen. Its literal,
single-line query covers the active screen and that screen's Scrollback; the Alternate Screen has
no Primary Scrollback. ASCII comparison is case-insensitive while non-ASCII UTF-8 comparison is
exact. Soft-wrapped rows form one logical line and hard line boundaries never match across.

The Terminal Session worker exclusively builds the search corpus, maps every UTF-8 byte to its
grapheme-head terminal cell, excludes wide-cell spacer tails, retains current-result anchors as
tracked grid references, and rebuilds results after terminal mutation or reflow. It publishes only
immutable viewport-relative highlight spans, count, current index, and query generation with the
Terminal Presentation. GPUI suppresses stale generations and converts spans only into geometry;
search and byte-to-cell mapping never run during prepaint or paint.

Terminal Find belongs to exactly one Pane. Command-F opens or refocuses its fixed top-right bar and
makes that native UTF-16 text editor the responder, so Terminal Input Focus becomes false and the
terminal Cursor uses its steady hollow presentation. Clicking the terminal may restore terminal
responder focus without closing Find. Losing Focused Pane status, Escape, or the close control ends
Find, clears worker search state, and restores terminal responder focus when the Pane remains
Focused. Query changes do not move the Terminal Viewport or choose a current result; navigation
wraps and minimally moves the Terminal Viewport to show the complete current match.

Terminal Find highlighting composes in this fixed order: terminal background, non-current matches,
current match, Selection, terminal text and decorations, Cursor, then Marked Text. Selection
therefore wins deterministically when it overlaps any Find result.

### Safe Terminal Insertion

Every text-insertion Adapter submits one owned Paste Payload through the Terminal Session command
lane. The worker rejects empty or over-one-mebibyte payloads, normalizes CRLF and CR to LF, and
classifies multiline input, the bracketed-paste closing fence, and the exact control-byte set that
`libghostty-vt` replaces. Multiline input is encoded immediately while the Terminal Emulator has
bracketed-paste mode enabled and otherwise remains immutable and worker-only behind one opaque,
thirty-second Paste Confirmation. An embedded bracketed-paste closing fence always requires that
confirmation. Control bytes alone do not prompt because the encoder replaces them before writing.
The Pane presents only byte and line counts plus risk categories, without transferring responder
ownership or exposing content in UI state or diagnostics.

Approval is valid only for the matching opaque identity while the Pane still has Terminal Input
Focus. Cancel, timeout, focus or hierarchy loss, Terminal Session shutdown, a stale identity, or a
lost reply writes no PTY bytes. Accepted input is encoded once by `libghostty-vt`: stripped
controls become spaces, unbracketed LF becomes CR, and bracketed input receives exactly one host
opening and closing fence so an embedded closing sequence cannot escape.

### Authorized OSC 52 Clipboard Access

OSC 52 is intercepted by a bounded streaming host filter before Terminal Emulator parsing. The
filter preserves sequences split across PTY reads, accepts only the standard, selection, and
primary targets, requires canonical padded base64 and UTF-8 text, and limits decoded writes and
encoded read replies to one mebibyte. Once the encoded bound is crossed, candidate storage is
cleared and input is discarded through the sequence terminator, so hostile output cannot force an
unbounded libghostty allocation. Malformed, oversized, unsupported, or denied operations are quiet:
they mutate no pasteboard and disclose no clipboard data.

Read and write authorization is deny-by-default and independently typed as deny, ask, or allow.
Ask mode permits one worker-owned pending operation with an opaque thirty-second authorization;
the Pane receives only access direction, target, and byte count. Later terminal output stays
bounded behind that operation until allow, deny, timeout, focus or hierarchy loss, or Terminal
Session shutdown resolves it. The worker flushes prior emulator replies before an allowed clipboard
effect and resumes later output afterward, preserving exact PTY reply order. Authorization and
failure diagnostics never contain clipboard contents.

### Typed Terminal Key Input

Terminal key input crosses the Terminal Session command lane as one ordered, typed event carrying
press, repeat, or release action; physical and native identity; logical identity; UTF-8 text and an
unshifted codepoint; and full and consumed side-aware modifiers. Application Actions resolve before
the raw terminal handler. An input-method commit is the explicit typed exception to physical-key
identity and carries exact committed UTF-8 through the same ordered lane without entering held-key
state. Unsupported ordinary physical identities produce a typed error and never degrade to guessed
terminal bytes. A worker-owned keyboard protocol Module reads Terminal Emulator modes for every
event and delegates cursor, keypad, DECBKM, modifyOtherKeys, fixterms, and negotiated Kitty keyboard
encoding to `libghostty-vt`; UI code never constructs terminal escape sequences. Modifier-only
transitions remain protocol-visible without clearing the Selection or forcing the viewport to the
bottom; the first non-modifier terminal input retains the normal clear-on-type behavior.

### Native macOS Keyboard Bridge

The focused Pane reads the current AppKit keyboard event synchronously from GPUI's raw keyboard
callbacks, after application Actions have had the first opportunity to consume Command bindings.
The bridge maps native macOS keycodes to stable physical identities while deriving logical text,
unshifted codepoints, and consumed modifiers from each event, so input-source changes require no
cached layout state or Terminal Session restart. It balances left/right modifier transitions and
carries the injected Option-as-Alt policy with every ordered key event. Key releases use the same
Terminal Session command lane as presses and repeats.

### Native Input Method Composition

The Pane implements GPUI's entity input handler, which supplies AppKit's native text-input client
and UTF-16 range contract without a parallel platform view. Marked Text is Pane-local presentation
state: updates, replacements, cancellation, and focus loss never mutate the Terminal Emulator or
write PTY bytes. Its complete grapheme clusters use the Terminal Emulator's width rules, wrap wide
clusters before the right edge, and overlay the immutable grid with an underline and composition
caret. Candidate-window bounds derive from that caret and the same logical cell geometry used for
rendering. A nonempty commit clears Marked Text and becomes exactly one typed input-method event on
the reliable Terminal Session command lane; the worker emits its UTF-8 once in order and never
tracks it as a held key. The Pane and terminal grid cache logical cluster layout and shaped overlay
geometry by the exact marked-text revision, caret, Cursor origin, grid metrics, font, colors, and
backing scale, so unrelated frames do not reshape stable composition while every native edit
invalidates it immediately.

### Terminal Accessibility

Each Pane exposes one native editable text-area model whose canonical indexes are UTF-16 code
units, matching AppKit. The model preserves complete grapheme text while mapping wide-cell spacer
tails, combining sequences, hard lines, soft wraps, the visible range, Selection, and Cursor back
to logical terminal cells. A retained text-bearing cell is the canonical accessibility grapheme:
range-for-index covers its complete UTF-16 span, partial string queries that would detach one of its
components are rejected, and partial Selection requests normalize to complete intersected cells.
Attributed range queries expose only the requested complete text together with the concrete
regular terminal font name, family, visible name, and logical point size resolved from the Pane's
actually selected font; backing scale never changes that point size. Candidate and range bounds use
the same logical cell origin, width, and line height as pointer and renderer geometry. Terminal
output, Selection, Terminal Input Focus, and Marked Text changes produce typed, coalesced native
accessibility notifications; a Pane retains
at most one pending Value, Selection, and Focus fact while it cannot present, then delivers those
facts against its newest model on the next native presentation. Parent layout membership remains a
separate synchronous hierarchy notification. Notifications carry no terminal contents.
Accessibility may request terminal-owned selection changes through the Terminal Session, but never
mutates the Terminal Emulator from the native callback.

### Demand-Driven Render Lifecycle

A Pane-owned Render Lifecycle separates one-shot presentation demand from recurring animation
eligibility. Immutable Terminal Presentations received while minimized, occluded, in a hidden
Workspace, or otherwise non-presentable coalesce to the newest Presentation Generation without
requesting frames; visibility restoration requests exactly one presentation of that newest state.
Cursor blink, text blink, and visual effects run only while the application, key Operating-System
Window, Workspace, and Pane can present them and while AppKit is not minimizing, occluding, or
live-resizing the surface. Display moves and backing-scale changes preserve logical grid state and
invalidate only scale-dependent prepared rows and symbol geometry. Pane destruction releases
render caches and cancels every owned presentation task and native resource.

### Authenticated Runtime Observation

Release acceptance may activate one dormant, content-free Runtime Observation through the private
Unix socket already authenticated to the exact mounted SpaceTerm process. The launch capability is
removed from the environment before GPUI or any Terminal Session starts and is never inherited by
a PTY or Shell Process. One claimed production Pane and its Terminal Session publish only bounded
numeric counters, closed lifecycle and visibility states, geometry, and Presentation Generations;
terminal cells or hashes, titles, commands, paths, environment, key identity, clipboard data,
Selection text, and hyperlink metadata are outside the protocol.

The trusted same-UID mounted-DMG acceptance verifier extends that authenticated app peer with an
**Acceptance Failure Action** channel. The app independently requires its own canonical packaged
executable vnode to be on a read-only mount and bound into the challenge before it constructs the
controller; arbitrary ordinary, source, or writable-bundle launches are inert. Its challenge commits to
`spaceterm.acceptance.failure-action/v1`; the verifier accepts one fixed, content-free case name
from an owner-private FIFO, generates the nonce-bound request identity and monotonic sequence, and
forwards the request exactly once. The claimed production Pane accepts at most one request at a
time and reports closed-enum `armed`, `injected`, `retry-requested`, and `completed` facts under
`spaceterm.acceptance.failure-action-result/v2`. No request or result field can carry terminal,
clipboard, path, environment, command, or arbitrary diagnostic content. Without the authenticated
launch challenge this controller is not constructed, so ordinary launches cannot reach an
injection Seam. The challenge and proof carry an exact authenticated `failure.action.enabled`
fact; when it is false the app allocates neither action channels nor a Pane controller. The result
schema's four bounded resource counters prove a real staged-image mutation and equal rollback for
the after-staging case and remain zero for every other case. The owner-private driver sends an
opaque one-request correlation nonce and accepts only fixed `accepted` and `completed` statuses
echoing that nonce, so stale receipts cannot authorize another request and the next case cannot be
submitted before authoritative completion.

This is not cryptographic mutual authentication of the dynamically compiled verifier. Another
same-UID process could launch its own exact read-only mounted SpaceTerm instance with a conforming
private peer and trigger only that instance; it cannot mint evidence accepted by the official
verifier, affect another SpaceTerm instance, or persist a global injection setting. Campaign
evidence therefore trusts the official verifier as the same-UID controller and relies on its live
peer, package, process, nonce, sequence, and artifact authentication.

Worker, UI, and render critical paths update atomics and a fixed-capacity transition queue only.
While observation is active, a Pane-owned main-thread monitor samples its retained exact AppKit
window at bounded 50-millisecond intervals so minimize, occlusion, and live-resize facts continue
to advance even after AppKit suspends rendering; ordinary launches do not create that monitor.
One background writer samples the latest state at one-second absolute deadlines on the original
authenticated socket, then performs a bounded final drain and acknowledgement during application
shutdown. A transition drop, deadline miss, counter overflow, transport or writer failure, unknown
schema, invalid ordering, or missing terminal lifecycle makes the observation **NOT-RUN**; it never
changes runtime behavior to manufacture a passing result. External acceptance analysis owns
PASS/FAIL decisions, RSS sampling, traces, and process identity artifacts.

Acceptance Failure Actions exercise the real presentation, glyph/image preflight,
renderer-resource synchronization, pasteboard-write, PTY, and Terminal Emulator failure mappings.
Recoverable actions must retain the last valid Presentation Generation through visible failure and
retry before reporting recovery. Fatal actions report the typed failed state and Pane-close receipt;
the campaign must separately prove the real PID/PGID was reaped and that a replacement Pane runs a
new command. The normal-exit control only arms observation: the operator enters real `exit 0`, and
no exit is injected.

After authenticating that same mounted-app peer, the verifier also publishes the provisional
native launch observation and its owner-private `spaceterm.acceptance.ax-subject/v1` live-subject
record before UI acceptance begins. The record binds the launch nonce, run and application-tree
digest to the exact process start time, executable vnode/filesystem identity, read-only mounted
bundle, and live code signature. Native accessibility probes must consume and independently
revalidate that record; they never discover a process by name or accept a caller-selected PID.

### Typed Terminal Failures and Local Diagnostics

Normal Shell Process exit is distinct from PTY, Terminal Emulator, presentation, macOS platform,
and renderer-resource failure. A Pane owns this typed state and presents an actionable recovery:
fatal runtime failures require closing the Pane and restarting the command, while recoverable
presentation or resource failures retain the last valid Presentation Generation and permit retry.
Failure mapping discards raw library messages at the boundary so terminal contents, clipboard
data, environment values, paths, and secrets never enter Pane state or diagnostics. Diagnostics
retain only a bounded sequence of failure class, recoverability, and static operation identifiers,
plus unhandled keyboard event kind, action, and native key code (at most 128 records and 64 KiB).
Logical and typed key text never enters diagnostics. SpaceTerm performs no automatic network
telemetry or crash upload. A local diagnostic file is created only after the user explicitly
chooses Export Terminal Diagnostics and confirms a path through the native save panel.

### Workspace-Bound Terminal Creation

Every runtime-only Workspace has an immutable Workspace Kind. New Workspace creates an Ad Hoc
Workspace at `HOME`; Open Local Project creates a Local Project Workspace from one native,
directory-only selection. The Workspace owns the exact selected or reported Workspace Directory and
its canonical macOS device/file identity. Equivalent Local Project selections activate the existing
Workspace, while an Ad Hoc Workspace at that identity remains distinct. No Workspace state is
persisted or watched.

An Ad Hoc Workspace's initial Pane is its Directory Authority. Valid live Reported Working Directory
changes from that Pane update the Workspace Directory even when its Workspace or Window is inactive.
Closing the authority Pane promotes the first remaining Pane in Pane Layout order; closing its Window
promotes the root Pane of the first remaining Window. A promoted Pane's current valid report is
adopted immediately. Local Project reports never change its immutable Project Root.

`WorkspaceTerminalSessionFactory` binds each creation operation to the Workspace-owned exact path.
Local Project children always start at Project Root; Ad Hoc children start at the latest Workspace
Directory. Create Window and Split Pane revalidate existence, readability, directory type, and
filesystem identity before hierarchy mutation. Failure leaves running Terminal Sessions intact,
marks the Workspace unavailable, and blocks only new children until validation succeeds.

Without a custom name, Workspace names follow the directory basename, use `Default` for the `HOME`
identity and `/` for the filesystem root, and number only unrenamed Ad Hoc Workspaces with the same
identity in sidebar order. An empty trimmed rename clears the custom name; duplicate custom names are
valid.

### Cross-Hierarchy Close Escalation

Closing a final child escalates to its owning hierarchy Module without first destroying the child.
Closing the final Pane requests its Window close; closing the final Window removes its Workspace
when another Workspace remains, or closes the Operating-System Window when globally final. Explicit
Close Workspace remains distinct and replaces the final Workspace. The Module that resolves each
close synchronously removes the entity and initiates one-shot shutdown of its Terminal Sessions.
Shell termination and PTY ownership cleanup continue on terminal worker threads so GPUI callers do
not wait for reader or Shell Process joins.
