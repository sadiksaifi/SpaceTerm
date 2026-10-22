# Keep Tab order and Pane Layouts specific to each client

Each SpaceTerm Client owns its Tab display order and Pane Layouts, including Pane placement, Split axes, and Split ratios. Reordering Tabs, moving Panes, or resizing a Split on Mac 1 leaves Mac 2's arrangement unchanged, so each client can arrange the same work for its own screen. These choices persist across client relaunches and machine reboots, outside ordinary preference import, export, and reset under [ADR 0039](0039-scope-settings-to-the-client-or-selected-server.md).

The server retains Workspace, Tab, and Pane identities and membership, Tab-to-Worktree association, and Root Tab and Root Pane identities under [ADR 0031](0031-retain-workspace-organization-across-server-restarts.md). Creating or deleting a Tab or Pane changes shared membership, and every client reconciles its view while preserving its arrangement of surviving targets. Rearrangement does not change the server's Root Tab or Root Pane; root replacement remains a server decision.

When a client has no saved arrangement for a Workspace, it starts with the existing Tabs in creation order and Pane Layouts derived from the splits that created their Panes. The server durably retains the creation information needed to produce this default for every surviving Tab and Pane, excluding deleted targets. Personal rearrangement does not change this default; a client with a saved arrangement uses its own view and reconciles shared creation and deletion.

A Tab created by another client appends to the receiving client's Tab display order without becoming active. A Pane created by a split on another client is inserted beside its source Pane in the receiving client's own Pane Layout, without changing focus or zoom state under [ADR 0041](0041-keep-each-clients-view-independent.md). The creator controls the new Pane, and other clients present its Takeover Screen under [ADR 0022](0022-give-one-client-control-of-each-pane.md).

This decision supersedes the single shared Tab order in [ADR 0020](0020-keep-one-tab-order-per-workspace-tagged-by-worktree.md). Pane control remains independent of arrangement.
