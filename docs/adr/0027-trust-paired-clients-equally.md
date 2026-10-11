# Trust paired clients equally

Every Paired Device has equal authority over its SpaceTerm Server, matching the owner's equally trusted personal devices. Its SpaceTerm Clients can create, take over, and close terminal work; change server settings; pair and unpair devices; update or repair the server; and Start Server or Stop Server. Individual device revocation follows [ADR 0038](0038-unpair-devices-without-blocking-local-connection-removal.md).

Management operations remain subject to the host operating system's authorization requirements and the existing confirmation rules, including [confirmation before a server update restarts all Terminal Sessions](0034-confirm-server-update-restarts-and-explain-incompatibility.md). Pairing grants SpaceTerm authority without supplying operating-system administrator credentials.
