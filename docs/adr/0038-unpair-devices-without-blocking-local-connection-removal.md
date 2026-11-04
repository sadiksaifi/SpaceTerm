# Unpair devices without blocking local connection removal

Every remote connection is paired QUIC under [ADR 0052](0052-carry-paired-connections-over-quic.md). Account-local IPC has no pairing to revoke. After confirmation, a paired client can unpair itself, including when it is the last Paired Device. Only an account-local client can unpair another device. Self-unpair immediately removes its Saved Connection and Workspace Attachments, independently of reachability. Unpairing another device preserves the initiating connection. Neither action kills Core-owned work or creates a replacement Workspace.

SpaceTerm Core durably records revocation, closes that device's connections, releases its Pane control and forgets its retained edit outcomes. Revoked credentials cannot reconnect or resume control. Other pairings and Machines remain unaffected. Remove Machine is the desktop's self-unpair: it removes that Machine's Saved Connection and requests Unpair Device. The CLI's `machine remove` does the same for its own Saved Connection.

Local removal and confirmed revocation have distinct outcomes. An unconfirmed result retains a restricted request across relaunch, presented by a dismissible Machines notice. Try Again uses only the original pinned QUIC endpoint and pairing authority or an account-local owning-account connection. It never reopens SSH, redirects to a newer pairing, restores presentation or enrolls a device. Credential-provider interaction requires explicit Resume or Try Again.

A revoked device may authenticate only to resolve its exact admitted revocation or confirm that its exact pairing is already revoked. The client retains a validated receipt before retiring the native credential reference. Failed credential cleanup remains pending and preserves newer or unrelated credentials, connections and attachments.

Cancelled or interrupted enrollment retains only its exact claim, pinned Machine ID, endpoint and credential reference for restricted cancellation. It retains no Invitation Link. If approval won the race, cancellation revokes only the pairing created by that claim.
