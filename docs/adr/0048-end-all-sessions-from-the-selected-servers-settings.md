# End All Sessions from the selected server's Settings

Offer "End All…" in the selected server's Terminal Sessions row in Settings > Servers, disabled when no Terminal Sessions are running. Confirmation names the selected server and states that its live Terminal Sessions end for every client and their terminal output and Scrollback are cleared, while the server and its saved Workspace, Tab, and Pane organization remain. Repository files and Git Worktree directories remain on disk.

End All Sessions preserves each Pane's control grant, including a disconnected client's grant. An unclaimed Pane remains unclaimed. Invoking the action gives its client no additional control; acquisition and takeover still follow [ADR 0022](0022-give-one-client-control-of-each-pane.md).

The server discards each ended Session's terminal output and Scrollback. Its Controlling Client presents "Session ended" with a "Start Session" button using SpaceTerm's existing blue primary-button style. Other clients retain the Takeover Screen under ADR 0022. A disconnected client clears its frozen terminal screen when reconciliation confirms that the Session ended.

After completion, each affected Pane remains ended until a Controlling Client explicitly selects it again or opens its Tab to start a fresh shell. Closing Settings, restoring window focus, and routine rendering cannot restart an ended Session. Fresh shell startup uses the first-use policy in [ADR 0043](0043-start-restored-shells-on-first-use.md) and the directory policy in [ADR 0044](0044-restore-panes-in-their-last-confirmed-directory.md).

Start Session uses the same control-checked startup operation as explicit Pane selection and Tab opening. Input, pending paste approvals, and delayed terminal events or snapshots belonging to an ended Session cannot restore its cleared output or affect its replacement Session.

The action requires a live connection to the selected server and uses the operation identity and retry rules in [ADR 0037](0037-resolve-shared-mutations-at-the-server.md). Its scope includes only that server's Terminal Sessions; other servers remain unaffected.
