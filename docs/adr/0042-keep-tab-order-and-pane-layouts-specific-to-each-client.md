# Share Tab order and keep Pane Layouts specific to each client

Each SpaceTerm Core owns its Workspace's Tab order. Work 1.6 clients present the order in Workspace Inventory and move a Tab with a shared edit. The new position is a zero-based index in the Workspace and the edit keeps the Tab's identity, Panes, and Worktree. The desktop's Tab bar displays the subsequence for its Active Worktree. A Core older than Work 1.6 leaves desktop Tab order local to the client.

Each SpaceTerm Client owns its Pane Layouts, including Pane placement, Split axes, and Split ratios. Moving Panes within a Tab or resizing a Split on one client leaves other clients' layouts unchanged. These choices persist across client relaunches and computer reboots, outside ordinary preference import, export, and reset under [ADR 0039](0039-scope-settings-to-the-client-or-selected-server.md).

SpaceTerm Core retains Workspace, Tab, and Pane identities and membership, Tab-to-Worktree association, and Root Tab and Root Pane identities under [ADR 0031](0031-retain-workspace-organization-across-server-restarts.md). Creating or deleting a Tab or Pane changes shared membership, and every client reconciles its view while preserving its arrangement of surviving targets. Moving a Pane into a new or another Tab, and merging a Tab into another, also change shared membership. Every moved Pane keeps its identity and Terminal Session, and SpaceTerm Core records the side of the Pane it lands beside, so clients without a saved arrangement place it there. Swapping two Panes within a Tab is rearrangement. Rearrangement does not change SpaceTerm Core's Root Tab or Root Pane; root replacement remains a Core decision.

When a client has no saved arrangement for a Workspace, its Pane Layouts derive from the splits that created their Panes. SpaceTerm Core durably retains the creation information needed to produce this default for every surviving Pane.

A Tab created by another client appears without becoming active. A Pane created by a split on another client is inserted beside its source Pane in the receiving client's own Pane Layout, without changing focus or zoom state under [ADR 0041](0041-keep-each-clients-view-independent.md). The creator controls the new Pane, and other clients present its Takeover Screen under [ADR 0022](0022-give-one-client-control-of-each-pane.md).

[ADR 0020](0020-tag-tabs-by-worktree.md) owns Tab-to-Worktree membership. Pane control remains independent of arrangement.
