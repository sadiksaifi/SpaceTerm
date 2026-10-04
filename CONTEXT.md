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

**Root Pane**:
The stable Pane that supplies a Tab's automatic directory, independent of focus and layout order.

**Root Tab**:
The stable Tab whose Root Pane supplies a Workspace's unpinned sidebar directory and automatic name.

**Pane Caption**:
The header a Pane presents above its terminal region, naming the Pane and carrying its direct
controls.

**Pane Origin**:
The account and machine a Pane's Terminal Session runs on, classified Local or Remote by the
Session itself rather than by the spelling of either value.

**Operating-System Window**:
A native window that presents SpaceTerm.

**Window Controls**:
The close, minimize, and maximize controls of an Operating-System Window: native traffic lights on
macOS; drawn by SpaceTerm at the top right on Linux, following the desktop button layout.

## Settings

**Settings**:
The application-scoped preferences SpaceTerm retains in `settings.json`.

**Settings Document**:
The retained form of Settings: every preference SpaceTerm persists together with the Terminal
Themes installed into it. It is named for what it holds rather than for any one Section, so an operation
scoped to all of Settings keeps that meaning as Sections are added.

**Settings Window**:
The separate modeless Operating-System Window that presents Settings. It presents no Workspace and
never stands in for a Workspace window.

**Settings Section**:
One named group of Settings presented as one navigation entry and one content region.

**Settings Row**:
One labeled Setting control within a Settings Section.

**Settings Search**:
Fuzzy search over Settings Row labels and keywords that reveals a matching Settings Row.

**Appearance Mode**:
The application-scoped Light, Dark, or Auto choice selecting the built-in Application Chrome and
the Terminal Theme slot of that appearance.

**Malformed Settings**:
A `settings.json` that SpaceTerm cannot read as a valid Settings Document, because it does not
parse, fails validation, or exceeds the size limit. A storage failure is not Malformed Settings.

**Settings Recovery**:
Replacing Malformed Settings with default Settings while keeping the unreadable file unchanged as
`settings.json.bak` beside it.

## Keybindings

**Command**:
A SpaceTerm operation whose Shortcut a person can change, such as New Tab or Split Right. Standard
application commands, such as Copy and Quit, are not Commands.

**Shortcut**:
One key chord: a key with any combination of the host modifiers. macOS names them Control, Option,
Shift, and Command; Linux names them Ctrl, Alt, Shift, and Super.
_Avoid_: Hotkey, key equivalent

**Keybinding**:
The Shortcut one Command resolves to, or Unassigned.

**Unassigned**:
The Keybinding of a Command that has no Shortcut.

**Keymap**:
Every Command's Keybinding: the host's default Shortcuts with the overrides retained in Settings
applied. A Shortcut belongs to at most one Command.

**Reserved Shortcut**:
A Shortcut no Command can be assigned. It is Terminal Reserved or System Reserved.

**Terminal Reserved**:
A Shortcut that types into the terminal or that programs running in the terminal read, such as
Ctrl-C or Alt-B (Control-C or Option-B on macOS).

**System Reserved**:
A Shortcut the Operating System or a standard application command owns, such as Command-Q on
macOS or Super shortcuts on Linux.

**Inactive Override**:
A retained override that is invalid or Reserved on this host, such as a macOS Command-T read on
Linux. Settings keep it unchanged, and the Command keeps its host default.

**Keyboard Shortcuts**:
The Command that opens Settings at its Keybindings section.

## Updates

**Update Deadline**:
The fixed release age after which a fresh launch requires an obtainable update before opening
Workspaces. It never revokes access to an existing Workspace or prevents offline terminal work.

**Update Reminder**:
A notice about an outstanding update that can be deferred without discarding the update.
It preserves Terminal Input Focus and running Terminal Sessions.

## Focus and transient UI

**Active Workspace**:
The one Workspace presented in an Operating-System Window.

**Active Tab**:
The one Tab presented within a Workspace.

**Next Tab** / **Previous Tab**:
The Commands that activate the Tab after or before the Active Tab, wrapping around at either end of
the Workspace's Tabs.

**Move Tab Right** / **Move Tab Left**:
The Commands that move the Active Tab one place among its Workspace's Tabs, stopping at either end.

**Focused Pane**:
The Pane selected by a Tab for Pane actions and focus restoration.

**Terminal Input Focus**:
The transient eligibility of a Pane to accept terminal input while its Workspace and Tab are Active,
it is the Focused Pane and responder, its window and application are active, and temporary UI has
released input.

**Zoomed Pane**:
The Focused Pane presented alone while its Pane Layout remains intact.

**Workspace Switcher**:
The transient chooser for selecting an existing Workspace or naming a new Workspace created with New
Workspace, New Remote Workspace, Open Local Directory, or Open Remote Directory.

**Open Local Directory**:
The Command that chooses a local directory with the Directory Picker and creates a Local Workspace
pinned to it. A Workspace already pinned to that directory is activated instead.

**Open Remote Directory**:
The Command that connects to an SSH Destination, chooses a directory there with the Directory
Picker, and creates a Remote Workspace pinned to it. A Workspace already pinned to the same SSH
Destination and Physical Directory Identity is activated instead, and dismissing the Directory
Picker closes the connection.

**Directory Picker**:
The Command Palette chooser that browses one machine's directories to select a Workspace's Pinned
Directory.

**System Directory Selection**:
The system chooser the Directory Picker offers on this machine for selecting a Pinned Directory.

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

**Scroll Commands**:
Scroll Page Up, Scroll Page Down, Scroll to Top, and Scroll to Bottom: Commands that move the Focused
Pane's view of its Scrollback and send nothing to the terminal. The alternate screen has no
Scrollback, so they leave it unchanged.

**Terminal Hyperlink**:
A validated target attached to complete terminal cells and activated through the current
modified-pointer gesture.

**File Preview**:
A host preview of a local file, using Quick Look on macOS or GNOME Sushi on Linux. The command is
absent when the host has no previewer.

**Paste Payload**:
A bounded text insertion candidate retained until accepted or cancelled.

**Paste Selection**:
The Linux standard command, Shift+Insert, that pastes PRIMARY Selection as a Paste Payload, as
middle-click does.

**Paste Confirmation**:
Time-bounded authorization for one unsafe Paste Payload while Terminal Input Focus remains valid.

**OSC 52 Filtering**:
Bounded recognition of terminal clipboard escape sequences before Terminal Emulation, producing ordered plain-text requests.

**Terminal Clipboard Access**:
Session-scoped permission to consult the system text clipboard while the originating Pane retains Terminal Input Focus.
Copying is enabled by default; reading requires the Privacy Setting.

**System Permission**:
An Operating-System privacy grant that programs in a Terminal Session inherit from SpaceTerm and that
only System Settings gives: Screen Recording and Accessibility. Microphone access is not one,
because the system can ask for it with a prompt.

**Permission Request**:
A Local Pane's offer to start a Permission Setup after its output carries `OSC 7701`. Any output the
Pane shows can carry one, including output a remote shell or a file relays, so it names no program.
It names the permissions SpaceTerm lacks, withdraws them once granted, and starts nothing until the
person chooses Set Up. A Remote Pane ignores Permission Requests. A request names `screen-recording`
for Screen Recording and `accessibility` for the Accessibility permission. SpaceTerm names that
permission as System Settings does: Accessibility before macOS 27, and Device Control from macOS 27,
after its Device Control and Data Access list.

**Permission Setup**:
One guided pass through System Settings that adds SpaceTerm to a System Permission's privacy list,
started only from a Settings Row or a Permission Request. System Permissions stay off until then;
onboarding never asks for them.

**Setup Guide**:
The non-activating panel a Permission Setup docks inside the bottom of System Settings' window
while it is in front. It offers SpaceTerm to drag into the privacy list and reports the grant.

**Terminal Local File Capabilities**:
Session-scoped authority for local path actions in a Local Pane.

**Terminal Failure**:
A typed terminal fault with an explicit recovery class.

**Local Diagnostics**:
Bounded content-free failure and unhandled-key metadata exported after an explicit user action.

**Close Confirmation**:
One authorization for an exact user-requested close that may discard running work.

## Appearance

**Application Chrome**:
Everything SpaceTerm draws around terminal output: windows, sidebars, tabs, controls, menus, and
dialogs. Its colors are built in, compiled from one set of Chrome tokens per appearance, and never
user-selectable. No Terminal Theme reaches it.

**Terminal Theme**:
Identifying metadata and the colors of terminal output in Panes: foreground, background, the
ANSI palette, cursor, selection, search matches, and links. Built-in Terminal Themes are always
installed; others are installed from Zed themes.

**Zed Theme**:
A theme in the format Zed publishes: one family document holding named Light or Dark themes.
SpaceTerm translates only its terminal roles, and the few editor roles a terminal needs, into a
Terminal Theme.

**Zed Extension**:
A package in the Zed extension registry. A theme extension contributes one or more Zed Theme
family documents. Installing it again, at any version, replaces every Terminal Theme it installed.

**Resolved Theme**:
Complete validated presentation colors after Terminal override precedence and built-in completion.

**Theme Origin**:
Source identity and attribution, separate from the installed identifier and display name.

**Window Background Appearance**:
Opaque, transparent or blurred native presentation, independent of Light/Dark and color alpha.

**Transparency**:
The application-scoped amount of underlying content visible through window backgrounds and floating
surfaces, from zero (opaque) to one (maximum transparency). Terminal text and explicit cell colors retain their own appearance.

**Density**:
The application-scoped Compact or Comfortable spacing of Application Chrome, its only preference.

**Blur**:
The application-scoped choice to soften the desktop behind window backgrounds and application
content behind floating surfaces. Foreground controls and text remain sharp.

**Surface Material**:
The translucency and neutral shading of a background fill. Window backgrounds admit the desktop;
floating surfaces combine softened application content with the same window backdrop where available,
while retaining shading for readability.

**Developer Workbench**:
The development window, present only in SpaceTerm Development, that previews Appearance changes
and presents every control, floating surface, and modal family as fixtures. Its previews change
the Settings Document only when committed.
