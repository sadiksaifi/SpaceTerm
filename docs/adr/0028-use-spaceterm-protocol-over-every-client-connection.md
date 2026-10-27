# Use SpaceTerm Protocol over every client connection

SpaceTerm Protocol carries operations and authoritative server state over account-local IPC and paired WSS. [ADR 0027](0027-trust-paired-clients-equally.md) defines admission authority. Remote connections never fall back to SSH.

Network access requires deliberate exposure authorization and retains one explicit bind policy and server identity across restarts. Turning access off retains the configured listener for the next deliberate enable. Configuration can change while exposure is off. The person supplies WSS reachability through their network, VPN or tailnet. SpaceTerm operates no relay; SSH aliases and ProxyJump establish no WSS route. Client endpoints accept DNS names and IP addresses independently of the server's bind address. DNS resolves on each connection; server certificate and proof-key pins remain the authority.

An Invitation Link contains the reachable address, pinned TLS certificate, public proof key and a strong one-use secret. The server enforces its five-minute expiry. Clients use the issued lifetime for monotonic waiting and presentation; differences between client and server wall clocks cannot reject a claim. The claimant verifies the pinned server before submitting a signed claim. Both proofs bind the server and exact claim; both screens display the same authenticated six-digit Comparison Code. An account-local client approves that exact claim after comparison.

Invite Device presents the link and QR code. Add Server waits for explicit Join. Operating-system `spaceterm://pair/…` delivery opens that same Settings journey on launch or in the running client. Invitations remain transient and never enter retained state, diagnostics or child commands.

Enrollment binds a device credential and pairing to one Client Identity before terminal admission. A new pairing cannot claim another pairing's views or Pane control by supplying its identity. Cancellation and lost replies follow [ADR 0038](0038-unpair-devices-without-blocking-local-connection-removal.md).

[ADR 0050](0050-push-terminal-presentation-over-one-multiplexed-connection.md) replaces this protocol's request-and-poll exchange with one pushed, multiplexed connection per server.
