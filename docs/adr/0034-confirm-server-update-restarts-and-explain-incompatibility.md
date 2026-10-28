# Confirm local server updates and explain incompatibility

Client and server release numbers may differ. Admission requires the [supported protocol major and required capabilities](0035-negotiate-one-protocol-major.md). Refusal distinguishes an unsupported major from a missing capability and leaves other connections and server work intact. Explain which side needs updating only when verified information establishes the direction.

Client updates restart the client and preserve server-owned work. Server updates are administered on the server's own machine. Settings, status controls and local incompatibility recovery open the same account-local Update Server journey when a verified different candidate is available. Remote WSS presentation offers no update delivery, activation or return action.

Confirmation names the server, its current version and the exact candidate. It states that every client's Terminal Sessions on a running server end, or that a server that isn't running ends none. Changed installation, run or Stop observations require fresh confirmation. Identity, pairings and [Workspace organization](0031-retain-workspace-organization-across-server-restarts.md) survive; retained Panes follow [ADR 0043](0043-start-restored-shells-on-first-use.md). Updating a stopped server leaves it stopped.

Admitted local operations belong to the application independently of the Settings Window. Recovery resolves the original exact-target operation without repeating destructive dispatch or choosing another candidate. Uncertain outcomes refuse terminal admission. Return to the previous verified runtime requires separate confirmation and proof that the candidate never became ready and saved state is unchanged. Ended Terminal Sessions never return. [ADR 0040](0040-keep-one-server-runtime-version-active.md) owns runtime activation.

[ADR 0052](0052-carry-paired-connections-over-quic.md) carries paired connections over QUIC instead of WSS.
