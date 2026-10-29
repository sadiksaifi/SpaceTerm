# Use one headless executable

`spaceterm` runs the SpaceTerm Server and provides the command-line client for local and paired servers. It links no GPUI, GPU, display or Sparkle code, so server administration and terminal work need no desktop process. The desktop remains a separate executable under [ADR 0012](0012-derive-one-application-identity-from-the-build.md).

The signed headless artifact contains `server/bin/spaceterm`. Native service units invoke `spaceterm server run --service --identity <identity>` from the approved runtime. Machine-facing provisioning, runtime verification, packaging checks and the transient service helper use hidden `server provision` commands. The CLI opens no SSH connection; [SSH Setup](0003-use-openssh-for-server-setup.md) remains a desktop operation.

The installer owns the command-line launcher in `~/.local/bin/`. It points to the approved runtime, or to the newest retained installation when none is approved. Repointing holds the service lock and replaces only a link into that identity's installations. A foreign executable or link stays untouched. Desktop and headless installation share one server under [ADR 0033](0033-reuse-one-server-across-headless-and-desktop-installations.md).

Executable and grammar replacement is direct. There is no `spaceterm-server` executable, alias, shim, symlink or old command grammar. [Signing] Developer ID, hardened runtime, notarization, the installer's requirement check and team pinning are future work; none is required for this executable split.
