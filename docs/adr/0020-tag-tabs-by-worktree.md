# Tag Tabs by Worktree

In a Workspace that lists Worktrees, each Tab carries the Worktree it belongs to. The tab strip, Next Tab, Previous Tab, Move Tab, and the numbered Tab Commands act only on the Active Worktree's Tabs. The Active Worktree is always derived from the Active Tab rather than stored. A Worktree is a tag on a Tab, not a container of Tabs, because the Workspace already owns the invariants that span all of its Tabs: unique Tab identities, one Root Tab, the rule that closing the final Tab closes the Workspace, Close Confirmation over the whole Workspace, and one stream of Tab events.

SpaceTerm Core owns membership under four rules.

1. A Workspace is in at most one repository. Every Tab that has a Worktree has one of that repository's, so a Workspace lists one repository's Worktrees and no others.
2. A Tab stays in its Worktree while its shells change directories. A new Tab opens in its source Tab's Worktree, and promotion to Root Tab moves no Tab. A Worktree row whose last-used Pane is outside it marks that, and the Tab stays.
3. A Workspace changes repository only when every Tab changes with it. Three things move a Workspace:
   - Its only Tab's shell changes directory. The Workspace follows that Tab into, out of, and between repositories and Worktrees. A Tab left alone by a closed Tab or an unpinned Workspace stays where it is until its next `cd`.
   - It is in no repository, and its Root Pane enters one. Every Tab joins: the Worktree its own shell is in when that Worktree is in the repository, else the Root Tab's.
   - Its Pinned Directory is set or changed. Every Tab joins the pin's repository the same way, or leaves its Worktree when the pin is outside any repository. Unpinning moves nothing.
4. A Workspace in a repository takes its automatic name from the Main Worktree's directory, wherever its shells are. A Workspace in no repository takes it from its Pinned Directory, else its Root Pane's directory.

Only a successful Git read moves a Tab. A failed or pending read never removes membership. A Tab saved outside its Workspace's repository joins it at the next read, which settles organization that an earlier SpaceTerm Core retained. [ADR 0044](0044-restore-panes-in-their-last-confirmed-directory.md) limits which Worktree a repaired Tab joins. [The membership module](../../crates/spaceterm-core/src/hierarchy/membership.rs) owns the rules.

These rules replace one in which the unpinned Root Tab alone followed its Root Pane. That rule let a `cd` in one shell move the Workspace's repository while other Tabs stayed in the old one, which the sidebar listed under a former-repository label.

Each client orders its own Tabs under [ADR 0042](0042-keep-tab-order-and-pane-layouts-specific-to-each-client.md). [ADR 0023](0023-allow-confirmed-tab-closure-across-clients.md) owns closure and Worktree fallback policy.
