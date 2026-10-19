# Keep one server runtime version active

Each operating-system account and application identity has one active SpaceTerm Server runtime version under [ADR 0033](0033-reuse-one-server-across-headless-and-desktop-installations.md). Existing clients, newly connected clients, and newly created Workspaces all use that running version, so server ownership and capabilities do not split by release version.

Newer server releases may be downloaded, verified, and staged alongside the approved installation without becoming active. Client launches, reconnects, Workspace creation, and protocol incompatibility do not activate a staged release or launch a second server. Settings distinguishes the running server version from an update ready to install.

Activation requires the explicit server-update confirmation in [ADR 0034](0034-confirm-server-update-restarts-and-explain-incompatibility.md) and replaces the running version after ending its Terminal Sessions. Boot, crash recovery, and Start Server select the durably approved installation rather than the newest staged release. Updating a deliberately stopped server leaves it stopped under [ADR 0030](0030-start-servers-at-boot-independently-of-clients.md).

Client installation and update lifecycles preserve the approved server's executable and resources independently of the client installation. Service registration resolves that server installation even when a different desktop or headless installation discovers it.

Desktop candidate preparation embeds the independently signed server archive and signature for its exact build. Source builds supply that pair explicitly for fresh setup; [the local workflow](../local-server.md) defines this development boundary. Launch never signs an artifact or promotes a sibling executable into an approved runtime.
