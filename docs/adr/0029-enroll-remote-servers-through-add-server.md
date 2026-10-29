# Enroll remote servers by invitation

A server's own `spaceterm server pair` creates the Invitation Link. The desktop's Add Server and the command line's `spaceterm connection add` consume it with the same outcomes. SSH Setup remains an optional desktop method that installs and enrolls a server through OpenSSH. Application-menu, command-palette and operating-system invitation shortcuts open Add Server. [ADR 0056](0056-install-and-pair-servers-from-their-own-command-line.md) owns the server-side command-line flow.

`server pair --address HOST[:PORT]` sets the invitation's advertised authority. It accepts a DNS name or any IP with an optional port and is not restricted to local interfaces, so NAT and cloud public addresses work.

SSH Setup uses [OpenSSH authority](0003-use-openssh-for-server-setup.md) temporarily to discover the approved account installation, offer verified installation when missing, obtain required operating-system consent, explicitly start the server, configure reachable QUIC exposure and enroll the client. Reversing deliberate Stop requires explicit startup authorization inside setup. Incompatibility leaves the installation intact for administration on its own machine.

The SSH destination is sufficient to begin setup. OpenSSH's evaluated HostName supplies the default QUIC hostname; its SSH port remains independent of the QUIC port. A collapsed optional network-address field accepts a DNS name or IP address with an optional QUIC port. DNS names remain in Saved Connections and resolve on each connection. The reachable address and the server's listening address are separate facts shown in network consent. SSH routing through a jump host does not establish QUIC reachability.

Installation consent binds one signed artifact's identity, target and complete digest; transfer and installation revalidate those bytes. Network exposure requires deliberate authorization for a reachable endpoint. Successful installation or startup alone never completes Add Server. Success requires persisted pairing credentials and saved connection, closed SSH resources and authenticated QUIC admission. Unreachable QUIC or failed credential persistence presents an actionable setup result without publishing an SSH connection.

One setup attempt owns one foreground SSH authentication connection through discovery and approval waits. Multiplexed commands require that exact owned connection and cannot fall back to fresh authentication. Setup shows its current phase and yields to Authentication Prompts only while interaction is required. The remote-command observation deadline begins after authentication; transfers have no whole-upload deadline. Cancellation releases owned processes and input suppression. Admitted service effects are observed and reported truthfully, without claiming rollback; enrollment cleanup targets only the exact claim and pairing.

After setup, terminal work, directories, status, reconnect, relaunch, Pane Try Again and pairing recovery use QUIC exclusively. [ADR 0057](0057-administer-servers-over-the-service-lane.md) owns remote service administration. Saved connections retain no SSH runtime route or fallback.

[ADR 0052](0052-carry-paired-connections-over-quic.md) owns the paired transport.
