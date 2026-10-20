# Reuse one server across headless and desktop installations

SSH Bootstrap and desktop startup discover and reuse one SpaceTerm Server for the same operating-system account and application identity, regardless of which installation created it. A desktop client installed after headless setup connects to that existing server, preserving its service registration, server identity, paired devices, durable Workspace organization, and running Terminal Sessions.

Discovery and setup coordinate across installation paths and concurrent launches so they cannot create competing servers for that account and identity. Release, Preflight, and Development remain isolated under [ADR 0012](0012-derive-one-application-identity-from-the-build.md), and a deliberately stopped server follows [ADR 0030](0030-start-servers-at-boot-independently-of-clients.md).

SSH discovery selects the account's approved retained installation independently of the client installation or shell search path. A compatible installed server needs no new artifact, device enrollment or replacement identity. A missing installation can offer the verified candidate described in [ADR 0029](0029-pair-clients-automatically-during-ssh-bootstrap.md). An incompatible installation remains intact until a separate authorized update.

The desktop application joins as another client with its own [Workspace Attachments](0032-keep-workspace-attachments-specific-to-each-client.md). Local and remote clients share the [Pane control rules](0022-give-one-client-control-of-each-pane.md), so changing the route to the server does not change ownership or takeover behavior.
