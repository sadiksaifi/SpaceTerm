# SpaceTerm

SpaceTerm organizes terminal work into Workspaces, Tabs, Pane Layouts, and Pane-owned Terminal Sessions.

## Hierarchy

**SpaceTerm**: The application that owns all Workspaces.

**Workspace**: A named top-level scope containing Tabs and an optional Pinned Directory.

**Local Workspace**: A Workspace whose Terminal Sessions run on this machine.

**Remote Workspace**: A Workspace whose Terminal Sessions run on the machine identified by an SSH Destination.

**Current Directory**: A Terminal Session's current working directory on its machine.

**Starting Directory**: The directory selected when creating a Terminal Session.

**Pinned Directory**: An explicitly selected directory that sets a Workspace's identity and the directory of its new Terminal Sessions.

**Tab**: An ordered work area belonging to one Workspace and owning one Pane Layout.
_Avoid_: Window, Terminal Session

**Pane Layout**: The recursive arrangement of Panes and Splits in a Tab.

**Split**: A Pane Layout node dividing its space between two children.

**Pane**: A leaf of a Tab's Pane Layout that owns one Terminal Session.

**Root Pane**: The stable Pane that supplies a Tab's automatic directory, independent of focus and layout order.

**Root Tab**: The stable Tab whose Root Pane supplies a Workspace's unpinned directory and automatic name.

**Pane Caption**: The header naming a Pane and carrying its direct controls.

**Pane Origin**: The account and machine where a Pane's Terminal Session runs, which make the Pane Local or Remote.

**Operating-System Window**: A native window that presents SpaceTerm.

**Window Controls**: The host's close, minimize, and maximize controls for an Operating-System Window.

## Settings

**Settings**: Application-scoped preferences retained across launches.

**Settings Document**: The retained Settings and installed Terminal Themes.

**Settings Window**: The Operating-System Window presenting Settings.

**Settings Section**: A named group of Settings presented together.

**Settings Row**: A labeled Setting control within a Settings Section.

**Settings Search**: Search over Settings Rows that reveals a matching row.

**Appearance Mode**: The Light, Dark, or Auto choice selecting the Application Chrome appearance and its Terminal Theme slot.

**Malformed Settings**: Retained data that cannot be read as a valid Settings Document. Storage failures are a distinct condition.

**Settings Recovery**: Replacing Malformed Settings with defaults.

## Keybindings

**Command**: A SpaceTerm operation with a configurable Shortcut. Standard application operations are not Commands.

**Shortcut**: One key chord with any combination of the host modifiers.
_Avoid_: Hotkey, key equivalent

**Keybinding**: A Command's resolved Shortcut, or Unassigned.

**Unassigned**: A Keybinding with no Shortcut.

**Keymap**: The host's Command defaults with retained overrides applied, with one owner per Shortcut.

**Reserved Shortcut**: A Shortcut unavailable to Commands because it is Terminal Reserved or System Reserved.

**Terminal Reserved**: A Shortcut owned by terminal input or programs in a Terminal Session.

**System Reserved**: A Shortcut owned by the operating system or a standard application operation.

**Inactive Override**: A retained override unavailable on the current host or keyboard layout.

**Keyboard Shortcuts**: The Command opening the Keybindings Settings Section.

## Updates

**Overdue Update**: An available update that a fresh launch requires before opening.

**Update Reminder**: A deferrable notice about an outstanding update.

## Focus and transient UI

**Active Workspace**: The Workspace presented in an Operating-System Window.

**Active Tab**: The Tab presented within a Workspace.

**Next Tab** / **Previous Tab**: The Commands activating the neighboring Tab in Workspace order.

**Move Tab Right** / **Move Tab Left**: The Commands moving the Active Tab among its Workspace's Tabs.

**Focused Pane**: The Pane selected by a Tab for Pane actions and focus restoration.

**Terminal Input Focus**: A Pane's current eligibility to accept terminal input.

**Zoomed Pane**: The Focused Pane presented alone while its Pane Layout remains intact.

**Workspace Switcher**: The transient chooser for activating an existing Workspace or naming a new one.

**Open Local Directory**: The Command opening a Local Workspace pinned to a selected directory.

**Open Remote Directory**: The Command opening a Remote Workspace pinned to a selected Remote Directory.

**Directory Picker**: The chooser browsing one machine's directories to select a Pinned Directory.

**System Directory Selection**: The system chooser offered for selecting a local Pinned Directory.

## Workspace location and remote identity

**Workspace Location**: Where a Workspace runs: Local or Remote. Pinning is independent of it.

**SSH Destination**: The OpenSSH destination token selected for a Remote Workspace. Distinct aliases are distinct destinations.

**Remote Directory**: An absolute or home-relative directory value on a remote machine.

**Remote Directory Identity**: The physical absolute path of a Remote Directory on its machine.

**Control Connection**: The Workspace-owned OpenSSH transport shared by its Remote Panes.
_Avoid_: Remote Workspace Session, SSH session

**Terminal Session Channel**: The single-use remote shell channel consumed by one Remote Pane through its Control Connection.
_Avoid_: remote channel, Pane channel

**Authentication Prompt**: An OpenSSH confirmation or obscured response request presented by SpaceTerm.

## Terminal

**Terminal Session**: The Pane-owned runtime joining a Terminal Emulator to a local shell or remote Terminal Session Channel.
_Avoid_: session, terminal

**Terminal Emulator**: The state machine interpreting terminal output and owning screen state.

**Terminal Metadata**: Sanitized facts about terminal screen state, such as its title and Current Directory.

## Terminal interaction and safety

**Selection**: A logical range of terminal content.

**Terminal Find**: Search within one Pane's screen and Scrollback.

**Scroll Commands**: The Commands moving the Focused Pane's view through Scrollback.

**Hyperlink Target**: A target attached to terminal cells for activation.

**File Preview**: A host preview of a local file.

**Paste Payload**: Text waiting for insertion until accepted or cancelled.

**Paste Selection**: The Linux operation inserting PRIMARY Selection as a Paste Payload.

**Paste Confirmation**: Authorization for one unsafe Paste Payload.

**Clipboard Authority**: A Terminal Session's grant to complete OSC 52 clipboard requests.

**System Permission**: An operating-system privacy grant that programs in a Terminal Session inherit.

**Permission Request**: A terminal-output request offering Permission Setup for missing System Permissions.

**Permission Setup**: A user-initiated guided pass through System Settings to grant a System Permission.

**Setup Guide**: The panel accompanying Permission Setup.

**Terminal Local File Capabilities**: Terminal Session-scoped authority for local path actions in a Local Pane.

**Terminal Failure**: A terminal fault with an explicit recovery class.

**Terminal Diagnostics**: Terminal fault metadata exported by the Export Terminal Diagnostics command.

**Close Confirmation**: Authorization for an exact user-requested close that may discard running work.

## Appearance

**Application Chrome**: SpaceTerm's built-in interface around terminal output.

**Terminal Theme**: The identifying metadata and colors of terminal output in Panes.

**Zed Theme**: A named Light or Dark theme in Zed's theme family format.

**Zed Extension**: A package in Zed's extension registry that can contribute Zed Theme family documents.

**Resolved Appearance**: The Application Chrome and terminal presentation resolved from Settings and the system appearance.

**Theme Origin**: Source identity and attribution, distinct from the installed identifier and display name.

**Window Background Appearance**: Native window presentation, independent of Light/Dark and color alpha.

**Transparency**: The amount of underlying content visible through window backgrounds and floating surfaces.

**Density**: The Compact or Comfortable spacing of Application Chrome.

**Blur**: Softening of content behind window backgrounds and floating surfaces.

**Surface Material**: The translucency and neutral shading of a background fill.

**Developer Workbench**: The Development window for previewing appearance and interface controls.
