# SpaceTerm

SpaceTerm organizes local and remote terminal work into Workspaces, Tabs, Pane Layouts, and
Pane-owned Terminal Sessions.

## Hierarchy

**SpaceTerm**:
The application that owns all Workspaces.

**Workspace**:
A named top-level scope with one immutable Workspace Kind, one Workspace Directory, and one or more
Tabs.
_Avoid_: session, project

**Workspace Kind**:
The immutable classification of a Workspace as Scratch, Local Project, or Remote Project.

**Scratch Workspace**:
A Workspace created at `HOME` whose Workspace Directory follows its Directory Authority.

**Local Project Workspace**:
A Workspace opened at one immutable local Project Root.

**Remote Project Workspace**:
A Workspace pinned to one SSH Destination and one Physical Directory Identity.

**Workspace Directory**:
The exact directory value used to start new Terminal Sessions in a Workspace.

**Directory Authority**:
The Pane whose valid Reported Working Directory may update a Scratch Workspace's Workspace
Directory.

**Project Root**:
The exact selected path and retained filesystem identity of a Local Project Workspace.

**Tab**:
An ordered work area that belongs to one Workspace and owns one Pane Layout.
_Avoid_: Window, Terminal Session

**Pane Layout**:
The recursive arrangement of Panes and Splits in a Tab.

**Split**:
A Pane Layout node with exactly two child Pane Layouts and a constrained ratio.

**Pane**:
A terminal region that is one leaf of a Tab's Pane Layout and owns one Terminal Session.

**Operating-System Window**:
A native window that presents SpaceTerm.

## Focus and transient UI

**Active Workspace**:
The one Workspace presented in an Operating-System Window.

**Active Tab**:
The one Tab presented within a Workspace.

**Focused Pane**:
The Pane selected by a Tab for Pane actions and focus restoration.

**Terminal Input Focus**:
The transient eligibility of a Pane to accept terminal input while its Workspace and Tab are Active,
it is the Focused Pane and responder, its window and application are active, and temporary UI has
released input.

**Zoomed Pane**:
The Focused Pane presented alone while its Pane Layout remains intact.

**New Workspace Panel**:
The transient chooser that selects a Workspace Source for later creation.

**Workspace Picker**:
The in-app, one-level local directory navigator used by Open Local Project.

**System Directory Selection**:
The system chooser available as an explicit fallback from the Workspace Picker.

## Workspace sources and remote identity

**Workspace Source**:
A New Workspace Panel choice for a Scratch, Local Project, or Remote Project Workspace.

**SSH Destination**:
The exact validated OpenSSH destination token selected for a Remote Project Workspace; different
aliases remain distinct.

**Remote Workspace Directory**:
The exact absolute or home-relative remote path spelling used to start remote Terminal Sessions.

**Physical Directory Identity**:
The resolved absolute remote directory paired with an SSH Destination for deduplication and
automatic naming.

**Control Connection**:
The Workspace-owned OpenSSH transport shared by its Remote Panes.

**Terminal Session Channel**:
The single-use remote shell channel consumed by one Remote Pane through its Control Connection.

**Authentication Prompt**:
One OpenSSH confirmation or obscured response request presented by SpaceTerm.

## Terminal

**Terminal Session**:
The runtime owned by one Pane, joining a Terminal Emulator to a local shell or remote Terminal
Session Channel.
_Avoid_: session, terminal

**Terminal Emulator**:
The state machine that interprets terminal output and owns screen state.

**Terminal Metadata**:
Sanitized title, Reported Working Directory, Semantic Zone, command, and progress facts associated
with terminal screen state.

**Reported Working Directory**:
The last valid local absolute directory reported by trusted OSC 7 metadata.

## Terminal interaction and safety

**Selection**:
A terminal-owned logical content range anchored across Scrollback movement, output, and reflow.

**Terminal Find**:
Literal search owned by one Pane over its active screen and available Scrollback.

**Terminal Hyperlink**:
A validated target attached to complete terminal cells and activated through the current
modified-pointer gesture.

**Paste Payload**:
A bounded text insertion candidate retained until accepted or cancelled.

**Paste Confirmation**:
Time-bounded authorization for one unsafe Paste Payload while Terminal Input Focus remains valid.

**OSC 52 Authorization**:
One deny-by-default decision for a bounded terminal clipboard read or write.

**Terminal Local File Capabilities**:
Session-scoped authority for local path actions in a Local Pane.

**Terminal Failure**:
A typed terminal fault with an explicit recovery class.

**Local Diagnostics**:
Bounded content-free failure and unhandled-key metadata exported after an explicit user action.

**Close Confirmation**:
One authorization for an exact user-requested close that may discard running work.
