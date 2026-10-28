# Working with the local server

The desktop and local server use your operating-system account's authority. They do not pair or open a credential store. Independent clients retain nonsecret identities so their views, attachments and Pane control survive relaunch. [ADR 0027](adr/0027-trust-paired-clients-equally.md) owns the trust decision.

Installed Preflight and Release desktop launch connects to the account-local server and presents its retained Workspaces in the main Workspace Window. Closing client windows and quitting the client leave server-owned terminal programs running; [ADR 0049](adr/0049-own-status-controls-in-the-desktop-client.md) defines the client and status-menu lifetime. A deliberately stopped server stays stopped until the person chooses Start. Settings > Servers retains service management. The `server:*` mise tasks select the Development identity unless their help asks for an identity; status never starts a server.

## Running current Development source

Run `mise run development` to build and start the client and a foreground Development Server from the current source, including uncommitted edits. This workflow needs no production signing credentials, signed Server Candidate, prior Server installation or service registration. It uses the current protocol and server-owned Workspaces. [ADR 0030](adr/0030-start-servers-at-boot-independently-of-clients.md) owns this lifetime exception.

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

For Preflight on macOS, use "SpaceTerm Preflight" for both the app and executable names. On Linux, the Preflight executable is `~/.local/lib/spaceterm-preflight/bin/spaceterm`, linked from `~/.local/bin/spaceterm-preflight`. For a custom installation, use the desktop executable inside that app or installation directory. The help names the compiled application identity and lists the service commands and confirmation flag. These service commands run without opening the client or initializing its graphical interface. Headless installations expose the same commands through `bin/spaceterm-server` in their installation directory. Source builds use `mise run server:service --help` for the Development identity.

## Signed server artifacts

For installed applications, the desktop startup adapter in [src/local_startup.rs](../src/local_startup.rs) prepares or reuses the account service on a worker. Existing service policy selects the approved installation. Launch does not select a newer artifact or reverse Stop.

Fresh service setup requires an independently signed server archive and signature for the desktop's application identity and native target. The signed manifest declares its protocol major and capabilities. The client requires protocol major 2 and the `workspace_policy` capability; client and server versions and commits may differ. Packaged candidates carry this pair in their resources. Signing happens during candidate preparation, never during app launch.

The default Development command does not use a Server Candidate. Explicit source clients that exercise installed service startup must supply both `SPACETERM_SERVER_ARCHIVE` and `SPACETERM_SERVER_SIGNATURE` when compiling and launching. Prepare an artifact with `mise run server:package --identity spaceterm-development`, then point those variables at its archive and adjacent `.sig` file. Compilation pins the complete archive SHA256. A client compiled without a pair has no candidate until rebuilt, even if a pair appears at launch. Existing approved service policy needs neither variable. A sibling `spaceterm-server` executable is never an approved retained service runtime.

Packaging prepares the signed server before compiling the desktop. The same variables can select an existing signed pair without recompressing it or matching its version to the client. Package verification compares the embedded archive with the digest reported by the desktop's `--build-info`. A running client refuses a different archive placed at its installation path and asks for a client restart.

When startup has no usable Server state, recovery fills the window beneath its window controls. The Server Settings action remains available there. A missing Server package and a package that fails verification have separate explanations. Usable Workspaces from another connected Server keep the normal Workspace presentation.

Linux first setup can return an authorization requirement before service registration. The caller must obtain explicit scoped linger authorization through the existing service control, then retry startup. Cancellation before service mutation preserves the previous policy; admitted service registration or Start is observed to completion without claiming rollback.

## Add a remote server

Use Settings > Servers > Add Server with SSH Setup or an Invitation Link. [ADR 0029](adr/0029-enroll-remote-servers-through-add-server.md) owns setup authority, consent, completion and cancellation. Both methods save a paired QUIC connection. Configure normal OpenSSH destinations outside SpaceTerm; SSH configuration is used only for setup.

Setup rejects SSH executable or runtime configuration paths containing whitespace or shell metacharacters before starting SSH, because OpenSSH leaves them unquoted in generated hop commands. System SSH `Include` paths must be absolute because `-F` treats included files as user configuration and resolves relative system includes under `~/.ssh`; platform defaults already use absolute paths. User configuration includes retain normal OpenSSH behavior.

Enter an SSH destination to begin setup. The evaluated OpenSSH HostName supplies the QUIC address with UDP port 9231. The optional Network Address disclosure accepts a DNS name or IP address and an optional UDP port. SSH aliases and ProxyJump can reach a machine without making its QUIC endpoint reachable; use the override when the client needs a different address. Network exposure requires approval of the reachable address and the server's listener. Installation and startup do not complete setup until credentials and the connection are saved, SSH resources close and QUIC admission succeeds. If setup fails, review its reported service and pairing effects before retrying.

Once added, select that server in either sidebar creation menu to create a Workspace or browse and select its directory. Terminal work, status, reconnect, relaunch, Retry and pairing cleanup use QUIC under [ADR 0052](adr/0052-carry-paired-connections-over-quic.md). Manage network exposure, invitations, other devices, End All Sessions, the native service and server updates on the server's own machine. Saved addresses are read-only; add the server again to use a different address.

### Reach one server over the LAN and a tailnet

A server listens at one of its addresses, or on all interfaces. All Interfaces binds `0.0.0.0` and `[::]` at the same UDP port and answers each client from the address it dialed. Choose it in Settings > Servers on the server's machine, or run the service command there:

```sh
# Preflight on Linux
"$HOME/.local/bin/spaceterm-preflight" service-network-access spaceterm-preflight all
```

Run it without a value to show where the server listens. Widening from one address to all interfaces at the same port keeps connected devices. Any other change disconnects devices connected over the network; their pairings stay. With all interfaces, add the server once per route, for example once by its LAN address and once by its MagicDNS name. Each route enrolls the address that client reached or its Network Address override. An invitation advertises the address the server's routing table suggests.

## Cross-target artifact catalogs

Standard Development and Preflight commands build signed headless packages for both supported targets from the current working tree, including uncommitted edits. Cargo and Zig build the foreign server locally; they require neither a foreign desktop build nor an SSH builder. The pinned tools and Rust targets live in [.mise.toml](../.mise.toml) and [rust-toolchain.toml](../rust-toolchain.toml).

Catalog preparation refuses source changes during its two builds and keeps separate Cargo outputs under `target/server`. `mise run server:catalog --help` describes standalone preparation. Matching-host packages receive native executable, signature and PTY checks. Foreign packages receive signature, static target, dependency and resource checks; acceptance also requires native verification on their own targets. Release packaging retains native production signing.

Clients read `server-artifacts` in their installation resources. Each target contains `artifact.tar.gz`, raw `artifact.sig` and lowercase `artifact.sha256`. Before offering installation, verification checks signature, identity, target, complete digest, resources and protocol requirements and retains those verified bytes. A missing package and failed verification have distinct results. A usable approved installation requires no replacement catalog. Source clients may select an isolated catalog through `SPACETERM_SERVER_ARTIFACTS`.

## Native setup verification

`mise run server:setup:native:test [test]` selects an ignored test in [the native setup fixture](../src/platform/unix_adapter_tests/server_setup.rs). Use a test binary compiled for Preflight and confirm the disposable account first. Set `SPACETERM_SETUP_NATIVE_DESTINATION`, `SPACETERM_SETUP_NATIVE_IDENTITY=spaceterm-preflight` and `SPACETERM_SETUP_NATIVE_EXPECT` to `missing`, `ready` or `stopped`. Optional `SPACETERM_SETUP_NATIVE_ADDRESS` overrides the derived QUIC address; `SPACETERM_SETUP_NATIVE_EXPECT_ADDRESS` verifies its canonical authority. Missing-server checks need a signed `SPACETERM_SERVER_ARTIFACTS` catalog; the damaged-package test also needs its `SPACETERM_SETUP_NATIVE_TARGET`.

Compile this task with `SPACETERM_PACKAGED=1` and the matching Preflight `SPACETERM_SERVER_ARCHIVE` and `SPACETERM_SERVER_SIGNATURE` pair. Keep those build inputs fixed across invocations so credential creation and cleanup use the same executable.

For default-DNS acceptance, `SPACETERM_SETUP_NATIVE_HOSTNAME_OVERRIDE` prepends a HostName for the selected alias in a private fixture configuration. It includes the existing user configuration and preserves normal host-key verification. Set the independent expected address and leave the address override unset. The fixture removes its private configuration before QUIC work.

The default acceptance test requires a usable native credential store and `SPACETERM_SETUP_NATIVE_EXPOSE=1`. `SPACETERM_SETUP_NATIVE_INSTALL=1` for missing servers or `SPACETERM_SETUP_NATIVE_START=1` for stopped servers permits the corresponding approval inside the same setup attempt. Operating-system authorization remains separate. The fixture owns only its created Workspaces and pairing; approved service installation, startup and exposure are real effects.

Consent, cancellation, credential and save-failure tests have separate function names and preconditions in the fixture. The lost-enrollment-reply test needs a ready server, working native credentials and reachable QUIC. The Linux authorization refusal test needs a missing Linux service with linger disabled, `SPACETERM_SETUP_NATIVE_TARGET=x86_64-unknown-linux-gnu` and `SPACETERM_SETUP_NATIVE_AUTHORIZATION=unavailable`. For the ready-server consent test, choose an endpoint that differs from the active policy or a disabled listener.

## Connections

Each client holds one SpaceTerm Protocol connection per server: the account-local Unix socket, or QUIC for a paired server. Every Pane, edit, inventory read and Settings read of that client shares it. Two uses open a connection of their own: each open directory picker, so the server cancels its reads when the picker closes, and the check for running Terminal Sessions when the client quits. Pings on the live connection judge its health, so a client opens a new connection only after the previous one ends. [ADR 0050](adr/0050-push-terminal-presentation-over-one-multiplexed-connection.md) owns the exchange. A server of another protocol major refuses admission with a typed incompatibility; replace both together.

## Paired connections

Saved QUIC routes and restricted unpair recovery live in `direct-device-connections.json` in private client data, outside the Settings Document. Routes saved over WSS, before QUIC, are dropped when the client loads that file; add those servers again. An unpair request still pending over WSS is dropped with its route and leaves a Servers notice; unpair that device again on the server's machine. Each pairing has a separate native credential. The account server keeps listener policy, trust and pairing grants in `direct.json`. Invitations are never retained. [ADR 0028](adr/0028-use-spaceterm-protocol-over-every-client-connection.md) owns enrollment and [ADR 0052](adr/0052-carry-paired-connections-over-quic.md) the transport; [ADR 0038](adr/0038-unpair-devices-without-blocking-local-connection-removal.md) owns removal and recovery.

`mise run server:direct:build` prepares the two-host verification helper. Its help defines the exact idle Development scope for `server:direct:smoke`; executing the built helper preserves its signature. `server:direct:credentials:native:test` separately verifies and removes two randomly scoped native-store items.

## Update the local server

Settings > Servers owns account-local Update Server. Status controls and local incompatibility recovery open the same journey. Staging verifies the exact pinned artifact before confirmation; Cancel leaves the approved runtime and Terminal Sessions running. Confirmation binds the observed installation, run and Stop policy. A changed observation requires fresh confirmation. Updating ends every client's Terminal Sessions, preserves server identity and organization and respects deliberate Stop.

Finish Update resumes the original admitted local operation. Headless controls expose `service-update`, `service-recover-update` and `service-return-update`; their help explains exact snapshot tokens. Recovery never selects newer bytes, repeats uncertain Stop or Start dispatch or silently returns to a previous runtime. [ADR 0040](adr/0040-keep-one-server-runtime-version-active.md) owns activation.

Update waits for the old server to exit. On Linux, systemd never limits the stop, so SpaceTerm caps the wait at 90 seconds, systemd's stock default stop timeout. On macOS, the wait matches the LaunchAgent's exit timeout. A slower stop leaves the update unfinished. Finish Update or `service-recover-update` completes it after the old server exits, without stopping the server again; until then recovery refuses. An interrupted Stop dispatch may require finishing the original run through the local service manager before Finish Update. Recovery waits for the next desktop launch or local service invocation; no permanent helper runs. Unresolved process cleanup blocks return to a previous runtime.

## Measure terminal latency

Run `mise tasks ls | grep bench:latency` for the latency tasks and `mise run <task> --help` for their options. They measure an installed or running build.

The pixel tasks post keyboard and wheel events and capture the screen with ScreenCaptureKit. The app that runs them, such as Terminal, needs Accessibility and Screen Recording permission in System Settings > Privacy & Security. They need a US keyboard layout and a shell prompt in the focused Pane of the frontmost window. Leave the keyboard, mouse and screen alone until they finish. A pixel task holds an flock on `/tmp/spaceterm-screen.flock`, so a second one waits.

The pixel tasks type commands into the focused Pane and force a steady cursor there. The workload task also splits that Pane to the right and runs `bench:workload` in the new Pane from this checkout's path relative to `~`, on the Pane's host. When a task finishes, or fails while its app is frontmost, it stops those commands, restores the cursor and closes the new Pane. When a failure leaves another app frontmost, the task prints what it left changed.

The workload task reports keystroke-to-pixels p99 with the other Pane idle, then while `bench:workload` draws in it, and the difference. Each miss, an echo not seen within 1 second, counts as 1000 ms in that p99. #670 requires a difference of at most 10 ms. In `scroll` and `partial` modes, the task fails if the workload Pane stops drawing during the measurement.

`bench:latency:protocol` creates a probe Workspace with its own client identity and closes it afterwards. A paired run enrolls a fresh pairing and revokes it afterwards, including after an interrupt; it also withdraws an invitation that no approved claim used. `--owner-host` runs the owner side through SSH on the server's machine, which needs this repository at the same commit. Without it, the server must run on this machine. `--expose ADDRESS` enables that server's network exposure for the run and then restores the previous address and enabled state. When no address was configured before, the probe's address stays configured, disabled. An exposure change other than widening to all interfaces disconnects every paired client, so the task changes nothing when exposure is already enabled at that address. The probe's protocol calls live in [one module](../crates/spaceterm-server-host/examples/latency_probe/protocol.rs).

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

After reset, `mise run server:setup-state` recreates Development state without starting a server or reversing Stop. Its CLI equivalent is `setup-state IDENTITY` in a client-enabled server build. Explicit Start is required for a stopped service, and its approved runtime must support the current state and protocol.
