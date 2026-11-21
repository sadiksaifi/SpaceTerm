# Use a portable client crate for paired connections

SpaceTerm shares the paired client in `spaceterm-client` so the desktop, the SpaceTerm CLI, and the mobile clients planned in [ADR 0026](0026-build-mobile-clients-with-expo-and-native-platform-ui.md) can use the same admission, reconnect, and pairing recovery behavior. Mobile clients will reach Machines only over paired QUIC. The desktop and CLI also use account-local IPC through native adapters. Each application supplies its own storage and system authority, which lets mobile clients reuse the paired behavior without embedding SpaceTerm Core.

## Ownership

`spaceterm-protocol` owns the wire types, codecs, versions, and limits for the work protocol, service lane, and Desktop Channel. It defines which capabilities each work and service version supports and owns the Invitation Link and pairing authentication formats. These contracts are independent of I/O and operating-system code.

`spaceterm-client` depends on `spaceterm-protocol`. It runs the paired QUIC connection and client-side admission, manages pairing and Saved Connections, and handles enrollment and revocation recovery. Its work link receives the Workspace Inventory and carries Workspace edits and directory reads. The Workspace editor records the Original Operation before dispatch and validates the matching outcome before retirement and acknowledgement. Its service-lane client can reach a Machine even when work admission is refused.

SpaceTerm Core owns authoritative Workspaces, Tabs, Panes, and Terminal Sessions. It decides admission and revocation, applies edits, and executes service-lane requests. It depends on both crates and enables the client's `core-transport` feature to reuse QUIC listener machinery. The native code in `spaceterm-core` handles account-local IPC and peer checks and provides the device-name, route-signal, and credential adapters used by the desktop and CLI. Native client wrappers handle private paths and retained Client View State and provide the journal's storage adapter. SSH Setup stays in the desktop's Add Machine.

## Construction

`DirectConnections::with_credentials` takes the application's build identity, `Records` for Saved Connections and pending recovery, and a `CredentialStore` for pairing secrets. The desktop supplies a `RecordFile` adapter for private file access. The CLI constructs its `ConnectionStore` with a `ConnectionStorage` adapter for its own private directory and uses a `CredentialProvider` to select a backend for enrollment and open the recorded backend afterward.

The registry also takes a `DeviceName` callback to supply the Client Name and a `NetworkPath` factory to open an optional route-change signal for each work connection. Periodic path checks still run when the factory supplies no signal. `PairedMachine::new` takes the expected build identity, the pinned pairing, a connector, and the same name and path inputs. The expected build identity must match the pairing. A native caller with an Account-local Connection can also give the registry authority to recover account-local revocations.

The Workspace editor opens an injected `JournalStore` under an exclusive writer lease. The store replaces the journal atomically and reports durable bytes. The editor binds the journal to the paired Machine and effective Client Identity before sending an edit. The desktop publishes reconciled Workspace membership before acknowledgement. The CLI resolves pending edits without dispatching them during passive recovery.

## Released formats

Move the released protocol encodings byte for byte, including the Invitation Link and pairing authentication exchange. [ADR 0035](0035-negotiate-protocol-versions-per-connection.md) governs changes to the negotiated protocols. Preserve the released Saved Connections, operation journal, Client View State, and selection formats under the Data Generation and Update Floor rules of [ADR 0064](0064-step-updates-through-update-floors-and-migrate-data-generations.md).
