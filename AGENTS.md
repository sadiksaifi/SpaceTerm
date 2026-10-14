# SpaceTerm

A native desktop terminal multiplexer for macOS and Linux, coming soon for Windows.

## Sources of truth

- Use the terms defined in [CONTEXT.md](CONTEXT.md).
- Read the relevant [ADRs](docs/adr/) before changing architecture or authentication. For issue #520, use the implemented account-service ownership below when older ADRs describe root custody or a system daemon.
- Write docs only for what code, configuration, or the UI cannot express. State each fact once and reference its owning file elsewhere.
- Keep task plans and validation reports in issues or PRs.

## Architecture

- The native application is a SpaceTerm Client. Server and client ownership is defined in [CONTEXT.md](CONTEXT.md#servers-and-clients).
- Keep protocol, core, terminal, host, server, client, client-native, and distribution crates headless. Native desktop effects and rendering belong in client adapters.
- Local IPC, direct WSS, and SSH Tunnel routes require pinned, paired-device SpaceTerm Protocol authentication in both supported major versions. OpenSSH reachability alone is not authorization.
- Production desktop clients do not launch remote PTYs. The owning server launches the shell on its machine.
- Put behavior, invariants, and cleanup in deep modules with narrow interfaces. Expose intentional operations and typed recoverable failures.
- Keep product policy portable. Reach frameworks, external systems, and operating system capabilities through narrow adapters chosen by constructor injection. Add a boundary only for durable replaceability or locality.
- Use Apple's Human Interface Guidelines for macOS 27 as the UI design baseline.

## Account service

- The production [server composition](crates/spaceterm-server/src/account_startup/production.rs) uses the owning nonroot account's service. The separate `spaceterm-server serve` executable hosts it; the headless client CLI does not.
- The macOS LaunchAgent starts at login when enabled and stops at logout. Its server survives desktop client quit or crash.
- The Linux systemd user service uses linger to start at boot and survive logout. Installation that starts the server and explicit Start require confirmed enabled linger.
- Server keys use account-private storage with `0700` directories and `0600` files. This is not protection against other processes running as the owning account.
- Release, Preflight, and Development have isolated server registrations. Network listening is off unless explicitly enabled in retained remote-access policy.
- Explicit Start restores automatic startup. Explicit Stop retains the stopped intent; client launch and reconnect do not override it.
- Server restart restores shared organization, not the previous running programs or terminal screen. Native startup can report unresolved cleanup when same-boot process ownership cannot be proved.
- For SSH Bootstrap integration, read the [public native providers](crates/spaceterm-client-native/src/bootstrap.rs) and [desktop enrollment composition](src/ui/settings_window/server_enrollment/native.rs). The providers implement explicit setup, but the default desktop lacks a foreground Bootstrap provider and owner-side public-device-claim import UI.

## Safety

- Errors and Terminal Diagnostics carry typed classifications and bounded metadata only. They never carry terminal or clipboard contents, environment values, paths, credentials, or raw native errors.
- SpaceTerm sends no automatic telemetry or crash reports.
- Local file actions require explicit local authority. Remote values grant none.
- Shipped releases are frozen. Before changing a retained format such as the Settings Document, confirm every shipped build reads the new form without data loss, or ship that handling in a release first.
- Issue #520 preserves Settings Document schema 7 and serialized Command IDs, including `close_workspace` for Kill Workspace. Server organization and Client View State remain outside preference import, export, and reset.

## Work

- Run commands with `mise run` and follow the task conventions in [`.mise.toml`](.mise.toml).
- For Linux work, connect with `ssh penguin`.
- Repositories live at `~/Projects/{SpaceTerm,zed,accesskit}` on both hosts. Clone missing forks from `github.com/sadiksaifi/{zed,accesskit}`.
