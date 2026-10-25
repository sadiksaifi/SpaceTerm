# Negotiate one protocol major

SpaceTerm uses one SpaceTerm Protocol major, major 1, with negotiated optional capabilities. Admission checks the shared major and required capabilities before reporting a server ready or admitting dependent operations. A missing optional capability disables only its feature; client and server release numbers need not match.

The server-client architecture has no backward-compatibility requirement. Replace obsolete protocols, Settings, serialized Commands and retained formats directly. Keep no migrations, legacy readers, compatibility shims, dual transports or SSH fallbacks. Incompatible connections follow [ADR 0034](0034-confirm-server-update-restarts-and-explain-incompatibility.md).

Resetting incompatible local state still requires explicit confirmation of its exact scope and loss, completed or reconciled started work and proven process cleanup. Compatibility policy grants no authority to delete unrelated data or credentials.
