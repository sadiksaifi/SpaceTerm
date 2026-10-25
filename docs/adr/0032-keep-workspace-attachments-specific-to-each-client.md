# Keep Workspace Attachments specific to each client

Each SpaceTerm Client chooses which server-owned Workspaces it attaches and presents in its sidebar. Attach Workspace presents an existing Workspace, while Detach Workspace removes that client's attachment and leaves the Workspace and its Terminal Sessions running on the server, so attachment changes do not interrupt other clients.

[ADR 0022](0022-give-one-client-control-of-each-pane.md) defines how Detach Workspace releases Pane control.

Offline Detach Workspace follows [ADR 0047](0047-keep-shared-workspace-edits-unavailable-offline.md).

Each client's attachment choices persist across quitting and relaunching that client. A detached Workspace stays detached until the person explicitly attaches it again; launching the client does not attach every Workspace available from its servers.

A Workspace context menu offers Detach Workspace. The existing Workspace Switcher lists attached and detached Workspaces available from the client's connected servers, using its existing filter and Workspace creation actions. Selecting an attached Workspace activates it; selecting a detached Workspace attaches and activates it, so attachment uses the same chooser as switching and creation.

The attached Workspace presents the Worktree information supplied by its owning server under [ADR 0031](0031-retain-workspace-organization-across-server-restarts.md); a sidebar entry does not transfer ownership of that information to the client.

Kill Workspace replaces Close Workspace in the sidebar context menu for both local and remote Workspaces. Close Confirmation states that the selected Workspace's Terminal Sessions end and its saved organization is permanently removed for every client. After confirmation, the server performs that removal. The server and its other Workspaces continue running, and repository files and Git Worktree directories remain on disk. Automatic restoration and saved client attachments cannot recreate a killed Workspace.

On macOS, Cmd+W closes the Focused Pane, Shift+Cmd+W closes the Active Tab, and Ctrl+Cmd+W runs the retained `close_workspace` Command with Close Confirmation before Kill Workspace. Keymap resolution follows [ADR 0010](0010-resolve-the-keymap-from-host-defaults-and-retained-overrides.md). This decision changes no Linux default. Detach Workspace has no default shortcut. [ADR 0023](0023-allow-confirmed-tab-closure-across-clients.md) owns close eligibility and final-close consequences.
