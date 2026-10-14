# SpaceTerm

SpaceTerm Servers own terminal work in Workspaces, Tabs, and Panes.
SpaceTerm Clients present that work through their own Workspace Attachments and Pane Layouts.

## Hierarchy

**SpaceTerm**: The product comprising SpaceTerm Servers and SpaceTerm Clients.

**Workspace**: A named, server-owned scope containing Tabs and an optional Pinned Directory.

**Local Workspace**: A Workspace presented through an authenticated private local connection to the client's account-owned SpaceTerm Server.

**Remote Workspace**: A Workspace presented through a network or SSH Tunnel connection to a SpaceTerm Server, including a server on the same machine.

**Current Directory**: A Terminal Session's current working directory on its machine.

**Starting Directory**: The directory selected when creating a Terminal Session.

**Pinned Directory**: An explicitly selected directory on the owning server used when choosing Starting Directories for a Workspace's new Terminal Sessions.

**Tab**: A server-owned work area belonging to one Workspace and containing Panes, with an optional Worktree association.
_Avoid_: Window, Terminal Session

**Tab Display Order**: One SpaceTerm Client's personal ordering of a Workspace's Tabs.

**Pane Layout**: One SpaceTerm Client's recursive arrangement of a Tab's Panes and Splits.

**Split**: A client-owned Pane Layout node dividing its space between two children.

**Pane**: A server-owned member of a Tab with a stable identity and at most one live Terminal Session.

**Root Pane**: The server-selected Pane that supplies a Tab's automatic directory, independent of client focus and Pane Layout.

**Root Tab**: The server-selected Tab whose Root Pane supplies a Workspace's unpinned directory and automatic name.

**Pane Caption**: The header naming a Pane and carrying its direct controls.

**Pane Origin**: The server account and machine where a Pane's Terminal Session runs, with Local or Remote classification from the client's authenticated connection.

**Operating-System Window**: A native window that presents SpaceTerm.

**Window Controls**: The host's close, minimize, and maximize controls for an Operating-System Window.

## Servers and clients

**SpaceTerm Server**: The headless application that owns shared Workspace organization, entity identities, Roots, Terminal Session lifecycles, Server Settings, and Git operations on its machine.

**SpaceTerm Client**: An application that connects to SpaceTerm Servers and owns Workspace Attachments, Client View State, terminal presentation, and client-local native effects.

**Workspace Window**: A desktop SpaceTerm Client's single main Operating-System Window presenting its Workspace Attachments.

**Workspace Attachment**: A SpaceTerm Client's association with a server-owned Workspace for presentation.

**Attach Workspace**: The action that creates a Workspace Attachment.

**Detach Workspace**: The action that removes a Workspace Attachment.

**Kill Workspace**: The action that removes a server-owned Workspace and its saved organization, ending all of its Terminal Sessions.

**SpaceTerm Protocol**: The versioned, language-neutral exchange of server state, terminal presentation, and client operations between SpaceTerm Clients and SpaceTerm Servers.

**SSH Bootstrap**: Explicit setup through an SSH Destination to discover or install an account-owned SpaceTerm Server and enroll the initiating device. Ordinary reconnect uses the existing pairing.

**Start Server**: The explicit action that starts a SpaceTerm Server.

**Stop Server**: The explicit action that stops a SpaceTerm Server and ends its Terminal Sessions.

**End All Sessions**: The action that ends every live Terminal Session on one SpaceTerm Server while retaining the running server and its Workspace organization.

**Paired Device**: A client device whose identity a SpaceTerm Server has authorized for terminal access and server management.

**Unpair Device**: The action that requests revocation of a Paired Device's access to one SpaceTerm Server and removes the initiating client's saved connection when it unpairs itself.

**Unpair Pending**: An Unpair Device request whose server-side revocation is awaiting confirmation, independently of completed local connection removal.

**Client Settings**: Preferences used by a SpaceTerm Client on its own device.

**Server Settings**: Preferences owned by one SpaceTerm Server and shared by its authorized clients.

**Client View State**: A SpaceTerm Client's presentation choices for server-owned work, including Tab Display Order, Pane Layouts and Split ratios, Active Workspace, Active Tab, Focused Pane, Zoomed Pane, Selection, Terminal Find, and scroll position.

**Controlling Client**: The SpaceTerm Client with authority to supply terminal input and terminal dimensions to a Pane.

**Take Over**: The action that replaces a Pane's Controlling Client.

**Takeover Screen**: The Pane presentation shown to a SpaceTerm Client when another client is its Controlling Client.

## Settings

**Settings**: Preferences retained across launches, scoped to the client or a selected server.

**Settings Document**: The retained Client Settings and installed Terminal Themes. Server organization, pairings, Workspace Attachments, and Client View State are retained separately.

**Settings Window**: The Operating-System Window presenting Settings.

**Settings Section**: A named group of Settings presented together.

**Settings Row**: A labeled Setting control within a Settings Section.

**Settings Search**: Search over Settings Rows that reveals a matching row.

**Appearance Mode**: The Light, Dark, or Auto choice selecting the Application Chrome appearance and its Terminal Theme slot.

**Malformed Settings**: Retained data that cannot be read as a valid Settings Document. Storage failures are a distinct condition.

**Unsupported Settings**: A retained Settings Document whose schema version is newer than this SpaceTerm reads. It is not Malformed Settings, so Settings Recovery never replaces it.

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

**Next Tab** / **Previous Tab**: The Commands activating the neighboring Tab in the client's Tab Display Order, within the Active Worktree.

**Move Tab Right** / **Move Tab Left**: The Commands moving the Active Tab in the client's Tab Display Order, within the Active Worktree.

**Next Worktree** / **Previous Worktree**: The Commands activating the neighboring Worktree that has Tabs, in sidebar order, wrapping at either end.

**Focused Pane**: The Pane selected in a client's Tab view for Pane actions and focus restoration.

**Terminal Input Focus**: A Pane's current eligibility to accept terminal input.

**Input Method Text**: Text the host's text input system delivers to a Pane, such as an input method commit or an emoji picker selection. The Focused Pane accepts it without Terminal Input Focus.

**Zoomed Pane**: The Focused Pane presented alone while its Pane Layout remains intact.

**Workspace Switcher**: The transient chooser for activating an existing Workspace or naming a new one.

**Open Local Directory**: The Command opening a Local Workspace pinned to a selected directory.

**Open Remote Directory**: The Command opening a Remote Workspace pinned to a selected Remote Directory.

**Directory Picker**: The chooser browsing one machine's directories to select a Pinned Directory.

**System Directory Selection**: The system chooser offered for selecting a local Pinned Directory.

## Workspace location and remote identity

**Workspace Location**: A Workspace's Local or Remote classification in the client. Pinning is independent of it.

**SSH Destination**: The OpenSSH destination token selected for SSH Bootstrap or an SSH Tunnel. Distinct aliases are distinct destinations, not proof of server identity.

**Remote Directory**: An absolute or home-relative directory value on a remote server's machine.

**Remote Directory Identity**: The physical absolute path of a Remote Directory on its machine.

**Server Connection**: A SpaceTerm Client's authenticated SpaceTerm Protocol connection to a pinned, paired SpaceTerm Server.

**SSH Tunnel**: An OpenSSH route carrying a Server Connection to a SpaceTerm Server. SSH access alone grants no SpaceTerm Protocol authority.

**Authentication Prompt**: An OpenSSH confirmation or obscured response request presented by SpaceTerm.

## Terminal

**Terminal Session**: The server-owned live runtime for one Pane, joining a Terminal Emulator to a PTY and shell on the server's machine.
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

## Repositories

**Repository Status**: The read-only git facts SpaceTerm presents for the repository containing a directory: branch, operation, upstream divergence, and changes.
_Avoid_: git status, VCS status, git info

**Repository Source Directory**: The directory Repository Status reads for a Pane: its Current Directory as of the last prompt.
_Avoid_: git directory, prompt directory

**Prompt Owner**: A random value each Terminal Session gives its owning shell, which SpaceTerm's shell integration attaches to that shell's prompt markers. Once the shell has sent it, only markers that carry it move the Repository Source Directory, so a nested `ssh` or `docker` shell cannot.
_Avoid_: shell token, session secret

**Pull Request**: The open GitHub pull request whose head is a repository's current branch, found through the client's GitHub CLI using server-supplied repository facts.
_Avoid_: PR, merge request

**Worktree**: One of a git repository's working trees on the owning server's machine, listed under a Workspace whose directory is in that repository. Its Tabs start inside it.
_Avoid_: checkout, working copy

**Main Worktree**: The Worktree git lists first, which holds the repository's own working tree. It cannot be removed.

**Active Worktree**: The Worktree of the Active Tab. The tab strip shows only its Tabs.

**Missing Worktree**: A Worktree git still lists whose directory no longer exists. It opens no Tabs.

**Worktree Location**: The Settings template naming the directory of a new Worktree from its repository and branch.

## Appearance

**Application Chrome**: SpaceTerm's built-in interface around terminal output.

**Terminal Theme**: The identifying metadata and colors of terminal output in Panes.

**Zed Theme**: A named Light or Dark theme in Zed's theme family format.

**Zed Extension**: A package in Zed's extension registry that can contribute Zed Theme family documents.

**Resolved Appearance**: The Application Chrome and terminal presentation resolved from Settings and the system appearance.

**Theme Harmonization**: Fitting a Terminal Theme's background to the band SpaceTerm's own Panes rest in, keeping its hue and restoring the text contrast that move costs.

**Theme Origin**: Source identity and attribution, distinct from the installed identifier and display name.

**Window Background Appearance**: Native window presentation, independent of Light/Dark and color alpha.

**Opacity**: The amount of underlying content covered by window backgrounds and floating surfaces.

**Density**: The Compact or Comfortable spacing of Application Chrome.

**Blur**: Softening of content behind window backgrounds and floating surfaces.

**Background Image**: An image a person chose that Workspace windows present behind their content in place of the desktop. Opacity and Blur apply to it as they do to the desktop.

**Surface Material**: The translucency and neutral shading of a background fill.

**Developer Workbench**: The Development window for previewing appearance and interface controls.
