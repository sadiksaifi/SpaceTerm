# Confirm server update restarts and explain incompatibility

Client and server release versions may differ when their SpaceTerm Protocol implementations are compatible. An incompatible connection explains which side needs updating and which process must restart; the failure is scoped to that server connection so the client can keep using its other compatible servers.

A client update restarts that client and preserves server-owned terminal work. An update that restarts a SpaceTerm Server requires explicit confirmation stating that every Terminal Session on that server ends for every client; the server's retained identity, paired devices, and [Workspace organization](0031-retain-workspace-organization-across-server-restarts.md) survive, and retained Panes follow the [fresh-shell startup policy](0043-start-restored-shells-on-first-use.md).

[ADR 0040](0040-keep-one-server-runtime-version-active.md) defines staging and activation of the single server runtime version.
