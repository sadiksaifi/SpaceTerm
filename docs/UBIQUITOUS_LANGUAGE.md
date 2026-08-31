# Ubiquitous Language

## Product hierarchy

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **SpaceTerm** | The macOS terminal application that owns all Workspaces. | tmux client, terminal client |
| **Workspace** | A named top-level scope of one immutable Workspace Kind that owns one Workspace Directory and one or more Windows. | Session, attached session, project |
| **Workspace Kind** | The immutable runtime-only classification of a Workspace as Scratch, Local Project, or Remote Project. | mode, source, converted Workspace |
| **Scratch Workspace** | A Workspace created at `HOME` whose Workspace Directory follows its Directory Authority. | Ad Hoc Workspace, default project, temporary session |
| **Local Project Workspace** | A Workspace opened from a native directory selection whose Project Root never changes. | repository, Git project |
| **Remote Project Workspace** | A Workspace pinned to one SSH Destination and one immutable remote Physical Directory Identity. | SSH session, remote terminal, remote host |
| **Workspace Directory** | The authoritative exact directory value used to start future Terminal Sessions, represented by a validated local directory or a Remote Workspace Directory according to Workspace Kind. | Focused Pane directory, fallback directory, untyped path |
| **Directory Authority** | The one Pane whose valid live Reported Working Directory may change a Scratch Workspace's Workspace Directory. | focused Pane, active Pane |
| **Project Root** | The exact originally selected directory and canonical filesystem identity owned immutably by a Local Project Workspace. | Git root, repository root |
| **SSH Destination** | The exact validated OpenSSH destination token selected for a Remote Project Workspace and retained as part of its identity. | physical host identity, resolved hostname, server |
| **Remote Workspace Directory** | The exact absolute or home-relative remote directory spelling retained for Remote Terminal Session startup. | local path, `PathBuf`, Project Root |
| **Physical Directory Identity** | The validated absolute physical remote directory returned by remote resolution for deduplication and automatic naming. | Remote Workspace Directory, local filesystem identity, display path |
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
| **Workspace Chip** | The Active Workspace's icon, name, and pinned state shown in the top-left chrome only while the sidebar is hidden; it is a label, never a control. | breadcrumb, workspace switcher |
| **New Workspace Panel** | The transient chooser that presents every Workspace Source as one row and performs no lifecycle action itself; it blocks Terminal Input Focus while open. | New Workspace command, workspace wizard |
| **Workspace Source** | The one row of the New Workspace Panel that produces a Workspace: Scratch, Local Project, or Remote Project. | Workspace Kind, project type |
| **Workspace Picker** | The transient in-app, keyboard-first, one-level filesystem navigator that is the primary Open Local Project selection mechanism and blocks Terminal Input Focus while open. | project index, recent projects, file browser |
| **Finder Fallback** | The explicit native directory chooser opened only from the Workspace Picker footer; it returns to the unchanged picker on cancellation and uses the same validation path on selection. | primary project picker, canonical selection path |
| **Zoomed Pane** | The Focused Pane temporarily shown alone while its Window's Pane Layout remains intact. | Maximized Window, fullscreen Pane |

## Terminal runtime

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Terminal Session** | The live terminal runtime owned by a Pane, joining terminal emulation and a PTY to either a local Shell Process or a remote Terminal Session Channel. | Session, Workspace session |
| **Control Connection** | The Remote Project runtime that exclusively owns one OpenSSH master process and private runtime socket for one SSH Destination. | Terminal Session, SSH client, shared Pane process |
| **Terminal Session Channel** | A single-use prepared OpenSSH channel command consumed by one Remote Pane's Terminal Session through its Control Connection. | Control Connection, shell command string, reusable channel |
| **Remote Connection Phase** | One of Connecting, Connected, Reconnecting, Disconnected, Failed, or Closing for a Remote Project Workspace's Control Connection. | network status string, Terminal Session exit state |
| **Connection Generation** | The monotonic identity of one Remote Project Workspace connection attempt and its accepted lifecycle observations. | Presentation Generation, retry count, frame number |
| **Runtime Observation** | An acceptance-only, authenticated, content-free stream of bounded numeric and closed-enum facts from one production Pane and Terminal Session; collection failure means NOT-RUN rather than PASS or FAIL. | telemetry, terminal transcript, runtime log |
| **Acceptance Failure Action** | A nonce-bound, sequenced, one-shot request from the authenticated mounted-app verifier that selects one fixed production failure Seam and returns only closed-enum state facts; it does not exist during an ordinary launch. | test flag, debug command, arbitrary fault payload |
| **Terminal Emulator** | The state machine that interprets terminal output and maintains the visible grid and Scrollback. | Terminal Session, Pane |
| **PTY** | The macOS pseudoterminal that connects a Terminal Emulator to its Shell Process. | Terminal, shell |
| **Shell Process** | The command interpreter process launched for a Pane through its PTY. | Terminal, session |
| **Shell Integration** | Versioned temporary startup resources injected only into a supported Shell Process to report safe directory, prompt, command, and completion facts without changing user configuration. | shell plugin installation, dotfile rewrite |
| **Terminal Capability Identity** | The canonical SpaceTerm program, version, `TERM`, terminfo, XTVERSION, device-attribute, and XTGETTCAP profile exposed by one Terminal Session. | emulator dependency identity, user-agent string |
| **Compatibility Identity** | The narrow default `TERM_PROGRAM=ghostty` ecosystem adapter exposed to name-gated Shell Processes while `SPACETERM=1` preserves direct identification; it is not terminal protocol authority. | Terminal Capability Identity, Ghostty runtime |
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
| **Terminal Local File Capabilities** | Session-scoped authority derived from Terminal metadata that enables local filesystem interactions only for Local Terminal Sessions. | filesystem access flag, remote path adapter |
| **Terminal Accessibility Model** | A Pane-owned native editable text-area projection whose cell-atomic UTF-16 ranges map visible and retained terminal text, selected terminal font metadata, Selection, Cursor, and bounds back to logical terminal cells. | flattened cell string, screen-reader transcript |
| **Render Lifecycle** | Pane-owned visibility, animation, scale, and newest-generation presentation state that schedules frames only when its native surface can present them. | render loop, global animation timer |
| **Terminal Failure** | A typed PTY, Terminal Emulator, presentation, platform, or renderer-resource fault with an explicit recoverability class and no terminal contents or secrets. | stderr message, shell exit |
| **Local Diagnostics** | A bounded content-free sequence of Terminal Failure identifiers and unhandled keyboard event kind, action, and native key code written only after explicit user export. | telemetry, crash upload, terminal log |
| **Paste Confirmation** | A transient authorization for one unsafe Paste Payload, identified opaquely and valid only while its Pane retains Terminal Input Focus and the worker deadline has not expired. | generic modal, clipboard permission |
| **OSC 52 Authorization** | A bounded, opaque, one-operation decision governing whether a terminal program may read or write a named clipboard target; access is denied unless explicit policy allows or asks for it. | paste confirmation, unrestricted clipboard access |

## Lifecycle actions

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Create Scratch Workspace** | Add and activate a new Scratch Workspace with its initial Window and Pane. | Create Workspace, start session, attach session |
| **Show New Workspace Panel** | Present the New Workspace Panel so one Workspace Source can be chosen. | New Workspace, Create Workspace |
| **Open Local Project** | Confirm one readable local directory through the Workspace Picker, or its explicit Finder Fallback, and create or activate its Local Project Workspace. | Open repository, convert Workspace |
| **Open Remote Project** | Validate one Remote Workspace Directory through an SSH Destination and create or activate its Remote Project Workspace. | open remote terminal, connect to host |
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
- A **Workspace Kind** is fixed for the lifetime of its runtime-only **Workspace** and is never persisted.
- A **Scratch Workspace** starts at `HOME`; its **Directory Authority** is initially the first Pane of its first Window.
- A **Local Project Workspace** preserves its exact selected or typed **Project Root** spelling and deduplicates equivalent selections by macOS device/file identity.
- A **Remote Project Workspace** preserves its first accepted **Remote Workspace Directory** spelling and deduplicates only by the composite of exact **SSH Destination** and **Physical Directory Identity**.
- Selecting the same Remote Project composite activates the existing **Workspace** without constructing a replacement payload; a different SSH alias is a different **SSH Destination** and remains distinct.
- An unrenamed **Remote Project Workspace** at its remote home **Physical Directory Identity** is named by its **SSH Destination** alone; every other one is named `<physical basename> · <SSH Destination>`.
- The **Workspace Picker** is the primary **Open Local Project** path. It starts at `HOME` on every open, performs live one-level directory reads, and owns no recents, index, persistence, fuzzy matching, or filesystem watching.
- A **Workspace Source** names the origin a Workspace is created from; a **Workspace Kind** is what the created Workspace immutably is.
- A **Workspace Chip** never duplicates the sidebar: it appears only while the sidebar is hidden, because an Active row already names the **Active Workspace**.
- The **New Workspace Panel** performs no lifecycle action. Choosing Local Project replaces it with the **Workspace Picker**, and Escape there returns to the panel while any other dismissal ends the flow.
- The **Finder Fallback** remains behind the open **Workspace Picker**; cancellation restores the picker unchanged, while selection joins the same background validation and Local Project identity-deduplication flow.
- Only the **Directory Authority** may update a Scratch **Workspace Directory**. Closing it promotes the first remaining Pane in Pane Layout order, or the root Pane of the first remaining Window when its Window closes.
- A valid promoted Pane report is adopted immediately. A missing or invalid report retains the previous Workspace Directory.
- Create Window and Split Pane revalidate the **Workspace Directory** before mutation. Unavailability blocks new children without stopping existing Terminal Sessions and clears after successful validation.
- Automatic Workspace names use the Workspace Directory basename, `Default` for `HOME`, and `/` for filesystem root. Only unrenamed Scratch Workspaces sharing one identity are numbered in sidebar order; empty rename input clears a custom name and duplicate custom names are valid.
- A Local **Workspace Directory** is local filesystem authority and is revalidated before child creation; a **Remote Workspace Directory** is never converted to a local path, queried through local directory APIs, or subjected to local filesystem validation.
- One live **Remote Project Workspace** owns one **Control Connection** for its current **Connection Generation**, and that Control Connection exclusively owns its master process, private runtime socket, shutdown, reap, and exact cleanup.
- Each Remote Pane owns one **Terminal Session** that consumes one **Terminal Session Channel**; the channel shares its Workspace's **Control Connection** but never shares Pane or Terminal Session ownership.
- A reconnect begins only after Disconnected or Failed and advances the **Connection Generation**; observations from older generations are stale, illegal same-generation transitions are rejected, and Closing is terminal.
- Delayed readiness, failure, or disconnection from a predecessor **Connection Generation** cannot mutate or resurrect its successor.
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
- **Terminal Local File Capabilities** are derived only from the Terminal metadata context: Local is Enabled and Remote is Disabled. A Remote Pane never classifies, emits, restores, validates, opens, previews, or inserts a local filesystem path, while web links, Selection, copy, ordinary text paste, Terminal Input, Terminal Find, and OSC 52 remain available under their existing policies.
- A **Terminal Accessibility Model** treats each retained text-bearing cell as one complete accessibility grapheme, rejects string reads that would expose only part of that cell, normalizes Selection requests to complete intersected cells, observes immutable terminal state, and may request worker-owned Selection changes, but never mutates the Terminal Emulator directly.
- A **Render Lifecycle** coalesces hidden Terminal Presentations to the newest **Presentation Generation** and schedules exactly one frame when visibility returns.
- A **Runtime Observation** is dormant without an authenticated mounted-app launch, never reaches a PTY or Shell Process, and never contains terminal content or derived content identity.
- An **Acceptance Failure Action** travels on the same authenticated app peer as its **Runtime Observation**, permits only one pending fixed case, and cannot carry terminal, clipboard, path, environment, or command content.
- Recoverable **Acceptance Failure Actions** complete only after the last valid **Presentation Generation** remains visible through retry; fatal actions complete at authenticated Pane-close receipt, while PID/PGID reap and a replacement Pane's command remain external campaign evidence.
- The normal-exit **Acceptance Failure Action** observes a real operator-entered `exit 0`; it never injects Shell Process exit.
- A recoverable **Terminal Failure** preserves the last valid **Presentation Generation** when possible; a fatal one requires closing the Pane and restarting its command.
- **Local Diagnostics** never contain terminal, clipboard, environment, path, secret, logical-key, or typed-key values and never leave the machine automatically.
- **OSC 52 Authorization** exposes only access direction, target, and byte count to UI; clipboard contents remain inside the worker-owned native-service operation and are never diagnostic metadata.
- The **Primary Screen** and **Alternate Screen** retain independent state; leaving the Alternate Screen restores the Primary Screen and its **Terminal Viewport**.
- A **Terminal Image** belongs to exactly one screen's bounded Kitty store; immutable snapshots may share its RGBA allocation only while its image ID and content generation are unchanged.
- An **Image Placement** is resolved by the Terminal Emulator, never reconstructed from placeholder cells by GPUI, and paints in the Kitty z layer relative to backgrounds and glyphs.
- A **Presentation Generation** belongs to one published Terminal Presentation; coordinate or Scrollback requests from an older generation never mutate current Terminal Emulator state.
- **Terminal Metadata** belongs to exactly one **Terminal Session**; Pane and Window chrome may consume its sanitized immutable values but never terminal control bytes or live emulator state.
- **Shell Integration** is scoped to one launched **Shell Process** and must preserve its ordinary startup files and environment or fall back without injection.
- **Terminal Capability Identity** selects `xterm-spaceterm` only with a discoverable packaged entry and otherwise retains `xterm-256color`; the default **Compatibility Identity** changes only `TERM_PROGRAM`, never SpaceTerm's terminfo, protocol replies, or supported capability set.
- A **Shell Process** receives no inherited foreign terminal-emulator or multiplexer resource, window, socket, session, or remote-control marker; **Compatibility Identity** cannot bind it to the runtime that launched SpaceTerm.
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

> **Dev:** "The user selected `~/project` and `/home/dev/project` through the same SSH Destination, and both resolve to one Physical Directory Identity. Do we create two Workspaces?"

> **Domain expert:** "No. **Open Remote Project** activates the existing **Remote Project Workspace** because its **SSH Destination** and **Physical Directory Identity** match, while preserving the first accepted **Remote Workspace Directory** spelling."

> **Dev:** "What if the same machine is selected through another SSH alias?"

> **Domain expert:** "That alias is a distinct **SSH Destination**, so it creates a distinct Workspace and **Control Connection**; each Remote Pane then consumes its own **Terminal Session Channel**."

> **Dev:** "Can output from that Remote Pane be treated as a local file path?"

> **Domain expert:** "No. Remote metadata disables **Terminal Local File Capabilities**, while web links, Selection, copy, ordinary text input, Terminal Find, and OSC 52 keep their existing policies."

## Flagged ambiguities

- "Window" can mean either a product **Window** or a native macOS **Operating-System Window**; use **Window** for the SpaceTerm hierarchy and qualify the native surface every time.
- "Tab" visually describes the Window selector but is not a SpaceTerm concept; always call the entity a **Window**.
- "Session" can incorrectly imply a tmux-style **Workspace** or describe a Pane's runtime; never use bare "session" for hierarchy concepts, and use **Terminal Session** only for the qualified runtime.
- tmux-inspired layout does not imply a server/client model; do not describe SpaceTerm concepts as clients, attached sessions, or shared Windows.
- "Active," "focused," and "selected" were used as near-synonyms; reserve **Active** for Workspace and Window state and **Focused** for Pane state.
- **Focused Pane** does not imply **Terminal Input Focus** while another responder, menu, modal, Operating-System Window, or application owns input.
- "Terminal" can mean the application, emulator, runtime, or visible region; prefer **SpaceTerm**, **Terminal Emulator**, **Terminal Session**, or **Pane** according to the intended concept.
- "Remote path" can mean the startup spelling or the resolved identity; use **Remote Workspace Directory** for the exact startup value and **Physical Directory Identity** for deduplication and naming, and use neither as a local path.
- "Host" can incorrectly collapse an SSH alias into a physical machine identity; use **SSH Destination** for the exact OpenSSH token because different aliases remain distinct.
- "Connection" can mean the shared SSH runtime or one Pane's channel; use **Control Connection** for the owned master and socket and **Terminal Session Channel** for the single-use Pane command.
- "Horizontal split" and "vertical split" can describe either divider orientation or Pane placement; use the user-facing actions **Split Right** and **Split Down**, and use an explicitly named split axis only in implementation discussions.
