# Keep one server runtime version active

Each operating-system account and application identity has one active SpaceTerm Server runtime. Existing clients, new clients and Workspaces use it. [ADR 0033](0033-reuse-one-server-across-headless-and-desktop-installations.md) owns shared discovery.

Signed candidates may be verified and staged beside the approved installation without activation. Launch, reconnect, Workspace creation and incompatibility never select another candidate. Boot, crash recovery and Start use the durably approved runtime. [Confirmed account-local updates](0034-confirm-server-update-restarts-and-explain-incompatibility.md) replace it and preserve deliberate Stop.

The approved executable and resources survive desktop installation changes. Candidate preparation embeds an independently signed archive and signature and pins the complete digest into the desktop executable. Multiple clients may reuse identical bytes. Verification checks signature, digest, application identity, native target, resources and protocol requirements. A running client must restart before using a changed embedded candidate.

Development and Preflight use the separate trust in [server-nonproduction-trust.json](../../packaging/server-nonproduction-trust.json). Its public seed grants no Release authority. Release accepts only production trust. Signing belongs to preparation; launch never signs artifacts or approves sibling executables.

Admitted local activation durably retains its exact candidate and observations. Recovery never chooses arbitrary staged bytes or installs a permanent helper. Source Development instead uses the [foreground lifecycle](0030-start-servers-at-boot-independently-of-clients.md) without replacing an approved service runtime.
