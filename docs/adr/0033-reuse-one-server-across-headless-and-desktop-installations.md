# Reuse one server across headless and desktop installations

SSH Setup and installed desktop startup reuse one SpaceTerm Server per operating-system account and application identity. Discovery coordinates across installation paths and concurrent launches. A desktop installed after headless setup preserves that server's service registration, identity, pairings, Workspace organization and running Terminal Sessions.

Setup selects the approved retained installation independently of shell search paths. A compatible existing installation requires no replacement artifact. Remote enrollment still follows [Add Server](0029-enroll-remote-servers-through-add-server.md). An incompatible installation remains intact until an authorized local update.

Each client keeps its own [Workspace Attachments](0032-keep-workspace-attachments-specific-to-each-client.md) and follows the same [Pane control rules](0022-give-one-client-control-of-each-pane.md). Release, Preflight and Development remain isolated under [ADR 0012](0012-derive-one-application-identity-from-the-build.md). Source Development uses the separately owned foreground lifecycle in [ADR 0030](0030-start-servers-at-boot-independently-of-clients.md).
