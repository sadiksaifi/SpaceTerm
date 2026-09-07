# Ubiquitous Language

This document is the canonical vocabulary for current SpaceTerm product and domain behavior.

## Hierarchy

| Term | Definition |
| --- | --- |
| **SpaceTerm** | The application that owns all Workspaces. |
| **Workspace** | A named top-level scope with one immutable Workspace Kind, one Workspace Directory, and one or more Tabs. |
| **Workspace Kind** | Scratch, Local Project, or Remote Project. It cannot change during a Workspace's lifetime. |
| **Scratch Workspace** | A Workspace created at `HOME` whose Workspace Directory follows its Directory Authority. |
| **Local Project Workspace** | A Workspace opened at a local Project Root that never changes. |
| **Remote Project Workspace** | A Workspace pinned to one SSH Destination and one Physical Directory Identity. |
| **Workspace Directory** | The exact directory value used to start new Terminal Sessions in a Workspace. |
| **Directory Authority** | The Pane whose valid Reported Working Directory may update a Scratch Workspace's Workspace Directory. |
| **Project Root** | The exact selected path and retained filesystem identity of a Local Project Workspace. |
| **Tab** | An ordered work area that belongs to one Workspace and owns one Pane Layout. |
| **Pane Layout** | The recursive arrangement of Panes and Splits in a Tab. |
| **Split** | A Pane Layout node with exactly two child Pane Layouts and a constrained ratio. |
| **Pane** | A terminal region that is one leaf of a Tab's Pane Layout and owns one Terminal Session. |
| **Operating-System Window** | A native window that presents SpaceTerm. |

Use **Workspace**, not session or project, for the top-level product entity. Use **Tab** only for the
Workspace-owned entity, not the keyboard key or focus traversal.

## Focus and transient UI

| Term | Definition |
| --- | --- |
| **Active Workspace** | The one Workspace presented in an Operating-System Window. |
| **Active Tab** | The one Tab presented within a Workspace. |
| **Focused Pane** | The Pane selected by a Tab for Pane actions and focus restoration. |
| **Terminal Input Focus** | The transient state in which a Pane may accept terminal input because its Workspace and Tab are Active, it is the Focused Pane and responder, its window and application are active, and no temporary UI owns input. |
| **Zoomed Pane** | The Focused Pane presented alone while its Pane Layout remains intact. |
| **Workspace Chip** | The Active Workspace label shown only while the sidebar is hidden. It is not a control. |
| **New Workspace Panel** | The transient chooser of Workspace Sources. It creates nothing itself. |
| **Workspace Picker** | The in-app, one-level local directory navigator used by Open Local Project. |
| **System Directory Selection** | The system chooser available as an explicit fallback from the Workspace Picker. |

Use **Active** for Workspace and Tab state, **Focused** for Pane identity, and **Terminal Input
Focus** for actual input eligibility.

## Workspace sources and remote identity

| Term | Definition |
| --- | --- |
| **Workspace Source** | A New Workspace Panel choice that produces a Scratch, Local Project, or Remote Project Workspace. |
| **SSH Destination** | The exact validated OpenSSH destination token selected for a Remote Project Workspace. Different aliases remain distinct. |
| **Remote Workspace Directory** | The exact absolute or home-relative remote path spelling used to start remote Terminal Sessions. It is never a local path. |
| **Physical Directory Identity** | The resolved absolute remote directory used with SSH Destination for deduplication and automatic naming. |
| **Control Connection** | The OpenSSH master process and private socket owned by one Remote Project Workspace. |
| **Terminal Session Channel** | A single-use remote shell channel consumed by one Pane through its Workspace's Control Connection. |
| **Connection Generation** | The identity of one connection or reconnect attempt; older observations cannot affect a newer attempt. |
| **Authentication Prompt** | One OpenSSH confirmation or obscured response request presented by SpaceTerm. |

Use **SSH Destination** for the selected alias, **Remote Workspace Directory** for startup spelling,
and **Physical Directory Identity** for resolved identity. Use **Control Connection** for the shared
Workspace transport and **Terminal Session Channel** for a Pane's single-use channel.

## Terminal

| Term | Definition |
| --- | --- |
| **Terminal Session** | The runtime owned by one Pane, joining a Terminal Emulator to a PTY for a local Shell Process or remote Terminal Session Channel. |
| **Terminal Emulator** | The state machine that interprets terminal output and owns screen state. |
| **Native PTY Owner** | The platform-neutral owner of one PTY and the complete Shell Process lifecycle behind a host Adapter. |
| **Shell Process** | The command interpreter launched for a Pane. |
| **Primary Screen** | The screen with bounded Scrollback. |
| **Alternate Screen** | The temporary screen used by full-screen terminal applications; it has no Primary Screen Scrollback. |
| **Terminal Viewport** | The visible rows over the active screen. |
| **Presentation Generation** | The identity of one immutable Terminal Presentation, used to reject stale coordinate and Scrollback operations. |
| **Terminal Metadata** | Sanitized title, Reported Working Directory, Semantic Zone, command, and progress facts published with a Terminal Presentation. |
| **Reported Working Directory** | The last valid local absolute directory reported by trusted OSC 7 metadata. |

Use **Terminal Session** only for the Pane-owned runtime. Use **SpaceTerm**, **Terminal Emulator**,
**Terminal Session**, or **Pane** instead of the ambiguous bare word terminal where the distinction
matters.

## Terminal interaction and safety

| Term | Definition |
| --- | --- |
| **Selection** | A terminal-owned logical content range anchored across Scrollback movement, output, and reflow. |
| **Terminal Find** | Literal search owned by one Pane over its active screen and available Scrollback. |
| **Terminal Hyperlink** | A validated target attached to complete presentation cells and activated only through the current modified-pointer gesture. |
| **Marked Text** | Pane-owned input-method preedit that becomes terminal input only when committed. |
| **Paste Payload** | A bounded text insertion candidate owned by the Terminal Session worker until accepted or cancelled. |
| **Paste Confirmation** | Time-bounded authorization for one unsafe Paste Payload while Terminal Input Focus remains valid. |
| **OSC 52 Authorization** | One deny-by-default decision for a bounded terminal clipboard read or write. |
| **Terminal Local File Capabilities** | Session-scoped authority that enables local path actions for Local Panes and disables them for Remote Panes. |
| **Terminal Failure** | A typed PTY, emulator, presentation, platform, or renderer fault with an explicit recovery class. |
| **Local Diagnostics** | Bounded content-free failure and unhandled-key metadata exported only after an explicit user action. |
| **Close Confirmation** | One authorization for an exact user-requested close that may discard running work. |

## Domain invariants

- SpaceTerm owns one or more Workspaces and an Operating-System Window has one Active Workspace.
- A Workspace owns one or more Tabs and has one Active Tab. A Tab owns one or more Panes and has
  one Focused Pane. A Split always has two children.
- Hierarchy identities are monotonic and never reused. Active and focused identities always refer
  to entities still owned by their parent.
- A Scratch Workspace starts at `HOME`. Closing its Directory Authority promotes the first
  remaining Pane in layout order, or the root Pane of the first remaining Tab. A valid promoted
  report is adopted; a missing or invalid report preserves the previous directory.
- A Local Project Workspace preserves its first Project Root spelling and deduplicates by retained
  filesystem identity. Scratch and Local Project Workspaces remain distinct at the same directory.
- A Remote Project Workspace preserves its first Remote Workspace Directory spelling and
  deduplicates only by SSH Destination plus Physical Directory Identity. Each Workspace owns its
  Control Connection; each Pane owns its Terminal Session.
- New Tabs and Panes start at the Workspace Directory and require successful identity
  revalidation. Failure blocks the new child without stopping existing Terminal Sessions.
- Remote Panes never treat remote values as local paths. They retain terminal input, Selection,
  copy, ordinary text paste, Terminal Find, web links, accessibility, and OSC 52.
- Primary and Alternate Screens retain independent state. Stale Presentation, query, connection,
  focus, and lifecycle generations cannot affect successors.
- Closing a hierarchy entity closes all owned Terminal Sessions and completes PTY and Shell Process
  cleanup. Closing the final Pane escalates to its Tab; closing the final Tab removes its Workspace
  when another exists or closes the window when globally final. Explicitly closing the final
  Workspace replaces it.
- User-requested close protects unknown or running work with one aggregate Close Confirmation.
  Cancellation or stale authority changes nothing; confirmation authorizes the captured target
  once. Automatic exit and internal failure cleanup do not prompt.
- No operation leaves an orphaned PTY, Shell Process, Control Connection, native registration, or
  filesystem authority.
