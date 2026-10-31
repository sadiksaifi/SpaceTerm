# Retain Workspace organization across server restarts

Each SpaceTerm Server durably retains the shared organization within its Workspaces, including Workspace, Tab, and Pane identities and membership, custom Workspace names, Pinned Directories, Root Tab and Root Pane identities, and Tab-to-Worktree membership. This organization survives Stop Server, crashes, forced termination, machine reboot, and power loss, so people do not have to rebuild their terminal setup; [ADR 0030](0030-start-servers-at-boot-independently-of-clients.md) defines the separate lifetime of live Terminal Sessions.

Client arrangements and the server-supplied default for clients without a saved arrangement follow [ADR 0042](0042-keep-tab-order-and-pane-layouts-specific-to-each-client.md).

Pane directory retention follows [ADR 0044](0044-restore-panes-in-their-last-confirmed-directory.md).

Intentional Workspace deletion follows [ADR 0032](0032-keep-workspace-attachments-specific-to-each-client.md).

Git remains the source of truth for Worktrees under [ADR 0019](0019-create-and-remove-worktrees-by-running-git.md), and the server reads that list on its machine. Saved Tab-to-Worktree membership follows [ADR 0020](0020-tag-tabs-by-worktree.md) and must be reconciled with that list when restoring a Workspace.
