# SpaceTerm

A native desktop terminal multiplexer for macOS and Linux, coming soon for Windows.

## Sources of truth

- Use the terms defined in [CONTEXT.md](CONTEXT.md).
- Read the relevant [ADRs](docs/adr/) before changing architecture or authentication.
- Write docs only for what code, configuration, or the UI cannot express. State each fact once and reference its owning file elsewhere.
- Keep task plans and validation reports in issues or PRs.

## Architecture

- The account-local SpaceTerm Server owns local Workspaces, Tabs, Panes, and Terminal Sessions. The desktop owns their presentation and client view. Production SSH Workspaces retain native ownership until their server integration lands.
- Put behavior, invariants, and cleanup in deep modules with narrow interfaces. Expose intentional operations and typed recoverable failures.
- Keep product policy portable. Reach frameworks, external systems, and operating system capabilities through narrow adapters chosen by constructor injection. Add a boundary only for durable replaceability or locality.
- Use Apple's Human Interface Guidelines for macOS 27 as the UI design baseline.

## Safety

- Errors and Terminal Diagnostics carry typed classifications and bounded metadata only. They never carry terminal or clipboard contents, environment values, paths, credentials, or raw native errors.
- SpaceTerm sends no automatic telemetry or crash reports.
- Local file actions require explicit local authority. Remote values grant none.
- Shipped releases are frozen. Before changing a retained format such as the Settings Document, confirm every shipped build reads the new form without data loss, or ship that handling in a release first.

## Work

- Run commands with `mise run` and follow the task conventions in [`.mise.toml`](.mise.toml).
- For local server setup or incompatible development state, follow [the local workflow](docs/local-server.md).
- For Linux work, connect with `ssh penguin`.
- Repositories live at `~/Projects/{SpaceTerm,zed,accesskit}` on both hosts. Clone missing forks from `github.com/sadiksaifi/{zed,accesskit}`.
