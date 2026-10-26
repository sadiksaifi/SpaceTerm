# Unpair devices without blocking local connection removal

Every remote connection is paired WSS. Account-local IPC has no pairing to revoke. After confirmation, a paired client can unpair itself, including when it is the last Paired Device. Only an account-local client can unpair another device. Self-unpair immediately removes its Saved Connection and Workspace Attachments, independently of reachability. Unpairing another device preserves the initiating connection. Neither action kills server-owned work or creates a replacement Workspace.

The server durably records revocation, closes that device's connections and releases its Pane control. Revoked credentials cannot reconnect or resume control. Other pairings and servers remain unaffected.

Local removal and confirmed revocation have distinct outcomes. An unconfirmed result retains a restricted request across relaunch, presented by a dismissible Servers notice. Retry uses only the original pinned WSS endpoint and pairing authority or an account-local owning-account connection. It never reopens SSH, redirects to a newer pairing, restores presentation or enrolls a device. Credential-provider interaction requires explicit Resume or Retry.

A revoked device may authenticate only to resolve its exact admitted revocation or confirm that its exact pairing is already revoked. The client retains a validated receipt before retiring the native credential reference. Failed credential cleanup remains pending and preserves newer or unrelated credentials, connections and attachments.

Cancelled or interrupted enrollment retains only its exact claim, pinned server, endpoint and credential reference for restricted cancellation. It retains no Invitation Link. If approval won the race, cancellation revokes only the pairing created by that claim.
