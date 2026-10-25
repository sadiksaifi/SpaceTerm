# Run account servers independently of clients

Installed Preflight and Release run their SpaceTerm Server through the owning nonroot account's host service manager. Normal desktop launch provisions an exact verified independent runtime only when service policy is absent, then presents server-owned local work. Client exits, crashes, and disconnections leave server-owned terminal work running.

On macOS, an account LaunchAgent starts at login and stops at logout. Installation requires no administrator password. On Linux, a systemd user service starts at boot and survives logout only with confirmed enabled linger. Enabling linger requires one scoped administrator authorization through CLI sudo, desktop pkexec, or explicit SSH Bootstrap. Installation that starts the Linux server and explicit Start Server require confirmed enabled linger.

No root process executes SpaceTerm code. The server may live in account application data. Server state uses account-private directories with mode `0700` and files with mode `0600`; these modes restrict other accounts, not processes running as the owning account.

Machine reboot and power loss end live Terminal Sessions and their in-memory terminal state. Automatic startup follows the platform's login or boot policy and the retained Stop Server state. A restarted server uses its durably retained nonsecret identity and route-specific admission authority; it does not resume the previous running programs.

[ADR 0043](0043-start-restored-shells-on-first-use.md) defines when retained Panes start fresh shells after a server restart.

Start automatically is a retained host-service policy, independent of service installation and deliberate Stop Server state. Changing it does not stop a running server, start a stopped server or uninstall the service. It controls login startup on macOS and boot startup on Linux. Only account-local and SSH account routes may change it.

Stop Server stops the SpaceTerm service, durably suspends automatic startup, and ends the server's Terminal Sessions. The server stays stopped across client launches, reconnect attempts, and machine reboots until an explicit Start Server action starts it. Start and Stop preserve the person's automatic-start preference. Service supervision respects deliberate Stop even when that preference is on. Stop and Restart confirmations name the Terminal Sessions that end. Uninstall uses a title-only confirmation; account-wide lingering is outside that confirmation.

Explicit Start, Stop, Restart and Uninstall use host-level service authority through the local service manager or the SSH account route, subject to the operating-system authorization requirements in [ADR 0027](0027-trust-paired-clients-equally.md). An SSH route carries the account's own authority and needs no SpaceTerm paired-device credential; direct-network pairing or reachability alone grants no service authority. Over either route, Start is the only operation that reverses a deliberate Stop. A client using only SpaceTerm Protocol reconnects after the service has been started. Local setup, service use and reconnect require no client credential store; terminal programs retain their normal credential and agent access.

[ADR 0048](0048-end-all-sessions-from-the-selected-servers-settings.md) defines End All Sessions while the server remains running.

Source Development uses a separate foreground Server built with its client from the current source, including uncommitted edits. The development command owns both processes and ends their Terminal Sessions when the run ends, including interruption. It requires neither a signed distribution artifact, production credentials nor service registration. It retains Development Settings and Workspace organization between runs. Ownership checks prevent a run from adopting or stopping an unrelated Server; existing service state and deliberate Stop remain unchanged. This exception changes process lifetime, not server-owned Workspaces or connection authority.
