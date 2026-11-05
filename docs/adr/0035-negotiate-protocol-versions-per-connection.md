# Negotiate protocol versions per connection

SpaceTerm has three protocols that change independently: the work protocol, the [service lane](0057-administer-servers-over-the-service-lane.md) and the [Desktop Channel](0060-open-files-and-show-work-in-the-desktop-over-the-desktop-channel.md). Each has a `major.minor` version. [The version module](../../crates/spaceterm-core/src/protocol/version.rs) owns this build's three versions and the rule that selects one. Client and SpaceTerm Core release numbers need not match, because only protocol versions decide compatibility.

## The rule

On every connection, one side calls and the other listens. The caller speaks its current major and the previous one. The listener speaks one major. The caller offers its current version, then the previous major at that major's last minor. The listener selects the shared major at the lower of the two minors. When the two share no major, neither proceeds, and each side reads from the other's versions which one is older: `listener_too_old` or `caller_too_old`.

The client is the caller on the work protocol and the service lane, and SpaceTerm Core is the listener. The CLI is the caller on the Desktop Channel, and the desktop is the listener. A client therefore keeps working with a SpaceTerm Core one major behind it. A SpaceTerm Core one major ahead refuses the older client and names the client as the side to update.

Minors are additive within a major. A minor may add only a frame kind, an enum variant appended after the existing ones, such as a `CoreMessage`, an `Operation`, or a lane request or reply, and new types reachable only through those additions. Any other change needs a new variant or a new major: adding, removing or reordering a field of an existing type, reordering variants, or changing a type or a limit. A sender never sends anything the selected minor lacks. A work-protocol receiver treats an unknown frame kind or variant as `Fatal::ProtocolViolation`. The service lane and the Desktop Channel refuse unknown fields, and the service lane answers a known request that its selected minor lacks with `unsupported`.

A new major replaces its protocol in place. A caller keeps the previous major's code until the next major, and nothing older. A listener keeps none. Beyond that window SpaceTerm keeps no protocol compatibility: no legacy readers, compatibility shims, dual transports or SSH fallbacks. Nothing has been released yet, so obsolete Settings, serialized Commands and retained formats are replaced directly. From the first release on, a breaking change to a retained format needs a Data Generation step under [ADR 0064](0064-step-updates-through-update-floors-and-migrate-data-generations.md).

## Where negotiation happens

Each connection negotiates in its first exchange, before any work. The account-local Unix socket and paired QUIC streams run the same [admission preamble](../../crates/spaceterm-core/src/protocol/admission.rs). The client's Hello names the lane, Work or Service, and lists the versions it offers for that lane. SpaceTerm Core's Offer lists the version it speaks on each lane, its release version, and the version selected for this connection, or none. The work connection and each service lane connection negotiate separately. A client whose work protocol is too old can still use the service lane, so it can update SpaceTerm Core and settle the same operation after reconnecting.

The Desktop Channel negotiates the same way on each connection. A ChannelHello and a ChannelOffer come before the one request, which the CLI encodes at the selected version. [The channel contract](../../src/desktop_channel.rs) owns those messages.

Every Hello and Offer is frozen. Each is bounded length-prefixed JSON that refuses unknown fields, and no major changes it. Two builds that share no major still read each other's Hello and Offer, which is how each learns which side is older.

## Outcomes

A Machine is ready only after its work admission selects a version. A refusal for want of a shared major is a typed outcome that names the older side. [ADR 0034](0034-confirm-server-update-restarts-and-explain-incompatibility.md) owns what the person sees. The CLI reports `incompatible_protocol`, or `desktop_incompatible` for the Desktop Channel, with `details.update` naming the side to update: `machine` for SpaceTerm Core on the Machine the command reached, `desktop` for the desktop, and `core` for SpaceTerm Core on this computer, which ships the CLI.

A client derives each Machine's capabilities from the versions its admissions selected, through [the capability check](../../crates/spaceterm-core/src/protocol/capability.rs). An operation the selected minor lacks is unavailable on that Machine, and the client never sends it.

After work admission, SpaceTerm Core sends its update facts, then sends them again after each change. The service lane's `Facts` and `WaitFacts` answer from the same [facts source](../../crates/spaceterm-core/src/update/source.rs).

## Compatibility testing

[The compatibility list](../../.github/compatibility.toml) names every published release whose work major is this build's or the previous one. [The compatibility workflow](../../.github/workflows/compatibility.yml) pairs this build with each listed release in both directions on x86_64 Linux. The pairs run `list` on the work protocol and `status` on the service lane. This build's CLI must work with the release's Core. The release's CLI must work with this build's Core on each protocol whose major the two share, and must be refused as the older side on each protocol whose major they do not share.
