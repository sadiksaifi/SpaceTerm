# Resolve application directories by XDG convention

SpaceTerm resolves its application directories from XDG roots on macOS as well as Linux. Credentials stay in native credential facilities without a fallback secret store.

For the planned server architecture in [issue #520](https://github.com/sadiksaifi/SpaceTerm/issues/520), [ADR 0030](0030-start-servers-at-boot-independently-of-clients.md) defines the server-key and pairing-storage exception. Client credentials retain the native credential-storage requirement.
