# Present Workspaces from several servers together

One SpaceTerm Client connects to several SpaceTerm Servers and groups its [Workspace Attachments](0032-keep-workspace-attachments-specific-to-each-client.md) by server in one Workspace Window. Each server exposes only its own Workspaces.

The sidebar places this computer first, followed by saved remote servers using their reported names and connection states. Reordering stays within a server's group. A server with no attached Workspaces keeps its heading while presented or reconnecting. Disconnect ends connection intent and keeps the Saved Connection.

Both sidebar creation menus offer Workspace creation and directory selection on explicitly selected saved servers. Directory browsing, creation, physical matching, pinning and recovery use the owning server's filesystem. Adding another server belongs exclusively to [Add Server](0029-enroll-remote-servers-through-add-server.md).
