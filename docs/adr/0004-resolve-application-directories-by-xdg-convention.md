# Resolve application directories by XDG convention

SpaceTerm resolves its application directories from XDG roots on macOS as well as Linux. The account-local server uses owning-account paths shared by the desktop and service, independently of client environment overrides.

Stable nonsecret client identities and their views, attachments and operation journals live outside the Settings Document. [ADR 0030](0030-start-servers-at-boot-independently-of-clients.md) defines account-private server storage.

Direct-network client secrets use native credential facilities without a fallback secret store. The CLI stores its pairing credential through the `CredentialStore` trait in `crates/spaceterm-connections`: Keychain on macOS and Secret Service on Linux. When the native store is unavailable, the CLI fails with `credential_unavailable` and never writes plaintext. The server role and local work require no client credential or native credential store. Terminal programs retain their own credential access.
