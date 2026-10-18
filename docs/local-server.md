# Working with the local server

The desktop and local server use your operating-system account's authority. They do not pair or open a credential store. Independent clients retain nonsecret identities so their views, attachments and Pane control survive relaunch. [ADR 0027](adr/0027-trust-paired-clients-equally.md) owns the trust decision.

The current server workflow is explicit: open Settings > Servers, set up or start the server, then choose Use Shared Workspaces. Normal production startup remains with [#561](https://github.com/sadiksaifi/SpaceTerm/issues/561). The `server:*` mise tasks exercise the Development identity. Setup and status use `server:setup` and `server:status`; status never starts a server. A deliberately stopped service requires its explicit Start action.

## Replacing incompatible development state

Unreleased local state is replaced directly. Quit the desktops and finish server shutdown before a reset. Resolve pending client operations and reconcile started Git writes with the build that admitted them first. A retained endpoint, active instance/write lock or unresolved service cleanup prevents reset. Never signal a process from a saved PID or remove a socket to bypass that refusal.

Run `mise run server:reset --identity spaceterm-development` to preview the exact selected account paths and digest. The task selects only `identity.json` and `clients` beneath that account/build's server directory. Obtain explicit owner confirmation of the displayed scope and loss before repeating with `--confirm DIGEST`. A changed scope requires a fresh preview. `spaceterm-preflight` must be selected separately; the task never selects Release.

The selected server identity, Workspace organization, completed outcomes, client identities, attachments, deferred releases and views leave active use. Their exact bytes remain in a private `reset-*` archive. Settings schema 7, serialized Commands, repository files, signed installations, service registration and Stop intent, SSH trust, credentials and signing keys remain outside the reset. Setup stays explicit. A stopped service stays stopped.

After reset, `mise run server:setup --state-only` recreates Development state without starting anything or reversing Stop. The CLI equivalent is `setup-state IDENTITY` in a client-enabled server build. Only the service's explicit Start can resume a stopped service. Its selected signed runtime must support the new state and protocol; reset never selects an update or weakens artifact verification.

## Optional obsolete credential cleanup

Old local credentials may remain in native storage after reset. They are unused and can stay there indefinitely. Cleanup is separate from local setup and use; an absent, locked or unavailable store is no reason to delay either.

Each obsolete item has service `io.github.sadiksaifi.BUILD.local-server`, where `BUILD` is the exact old application directory identity, and account `desktop` or `desktop-NAME` for a named test client. Select and confirm one exact item before removing it. On macOS, `security delete-generic-password -s SERVICE -a ACCOUNT` removes that generic-password item. On Linux, the old adapter's exact attributes are `application=rust-keyring`, `target=default`, `service=SERVICE`, and `username=ACCOUNT`; `secret-tool clear` accepts those attribute/value pairs. These commands are optional and may need normal OS interaction. Never delete an entire collection, Keychain, SSH credential or signing key.

Local admission removal does not change terminal programs' credential access, signed-artifact verification or operating-system authorization. Direct-network credential storage and SSH Connect/Resume remain with their owning remote issues.
