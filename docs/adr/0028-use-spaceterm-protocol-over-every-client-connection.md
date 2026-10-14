# Use SpaceTerm Protocol over every client connection

Every SpaceTerm Client connection uses SpaceTerm Protocol. Private local endpoints, secure WebSocket connections, and optional SSH tunnels carry the same protocol, so the chosen route does not change client operations or authoritative server state.

Every route requires pinned-server and paired-device authentication in both supported protocol major versions. Private local endpoints also check that the peer belongs to the owning account. SSH reachability alone does not authorize SpaceTerm operations. Network listeners stay disabled until explicitly retained remote-access policy enables them.

Users provide network reachability through their home or local network, VPN, tailnet, reachable public server address, or SSH route. SpaceTerm does not provision that network access or operate a relay service. A SpaceTerm-operated connectivity service would require a separate feature decision; it is outside this server architecture.

SSH Bootstrap can detect and reuse a compatible SpaceTerm Server or install and start one when missing. An SSH tunnel can provide a route when a direct SpaceTerm connection is unavailable, including hosts reached through SSH jump hosts. Setup and tunneling also use SSH authentication.

For the planned [server architecture](https://github.com/sadiksaifi/SpaceTerm/issues/520), this revises the complete SSH-removal direction explored in [issue #162](https://github.com/sadiksaifi/SpaceTerm/issues/162): SpaceTerm retains SSH for setup and optional tunneling, and SpaceTerm Servers replace SSH-based Remote Workspaces. [ADR 0003](0003-let-openssh-own-remote-authentication.md) remains relevant to desktop OpenSSH authentication and host verification in those roles.
