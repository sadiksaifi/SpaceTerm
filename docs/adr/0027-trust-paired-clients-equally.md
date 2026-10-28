# Admit local accounts and paired remote clients

Account-local IPC admits the server's owning account through peer-account, private-endpoint and application-identity checks. It trusts processes running as that account and requires no pairing or credential store. Build identities prevent accidental cross-use, not hostile owner processes.

Every remote client uses WSS with pinned server trust and a deliberately enrolled device credential. Account-local and paired clients can control terminal work and edit Server Settings. Server administration requires an account-local connection: network exposure, invitations, approval, removal of other devices and End All Sessions. Keeping these controls on the owning machine gives clients one role across desktop and headless servers. A paired client can revoke its own pairing. [ADR 0038](0038-unpair-devices-without-blocking-local-connection-removal.md) defines pairing-specific revocation. [ADR 0029](0029-enroll-remote-servers-through-add-server.md) defines enrollment.

Client Identity distinguishes views, attachments, Pane control and operations; Client Name labels control for other clients. Neither grants authority. Connection, Terminal Session, control and attachment generations independently fence operations.

Native service administration requires owning-account operating-system authority on the server's machine. WSS grants neither that authority nor client-local file access. Signed-artifact verification, deliberate Stop, bounded process ownership and reconciliation of started Git writes remain required.

[ADR 0052](0052-carry-paired-connections-over-quic.md) carries paired connections over QUIC instead of WSS.
