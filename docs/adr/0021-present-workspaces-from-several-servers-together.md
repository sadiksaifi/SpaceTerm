# Present Workspaces from several servers together

One SpaceTerm Client connects to several SpaceTerm Servers and presents its [Workspace Attachments](0032-keep-workspace-attachments-specific-to-each-client.md) in one continuous list in its Workspace Window. Each server exposes only its own Workspaces.

The client owns one persistent Workspace display order across all servers. People can interleave local and remote Workspaces freely. The sidebar, Workspace Switcher and position shortcuts follow that order. Each remote Workspace carries its machine's reported name inline; the list has no machine headings or empty-server placeholders. Server ownership determines where work runs without constraining how the person arranges it.

Both sidebar creation menus offer Workspace creation and directory selection on explicitly selected saved servers. Directory browsing, creation, physical matching, pinning and recovery use the owning server's filesystem. Adding another server belongs exclusively to [Add Server](0029-enroll-remote-servers-through-add-server.md).
