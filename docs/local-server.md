# Working with the local server

The desktop and local server use your operating-system account's authority. They do not pair or open a credential store. Independent clients retain nonsecret identities so their views, attachments and Pane control survive relaunch. [ADR 0027](adr/0027-trust-paired-clients-equally.md) owns the trust decision.

Installed Preflight and Release desktop launch connects to the account-local server and presents its retained Workspaces in the main Workspace Window. Closing client windows and quitting the client leave server-owned terminal programs running; [ADR 0049](adr/0049-own-status-controls-in-the-desktop-client.md) defines the client and status-menu lifetime. A deliberately stopped server stays stopped until the person chooses Start. Settings > Servers retains service management. The `server:*` mise tasks select the Development identity unless their help asks for an identity; status never starts a server.

## Running current Development source

Run `mise run development` to build and start the client and a foreground Development Server from the current source, including uncommitted edits. This workflow needs no production signing credentials, signed Server Candidate, prior Server installation or service registration. It keeps the existing protocol and server-owned Workspaces. [ADR 0030](adr/0030-start-servers-at-boot-independently-of-clients.md) owns this lifetime exception.

The command owns the run through a private connection to the client and an input pipe to its foreground Server; it does not recover process ownership from saved PIDs. Ctrl+C, termination or complete client exit ends its client, Server and Terminal Sessions and removes its runtime resources. Closing client windows leaves the run available in the status area. Development Settings and Workspace organization remain for the next run; retained Panes start fresh shells on first use. A conflicting Server remains untouched. End its owning run or explicitly manage its installed service before retrying; never delete a socket or signal a saved PID to bypass ownership checks.

Installed Preflight exercises the persistent service lifecycle. `mise run preflight:install:macos` or `mise run preflight:install:linux` builds, verifies and installs it without production signing credentials. Preflight Server Candidates use separate nonproduction trust; Release rejects those signatures. Production release trust and service policy remain separate from the foreground Development workflow.

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

For installed applications, the desktop startup adapter in [src/local_startup.rs](../src/local_startup.rs) prepares or reuses the account service on a worker. Existing service policy selects the approved installation. Launch does not select a newer artifact or reverse Stop.

Fresh service setup requires an independently signed server archive and signature for the desktop's application identity and native target. The signed manifest declares its protocol major and capabilities. The client requires major 1 and `workspace_policy`; client and server versions and commits may differ. Packaged candidates carry this pair in their resources. Signing happens during candidate preparation, never during app launch.

The default Development command does not use a Server Candidate. Explicit source clients that exercise installed service startup must supply both `SPACETERM_SERVER_ARCHIVE` and `SPACETERM_SERVER_SIGNATURE` when compiling and launching. Prepare an artifact with `mise run server:package --identity spaceterm-development`, then point those variables at its archive and adjacent `.sig` file. Compilation pins the complete archive SHA256. A client compiled without a pair has no candidate until rebuilt, even if a pair appears at launch. Existing approved service policy needs neither variable. A sibling `spaceterm-server` executable is never an approved retained service runtime.

Packaging prepares the signed server before compiling the desktop. The same variables can select an existing signed pair without recompressing it or matching its version to the client. Package verification compares the embedded archive with the digest reported by the desktop's `--build-info`. A running client refuses a different archive placed at its installation path and asks for a client restart.

When startup has no usable Server state, recovery fills the window beneath its window controls. The Server Settings action remains available there. A missing Server package and a package that fails verification have separate explanations. Usable Workspaces from another connected Server keep the normal Workspace presentation.

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

Each SSH destination you connect to stays saved in Settings > Servers until you remove it. Saved connections and whether each one is active are retained separately from the shipped Settings Document, together with the original route, account, server and application identity. An inactive saved host causes no SSH discovery. Quiet continuation can reuse an authenticated connection or a frozen direct software-key route whose configuration, keys and trust files still match its admission. That proof lasts for the client process; after relaunch, restored intent waits for Connect. Other fresh SSH authentication waits for Connect/Resume because an agent or credential provider may require consent even with BatchMode. Disconnect ends ordinary continuation intent and keeps the connection saved. A connection whose requested Connect failed keeps its intent, so SpaceTerm tries it again quietly after relaunch; Don't Reconnect ends that intent and keeps it saved. Remove forgets it at once. If SpaceTerm can't save a removal, Settings names the connection, warns that it may return at relaunch, and offers Remove Again. None of these change SSH access, and all leave the server and its Terminal Sessions running.

Check Status signs in to a saved inactive host and reads its account server's state once: running, not running, not installed, stopped or incompatible, with its version and platform. Settings reports the result as of the check and says how long ago it ran. It connects, starts and installs nothing, and it closes its SSH connection when it finishes.

## Direct network connections

Direct routes use the paired server's pinned identity and a separate native credential for each pairing. Saved routes and restricted unpair recovery live in `direct-device-connections.json` in the client's private data, outside Settings schema 7. Invitations are never retained there. The account server keeps its listener policy, trust and pairing grants in `direct.json`; neither file replaces SSH account authority or the operating-system service controls. [ADR 0028](adr/0028-use-spaceterm-protocol-over-every-client-connection.md) owns the transport and trust boundary, and [ADR 0038](adr/0038-unpair-devices-without-blocking-local-connection-removal.md) owns removal and recovery.

`mise run server:direct:build` prepares the native verification helper. Its `--help` describes the exact idle Development scope required by `server:direct:smoke`; executing the built helper avoids rebuilding a signed candidate. `server:direct:credentials:native:test` separately exercises two randomly scoped Development native-store items and removes only those items.

## Updating the approved server

Settings > Servers owns the Update Server journey. The status menu and incompatible-server screen open that same journey when the client includes a verified different candidate. Reading these surfaces does not stage or activate anything. Choosing Update Server stages and fully verifies the exact pinned artifact before asking for confirmation. Cancel leaves the approved runtime and its Terminal Sessions running. Confirmation names both versions and binds the observed approved installation, run and Stop policy. A changed observation requires another confirmation. Updating ends Terminal Sessions for every client, retains server identity and Workspace organization, and preserves deliberate Stop. Retained Panes remain dormant until explicitly selected.

Finish Update resumes the original admitted operation. Headless controls expose the same operation through `service-update`, `service-recover-update` and `service-return-update`; their help explains exact snapshot confirmation tokens. Recovery never chooses a newer artifact, repeats an uncertain Stop or Start dispatch, or silently returns to the previous runtime. Return to Previous Version requires separate confirmation and proof that the candidate never became ready, saved server state is unchanged, and cleanup finished. Return keeps the server stopped; ended Terminal Sessions do not return. [ADR 0040](adr/0040-keep-one-server-runtime-version-active.md) owns these invariants.

Some interrupted operations require intervention. If interruption follows the durable Stop dispatch marker but precedes native disable, a still-running original process blocks recovery. Finish that original run through the operating-system service manager, then use Finish Update; recovery never repeats the kill. If the host stops while native startup is disabled, recovery waits for the next ordinary desktop launch, service control or `service-recover-update` invocation. No permanent helper runs. An uncertain Start dispatch is never automatically resent. Same-boot unresolved run cleanup prevents return; recovery does not discard its evidence to proceed.

## Maintaining an SSH account server

Settings > Servers and each machine heading in the sidebar offer Start, Stop, Restart, Uninstall and Update Server for the selected SSH server. They use that account's own authority over the route that admitted its connection, with no SpaceTerm paired device and no helper to install. Stop, Restart, Uninstall and Update name the machine and say that every client's Terminal Sessions on that server end. Only explicit Start reverses a deliberate Stop; Connect and Check Status still start nothing.

Each operation runs the approved installation's `ssh-maintain` command, which reads one bounded request and writes one bounded reply. Nothing the request asks for becomes a shell argument. The service describes a destructive action against one exact observation and issues the confirmation for it; performing the action recomputes that observation and refuses when it differs, so a Stop confirmed while the server was running cannot stop work started afterwards and an update confirmed for one build cannot replace a later approved one. Remote staging transfers the verified artifact for that machine's target through `ssh-stage`, which retains it beside the approved installation and installs, activates and starts nothing. The artifacts come from the same `SPACETERM_SSH_SERVER_ARTIFACTS` directory that first installation uses.

The service retains each submitted operation under the identity the client bound before dispatch, in a private `maintenance.json` beside its service policy. Resubmitting that identity returns the retained outcome instead of dispatching again. The client retains the matching record in `ssh-remote-operations.json` in its own private data, with the original route, account, application identity, server, client and operation identities.

Those records are what makes an interrupted operation resolvable. Connect and Resume prepare the bounded route a resolution needs; there is no separate preparation step and no permanent listener. After relaunch, SpaceTerm resolves unresolved originals quietly using authority that is already available: it asks for no password, host approval or credential-provider consent, and an unavailable authority leaves the outcome unconfirmed rather than repeating it. Choosing Resume on the connection asks with the authority the person just granted. Resolution reads and acknowledges; it never repeats a dispatch, renews an approval, stages or installs anything, or starts a stopped server.

A validated result stays until the person acknowledges it. Removing the saved connection ends presentation and reconnect intent at once and keeps that record, which the Servers section still presents with only its resolution and acknowledgement. Removal never recreates the connection, and a connection saved again under the same destination inherits neither the record nor its late results.

`mise run server:maintenance:smoke` exercises these operations against disposable Development service state. Three native checks reach a real host, each selecting it through `SPACETERM_SSH_NATIVE_DESTINATION` with `SPACETERM_SSH_NATIVE_EXPECT=ready`: `server:ssh:maintenance:native:test` only observes, while `server:ssh:maintenance:native:originals` and `server:ssh:maintenance:native:recovery` stop, start and update that account's server and need their own `SPACETERM_SSH_NATIVE_MAINTAIN=1`. The update step needs `SPACETERM_SSH_SERVER_ARTIFACTS` to hold a verified candidate that differs from the account's approved build. The recovery check finishes an original that asked for Stop, so it leaves that server stopped; start it again explicitly afterwards.

## Replacing incompatible development state

Unreleased local state is replaced directly. Quit the desktops and finish server shutdown before a reset. Resolve pending client operations and reconcile started Git writes with the build that admitted them first. A retained endpoint, active instance/write lock or unresolved service cleanup prevents reset. Never signal a process from a saved PID or remove a socket to bypass that refusal.

Run `mise run server:reset --identity spaceterm-development` to preview the exact selected account paths and digest. The task selects only `identity.json`, `direct.json` and `clients` beneath that account/build's server directory. Obtain explicit owner confirmation of the displayed scope and loss before repeating with `--confirm DIGEST`. A changed scope requires a fresh preview. `spaceterm-preflight` must be selected separately; the task never selects Release.

The selected server identity, Workspace organization, direct server trust and paired-device grants, completed outcomes, client identities, attachments, deferred releases and views leave active use. Their exact bytes remain in a private `reset-*` archive. Settings schema 7, serialized Commands, repository files, signed installations, service registration and Stop intent, SSH trust, native client credentials and signing keys remain outside the reset. A stopped service stays stopped.

After reset, `mise run server:setup-state` recreates Development state without starting anything or reversing Stop. The CLI equivalent is `setup-state IDENTITY` in a client-enabled server build. Only the service's explicit Start can resume a stopped service. Its selected signed runtime must support the new state and protocol; reset never selects an update or weakens artifact verification.

## Optional obsolete credential cleanup

Old local credentials may remain in native storage after reset. They are unused and can stay there indefinitely. Cleanup is separate from local setup and use; an absent, locked or unavailable store is no reason to delay either.

Each obsolete item has service `io.github.sadiksaifi.BUILD.local-server`, where `BUILD` is the exact old application directory identity, and account `desktop` or `desktop-NAME` for a named test client. Select and confirm one exact item before removing it. On macOS, `security delete-generic-password -s SERVICE -a ACCOUNT` removes that generic-password item. On Linux, the old adapter's exact attributes are `application=rust-keyring`, `target=default`, `service=SERVICE`, and `username=ACCOUNT`; `secret-tool clear` accepts those attribute/value pairs. These commands are optional and may need normal OS interaction. Never delete an entire collection, Keychain, SSH credential or signing key.

Local admission removal does not change terminal programs' credential access, signed-artifact verification or operating-system authorization. Direct-network credentials and SSH Connect/Resume retain their separate authority.
