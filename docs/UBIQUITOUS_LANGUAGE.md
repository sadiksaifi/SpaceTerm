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
| **Primary Screen** | The Terminal Emulator screen whose logical content includes bounded Scrollback and whose Terminal Viewport is restored after Alternate Screen use. | main buffer, normal buffer |
| **Alternate Screen** | The temporary Terminal Emulator screen used by full-screen terminal applications; it has no Scrollback and does not replace Primary Screen state. | alternate buffer, secondary terminal |
| **Terminal Viewport** | The visible row window over the active screen; it follows new output only while already at the bottom and otherwise remains anchored to logical content. | Workspace viewport, Pane scroll |
| **Scrollback** | Bounded Primary Screen output retained outside the bottom Terminal Viewport and available by moving that viewport. | Workspace scroll, history |
| **Presentation Generation** | The monotonic identity of a published Terminal Presentation grid, carried by coordinate and Scrollback mappings so stale mappings and older presentations can be rejected. | frame number, emulator generation |
| **Synchronized Output** | A bounded DEC 2026 transaction whose intermediate Terminal Emulator changes are withheld until one atomic Terminal Presentation is published. | render lock, output buffering |
| **Selection** | A terminal-owned logical content range created by pointer gestures and kept anchored across Scrollback movement, output, and resize reflow. | selected Workspace, selected Pane, text highlight |
| **Terminal Hyperlink** | A validated immutable target mapped to complete Terminal Presentation cells and opened only by an explicit same-generation activation. | arbitrary terminal text, automatic file execution |
| **Marked Text** | Transient input-method preedit owned by the Pane with Terminal Input Focus; it is presented at the Cursor but is not Terminal Emulator or PTY input until committed. | terminal output, committed text |
| **Secure Event Input** | Process-global macOS keyboard protection held only while exactly one live Pane both reads hidden canonical PTY input and owns Terminal Input Focus. | password text, per-Pane secure mode |
| **Paste Payload** | An immutable, size-bounded text insertion candidate owned by the Terminal Session worker from normalization through encoding or cancellation. | clipboard contents, typed key input |
| **File Insertion** | Ordered native file URLs converted to absolute POSIX-shell-quoted paths before becoming a Paste Payload. | raw URL paste, direct PTY write |
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
- **Marked Text** exists only while its Pane has **Terminal Input Focus**; cancellation or focus loss discards it without Terminal Session input.
- A **Paste Payload** that requires **Paste Confirmation** remains worker-owned; UI and diagnostics receive only bounded risk metadata, never its content.
- A **Paste Confirmation** is cancelled by focus or hierarchy loss, timeout, Terminal Session shutdown, explicit cancellation, or a stale identity, and cancellation writes no PTY bytes.
- **OSC 52 Authorization** exposes only access direction, target, and byte count to UI; clipboard contents remain inside the worker-owned native-service operation and are never diagnostic metadata.
- The **Primary Screen** and **Alternate Screen** retain independent state; leaving the Alternate Screen restores the Primary Screen and its **Terminal Viewport**.
- A **Presentation Generation** belongs to one published Terminal Presentation; coordinate or Scrollback requests from an older generation never mutate current Terminal Emulator state.
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
