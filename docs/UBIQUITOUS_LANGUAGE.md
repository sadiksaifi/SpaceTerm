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
| **Focused Pane** | The one Pane in a Window that is the target for terminal input. | Active Pane, selected Pane |
| **Zoomed Pane** | The Focused Pane temporarily shown alone while its Window's Pane Layout remains intact. | Maximized Window, fullscreen Pane |

## Terminal runtime

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Terminal Session** | The live terminal runtime owned by a Pane, joining terminal emulation, a PTY, and a Shell Process. | Session, Workspace session |
| **Terminal Emulator** | The state machine that interprets terminal output and maintains the visible grid and Scrollback. | Terminal Session, Pane |
| **PTY** | The macOS pseudoterminal that connects a Terminal Emulator to its Shell Process. | Terminal, shell |
| **Shell Process** | The command interpreter process launched for a Pane through its PTY. | Terminal, session |
| **Scrollback** | Terminal output retained above the currently visible grid. | Workspace scroll, history |

## Lifecycle actions

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Create Workspace** | Add and activate a new Workspace with its initial Window and Pane. | Start session, attach session |
| **Close Workspace** | Remove a Workspace and its owned terminal runtimes, replacing it when it is the last Workspace. | Detach session, delete session |
| **Create Window** | Add and activate a new Window in the Active Workspace. | New tab, link Window |
| **Close Window** | Remove a Window and its owned terminal runtimes, or close the Operating-System Window when it is the final Window. | Close tab, detach Window |
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
- A **Pane** belongs to exactly one **Window** and owns one **Terminal Session**.
- A **Split** has exactly two child **Pane Layouts**; each child is either another **Split** or a **Pane**.
- Closing any hierarchy entity closes every **Terminal Session**, **PTY**, and **Shell Process** it owns.
- No operation may leave an orphaned **PTY** or **Shell Process**.
- Active and focused identities always reference entities still owned by their parent.

## Example dialogue

> **Dev:** "The user clicked Split Right in the Active Window. Should that create another Window?"

> **Domain expert:** "No. **Split Pane** adds a **Pane** to the same **Window**, updates its **Pane Layout**, and makes the new Pane the **Focused Pane**."

> **Dev:** "What happens if they close that Pane after it becomes the only Pane?"

> **Domain expert:** "**Close Pane** escalates to **Close Window**. If it is also the final Window, SpaceTerm closes the **Operating-System Window** instead of leaving an empty Workspace."

## Flagged ambiguities

- "Window" can mean either a product **Window** or a native macOS **Operating-System Window**; use **Window** for the SpaceTerm hierarchy and qualify the native surface every time.
- "Tab" visually describes the Window selector but is not a SpaceTerm concept; always call the entity a **Window**.
- "Session" can incorrectly imply a tmux-style **Workspace** or describe a Pane's runtime; never use bare "session" for hierarchy concepts, and use **Terminal Session** only for the qualified runtime.
- tmux-inspired layout does not imply a server/client model; do not describe SpaceTerm concepts as clients, attached sessions, or shared Windows.
- "Active," "focused," and "selected" were used as near-synonyms; reserve **Active** for Workspace and Window state and **Focused** for Pane state.
- "Terminal" can mean the application, emulator, runtime, or visible region; prefer **SpaceTerm**, **Terminal Emulator**, **Terminal Session**, or **Pane** according to the intended concept.
- "Horizontal split" and "vertical split" can describe either divider orientation or Pane placement; use the user-facing actions **Split Right** and **Split Down**, and use an explicitly named split axis only in implementation discussions.
