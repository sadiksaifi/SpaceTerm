# Ubiquitous Language

## Product hierarchy

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **SpaceTerm** | The macOS terminal application that owns all Workspaces. | tmux client, terminal client |
| **Workspace** | A named top-level scope containing one or more Windows and a root working directory. | Session, attached session, project |
| **Window** | An ordered terminal work area that belongs to exactly one Workspace and contains a Pane Layout. | Tab, session, macOS window |
| **Pane** | A terminal region that is one leaf of exactly one Window's Pane Layout. | Window, tab, split |
| **Pane Layout** | The recursive arrangement of Panes and Splits within a Window. | Grid, pane tree |
| **Split** | A layout division with exactly two child Pane Layouts and a constrained size ratio. | Pane, divider |
| **Operating-System Window** | A native macOS application surface that presents SpaceTerm. | Window when discussing the product hierarchy |

## Selection and visibility

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Active Workspace** | The one Workspace currently presented by SpaceTerm. | Selected Workspace, focused Workspace, attached Workspace |
| **Active Window** | The one Window selected within a Workspace. | Tab, selected Window, focused Window |
| **Focused Pane** | The one Pane selected by a Window for Pane operations and focus restoration. | Active Pane, selected Pane, Terminal Input Focus |
| **Terminal Input Focus** | Transient truth that a Pane may accept terminal key input because its Workspace and Window are Active, it is the Focused Pane and current responder, its Operating-System Window and SpaceTerm are active, and no temporary UI owner blocks it. | Focused Pane, selected terminal |
| **Zoomed Pane** | The Focused Pane temporarily shown alone while its Window's Pane Layout remains intact. | Maximized Window, fullscreen Pane |

## Terminal runtime

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Terminal Session** | The live terminal runtime owned by a Pane, joining terminal emulation, a PTY, and a Shell Process. | Session, Workspace session |
| **Terminal Emulator** | The state machine that interprets terminal output and maintains the visible grid and Scrollback. | Terminal Session, Pane |
| **PTY** | The macOS pseudoterminal that connects a Terminal Emulator to its Shell Process. | Terminal, shell |
| **Shell Process** | The command interpreter process launched for a Pane through its PTY. | Terminal, session |
| **Shell Integration** | Versioned temporary startup resources injected only into a supported Shell Process to report safe directory, prompt, command, and completion facts without changing user configuration. | shell plugin installation, dotfile rewrite |
| **Terminal Capability Identity** | The canonical truthful program, version, `TERM`, terminfo, XTVERSION, device-attribute, and XTGETTCAP profile exposed by one Terminal Session. | emulator dependency identity, user-agent string |
| **Terminal Attention** | Pane-owned unread and visual state derived from bounded BEL or command-completion events without moving Terminal Input Focus. | global alarm, focus request |
| **Primary Screen** | The Terminal Emulator screen whose logical content includes bounded Scrollback and whose Terminal Viewport is restored after Alternate Screen use. | main buffer, normal buffer |
| **Alternate Screen** | The temporary Terminal Emulator screen used by full-screen terminal applications; it has no Scrollback and does not replace Primary Screen state. | alternate buffer, secondary terminal |
| **Terminal Viewport** | The visible row window over the active screen; it follows new output only while already at the bottom and otherwise remains anchored to logical content. | Workspace viewport, Pane scroll |
| **Scrollback** | Bounded Primary Screen output retained outside the bottom Terminal Viewport and available by moving that viewport. | Workspace scroll, history |
| **Presentation Generation** | The monotonic identity of a published Terminal Presentation grid, carried by coordinate and Scrollback mappings so stale mappings and older presentations can be rejected. | frame number, emulator generation |
| **Synchronized Output** | A bounded DEC 2026 transaction whose intermediate Terminal Emulator changes are withheld until one atomic Terminal Presentation is published. | render lock, output buffering |
| **Terminal Image** | Immutable decoded RGBA content identified by a Kitty image ID and content generation within one Primary or Alternate Screen. | file image, texture path, live emulator pointer |
| **Image Placement** | Renderer-ready geometry that references a Terminal Image and remains attached to terminal content or U+10EEEE placeholder cells across scrolling and reflow. | UI image, copied crop, floating overlay |
| **Terminal Metadata** | Immutable session-scoped title, Reported Working Directory, Semantic Zone, command, and progress facts published with explicit provenance and freshness in a Terminal Presentation. | live emulator state, arbitrary OSC text |
| **Reported Working Directory** | The last valid local absolute directory reported by a Terminal Session, initially inherited from its Workspace and updated only by trusted OSC 7. | process-global current directory, remote URL |
| **Semantic Zone** | A Prompt, Command Input, or Command Output region marked by OSC 133 and owned by immutable terminal cells. | UI text classification, shell transcript parser |
| **Selection** | A terminal-owned logical content range created by pointer gestures and kept anchored across Scrollback movement, output, and resize reflow. | selected Workspace, selected Pane, text highlight |
| **Terminal Find** | Transient Pane-owned literal search over one Terminal Session's active screen and its available Scrollback, with worker-owned grapheme-cell mapping and immutable viewport highlights. | global search, regex search, Selection |
| **Find Query Generation** | The identity of the current Terminal Find query, carried by navigation commands and result snapshots so stale results cannot be presented or selected. | Presentation Generation, frame number |
| **Terminal Hyperlink** | A validated immutable target mapped to complete Terminal Presentation cells and opened only by an explicit same-generation activation. | arbitrary terminal text, automatic file execution |
| **Marked Text** | Transient input-method preedit owned by the Pane with Terminal Input Focus; it is presented at the Cursor but is not Terminal Emulator or PTY input until committed. | terminal output, committed text |
| **Secure Event Input** | Process-global macOS keyboard protection held only while exactly one live Pane both reads hidden canonical PTY input and owns Terminal Input Focus. | password text, per-Pane secure mode |
| **Paste Payload** | An immutable, size-bounded text insertion candidate owned by the Terminal Session worker from normalization through encoding or cancellation. | clipboard contents, typed key input |
| **File Insertion** | Ordered native file URLs converted to absolute POSIX-shell-quoted paths before becoming a Paste Payload. | raw URL paste, direct PTY write |
| **Native Terminal Service** | A macOS Services, contextual-action, Quick Look, pasteboard, or drag/drop adapter that requests existing Selection, Terminal Hyperlink, File Insertion, and Paste Payload policies without mutating the Terminal Emulator. | independent input path, direct PTY service |
| **Terminal Accessibility Model** | A Pane-owned native editable text-area projection whose UTF-16 ranges map visible and retained terminal text, Selection, Cursor, and bounds back to logical terminal cells. | flattened cell string, screen-reader transcript |
| **Render Lifecycle** | Pane-owned visibility, animation, scale, and newest-generation presentation state that schedules frames only when its native surface can present them. | render loop, global animation timer |
| **Terminal Failure** | A typed PTY, Terminal Emulator, presentation, platform, or renderer-resource fault with an explicit recoverability class and no terminal contents or secrets. | stderr message, shell exit |
| **Local Diagnostics** | A bounded content-free sequence of Terminal Failure identifiers and unhandled keyboard event kind, action, and native key code written only after explicit user export. | telemetry, crash upload, terminal log |
| **Paste Confirmation** | A transient authorization for one unsafe Paste Payload, identified opaquely and valid only while its Pane retains Terminal Input Focus and the worker deadline has not expired. | generic modal, clipboard permission |
| **OSC 52 Authorization** | A bounded, opaque, one-operation decision governing whether a terminal program may read or write a named clipboard target; access is denied unless explicit policy allows or asks for it. | paste confirmation, unrestricted clipboard access |

## Lifecycle actions

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Create Workspace** | Add and activate a new Workspace with its initial Window and Pane. | Start session, attach session |
| **Close Workspace** | Remove a Workspace and its owned terminal runtimes, replacing it when it is the last Workspace. | Detach session, delete session |
| **Create Window** | Add and activate a new Window in the Active Workspace. | New tab, link Window |
| **Close Window** | Remove a Window and its owned terminal runtimes, escalating through its Workspace when it is the final Window. | Close tab, detach Window |
| **Split Pane** | Divide a target Pane and focus the newly created Pane. | Create Window, split Window |
| **Close Pane** | Remove a Pane and its Terminal Session, escalating to Close Window when it is the final Pane. | Close split, close terminal |
| **Focus Pane** | Make an owned Pane the Focused Pane without changing its owning Window. | Activate Pane, select terminal |
| **Resize Split** | Change a Split's ratio within the minimum dimensions required by both child layouts. | Resize Pane |
| **Zoom Pane** | Temporarily present the Focused Pane alone without changing the Pane Layout. | Maximize Window, fullscreen |
| **Restore Panes** | End Pane zoom and present the Window's full Pane Layout again. | Unmaximize Window, unzoom Window |

## Relationships

- **SpaceTerm** owns one or more **Workspaces** and has exactly one **Active Workspace**.
- A **Workspace** owns one or more **Windows** and has exactly one **Active Window**.
- A **Window** belongs to exactly one **Workspace** and cannot be linked, shared, or attached elsewhere.
- A **Window** owns one or more **Panes**, exactly one **Focused Pane**, and one arbitrarily nested **Pane Layout**.
- **Terminal Input Focus** is derived transient state and never replaces or duplicates a Window's **Focused Pane** identity.
- **Terminal Find** belongs to one **Pane** and one **Terminal Session**; losing that Pane's **Focused Pane** status closes it and clears its worker-owned state.
- A **Find Query Generation** belongs to one **Terminal Find** query; stale navigation commands and highlight snapshots cannot affect or present a newer query.
- Opening **Terminal Find** transfers responder ownership away from terminal input without changing **Focused Pane** identity; clicking the terminal may restore **Terminal Input Focus** while Find remains open.
- **Terminal Find** searches across soft wraps but not hard line boundaries, maps UTF-8 bytes to grapheme-head cells, excludes wide spacer tails, and never derives search results in GPUI rendering.
- **Selection** is painted above every **Terminal Find** result, including its current result.
- **Marked Text** exists only while its Pane has **Terminal Input Focus**; cancellation or focus loss discards it without Terminal Session input.
- A **Paste Payload** that requires **Paste Confirmation** remains worker-owned; UI and diagnostics receive only bounded risk metadata, never its content.
- A multiline **Paste Payload** requires **Paste Confirmation** only when bracketed-paste mode is inactive; an embedded closing fence always requires confirmation, while encoder-replaced control bytes alone do not.
- A **Paste Confirmation** is cancelled by focus or hierarchy loss, timeout, Terminal Session shutdown, explicit cancellation, or a stale identity, and cancellation writes no PTY bytes.
- A **Native Terminal Service** may submit a **Paste Payload** only while its Pane owns **Terminal Input Focus**, and may offer Quick Look only for an existing validated local-file **Terminal Hyperlink**.
- A **Terminal Accessibility Model** observes immutable terminal state and may request worker-owned Selection changes, but never mutates the Terminal Emulator directly.
- A **Render Lifecycle** coalesces hidden Terminal Presentations to the newest **Presentation Generation** and schedules exactly one frame when visibility returns.
- A recoverable **Terminal Failure** preserves the last valid **Presentation Generation** when possible; a fatal one requires closing the Pane and restarting its command.
- **Local Diagnostics** never contain terminal, clipboard, environment, path, secret, logical-key, or typed-key values and never leave the machine automatically.
- **OSC 52 Authorization** exposes only access direction, target, and byte count to UI; clipboard contents remain inside the worker-owned native-service operation and are never diagnostic metadata.
- The **Primary Screen** and **Alternate Screen** retain independent state; leaving the Alternate Screen restores the Primary Screen and its **Terminal Viewport**.
- A **Terminal Image** belongs to exactly one screen's bounded Kitty store; immutable snapshots may share its RGBA allocation only while its image ID and content generation are unchanged.
- An **Image Placement** is resolved by the Terminal Emulator, never reconstructed from placeholder cells by GPUI, and paints in the Kitty z layer relative to backgrounds and glyphs.
- A **Presentation Generation** belongs to one published Terminal Presentation; coordinate or Scrollback requests from an older generation never mutate current Terminal Emulator state.
- **Terminal Metadata** belongs to exactly one **Terminal Session**; Pane and Window chrome may consume its sanitized immutable values but never terminal control bytes or live emulator state.
- **Shell Integration** is scoped to one launched **Shell Process** and must preserve its ordinary startup files and environment or fall back without injection.
- **Terminal Capability Identity** selects `xterm-spaceterm` only with a discoverable packaged entry, otherwise retains `xterm-256color`, and never advertises the Terminal Emulator dependency as SpaceTerm's identity.
- **Terminal Attention** remains scoped to its owning Pane and Window; focus gain or accepted input clears it, while repeated native effects are rate-limited and notifications occur only when SpaceTerm is inactive.
- A **Reported Working Directory** accepts only a local absolute `file://` report and retains its last valid provenance when a malformed or remote report arrives.
- A **Pane** belongs to exactly one **Window** and owns one **Terminal Session**.
- A **Split** has exactly two child **Pane Layouts**; each child is either another **Split** or a **Pane**.
- Closing any hierarchy entity closes every **Terminal Session**, **PTY**, and **Shell Process** it owns.
- Closing the final **Window** closes its **Workspace** when another Workspace remains, or closes the **Operating-System Window** when it is globally final.
- Explicitly closing the final **Workspace** replaces it; escalation from its final **Window** closes the **Operating-System Window** instead.
- No operation may leave an orphaned **PTY** or **Shell Process**.
- Active and focused identities always reference entities still owned by their parent.

## Example dialogue

> **Dev:** "The user clicked Split Right in the Active Window. Should that create another Window?"

> **Domain expert:** "No. **Split Pane** adds a **Pane** to the same **Window**, updates its **Pane Layout**, and makes the new Pane the **Focused Pane**."

> **Dev:** "What happens if they close that Pane after it becomes the only Pane?"

> **Domain expert:** "**Close Pane** escalates to **Close Window**. A final Window closes its Workspace when another remains, and closes the **Operating-System Window** only when it is globally final."

## Flagged ambiguities

- "Window" can mean either a product **Window** or a native macOS **Operating-System Window**; use **Window** for the SpaceTerm hierarchy and qualify the native surface every time.
- "Tab" visually describes the Window selector but is not a SpaceTerm concept; always call the entity a **Window**.
- "Session" can incorrectly imply a tmux-style **Workspace** or describe a Pane's runtime; never use bare "session" for hierarchy concepts, and use **Terminal Session** only for the qualified runtime.
- tmux-inspired layout does not imply a server/client model; do not describe SpaceTerm concepts as clients, attached sessions, or shared Windows.
- "Active," "focused," and "selected" were used as near-synonyms; reserve **Active** for Workspace and Window state and **Focused** for Pane state.
- **Focused Pane** does not imply **Terminal Input Focus** while another responder, menu, modal, Operating-System Window, or application owns input.
- "Terminal" can mean the application, emulator, runtime, or visible region; prefer **SpaceTerm**, **Terminal Emulator**, **Terminal Session**, or **Pane** according to the intended concept.
- "Horizontal split" and "vertical split" can describe either divider orientation or Pane placement; use the user-facing actions **Split Right** and **Split Down**, and use an explicitly named split axis only in implementation discussions.
