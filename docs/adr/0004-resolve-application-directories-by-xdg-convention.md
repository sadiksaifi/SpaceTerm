# Resolve application directories by XDG convention

SpaceTerm resolves its application directories from XDG roots on macOS as well as Linux. The account-local server uses owning-account paths shared by the desktop and service, independently of client environment overrides.

Stable nonsecret client identities and their views, attachments and operation journals live outside the Settings Document. [ADR 0030](0030-start-servers-at-boot-independently-of-clients.md) defines account-private server storage.

Direct-network client secrets use native credential facilities without a fallback secret store. The desktop and CLI use [the shared credential interface](../../crates/spaceterm-connections/src/credentials.rs), with separate credential references for their pairings. A native item holds one immutable pairing secret; inserting an existing key cannot replace it. Connection records retain references, never secret bytes.

On macOS, Release and Development use the login Keychain through [the Security framework adapter](../../crates/spaceterm-connections/src/credentials/macos.rs). Neither build identity has Apple team signing, so an update or rebuild can require renewed Keychain approval. [Signing] A stable Developer ID requirement is future work.

Preflight uses [the `/usr/bin/security` adapter](../../crates/spaceterm-connections/src/credentials/macos/security_tool.rs) so its ad hoc builds share Keychain access. The secret remains in the login Keychain, with no per-application protection from other owning-account processes. A credential created by an older Preflight build can require one approval; "Always Allow" permits later access. On Linux, [the Secret Service adapter](../../crates/spaceterm-connections/src/credentials/linux.rs) uses the account's session bus and persistent native collection.

CLI pairing-secret access without a usable store fails with `credential_unavailable` and exits 5. An access requiring interaction fails with `credential_interaction_required` and exits 3 when interaction is suppressed. JSON, `--no-input` and a missing terminal use quiet credential reads. Enrollment inserts and restricted cleanup also suppress native prompts. [CLI route selection](../../crates/spaceterm-cli/src/connection/route.rs) and [connection error mapping](../../crates/spaceterm-cli/src/connection.rs) own these choices. The CLI never writes a plaintext credential fallback.

The server role, account-local work and `connection list` require no client credential or native credential store. Terminal programs retain their own credential access.
