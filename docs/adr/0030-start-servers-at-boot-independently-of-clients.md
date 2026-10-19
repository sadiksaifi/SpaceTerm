# Run account servers independently of clients

The owning nonroot account runs its SpaceTerm Server through the host's service manager. Normal desktop launch provisions an exact verified independent runtime only when service policy is absent, then presents server-owned local work. Client exits, crashes, and disconnections leave server-owned terminal work running.

On macOS, an account LaunchAgent starts at login and stops at logout. Installation requires no administrator password. On Linux, a systemd user service starts at boot and survives logout only with confirmed enabled linger. Enabling linger requires one scoped administrator authorization through CLI sudo, desktop pkexec, or explicit SSH Bootstrap. Installation that starts the Linux server and explicit Start Server require confirmed enabled linger.

No root process executes SpaceTerm code. The server may live in account application data. Server state uses account-private directories with mode `0700` and files with mode `0600`; these modes restrict other accounts, not processes running as the owning account.

Machine reboot and power loss end live Terminal Sessions and their in-memory terminal state. Automatic startup follows the platform's login or boot policy and the retained Stop Server state. A restarted server uses its durably retained nonsecret identity and route-specific admission authority; it does not resume the previous running programs.

[ADR 0043](0043-start-restored-shells-on-first-use.md) defines when retained Panes start fresh shells after a server restart.

Stop Server stops the SpaceTerm service, durably suspends automatic startup, and ends the server's Terminal Sessions. The server stays stopped across client launches, reconnect attempts, and machine reboots until an explicit Start Server action starts it and restores automatic startup. Service supervision must respect this deliberate stopped state, so it cannot immediately restart a server that the person stopped.

Start Server uses host-level startup authority through the local service manager or SSH, subject to the operating-system authorization requirements in [ADR 0027](0027-trust-paired-clients-equally.md). A client using only SpaceTerm Protocol reconnects after the service has been started. Local setup, service use and reconnect require no client credential store; terminal programs retain their normal credential and agent access.

[ADR 0048](0048-end-all-sessions-from-the-selected-servers-settings.md) defines End All Sessions while the server remains running.
