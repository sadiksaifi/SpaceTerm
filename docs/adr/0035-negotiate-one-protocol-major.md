# Negotiate one protocol major

SpaceTerm uses one SpaceTerm Protocol work major at a time. [The protocol constant](../../crates/spaceterm-server/src/protocol/major.rs) owns the work major. Work admission requires a shared major before reporting a server ready or admitting dependent operations. Client and server release numbers need not match.

The server-client architecture has no backward-compatibility requirement. Replace obsolete protocols, Settings, serialized Commands and retained formats directly. Keep no migrations, legacy readers, compatibility shims, dual transports or SSH fallbacks. Incompatible connections follow [ADR 0034](0034-confirm-server-update-restarts-and-explain-incompatibility.md).

The admission preamble and [service lane](0057-administer-servers-over-the-service-lane.md) are frozen across majors. They use bounded length-prefixed JSON on the same authenticated connection. Service admission needs no shared work major, so an incompatible client can update a running server and resolve the original operation. This contract preserves administration across work-major changes without a reader for an obsolete work protocol.
