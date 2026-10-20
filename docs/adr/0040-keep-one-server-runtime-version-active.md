# Keep one server runtime version active

Each operating-system account and application identity has one active SpaceTerm Server runtime version under [ADR 0033](0033-reuse-one-server-across-headless-and-desktop-installations.md). Existing clients, newly connected clients, and newly created Workspaces all use that running version, so server ownership and capabilities do not split by release version.

Signed server candidates may be verified and staged alongside the approved installation without becoming active. Client launches, reconnects, Workspace creation, and protocol incompatibility do not activate a staged candidate or launch a second server. Settings distinguishes the approved or running server version from the candidate included with that client.

Activation requires the explicit server-update confirmation in [ADR 0034](0034-confirm-server-update-restarts-and-explain-incompatibility.md) and replaces the running version after ending its Terminal Sessions. Boot, crash recovery, and Start Server select the durably approved installation rather than the newest staged release. Updating a deliberately stopped server leaves it stopped under [ADR 0030](0030-start-servers-at-boot-independently-of-clients.md).

Client installation and update lifecycles preserve the approved server's executable and resources independently of the client installation. Service registration resolves that server installation even when a different desktop or headless installation discovers it.

Desktop candidate preparation embeds an explicitly selected, independently signed server archive and signature and pins the complete archive digest into the client executable. Several client releases may reuse the same byte-identical archive, including its executable, resources and build metadata. Matching client and server release numbers are unnecessary. An identical approved artifact needs no server update.

Candidate verification checks signature, complete archive digest, application identity, native target, resources and declared compatibility before activation is offered. A running client whose installation now contains another candidate must restart before using it. Source builds retain an explicit signed-artifact workflow; [the local workflow](../local-server.md) defines that development boundary. Launch never signs an artifact or promotes a sibling executable into an approved runtime.

An admitted activation retains its exact candidate and original observations durably, separately from deliberate Stop. Ordinary service, desktop and control entry points recover that operation under [ADR 0034](0034-confirm-server-update-restarts-and-explain-incompatibility.md). Recovery never chooses an arbitrary staged artifact. No permanent update helper is installed.
