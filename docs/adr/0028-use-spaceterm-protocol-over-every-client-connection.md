# Use SpaceTerm Protocol over every client connection

Every SpaceTerm Client connection uses SpaceTerm Protocol. Account-local IPC, non-PTY SSH account bridges and secure WebSocket connections carry the same operations and authoritative server state. Their admission authority follows [ADR 0027](0027-trust-paired-clients-equally.md).

Local IPC checks owning-account peers, private endpoints and build identity automatically. SSH uses OpenSSH host/account authority without SpaceTerm device enrollment. Direct-network/WSS access uses pinned-server trust and deliberately paired devices. Network listeners remain disabled until the person explicitly enables remote access.

Direct access retains one explicit bind policy and server identity across restarts. An Invitation Link carries the reachable address, pinned TLS certificate, public proof key and strong one-use secret. It expires after five minutes. The claiming device verifies the pinned server before supplying its signed claim. Both proofs bind the server and exact claim. Both screens display the same six-digit Comparison Code derived from that authenticated exchange. The owning account or an authorized client compares it and approves the exact device and claim, without typing a code or fingerprint.

Invite Device presents the link and its QR code. Add Server accepts the link without connecting until the person chooses Join. Operating-system `spaceterm://pair/…` delivery opens that same sheet on initial launch or in the running client. Links remain transient and never enter retained state or diagnostics. Linux desktop URL delivery may place the link in the initial process arguments; SpaceTerm never forwards it to a child command or helper.

Enrollment assigns a Client Identity to that pairing. The direct transport enforces the binding between the device credential, pairing and Client Identity before terminal admission. A new pairing cannot claim an old pairing's views or Pane control by supplying its Client Identity. Direct clients use the same semantic terminal protocol and named Controlling Client presentation as account-local and SSH clients, without acquiring client-local file authority, SSH credentials or operating-system administrator authorization.

Users supply network reachability through their network, VPN, tailnet, reachable server address or SSH route. SpaceTerm operates no relay or connectivity service. SSH Connect and Bootstrap follow [ADR 0029](0029-pair-clients-automatically-during-ssh-bootstrap.md).
