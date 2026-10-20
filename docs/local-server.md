# Working with the local server

The desktop and local server use your operating-system account's authority. They do not pair or open a credential store. Independent clients retain nonsecret identities so their views, attachments and Pane control survive relaunch. [ADR 0027](adr/0027-trust-paired-clients-equally.md) owns the trust decision.

Normal desktop launch connects to the account-local server and presents its retained Workspaces in the main Workspace Window. Closing client windows and quitting the client leave server-owned terminal programs running; [ADR 0049](adr/0049-own-status-controls-in-the-desktop-client.md) defines the client and status-menu lifetime. A deliberately stopped server stays stopped until the person chooses Start. Settings > Servers retains service management. The `server:*` mise tasks exercise the Development identity; status never starts a server.

## Controls without the desktop

The desktop installation includes a CLI fallback for use without a status area or graphical login. For a standard installation, open its command help with:

```sh
# macOS
"/Applications/SpaceTerm.app/Contents/MacOS/SpaceTerm" --help

# Linux
"$HOME/.local/lib/spaceterm/bin/spaceterm" --help
```

For Preflight, use "SpaceTerm Preflight" for both the macOS app and executable names, or `spaceterm-preflight` in the Linux directory. For a custom installation, use the desktop executable inside that app or installation directory. The help names the compiled application identity and lists the service commands and confirmation flag. These service commands run without opening the client or initializing its graphical interface. Headless installations expose the same commands through `bin/spaceterm-server` in their installation directory. Source builds use `mise run server:service --help` for the Development identity.

## Server artifacts for desktop startup

The desktop startup adapter in [src/local_startup.rs](../src/local_startup.rs) prepares or reuses the account service on a worker. Existing service policy selects the approved installation. Launch does not select a newer artifact or reverse Stop.

Fresh service setup requires an independently signed server archive and signature for the desktop's application identity and native target. The signed manifest declares its protocol major and capabilities. The client requires major 1 and `workspace_policy`; client and server versions and commits may differ. Packaged candidates carry this pair in their resources. Signing happens during candidate preparation, never during app launch.

Source builds must supply both `SPACETERM_SERVER_ARCHIVE` and `SPACETERM_SERVER_SIGNATURE` when compiling and launching a client that offers a server candidate. Prepare an artifact with `mise run server:package --identity spaceterm-development` using the existing signing inputs, then point those variables at its archive and adjacent `.sig` file. Compilation pins the complete archive SHA256. A client compiled without a pair has no candidate until rebuilt, even if a pair appears at launch. Existing approved service policy needs neither variable. A sibling `spaceterm-server` executable is never an approved retained service runtime.

Packaging prepares the signed server before compiling the desktop. The same variables can select an existing signed pair without recompressing it or matching its version to the client. Package verification compares the embedded archive with the digest reported by the desktop's `--build-info`. A running client refuses a different archive placed at its installation path and asks for a client restart.

Linux first setup can return an authorization requirement before service registration. The caller must obtain explicit scoped linger authorization through the existing service control, then retry startup. Cancellation before service mutation preserves the previous policy; admitted service registration or Start is observed to completion without claiming rollback.

## SSH account servers

Settings > Servers connects a selected SSH destination through normal OpenSSH host verification and account authentication. Connect discovers the account's approved installation before looking for an installation candidate. The SSH bridge uses the same account server as a desktop on that machine. It neither pairs a device nor creates a client signing secret. [ADR 0029](adr/0029-pair-clients-automatically-during-ssh-bootstrap.md) owns route authority and continuation.

Development clients offering first installation on another machine need an artifact for that machine's target. Set `SPACETERM_SSH_SERVER_ARTIFACTS` to a directory containing these files under each supported target, `aarch64-apple-darwin` or `x86_64-unknown-linux-gnu`:

```text
TARGET/artifact.tar.gz
TARGET/artifact.sig
TARGET/artifact.sha256
```

The archive and raw signature come from `mise run server:package --identity spaceterm-development` on the target platform. `artifact.sha256` contains only the archive's lowercase SHA256 digest. The client verifies the signature, application identity, target, complete digest, resources and declared compatibility before offering that candidate. It retains the verified bytes for the installation offer. Packaged clients read the equivalent `server-artifacts` directory in their resources. A missing suitable candidate prevents first installation while leaving existing installations usable.

Each SSH destination you connect to stays saved in Settings > Servers until you remove it. Saved connections and whether each one is active are retained separately from the shipped Settings Document, together with the original route, account, server and application identity. An inactive saved host causes no SSH discovery. Quiet continuation can reuse an authenticated connection or a frozen direct software-key route whose configuration, keys and trust files still match its admission. That proof lasts for the client process; after relaunch, restored intent waits for Connect. Other fresh SSH authentication waits for Connect/Resume because an agent or credential provider may require consent even with BatchMode. Disconnect ends ordinary continuation intent and keeps the connection saved. Remove forgets it at once. Neither changes SSH access, and both leave the server and its Terminal Sessions running.

Check Status signs in to a saved inactive host and reads its account server's state once: running, not running, not installed, stopped or incompatible, with its version and platform. It connects, starts and installs nothing, and it closes its SSH connection when it finishes.

## Updating the approved server

Settings > Servers owns the Update Server journey. The status menu and incompatible-server screen open that same journey when the client includes a verified different candidate. Reading these surfaces does not stage or activate anything. Choosing Update Server stages and fully verifies the exact pinned artifact before asking for confirmation. Cancel leaves the approved runtime and its Terminal Sessions running. Confirmation names both versions and binds the observed approved installation, run and Stop policy. A changed observation requires another confirmation. Updating ends Terminal Sessions for every client, retains server identity and Workspace organization, and preserves deliberate Stop. Retained Panes remain dormant until explicitly selected.

Finish Update resumes the original admitted operation. Headless controls expose the same operation through `service-update`, `service-recover-update` and `service-return-update`; their help explains exact snapshot confirmation tokens. Recovery never chooses a newer artifact, repeats an uncertain Stop or Start dispatch, or silently returns to the previous runtime. Return to Previous Version requires separate confirmation and proof that the candidate never became ready, saved server state is unchanged, and cleanup finished. Return keeps the server stopped; ended Terminal Sessions do not return. [ADR 0040](adr/0040-keep-one-server-runtime-version-active.md) owns these invariants.

Some interrupted operations require intervention. If interruption follows the durable Stop dispatch marker but precedes native disable, a still-running original process blocks recovery. Finish that original run through the operating-system service manager, then use Finish Update; recovery never repeats the kill. If the host stops while native startup is disabled, recovery waits for the next ordinary desktop launch, service control or `service-recover-update` invocation. No permanent helper runs. An uncertain Start dispatch is never automatically resent. Same-boot unresolved run cleanup prevents return; recovery does not discard its evidence to proceed.

## Replacing incompatible development state

Unreleased local state is replaced directly. Quit the desktops and finish server shutdown before a reset. Resolve pending client operations and reconcile started Git writes with the build that admitted them first. A retained endpoint, active instance/write lock or unresolved service cleanup prevents reset. Never signal a process from a saved PID or remove a socket to bypass that refusal.

Run `mise run server:reset --identity spaceterm-development` to preview the exact selected account paths and digest. The task selects only `identity.json` and `clients` beneath that account/build's server directory. Obtain explicit owner confirmation of the displayed scope and loss before repeating with `--confirm DIGEST`. A changed scope requires a fresh preview. `spaceterm-preflight` must be selected separately; the task never selects Release.

The selected server identity, Workspace organization, completed outcomes, client identities, attachments, deferred releases and views leave active use. Their exact bytes remain in a private `reset-*` archive. Settings schema 7, serialized Commands, repository files, signed installations, service registration and Stop intent, SSH trust, credentials and signing keys remain outside the reset. A stopped service stays stopped.

After reset, `mise run server:setup-state` recreates Development state without starting anything or reversing Stop. The CLI equivalent is `setup-state IDENTITY` in a client-enabled server build. Only the service's explicit Start can resume a stopped service. Its selected signed runtime must support the new state and protocol; reset never selects an update or weakens artifact verification.

## Optional obsolete credential cleanup

Old local credentials may remain in native storage after reset. They are unused and can stay there indefinitely. Cleanup is separate from local setup and use; an absent, locked or unavailable store is no reason to delay either.

Each obsolete item has service `io.github.sadiksaifi.BUILD.local-server`, where `BUILD` is the exact old application directory identity, and account `desktop` or `desktop-NAME` for a named test client. Select and confirm one exact item before removing it. On macOS, `security delete-generic-password -s SERVICE -a ACCOUNT` removes that generic-password item. On Linux, the old adapter's exact attributes are `application=rust-keyring`, `target=default`, `service=SERVICE`, and `username=ACCOUNT`; `secret-tool clear` accepts those attribute/value pairs. These commands are optional and may need normal OS interaction. Never delete an entire collection, Keychain, SSH credential or signing key.

Local admission removal does not change terminal programs' credential access, signed-artifact verification or operating-system authorization. Direct-network credential storage and SSH Connect/Resume remain with their owning remote issues.
