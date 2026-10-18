# Use SpaceTerm Protocol over every client connection

Every SpaceTerm Client connection uses SpaceTerm Protocol. Account-local IPC, non-PTY SSH account bridges and secure WebSocket connections carry the same operations and authoritative server state. Their admission authority follows [ADR 0027](0027-trust-paired-clients-equally.md).

Local IPC checks owning-account peers, private endpoints and build identity automatically. SSH uses OpenSSH host/account authority without SpaceTerm device enrollment. Direct-network/WSS access uses pinned-server trust and deliberately paired devices. Network listeners remain disabled until the person explicitly enables remote access.

Users supply network reachability through their network, VPN, tailnet, reachable server address or SSH route. SpaceTerm operates no relay or connectivity service. SSH Connect and Bootstrap follow [ADR 0029](0029-pair-clients-automatically-during-ssh-bootstrap.md); the owning remote issues implement those routes. Existing desktop SSH behavior remains until that implementation replaces it.
