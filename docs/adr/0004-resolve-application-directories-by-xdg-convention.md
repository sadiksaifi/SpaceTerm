# Resolve application directories by XDG convention

SpaceTerm resolves its application directories from XDG roots on macOS as well as Linux. The account-local server uses owning-account paths shared by the desktop and service, independently of client environment overrides.

Local setup and use require no SpaceTerm client secret or native credential storage. Stable nonsecret client identities and their views, attachments and operation journals live outside the Settings Document. Direct-network client secrets use native credential facilities without a fallback secret store; an unavailable store affects only that connection. Terminal programs retain their own credential access. [ADR 0030](0030-start-servers-at-boot-independently-of-clients.md) defines account-private server storage.
