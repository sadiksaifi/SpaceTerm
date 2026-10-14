# Freeze terminal screens while reconnecting

When a SpaceTerm Client unexpectedly loses its server connection, it keeps its last displayed terminal screen visibly frozen with "Reconnecting..." and a Retry action. The client reconnects automatically while terminal input remains disabled. Input entered during the interruption is discarded, and reconnection never replays terminal input from the failed connection.

Before enabling input, the client reconciles authoritative server state under [ADR 0036](0036-publish-workspace-changes-to-connected-clients.md) and verifies Pane control under [ADR 0022](0022-give-one-client-control-of-each-pane.md). If another client took control, the Pane presents the Takeover Screen. A Pane already showing the Takeover Screen retains that presentation during the interruption instead of revealing an older terminal screen.

Automatic reconnect uses the existing server identity and pairing under [ADR 0029](0029-pair-clients-automatically-during-ssh-bootstrap.md). Deliberate Stop Server and Unpair Device retain their respective workflows under [ADR 0030](0030-start-servers-at-boot-independently-of-clients.md) and [ADR 0038](0038-unpair-devices-without-blocking-local-connection-removal.md).

[ADR 0047](0047-keep-shared-workspace-edits-unavailable-offline.md) defines which actions remain available during the interruption.
