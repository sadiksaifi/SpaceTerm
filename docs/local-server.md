# Working with the local server

The desktop and local server use your operating-system account's authority. They do not pair or open a credential store. Independent clients retain nonsecret identities so their views, attachments and Pane control survive relaunch. [ADR 0027](adr/0027-trust-paired-clients-equally.md) owns the trust decision.

Normal desktop launch connects to the account-local server and presents its retained Workspaces in the main Workspace Window. Quitting the desktop leaves server-owned terminal programs running. A deliberately stopped server stays stopped until the person chooses Start. Settings > Servers retains service management. The `server:*` mise tasks exercise the Development identity; status never starts a server.

## Controls without the desktop

The desktop installation includes a CLI fallback for use without a status area or graphical login. For a standard installation, open its command help with:

```sh
# macOS
"/Applications/SpaceTerm.app/Contents/Library/LoginItems/SpaceTerm Server Status.app/Contents/MacOS/spaceterm-status-area" --help

# Linux
"$HOME/.local/lib/spaceterm/bin/spaceterm-status-area" --help
```

For Preflight, use "SpaceTerm Preflight" in both macOS app names or `spaceterm-preflight` in the Linux directory. For a custom installation, use the helper inside that app or installation directory. The help names the compiled application identity and lists the service commands and confirmation flag. Headless installations expose the same commands through `bin/spaceterm-server` in their installation directory. Source builds use `mise run server:service --help` for the Development identity.

## Server artifacts for desktop startup

The desktop startup adapter in [src/local_startup.rs](../src/local_startup.rs) prepares or reuses the account service on a worker. Existing service policy selects the approved installation. Launch does not select a newer artifact or reverse Stop.

Fresh service setup requires an independently signed server archive and signature for the desktop's exact application identity, target, commit and version. Packaged candidates carry this pair in their resources. Signing happens during candidate preparation, never during app launch.

Source builds must supply both `SPACETERM_SERVER_ARCHIVE` and `SPACETERM_SERVER_SIGNATURE` for fresh service setup. Prepare the exact clean source revision with `mise run server:package --identity spaceterm-development` using the existing signing inputs, then point those variables at its archive and adjacent `.sig` file. An absent or incomplete pair refuses fresh setup. Existing approved service policy needs neither variable. A sibling `spaceterm-server` executable is never an approved retained service runtime.

Linux first setup can return an authorization requirement before service registration. The caller must obtain explicit scoped linger authorization through the existing service control, then retry startup. Cancellation before service mutation preserves the previous policy; admitted service registration or Start is observed to completion without claiming rollback.

## Replacing incompatible development state

Unreleased local state is replaced directly. Quit the desktops and finish server shutdown before a reset. Resolve pending client operations and reconcile started Git writes with the build that admitted them first. A retained endpoint, active instance/write lock or unresolved service cleanup prevents reset. Never signal a process from a saved PID or remove a socket to bypass that refusal.

Run `mise run server:reset --identity spaceterm-development` to preview the exact selected account paths and digest. The task selects only `identity.json` and `clients` beneath that account/build's server directory. Obtain explicit owner confirmation of the displayed scope and loss before repeating with `--confirm DIGEST`. A changed scope requires a fresh preview. `spaceterm-preflight` must be selected separately; the task never selects Release.

The selected server identity, Workspace organization, completed outcomes, client identities, attachments, deferred releases and views leave active use. Their exact bytes remain in a private `reset-*` archive. Settings schema 7, serialized Commands, repository files, signed installations, service registration and Stop intent, SSH trust, credentials and signing keys remain outside the reset. A stopped service stays stopped.

After reset, `mise run server:setup-state` recreates Development state without starting anything or reversing Stop. The CLI equivalent is `setup-state IDENTITY` in a client-enabled server build. Only the service's explicit Start can resume a stopped service. Its selected signed runtime must support the new state and protocol; reset never selects an update or weakens artifact verification.

## Optional obsolete credential cleanup

Old local credentials may remain in native storage after reset. They are unused and can stay there indefinitely. Cleanup is separate from local setup and use; an absent, locked or unavailable store is no reason to delay either.

Each obsolete item has service `io.github.sadiksaifi.BUILD.local-server`, where `BUILD` is the exact old application directory identity, and account `desktop` or `desktop-NAME` for a named test client. Select and confirm one exact item before removing it. On macOS, `security delete-generic-password -s SERVICE -a ACCOUNT` removes that generic-password item. On Linux, the old adapter's exact attributes are `application=rust-keyring`, `target=default`, `service=SERVICE`, and `username=ACCOUNT`; `secret-tool clear` accepts those attribute/value pairs. These commands are optional and may need normal OS interaction. Never delete an entire collection, Keychain, SSH credential or signing key.

Local admission removal does not change terminal programs' credential access, signed-artifact verification or operating-system authorization. Direct-network credential storage and SSH Connect/Resume remain with their owning remote issues.
