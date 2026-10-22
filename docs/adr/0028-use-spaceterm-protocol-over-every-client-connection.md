# Use SpaceTerm Protocol over every client connection

Every SpaceTerm Client connection uses SpaceTerm Protocol. Account-local IPC, non-PTY SSH account bridges and secure WebSocket connections carry the same operations and authoritative server state. Their admission authority follows [ADR 0027](0027-trust-paired-clients-equally.md).

Local IPC checks owning-account peers, private endpoints and build identity automatically. SSH uses OpenSSH host/account authority without SpaceTerm device enrollment. Direct-network/WSS access uses pinned-server trust and deliberately paired devices. Network listeners remain disabled until the person explicitly enables remote access.

Direct access retains one explicit bind policy and server identity across restarts. An invitation pins the server's TLS certificate and public proof key before the claiming device supplies its signed claim. Both proofs bind the server and exact claim. The owning account or an authorized client approves the displayed claiming device and its actual fingerprint. Invitations are single-use and expire; displaying or explicitly copying one never puts it in retained state, diagnostics or command arguments. Approval requires no fingerprint transcription.

Enrollment assigns a Client Identity to that pairing. The direct transport enforces the binding between the device credential, pairing and Client Identity before terminal admission. A new pairing cannot claim an old pairing's views or Pane control by supplying its Client Identity. Direct clients use the same semantic terminal protocol and named Controlling Client presentation as account-local and SSH clients, without acquiring client-local file authority, SSH credentials or operating-system administrator authorization.

Users supply network reachability through their network, VPN, tailnet, reachable server address or SSH route. SpaceTerm operates no relay or connectivity service. SSH Connect and Bootstrap follow [ADR 0029](0029-pair-clients-automatically-during-ssh-bootstrap.md); the owning remote issues implement those routes. Existing desktop SSH behavior remains until that implementation replaces it.
