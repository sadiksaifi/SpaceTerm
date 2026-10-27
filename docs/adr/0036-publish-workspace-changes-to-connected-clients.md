# Publish Workspace changes to connected clients

The SpaceTerm Server promptly publishes accepted changes to its Workspace Inventory and shared organization to every connected, authorized client, including Workspace renames, pinning and unpinning, Tab and Pane creation and deletion, and Worktree creation and removal. Clients update their sidebar, Workspace Switcher, and affected Workspace state regardless of the Tab they are viewing, so an inactive view does not become a separate copy of shared state.

Remote changes preserve each client's [Workspace Attachments](0032-keep-workspace-attachments-specific-to-each-client.md) and independent [Client View State](0041-keep-each-clients-view-independent.md) when the targets still exist. Personal rearrangement follows [ADR 0042](0042-keep-tab-order-and-pane-layouts-specific-to-each-client.md) and does not change another client's view. Worktree changes come from Git on the server under [ADR 0031](0031-retain-workspace-organization-across-server-restarts.md), and affect every Workspace that presents the changed repository.

Disconnected clients reconcile with authoritative server state when they reconnect, removing deleted targets while retaining their arrangement of surviving Tabs and Panes. Shared organization follows the durability requirement in [ADR 0031](0031-retain-workspace-organization-across-server-restarts.md).

[ADR 0037](0037-resolve-shared-mutations-at-the-server.md) defines optimistic presentation, ordered pending changes and their reconciliation with published membership.

[ADR 0050](0050-push-terminal-presentation-over-one-multiplexed-connection.md) pushes these changes as whole Workspace Inventory revisions on each client's connection.
