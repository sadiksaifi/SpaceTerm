# Start servers at boot independently of clients

For the [server architecture](https://github.com/sadiksaifi/SpaceTerm/issues/162), setup registers automatic SpaceTerm Server startup with the host's service manager. Server lifetime is independent of every SpaceTerm Client, whether clients connect through a private local endpoint or over a network, so client exits, crashes, and disconnections leave server-owned terminal work running.

Machine reboot and power loss end live Terminal Sessions and their in-memory terminal state. After the operating system boots again, automatic startup brings up a fresh server using its durably retained identity and paired-device authorization; it does not resume the previous running programs.

[ADR 0043](0043-start-restored-shells-on-first-use.md) defines when retained Panes start fresh shells after a server restart.

Stop Server stops the SpaceTerm service, durably suspends automatic startup, and ends the server's Terminal Sessions. The server stays stopped across client launches, reconnect attempts, and machine reboots until an explicit Start Server action starts it and restores automatic startup. Service supervision must respect this deliberate stopped state, so it cannot immediately restart a server that the person stopped.

Start Server uses host-level startup authority through the local service manager or SSH, subject to the operating-system authorization requirements in [ADR 0027](0027-trust-paired-clients-equally.md). A client using only SpaceTerm Protocol reconnects after the service has been started.

[ADR 0048](0048-end-all-sessions-from-the-selected-servers-settings.md) defines End All Sessions while the server remains running.
