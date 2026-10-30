# End All Sessions on the server's own machine

Offer "End All…" in the account-local server's Terminal Sessions row in Settings > Servers, disabled when no Terminal Sessions are running. Paired connections cannot invoke this server-wide action. Confirmation names the selected server and states that its live Terminal Sessions end for every client and their terminal output and Scrollback are cleared, while the server and its saved Workspace, Tab, and Pane organization remain. Repository files and Git Worktree directories remain on disk.

End All Sessions preserves each Pane's control grant, including a disconnected client's grant. An unclaimed Pane remains unclaimed. Invoking the action gives its client no additional control; acquisition and takeover still follow [ADR 0022](0022-give-one-client-control-of-each-pane.md).

The server discards each ended Session's terminal output and Scrollback. Each affected Pane then presents Session Ended under [ADR 0053](0053-keep-a-pane-whose-shell-exits-unsuccessfully.md). A disconnected client clears its frozen terminal screen when reconciliation confirms that the Session ended. A fresh shell uses the directory policy in [ADR 0044](0044-restore-panes-in-their-last-confirmed-directory.md).

Input, pending paste approvals, and delayed terminal events or snapshots belonging to an ended Session cannot restore its cleared output or affect its replacement Session.

The action requires a live connection to the selected server and uses the operation identity and retry rules in [ADR 0037](0037-resolve-shared-mutations-at-the-server.md). Its scope includes only that server's Terminal Sessions; other servers remain unaffected.

`spaceterm server end-all-sessions` offers the same action through account-local admission. Without `--confirm TOKEN` it only previews, even in an interactive terminal. The preview lists the live Terminal Sessions with Pane selectors and labels. Confirmation binds the server, Client Identity, boot and exact Pane-to-Session membership through the [existing CLI tokens](0057-administer-servers-over-the-service-lane.md). Control changes do not invalidate approval because they cannot broaden the affected Sessions. The CLI submits the confirmed mutation through its shared journal. A paired server selector fails before opening credentials or a connection. [ADR 0055](0055-define-the-cli-output-contract-and-scope-model.md) owns output, exit codes and interruption.
