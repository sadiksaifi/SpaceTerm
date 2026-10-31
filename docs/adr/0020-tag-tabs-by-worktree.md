# Tag Tabs by Worktree

In a Workspace that lists Worktrees, each Tab carries the Worktree it belongs to. The tab strip, Next Tab, Previous Tab, Move Tab, and the numbered Tab Commands act only on the Active Worktree's Tabs. The Active Worktree is always derived from the Active Tab rather than stored. A Worktree is a tag on a Tab, not a container of Tabs, because the Workspace already owns the invariants that span all of its Tabs: unique Tab identities, one Root Tab, the rule that closing the final Tab closes the Workspace, Close Confirmation over the whole Workspace, and one stream of Tab events.

The server owns membership. The Root Tab of an unpinned Workspace follows its Root Pane. It joins the Worktree that holds the Root Pane's directory. It leaves its Worktree once a successful Git read places that directory outside any repository. A Tab promoted to Root Tab follows the same rule. Other Tabs keep their Worktree while their shells change directories. A failed or pending read never removes membership. [ADR 0044](0044-restore-panes-in-their-last-confirmed-directory.md) limits which Worktree a repaired Tab joins.

Each client orders its own Tabs under [ADR 0042](0042-keep-tab-order-and-pane-layouts-specific-to-each-client.md). [ADR 0023](0023-allow-confirmed-tab-closure-across-clients.md) owns closure and Worktree fallback policy.
