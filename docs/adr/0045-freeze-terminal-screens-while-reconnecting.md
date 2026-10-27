# Freeze terminal screens while reconnecting

Unexpected connection loss freezes the last terminal screen with "Reconnecting..." and Retry. Automatic recovery uses bounded backoff with input disabled. Interrupted input is discarded and never replayed.

Protocol failures and unconfirmed shell starts have a bounded recovery episode that retains the original typed cause across later transport failures. Sustained healthy traffic on the live connection renews recovery. Persistent failure presents its cause and explicit controls. Retry observes passively; it never starts a replacement shell, restarts or updates the server. An unconfirmed shell start remains guarded until its original outcome or server-run evidence settles it.

Before enabling input, reconcile [server state](0036-publish-workspace-changes-to-connected-clients.md) and [Pane control](0022-give-one-client-control-of-each-pane.md). Takeover remains effective through interruption without exposing an older screen. Reconnect never takes control automatically.

Account-local reconnect uses owning-account IPC authority. Remote reconnect, sleep recovery, relaunch, status reads and Pane Retry use the saved pairing and pinned WSS identity exclusively. Revocation or identity mismatch requires explicit recovery; no SSH fallback or replacement enrollment runs. Client Identity preserves eligible personal state and grants no authority by itself.

One connection owner publishes status for the Workspace Window and Settings. Saved active intent may resume with bounded retries after relaunch; saving or rendering an inactive connection starts nothing. Disconnect ends ordinary retries and preserves the Saved Connection. [Unpair Device](0038-unpair-devices-without-blocking-local-connection-removal.md) removes it; [offline edits and deferred releases](0047-keep-shared-workspace-edits-unavailable-offline.md) retain their own outcomes.

[ADR 0050](0050-push-terminal-presentation-over-one-multiplexed-connection.md) judges connection health from traffic and pings on the live connection.
