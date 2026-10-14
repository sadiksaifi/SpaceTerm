# Keep one Tab order per Workspace, tagged by Worktree

A Workspace keeps one ordered list of Tabs, and in a Workspace that lists Worktrees each Tab carries the Worktree it belongs to. The tab strip, Next Tab, Previous Tab, Move Tab, and the numbered Tab Commands act only on the Active Worktree's Tabs, and the Active Worktree is always derived from the Active Tab rather than stored. We chose this over one Tab list per Worktree because the Workspace already owns the invariants that need a single list: unique Tab identities, one Root Tab, the rule that closing the final Tab closes the Workspace, Close Confirmation over the whole Workspace, and one stream of Tab events.

For the planned server architecture in [issue #520](https://github.com/sadiksaifi/SpaceTerm/issues/520), [ADR 0042](0042-keep-tab-order-and-pane-layouts-specific-to-each-client.md) supersedes the single shared Tab order with independent client presentation. Tab identities and membership become server-owned.
