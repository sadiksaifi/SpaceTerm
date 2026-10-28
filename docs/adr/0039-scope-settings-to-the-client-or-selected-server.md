# Scope Settings to the client or selected server

Client Settings belong to the device; Server Settings belong to one SpaceTerm Server. Servers and Git share a searchable server picker listing the account-local server, then saved paired servers, and Add Server. Selection persists outside the Settings Document and falls back to the local server when removed. [ADR 0029](0029-enroll-remote-servers-through-add-server.md) owns Add Server.

The selected Servers card presents status, version and Terminal Sessions. Only the account-local server offers automatic-start policy, native service or update controls, Network Access, invitations, device administration and End All Sessions. Remote cards retain connection facts, Connect or Retry, Disconnect and Unpair This Computer. Their saved address is read-only; using a different address requires adding that server again. End All Sessions follows [ADR 0048](0048-end-all-sessions-from-the-selected-servers-settings.md).

Opening, selecting, rendering or searching Settings creates no remote connection. Connected observations use the existing WSS connection; inactive servers show last-known facts and how to reconnect. Each new connection reads the server's own facts again, so a server that restarted or updated shows its current version. Unreachable network policy and device lists remain distinct from confirmed Off or an empty list. Displayed facts grant no mutation authority. Connection state follows [ADR 0045](0045-freeze-terminal-screens-while-reconnecting.md).

Interface, Font, Themes, Keybindings, clipboard, Git presentation and client updates remain client-owned. [Clipboard authority](0013-order-terminal-clipboard-access-by-session-focus.md), [Pane control](0022-give-one-client-control-of-each-pane.md) and [client-local GitHub CLI lookups](0018-read-repository-status-by-running-git-and-the-github-cli.md) retain their boundaries.

Git discovery and writes use the owning server's tools and filesystem. "Git on <server>" owns New Worktree Location; validation checks that server's filesystem and effective directory permissions. Concurrent edits reload the authoritative value and explain the change. Offline server edits remain unavailable. [System Permissions](0014-guide-permission-setup-on-system-settings.md) belong to the machine using the protected resource.

The account-local server offers no removal. Remote removal and revocation follow [ADR 0038](0038-unpair-devices-without-blocking-local-connection-removal.md). Import, export and reset affect Client Settings and installed Terminal Themes only. Connections, authorization, operations, attachments, views, server organization, Server Settings and deliberate Stop remain outside the Settings Document.

[ADR 0052](0052-carry-paired-connections-over-quic.md) carries paired connections over QUIC instead of WSS.
