# Present Workspaces from several Machines together

One SpaceTerm Client connects to several Machines and presents its [Workspace Attachments](0032-keep-workspace-attachments-specific-to-each-client.md) in one continuous list in its Workspace Window. Each Machine exposes only its own Workspaces.

The client owns one persistent Workspace display order across all Machines. People can interleave local and remote Workspaces freely. The sidebar, Workspace Switcher and position shortcuts follow that order. Each remote Workspace carries its Machine's reported name inline; the list has no Machine headings or empty-Machine placeholders. An empty list shows one caption for the whole list, never one for a Machine. Machine ownership determines where work runs without constraining how the person arranges it.

Separately paired Client Identities retain independent attachments and views, including when their Saved Connections reach the same Machine. Workspace ordering preserves those scopes; it transfers no connection, operation or Pane-control authority between them.

The sidebar's Create Workspace menu, the Workspace Switcher and the File menu offer the same three creation Commands: New Scratch Workspace, New Workspace and New Remote Workspace. New Remote Workspace opens the Machine Chooser, which lists every saved Machine and disables one this window isn't connected to, then the Directory Picker on the chosen Machine. It always creates a pinned Remote Workspace; Unpin Directory makes an unpinned one. Directory browsing, creation, physical matching, pinning and recovery use the owning Machine's filesystem. Adding another Machine belongs exclusively to [Add Machine](0029-enroll-remote-servers-through-add-server.md).
