# SpaceTerm Context

This document is canonical for SpaceTerm's product intent, technology decisions, UI constraints,
and architectural principles. Domain terminology, relationships, and invariants are canonical in
`docs/UBIQUITOUS_LANGUAGE.md`.

## Product intent

SpaceTerm is a modern native macOS terminal. Its layout hierarchy takes useful inspiration from
tmux, but SpaceTerm is not a tmux client and has no tmux-style server/client model.

## Technology decisions

- Use Rust 2024 and GPUI for native UI and GPU rendering.
- Use `libghostty-vt` for terminal emulation.
- Use a macOS PTY to launch and communicate with shells.
- Use `gpui-symbols` for native macOS SF Symbols.
- Support macOS only; do not create Linux or Windows abstractions.
- Treat Vague as the only application and terminal theme and as the source of all brand colors.
  Reuse the tokens in `src/theme.rs`; never hardcode or redefine colors in UI, terminal, platform,
  or domain code.
- Prefer `JetBrainsMono Nerd Font` for terminal text and use a sensible system monospace fallback
  when it is unavailable.

## UI direction

- Build the interface directly with GPUI as a compact, Zed-like desktop experience.
- Use `gpui-symbols` for icons and native macOS menus and system dialogs where appropriate.
- Keep reusable SpaceTerm controls in a small internal UI Module.

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

Prefer one application crate with well-designed Modules over a workspace of small crates. Add
abstractions only when they create a meaningful Seam, Leverage, or Locality.

## Current architectural decisions

### Overlay Scrollbar

The Overlay Scrollbar is a compact vertical control that presents scroll position without reserving
layout space. It owns thumb geometry, transient visibility, hover retention, drag capture, and
Vague-themed rendering. Terminal Scrollback and the Workspace list adapt their native offset units
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

### Bounded PTY Output Transport

Terminal Session control Commands, including Shutdown, use a reliable latency-sensitive lane. PTY
reader events use a separate bounded queue with backpressure and ordered worker notifications.
Workers coalesce only consecutive output notifications up to a control boundary; reader completion
remains reliable and ordered after its output. Closing the receiver wakes a blocked reader producer
so terminal cleanup can continue off-thread.

### Immutable Terminal Presentation Snapshots

Each Terminal Session worker exclusively mutates its Terminal Emulator and publishes owned semantic
snapshots to GPUI. Snapshots retain shared identity for unchanged rows and metadata, carry cursor,
viewport, active-screen, size, and precise damage independently of cell content, and cross the
worker boundary through a bounded latest-screen channel. GPUI renders only these immutable values
and never borrows Terminal Emulator state.

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
their minimum thickness and positions snap to one backing device pixel. GPUI builds quads and curly
paths during prepaint and clips them to terminal grid bounds. The fixed paint order is background
and Selection, underline and overline, glyph or generated symbol, then strikethrough; marked-text
overlays remain above the immutable grid presentation.

### Terminal Drawing Symbols

GPUI generates cell-local geometry for exact single-scalar box-drawing, block-element, Braille,
Powerline, and legacy sextant cells instead of delegating them to font shaping. Combined graphemes,
variation-selector forms, ZWJ sequences, and unsupported scalars continue through the normal
whole-cell shaping path. Generated symbols use the cell's resolved foreground after inverse and
faint processing, while Selection remains an independent background overlay.

Each terminal grid owner caches immutable symbol plans by scalar, device-pixel dimensions, cell
width, and backing scale. Plans are released when scale-dependent rendering state is invalidated
or the owner is dropped. Their geometry reaches shared device-pixel cell edges without font
bearings, paints between under-text decorations and strikethrough, participates in text blink
filtering, and receives the same Cursor block recoloring overlay as shaped glyphs. Logical origins
still derive from Unified Terminal Geometry so fractional cell widths do not accumulate drift.

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

### Safe Terminal Insertion

Every text-insertion Adapter submits one owned Paste Payload through the Terminal Session command
lane. The worker rejects empty or over-one-mebibyte payloads, normalizes CRLF and CR to LF, and
classifies multiline input, the bracketed-paste closing fence, and the exact control-byte set that
`libghostty-vt` replaces. Safe input is encoded immediately; unsafe input remains immutable and
worker-only behind one opaque, thirty-second Paste Confirmation. The Pane presents only byte and
line counts plus risk categories, without transferring responder ownership or exposing content in
UI state or diagnostics.

Approval is valid only for the matching opaque identity while the Pane still has Terminal Input
Focus. Cancel, timeout, focus or hierarchy loss, Terminal Session shutdown, a stale identity, or a
lost reply writes no PTY bytes. Confirmed input is encoded once by `libghostty-vt`: stripped
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
encoding to `libghostty-vt`; UI code never constructs terminal escape sequences.

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
tracks it as a held key.

### Workspace-Bound Terminal Creation

`WorkspaceCollection` owns default Workspace naming and passes the exact stored Workspace root into
payload construction. `WorkspaceTerminalSessionFactory` binds the existing dynamic Terminal Session
factory Seam to that root, and Windows, Pane hosts, and terminal Panes carry only this scoped Module.
Terminal creation therefore cannot select a working directory independently of its owning
Workspace, and no additional provider trait or global state is introduced.

### Cross-Hierarchy Close Escalation

Closing a final child escalates to its owning hierarchy Module without first destroying the child.
Closing the final Pane requests its Window close; closing the final Window removes its Workspace
when another Workspace remains, or closes the Operating-System Window when globally final. Explicit
Close Workspace remains distinct and replaces the final Workspace. The Module that resolves each
close synchronously removes the entity and initiates one-shot shutdown of its Terminal Sessions.
Shell termination and PTY ownership cleanup continue on terminal worker threads so GPUI callers do
not wait for reader or Shell Process joins.
