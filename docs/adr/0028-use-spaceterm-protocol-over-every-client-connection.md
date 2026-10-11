# Use SpaceTerm Protocol over every client connection

Every SpaceTerm Client connection uses SpaceTerm Protocol. Private local endpoints, secure WebSocket connections, and optional SSH tunnels carry the same protocol, so the chosen route does not change client operations or authoritative server state.

Users provide network reachability through their home or local network, VPN, tailnet, reachable public server address, or SSH route. SpaceTerm does not provision that network access or operate a relay service. A SpaceTerm-operated connectivity service would require a separate feature decision; it is outside this server architecture.

SSH Bootstrap can detect and reuse a compatible SpaceTerm Server or install and start one when missing. An SSH tunnel can provide a route when a direct SpaceTerm connection is unavailable, including hosts reached through SSH jump hosts. Setup and tunneling use SSH authentication; direct SpaceTerm connections support authentication independently of SSH.

For the [server architecture](https://github.com/sadiksaifi/SpaceTerm/issues/162), this revises the issue's complete SSH-removal direction: SpaceTerm retains SSH for setup and optional tunneling, and SpaceTerm Servers replace SSH-based Remote Workspaces. [ADR 0003](0003-let-openssh-own-remote-authentication.md) remains relevant to desktop OpenSSH authentication and host verification in those roles.
