# SpaceTerm

SpaceTerm organizes terminal work into Workspaces, Tabs, Pane Layouts, and Pane-owned Terminal Sessions.

## Hierarchy

**SpaceTerm**: The application for organizing and presenting terminal work.

**Workspace**: A named top-level scope containing Tabs and an optional Pinned Directory.

**Local Workspace**: A Workspace whose Terminal Sessions run on This Mac or This Computer.

**Remote Workspace**: A Workspace whose Terminal Sessions run on another Machine, reached through a paired QUIC connection.

**Current Directory**: A Terminal Session's current working directory on its Machine.

**Starting Directory**: The directory selected when creating a Terminal Session.

**Pinned Directory**: An explicitly selected directory that sets a Workspace's identity and the directory of its new Terminal Sessions.

**Tab**: An ordered work area belonging to one Workspace and owning one Pane Layout. In a Workspace that lists Worktrees, each Tab also belongs to one Worktree.
_Avoid_: Window, Terminal Session

**Pane Layout**: The recursive arrangement of Panes and Splits in a Tab.

**Split**: A Pane Layout node dividing its space between two children.

**Pane**: A leaf of a Tab's Pane Layout that owns one Terminal Session.

**Root Pane**: The stable Pane that supplies a Tab's automatic directory, independent of focus and layout order.

**Root Tab**: The stable Tab whose Root Pane supplies a Workspace's unpinned directory and automatic name.

**Pane Caption**: The header naming a Pane and carrying its direct controls.

**Pane Origin**: The Machine where a Pane's Terminal Session runs, which makes the Pane Local or Remote.

**Operating-System Window**: A native window that presents SpaceTerm.

**Window Controls**: The host's close, minimize, and maximize controls for an Operating-System Window.

## Core architecture vocabulary

Each SpaceTerm Core owns terminal work on its Machine. SpaceTerm Clients control, observe and present that work over account-local IPC or paired QUIC connections.

**SpaceTerm Core**: The package containing the SpaceTerm CLI, and the account service it runs on each computer, which owns that Machine's Workspaces and their Terminal Sessions independently of the applications presenting them.
_Avoid_: server

**Machine**: One account's SpaceTerm Core on one computer, as a SpaceTerm Client reaches it: This Mac or This Computer over the Account-local Connection, or a paired Machine through one or more Saved Connections.
_Avoid_: server, host

**This Mac** / **This Computer**: The local Machine's name: This Mac on macOS, This Computer on Linux.

**Machine ID**: The pinned identity of one SpaceTerm Core, which every Saved Connection to that Machine carries.

**SpaceTerm Client**: An application that controls, observes or presents SpaceTerm Core state.

**SpaceTerm CLI**: The `spaceterm` program in SpaceTerm Core, which runs the Core's account service and acts as a command-line SpaceTerm Client.

**Command Line Tool**: The link in the account's `~/.local/bin`, named for the build identity, that runs that identity's SpaceTerm CLI from any terminal.
_Avoid_: shim, alias, CLI install

**Workspace Window**: A desktop SpaceTerm Client's single main Operating-System Window presenting its Workspace Attachments.

**Workspace Attachment**: A SpaceTerm Client's association with a Core-owned Workspace for presentation.

**Attach Workspace**: The action that creates a Workspace Attachment.

**Detach Workspace**: The action that removes a Workspace Attachment.

**Kill Workspace**: The action that removes a Core-owned Workspace and its saved organization, ending all of its Terminal Sessions.

**Workspace Inventory**: A SpaceTerm Core's authoritative Workspaces, Tabs, Panes, Worktrees, and their shared organization, with the live Terminal Session and Pane control facts its clients reconcile against.

**SpaceTerm Protocol**: The versioned exchange of Core state, terminal presentation, and client operations between a SpaceTerm Client and SpaceTerm Core over one multiplexed connection, on which the Core pushes changes as they happen.

**Service lane**: The admission-time exchange for administering SpaceTerm Core, with its own protocol version, independent of the work protocol's, over an authenticated account-local or paired connection.

**SSH Setup**: Temporary OpenSSH provisioning inside Add Machine that installs or reuses SpaceTerm Core on the remote computer and enrolls the client for paired QUIC access.

**Start Core**: The explicit action that starts SpaceTerm Core on a Machine.

**Stop Core**: The explicit action that stops SpaceTerm Core and ends its Terminal Sessions, available only on This Mac or This Computer.

**Restart Core**: The explicit action that stops and starts SpaceTerm Core, ending its Terminal Sessions and retaining its Workspace organization.

**Uninstall Core**: The explicit action that removes SpaceTerm Core's service registration, Command Line Tool and installed versions, ending its Terminal Sessions. It retains the Machine's Workspace organization, Machine Settings and pairings unless the person also chooses to delete that data.

**End All Sessions**: The action that ends every live Terminal Session on one Machine while SpaceTerm Core keeps running and retains its Workspace organization.

**Pending Version**: A verified SpaceTerm Core version for one application identity and computer, installed beside the running version and waiting for a restart to run it.

**Update Core**: The action that installs a Pending Version and, after confirmation, restarts SpaceTerm Core into it, ending that Machine's Terminal Sessions.

**Original Operation**: An explicit operation a SpaceTerm Client submitted to SpaceTerm Core, bound to its admitted connection, application, Machine, client and operation identities until its outcome is validated and acknowledged.

**Unconfirmed Outcome**: An Original Operation whose dispatch SpaceTerm Core admitted and whose result could not be validated. Only resolving that same Original Operation settles it.

**Client Identity**: A stable nonsecret identifier distinguishing one SpaceTerm Client's views, attachments, Pane control and operations.

**Agent Name**: A reusable name supplied at admission to derive an agent's Client Identity from its base Client Identity.

**Client Name**: The person-readable name a SpaceTerm Client supplies when it connects, such as its computer's name, which other clients see while it controls or observes a Pane. It grants no authority.

**Account-local Connection**: A connection authorized by the operating system as belonging to the account that owns SpaceTerm Core.

**Desktop Channel**: The account-local connection on which `spaceterm` asks the desktop on its computer and account to open a local file, show work in that desktop's own view, or report its Terminal Diagnostics counters.

**Paired Device**: A client device deliberately enrolled for direct-network access to a Machine.

**Add Machine**: The Settings-owned journey that enrolls a remote Machine through SSH Setup or an Invitation Link.

**Remove Machine**: The action that uninstalls SpaceTerm Core from a paired Machine, revoking every pairing, and removes that Machine's Saved Connection from a desktop SpaceTerm Client. The Machine keeps its Workspace organization and Machine Settings.

**Network Access**: A Machine's deliberate QUIC exposure: off, or on at one address of its computer or every interface, at one port.

**Change Network Access**: The confirmed action that turns Network Access on or off or moves it to another address or port, reporting first whether it ends the caller's connection and which connected Paired Devices lose their route.

**Invite Device**: The SpaceTerm Core action that creates an Invitation Link for one new Paired Device.

**Invitation Link**: A one-use `spaceterm://pair/…` link containing the Machine's address, its Machine ID and a strong secret, valid for five minutes.

**Comparison Code**: The six-digit value both devices display from the authenticated enrollment exchange for the person to compare before allowing access.

**Unpair Device**: The action that requests revocation of a Paired Device's direct-network access to one Machine and removes the initiating client's Saved Connection when it unpairs itself.

**Unpair Pending**: An Unpair Device request whose revocation on the Machine is awaiting confirmation, independently of completed local connection removal.

**Saved Connection**: A paired QUIC route to a Machine that a SpaceTerm Client retains with its Machine ID and credential reference. The CLI's Saved Connection names select the Machine for `--machine`. The desktop also retains whether it should stay connected.

**Client Settings**: Preferences used by a SpaceTerm Client on its own device.

**Machine Settings**: Preferences owned by one SpaceTerm Core and shared by its authorized clients.

**Client View State**: A SpaceTerm Client's presentation choices for Core-owned work, including Workspace and Tab display order, Pane Layouts and Split ratios, Active Workspace, Active Tab, Focused Pane, Zoomed Pane, Selection, Terminal Find, and scroll position.

**Controlling Client**: The SpaceTerm Client with authority to supply terminal input and terminal dimensions to a Pane.

**Observation**: A channel that reads a Pane's terminal presentation and Scrollback without control or native effects.

**Take Over**: The action that replaces a Pane's Controlling Client.

**Takeover Screen**: The Pane presentation shown to a desktop SpaceTerm Client that is not a Pane's Controlling Client, unless the Pane's Terminal Session ended. It names the Controlling Client, or says that no client controls the Pane.

## Settings

**Settings**: Application-scoped preferences retained across launches.

**Settings Document**: The retained Settings and installed Terminal Themes.

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

**Overdue Update**: An available update whose release is at least two days old. Its Update Reminders repeat until it is installed. It never holds a launch, and nothing installs it without the person.

**Update Reminder**: A deferrable notice about an outstanding update.

**Update Floor**: The oldest release version allowed to update directly to a release. Every release publishes one for SpaceTerm Core and one for the desktops.

**Stepping Stone**: A release that an updater installs because it is the Update Floor of a newer release.

**Data Generation**: The integer that names the format of every file in one retained data root: Core data, CLI state, desktop config or desktop state.

## Focus and transient UI

**Active Workspace**: The Workspace presented in an Operating-System Window.

**Active Tab**: The Tab presented within a Workspace.

**Next Tab** / **Previous Tab**: The Commands activating the neighboring Tab in Workspace order, within the Active Worktree.

**Move Tab Right** / **Move Tab Left**: The Commands moving the Active Tab among its Workspace's Tabs, within the Active Worktree.

**Next Worktree** / **Previous Worktree**: The Commands activating the neighboring Worktree that has Tabs, in sidebar order, wrapping at either end.

**Focused Pane**: The Pane selected by a Tab for Pane actions and focus restoration.

**Terminal Input Focus**: A Pane's current eligibility to accept terminal input.

**Input Method Text**: Text the host's text input system delivers to a Pane, such as an input method commit or an emoji picker selection. The Focused Pane accepts it without Terminal Input Focus.

**Zoomed Pane**: The Focused Pane presented alone while its Pane Layout remains intact.

**Workspace Switcher**: The transient chooser for activating an existing Workspace or naming a new one.

**Open Local Directory**: The Command opening a Local Workspace pinned to a selected directory.

**Directory Picker**: The chooser browsing one Machine's directories to select a Pinned Directory.

**System Directory Selection**: The system chooser offered for selecting a local Pinned Directory.

## Workspace location and remote identity

**Workspace Location**: Where a Workspace runs: Local or Remote. Pinning is independent of it.

**SSH Destination**: The OpenSSH destination token used only during SSH Setup.

**Remote Directory**: An absolute or home-relative directory value on a remote Machine.

**Remote Directory Identity**: The physical absolute path of a Remote Directory on its Machine.

**Authentication Prompt**: An OpenSSH confirmation or obscured response request presented by SpaceTerm.

## Terminal

**Terminal Session**: The Core-owned runtime joining a Pane's Terminal Emulator to a shell on its Machine.
_Avoid_: session, terminal

**Terminal Emulator**: The state machine interpreting terminal output and owning screen state.

**Terminal Metadata**: Sanitized facts about terminal screen state, such as its title and Current Directory.

**Scrollback**: The rows of a Terminal Session's history above its screen, owned by SpaceTerm Core.

**Scrollback Cache**: The Scrollback rows a SpaceTerm Client holds for one Pane, over which it scrolls, selects, and runs Terminal Find.

**Line Mode**: Whether a Terminal Session's PTY collects lines, echoes input and turns signal keys into signals, and whether the shell owns its foreground, as SpaceTerm Core last read them.

**Run**: One command admitted against an exact Terminal Session and shell prompt, with a receipt from SpaceTerm Core that correlates its dispatch and completion.

**Predictive Echo**: A SpaceTerm Client drawing printable keys typed at the shell's line editor before SpaceTerm Core's echo arrives, marked until a frame confirms them.
_Avoid_: local echo, speculative echo

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

**System Permission**: An operating-system privacy grant that programs in a Terminal Session inherit from SpaceTerm Core, set up only on that Machine.

**Permission Holder**: The System Settings entry whose System Permissions a Machine's Terminal Sessions receive: the SpaceTerm Core bundle, listed under its name, such as SpaceTerm Core Preflight, or the application that started SpaceTerm Core.

**Permission Request**: A terminal-output request offering Permission Setup for missing System Permissions.

**Permission Setup**: A user-initiated guided pass through System Settings to turn on a System Permission for its Permission Holder.

**Setup Guide**: The panel accompanying Permission Setup.

**Terminal Local File Capabilities**: Terminal Session-scoped authority for local path actions in a Local Pane.

**Terminal Failure**: A terminal fault with an explicit recovery class.

**Terminal Diagnostics**: Terminal fault metadata, which a desktop's Panes record and the Export Terminal Diagnostics command writes, and typed counts of terminal pipeline stages, which each SpaceTerm Core and each desktop keep separately.

**Close Confirmation**: Authorization for an exact user-requested close that may discard running work.

## Repositories

**Repository Status**: The read-only git facts SpaceTerm presents for the repository containing a directory: branch, operation, upstream divergence, and changes.
_Avoid_: git status, VCS status, git info

**Repository Source Directory**: The directory Repository Status reads for a Pane: its Current Directory as of the last prompt.
_Avoid_: git directory, prompt directory

**Prompt Owner**: A random value each Local Terminal Session gives its shell, which SpaceTerm's shell integration attaches to the shell's own prompt markers. Once the shell has sent it, only markers that carry it move the Repository Source Directory, so a nested `ssh` or `docker` shell cannot.
_Avoid_: shell token, session secret

**Pull Request**: The open GitHub pull request whose head is a repository's current branch, found through the local GitHub CLI.
_Avoid_: PR, merge request

**Worktree**: One of a git repository's working trees, listed under a Workspace whose directory is in that repository. Its Tabs start inside it.
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
