# SpaceTerm

SpaceTerm organizes local and remote terminal work into Workspaces, Tabs, Pane Layouts, and
Pane-owned Terminal Sessions.

## Hierarchy

**SpaceTerm**:
The application that owns all Workspaces.

**Workspace**:
A named top-level scope containing one or more Tabs and an optional Pinned Directory.

**Local Workspace**:
A Workspace whose Terminal Sessions run on this machine.

**Remote Workspace**:
A Workspace whose Terminal Sessions run on a machine identified by an SSH Destination.

**Current Directory**:
An individual Terminal Session's current working directory on its machine.

**Starting Directory**:
The directory selected when creating one Terminal Session.

**Pinned Directory**:
An explicitly selected directory used for a Workspace's identity and future Terminal Sessions.

**Tab**:
An ordered work area that belongs to one Workspace and owns one Pane Layout.
_Avoid_: Window, Terminal Session

**Pane Layout**:
The recursive arrangement of Panes and Splits in a Tab.

**Split**:
A Pane Layout node with exactly two child Pane Layouts and a constrained ratio.

**Pane**:
A terminal region that is one leaf of a Tab's Pane Layout and owns one Terminal Session.

**Pane Caption**:
The header a Pane presents above its terminal region, naming the Pane and carrying its direct
controls.

**Pane Origin**:
The account and machine a Pane's Terminal Session runs on, classified Local or Remote by the
Session itself rather than by the spelling of either value.

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

**Workspace Switcher**:
The transient chooser for selecting an existing Workspace or naming a new Local Workspace or Remote Workspace.

**Directory Picker**:
The directory navigator used to select a Pinned Directory explicitly.

**System Directory Selection**:
The system chooser available as an explicit fallback from the Directory Picker.

## Workspace sources and remote identity

**Workspace Source**:
A choice identifying whether a Workspace runs locally or on a remote machine.

**SSH Destination**:
The exact validated OpenSSH destination token selected for a Remote Workspace; different
aliases remain distinct.

**Remote Directory**:
An absolute or home-relative directory value on the remote machine, without local filesystem authority.

**Physical Directory Identity**:
The resolved absolute identity of a Remote Directory used to validate an explicit pin.

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
Sanitized title, Current Directory, Semantic Zone, command, and progress facts associated
with terminal screen state.

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

**OSC 52 Filtering**:
Bounded recognition and unconditional denial of terminal clipboard read and write escape sequences.

**Terminal Local File Capabilities**:
Session-scoped authority for local path actions in a Local Pane.

**Terminal Failure**:
A typed terminal fault with an explicit recovery class.

**Local Diagnostics**:
Bounded content-free failure and unhandled-key metadata exported after an explicit user action.

**Close Confirmation**:
One authorization for an exact user-requested close that may discard running work.
