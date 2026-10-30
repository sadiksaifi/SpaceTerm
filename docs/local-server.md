# Working with the local server

The desktop and local server use your operating-system account's authority. They do not pair or open a credential store. Independent clients retain nonsecret identities so their views, attachments and Pane control survive relaunch. [ADR 0027](adr/0027-trust-paired-clients-equally.md) owns the trust decision.

Installed Preflight and Release desktop launch connects to the account-local server and presents its retained Workspaces in the main Workspace Window. Closing client windows and quitting the client leave server-owned terminal programs running; [ADR 0049](adr/0049-own-status-controls-in-the-desktop-client.md) defines the client and status-menu lifetime. A deliberately stopped server stays stopped until the person chooses Start. Settings > Servers retains service management. The `server:*` mise tasks select the Development identity unless their help asks for an identity; status never starts a server.

## Running current Development source

Run `mise run development` to build and start the client and a foreground Development Server from the current source, including uncommitted edits. This workflow needs no production signing credentials, signed Server Candidate, prior Server installation or service registration. It uses the current protocol and server-owned Workspaces. [ADR 0030](adr/0030-start-servers-at-boot-independently-of-clients.md) owns this lifetime exception.

The command owns the run through a private connection to the client and an input pipe to its foreground Server; it does not recover process ownership from saved PIDs. Ctrl+C, termination or complete client exit ends its client, Server and Terminal Sessions and removes its runtime resources. Closing client windows leaves the run available in the status area. Development Settings and Workspace organization remain for the next run; retained Panes start fresh shells on first use. A conflicting Server remains untouched. End its owning run or explicitly manage its installed service before retrying; never delete a socket or signal a saved PID to bypass ownership checks.

Installed Preflight exercises the persistent service lifecycle. `mise run preflight:install:macos` or `mise run preflight:install:linux` builds, verifies and installs it without production signing credentials. Preflight Server Candidates use separate nonproduction trust; Release rejects those signatures. Production release trust and service policy remain separate from the foreground Development workflow.

## Controls without the desktop

Install the headless CLI as described in [Install the command line](#install-the-command-line). Run `spaceterm server --help` for service controls. Development and Preflight use their own launcher names under [ADR 0012](adr/0012-derive-one-application-identity-from-the-build.md). The desktop executable has no service-command parser. macOS service setup requires a logged-in Aqua session. Linux service setup requires linger authorization.

`mise run server:service status` builds the source CLI and reads the Development service. The task also accepts `setup`, `start`, `stop`, `restart` and `update`. Setup accepts `--network` and `--automatic-start`; a destructive preview supplies `--confirm TOKEN` for the repeated action. These commands administer installed service state separately from the foreground `development` run. [.mise.toml](../.mise.toml) owns the task's arguments; [the CLI command tree](../crates/spaceterm-cli/src/args.rs) owns the `spaceterm server …` grammar.

## Signed server artifacts

For installed applications, the desktop startup adapter in [src/local_startup.rs](../src/local_startup.rs) prepares or reuses the account service on a worker. Existing service policy selects the approved installation. Launch does not select a newer artifact or reverse Stop.

Fresh service setup requires an independently signed server archive and signature for the desktop's application identity and native target. The signed manifest declares its protocol major and capabilities. The client requires protocol major 4 and the `workspace_policy` capability; client and server versions and commits may differ. Packaged candidates carry this pair in their resources. Signing happens during candidate preparation, never during app launch.

The default Development command does not use a Server Candidate. Explicit source clients that exercise installed service startup must supply both `SPACETERM_SERVER_ARCHIVE` and `SPACETERM_SERVER_SIGNATURE` when compiling and launching. Prepare an artifact with `mise run server:package --identity spaceterm-development`, then point those variables at its archive and adjacent `.sig` file. Compilation pins the complete archive SHA256. A client compiled without a pair has no candidate until rebuilt, even if a pair appears at launch. Existing approved service policy needs neither variable. A sibling `spaceterm` executable is never an approved retained service runtime.

Packaging prepares the signed server before compiling the desktop. The same variables can select an existing signed pair without recompressing it or matching its version to the client. Package verification compares the embedded archive with the digest reported by the desktop's `--build-info`. A running client refuses a different archive placed at its installation path and asks for a client restart.

When startup has no usable Server state, recovery fills the window beneath its window controls. The Server Settings action remains available there. A missing Server package and a package that fails verification have separate explanations. Usable Workspaces from another connected Server keep the normal Workspace presentation.

Linux first setup can return an authorization requirement before service registration. The caller must obtain explicit scoped linger authorization through the existing service control, then retry startup. Cancellation before service mutation preserves the previous policy; admitted service registration or Start is observed to completion without claiming rollback.

To end the local server's Terminal Sessions while keeping it running, preview the action and copy its exact token:

```sh
spaceterm --server local server end-all-sessions
spaceterm --server local server end-all-sessions --confirm TOKEN
```

Use `--json` to read the preview from `data.confirmation`, including `token`, `expires_at` and `effects.sessions`. A preview exits 3; confirmation succeeds with exit 0 and reports `data.ended`. A wrong, expired or stale token reports `changed` and ends nothing. A paired server selector reports `local_only` and exits 5. [ADR 0048](adr/0048-end-all-sessions-from-the-selected-servers-settings.md) owns the action and its confirmation scope.

## Add a remote server

Use Settings > Servers > Add Server with SSH Setup or an Invitation Link. [ADR 0029](adr/0029-enroll-remote-servers-through-add-server.md) owns setup authority, consent, completion and cancellation. Both methods save a paired QUIC connection. Configure normal OpenSSH destinations outside SpaceTerm; SSH configuration is used only for setup.

Setup rejects SSH executable or runtime configuration paths containing whitespace or shell metacharacters before starting SSH, because OpenSSH leaves them unquoted in generated hop commands. System SSH `Include` paths must be absolute because `-F` treats included files as user configuration and resolves relative system includes under `~/.ssh`; platform defaults already use absolute paths. User configuration includes retain normal OpenSSH behavior.

Enter an SSH destination to begin setup. The evaluated OpenSSH HostName supplies the QUIC address with UDP port 9231. The optional Network Address disclosure accepts a DNS name or IP address and an optional UDP port. SSH aliases and ProxyJump can reach a machine without making its QUIC endpoint reachable; use the override when the client needs a different address. Network exposure requires approval of the reachable address and the server's listener. Installation and startup do not complete setup until credentials and the connection are saved, SSH resources close and QUIC admission succeeds. If setup fails, review its reported service and pairing effects before retrying.

Once added, select that server in either sidebar creation menu to create a Workspace or browse and select its directory. Terminal work, status, reconnect, relaunch, Try Again and pairing cleanup use QUIC under [ADR 0052](adr/0052-carry-paired-connections-over-quic.md). The CLI can Stop, Restart and Update a running paired server through [the service lane](adr/0057-administer-servers-over-the-service-lane.md). Network exposure, invitations, unpairing other devices, End All Sessions, Start and setup stay on the server's own machine. Saved addresses are read-only; add the server again to use a different address. [ADR 0056](adr/0056-install-and-pair-servers-from-their-own-command-line.md) describes CLI enrollment and its address override.

### Reach one server over the LAN and a tailnet

A server listens at one of its addresses, or on all interfaces. All Interfaces binds `0.0.0.0` and `[::]` at the same UDP port and answers each client from the address it dialed. It listens on every network the machine is on, including public networks and any network it joins later. Only paired devices are admitted. Choose it in Settings > Servers on the server's machine, or run the service command there:

```sh
# Preflight on Linux
"$HOME/.local/bin/spaceterm-preflight" server setup --network all
```

Run `spaceterm-preflight server status` to show where the server listens. `server setup --network` also takes `off`, an IP address, `IP:PORT`, or `[IPV6]:PORT`, and refuses an address the machine doesn't have. Widening from one address to all interfaces at the same port keeps connected devices. Any other change disconnects devices connected over the network; their pairings stay.

With all interfaces, add the server once per route, for example once by its LAN address and once by its MagicDNS name. SSH setup enrolls the address that client reached or its Network Address override, so it reaches the server over any route SSH reaches. An invitation link carries one address. Invite Device offers the server's addresses and starts with the one its routing table suggests; without that suggestion the server picks its first address that no container or virtual machine bridge holds. A device on a network the link's address doesn't reach can't use the link, and a saved address can't change, so add that device with SSH setup and the route's name instead.

## Install the command line

The desktop executable is `spaceterm-desktop` on Linux. The headless `spaceterm` executable runs the server and controls it. A headless installation needs no desktop, graphics driver or credential store to run its local server commands.

```sh
curl -fsSL https://github.com/sadiksaifi/SpaceTerm/releases/latest/download/install.sh | sh -s -- --headless
spaceterm server setup --network off
```

For a local Development package, run the installer with `--headless --identity spaceterm-development --archive FILE --signature FILE.sig`. Preflight uses `--identity spaceterm-preflight` in the same form. Retention verifies the signed package and creates `~/.local/bin/<identity>`. It does not register or start a service. On a computer with the desktop, Settings > Servers > Command Line Tool creates the same launcher. [ADR 0054](adr/0054-use-one-headless-executable.md) owns launcher selection and replacement.

```sh
curl -fsSL https://github.com/sadiksaifi/SpaceTerm/releases/latest/download/install.sh | sh -s -- --headless --uninstall
```

Uninstall previews any Terminal Sessions it would end. Workspaces, paired devices and retained signed installations stay on disk. A noninteractive uninstall returns the preview token for `--confirm TOKEN`. [ADR 0054](adr/0054-use-one-headless-executable.md) owns the installer's service and launcher removal.

[Signing] Developer ID signing, hardened runtime, notarization and the installer's Developer ID requirement check are future work. Ed25519 authenticates the package and feed independently.

## Cross-target artifact catalogs

Standard Development and Preflight commands prepare signed headless packages for both supported targets from the current working tree, including uncommitted edits. macOS builds natively on a Mac so the CLI can use Security.framework. Cargo and Zig cross-build Linux locally. A Linux host needs `SPACETERM_SERVER_ARTIFACTS` pointing at a Mac-built catalog. Its macOS artifact must carry the same signed source digest as the Linux checkout. The pinned tools and Rust targets live in [.mise.toml](../.mise.toml) and [rust-toolchain.toml](../rust-toolchain.toml).

Catalog preparation refuses source changes during preparation and keeps separate Cargo outputs under `$CARGO_TARGET_DIR/server`, or `target/server` when no target directory is set. `mise run server:catalog --help` describes standalone preparation. Matching-host packages receive native executable, signature and PTY checks. Foreign packages receive signature, static target, dependency and resource checks; acceptance also requires native verification on their own targets. Release packaging retains native production signing.

Clients read `server-artifacts` in their installation resources. Each target contains `artifact.tar.gz`, raw `artifact.sig` and lowercase `artifact.sha256`. Before offering installation, verification checks signature, identity, target, complete digest, resources and protocol requirements and retains those verified bytes. A missing package and failed verification have distinct results. A usable approved installation requires no replacement catalog. Source clients may select an isolated catalog through `SPACETERM_SERVER_ARTIFACTS`.

## Native setup verification

`mise run server:setup:native:test [test]` selects an ignored test in [the native setup fixture](../src/platform/unix_adapter_tests/server_setup.rs). Use a test binary compiled for Preflight and confirm the disposable account first. Set `SPACETERM_SETUP_NATIVE_DESTINATION`, `SPACETERM_SETUP_NATIVE_IDENTITY=spaceterm-preflight` and `SPACETERM_SETUP_NATIVE_EXPECT` to `missing`, `ready` or `stopped`. Optional `SPACETERM_SETUP_NATIVE_ADDRESS` overrides the derived QUIC address; `SPACETERM_SETUP_NATIVE_EXPECT_ADDRESS` verifies its canonical authority. Missing-server checks need a signed `SPACETERM_SERVER_ARTIFACTS` catalog; the damaged-package test also needs its `SPACETERM_SETUP_NATIVE_TARGET`.

Compile this task with `SPACETERM_PACKAGED=1` and the matching Preflight `SPACETERM_SERVER_ARCHIVE` and `SPACETERM_SERVER_SIGNATURE` pair. Keep those build inputs fixed across invocations so credential creation and cleanup use the same executable.

For default-DNS acceptance, `SPACETERM_SETUP_NATIVE_HOSTNAME_OVERRIDE` prepends a HostName for the selected alias in a private fixture configuration. It includes the existing user configuration and preserves normal host-key verification. Set the independent expected address and leave the address override unset. The fixture removes its private configuration before QUIC work.

The default acceptance test requires a usable native credential store and `SPACETERM_SETUP_NATIVE_EXPOSE=1`. `SPACETERM_SETUP_NATIVE_INSTALL=1` for missing servers or `SPACETERM_SETUP_NATIVE_START=1` for stopped servers permits the corresponding approval inside the same setup attempt. Operating-system authorization remains separate. The fixture owns only its created Workspaces and pairing; approved service installation, startup and exposure are real effects.

Consent, cancellation, credential and save-failure tests have separate function names and preconditions in the fixture. The lost-enrollment-reply test needs a ready server, working native credentials and reachable QUIC. The Linux authorization refusal test needs a missing Linux service with linger disabled, `SPACETERM_SETUP_NATIVE_TARGET=x86_64-unknown-linux-gnu` and `SPACETERM_SETUP_NATIVE_AUTHORIZATION=unavailable`. For the ready-server consent test, choose an endpoint that differs from the active policy or a disabled listener.

## Connections

Each desktop client holds one work connection per server: the account-local Unix socket, or QUIC for a paired server. Every Pane, edit, inventory read and Settings read of that client shares it. Two uses open a connection of their own: each open directory picker, so the server cancels its reads when the picker closes, and the check for running Terminal Sessions when the client quits. Pings on the live connection judge its health, so a client opens a new connection only after the previous one ends. [ADR 0050](adr/0050-push-terminal-presentation-over-one-multiplexed-connection.md) owns the exchange. A different work major refuses work admission; [the service lane](adr/0057-administer-servers-over-the-service-lane.md) remains available for CLI administration across that mismatch.

## Paired connections

Desktop Saved Connections and restricted unpair recovery live in `direct-device-connections.json` in private client data, outside the Settings Document. [ADR 0058](adr/0058-derive-agent-identities-at-admission.md) owns the CLI's separate connection and journal layout. The account server keeps listener policy, trust and pairing grants in `direct.json`. Invitations are never retained. [ADR 0028](adr/0028-use-spaceterm-protocol-over-every-client-connection.md) owns enrollment and [ADR 0052](adr/0052-carry-paired-connections-over-quic.md) the transport; [ADR 0038](adr/0038-unpair-devices-without-blocking-local-connection-removal.md) owns removal and recovery.

[ADR 0004](adr/0004-resolve-application-directories-by-xdg-convention.md) owns native credential storage, the Preflight Keychain policy and CLI failures when access is unavailable. In the desktop, the first connection attempt after launch can request Keychain approval. Later automatic retries do not prompt; Resume explicitly requests access again.

`mise run server:direct:build` prepares the two-host verification helper. Its help defines the exact idle Development scope for `server:direct:smoke`; executing the built helper preserves its signature. `server:direct:credentials:native:test` separately verifies and removes two randomly scoped native-store items. `mise run test:native:macos` verifies the Preflight store in a throwaway Keychain.

## Update the local server

Settings > Servers owns account-local Update Server. Status controls and local incompatibility recovery open the same journey. Staging verifies the exact pinned artifact before confirmation; Cancel leaves the approved runtime and Terminal Sessions running. Confirmation binds the observed installation, run and Stop policy. A changed observation requires fresh confirmation. Updating ends every client's Terminal Sessions, preserves server identity and organization and respects deliberate Stop.

Finish Update resumes the original admitted local operation. `spaceterm server update` settles interrupted activation before it selects a new candidate. It previews a return to the previous runtime when that is the available recovery action. Recovery never selects newer bytes, repeats uncertain Stop or Start dispatch or silently returns to a previous runtime. [ADR 0040](adr/0040-keep-one-server-runtime-version-active.md) owns activation.

Update waits for the old server to exit. On Linux, systemd never limits the stop, so SpaceTerm caps the wait at 90 seconds, systemd's stock default stop timeout. On macOS, the wait matches the LaunchAgent's exit timeout. A slower stop leaves the update unfinished. Finish Update or `spaceterm server update` completes it after the old server exits, without stopping the server again; until then recovery refuses. An interrupted Stop dispatch may require finishing the original run through the local service manager before Finish Update. Recovery waits for the next desktop launch or local service invocation; no permanent helper runs. Unresolved process cleanup blocks return to a previous runtime.

## Measure terminal latency

Run `mise tasks ls | grep bench:latency` for the latency tasks and `mise run <task> --help` for their options. They measure an installed or running build.

The pixel tasks post keyboard and wheel events and capture the screen with ScreenCaptureKit. The app that runs them, such as Terminal, needs Accessibility and Screen Recording permission in System Settings > Privacy & Security. They need a US keyboard layout and a shell prompt in the focused Pane of the frontmost window. Leave the keyboard, mouse and screen alone until they finish. A pixel task holds an flock on `/tmp/spaceterm-screen.flock`, so a second one waits.

The pixel tasks type commands into the focused Pane and force a steady cursor there. The workload task also splits that Pane to the right and runs `bench:workload` in the new Pane from this checkout's path relative to `~`, on the Pane's host. When a task finishes, or fails while its app is frontmost, it stops those commands, restores the cursor and closes the new Pane. When a failure leaves another app frontmost, the task prints what it left changed.

The workload task reports keystroke-to-pixels p99 with the other Pane idle, then while `bench:workload` draws in it, and the difference. Each miss, an echo not seen within 1 second, counts as 1000 ms in that p99. #670 requires a difference of at most 10 ms. In `scroll` and `partial` modes, the task fails if the workload Pane stops drawing during the measurement.

`bench:latency:protocol` creates a probe Workspace with its own client identity and closes it afterwards. A paired run enrolls a fresh pairing and revokes it afterwards, including after an interrupt; it also withdraws an invitation that no approved claim used. `--owner-host` runs the owner side through SSH on the server's machine, which needs this repository at the same commit. Without it, the server must run on this machine. `--expose ADDRESS` enables that server's network exposure for the run and then restores the previous address and enabled state. When no address was configured before, the probe's address stays configured, disabled. An exposure change other than widening to all interfaces disconnects every paired client, so the task changes nothing when exposure is already enabled at that address. The probe's protocol calls live in [one module](../crates/spaceterm-cli/examples/latency_probe/protocol.rs).

`bench:latency:quic` measures a paired request's round trip over QUIC on loopback, in a release build, without a desktop or installed server.

### Loss test

`bench:latency:netem` measures a paired connection to a foreground Development server on a Linux owner host, first clean and then with netem loss and delay. Besides key-to-echo and request round trip, it times a typed screenful until its last row arrives and 200-row Scrollback page reads. The impairment applies only to the owner host's traffic to this machine, so only the server's direction is impaired. The owner host needs passwordless sudo for `tc`. The task removes its qdisc after every impaired cell, also after a failure or an interrupt, and refuses to start while the device already has a `prio` or `netem` qdisc. If a run is killed outright, remove the qdisc by hand with `sudo tc qdisc del dev DEVICE root`.

For example, from the Mac against Penguin:

```sh
mise run bench:latency:netem --owner-host penguin --jail /var/tmp/spaceterm-netem \
  --expose 192.168.1.206:9341 --client-address "$(ipconfig getifaddr en8)" --interface en8 \
  --rounds 2 --output .snap/loss-test
```

`--client-address` must be an address of this machine. `--expose` uses a port of its own, so an installed server keeps its listener. The owner host builds the server and probe from the same checkout path; `--owner-target` selects its Cargo target directory. `--jail` gives the server private, disposable state through bwrap, so a Development server already running there is left alone. Defaults are 2% loss, 25 ms delay and 300 samples of each measurement per cell. `--output` receives raw results and a percentile table.

The idle task reads per-flow UDP byte counters with `nettop`, so it sees paired QUIC connections but not account-local IPC.

On macOS, remote tasks accept `--interface` with a remote host, and the pixel, idle and protocol tasks take that host as `--route-host`. A task given them refuses to run unless the route to the host uses that interface, and every established TCP connection and connected UDP socket to the host uses an address on it. A paired connection's QUIC socket is a connected UDP socket, so the guard also refuses an app whose socket stayed on another interface. Use them to keep a wired batch on Ethernet.

## Replace incompatible development state

Obsolete server-client formats are replaced directly under [ADR 0035](adr/0035-negotiate-one-protocol-major.md). Quit desktops and finish server shutdown before resetting local state. Complete or reconcile started operations and Git writes first. A retained endpoint, active lock or unresolved service cleanup refuses reset. Never remove a socket or signal a saved PID to bypass ownership checks.

Run `mise run server:reset --identity spaceterm-development` to preview exact paths and a digest. It selects `identity.json`, `direct.json` and `clients` beneath that account/build's server directory. Obtain explicit owner confirmation of the displayed scope and loss before repeating with `--confirm DIGEST`. A changed scope needs a fresh preview. Select Preflight separately; the task never selects Release.

The selected identity, organization, pairing grants, outcomes, attachments and views leave active use; exact bytes remain in a private `reset-*` archive. Settings, Commands, repositories, signed installations, service registration, Stop intent, SSH trust, native credentials and signing keys are outside that task's scope.

After reset, `spaceterm-development server setup --network off` recreates Development state using the installed CLI. It preserves deliberate Stop. Explicit `spaceterm-development server start` is required for a stopped service, and its approved runtime must support the current state and protocol.
