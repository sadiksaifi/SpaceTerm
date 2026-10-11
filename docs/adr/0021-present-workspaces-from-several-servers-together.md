# Present Workspaces from several servers together

For the server architecture in [issue #162](https://github.com/sadiksaifi/SpaceTerm/issues/162), one SpaceTerm Client connects to several SpaceTerm Servers simultaneously and groups their Workspaces by machine in the sidebar. This preserves access to work on several machines in one Operating-System Window when SpaceTerm Servers replace SSH-based Remote Workspaces. Each Workspace remains owned by its server.

The sidebar presents that client's [Workspace Attachments](0032-keep-workspace-attachments-specific-to-each-client.md). Connecting to a server makes its own Workspaces available; it does not expose Workspaces on other servers that a client on that machine happens to use.
