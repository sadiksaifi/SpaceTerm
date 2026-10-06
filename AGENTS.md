# SpaceTerm

SpaceTerm is a native desktop terminal multiplexer.

## Sources of truth

- Use the terms defined in [CONTEXT.md](CONTEXT.md).
- Read the relevant [ADRs](docs/adr/) before changing architecture or authentication.
- Write docs only for what code, configuration, or the UI cannot express. State each fact once and reference its owning file elsewhere.
- Keep task plans and validation reports in issues or PRs.

## Architecture

- The native application owns Workspace and Terminal Session lifecycles.
- Put behavior, invariants, and cleanup in deep modules with narrow interfaces. Expose intentional operations and typed recoverable failures.
- Keep product policy portable. Reach frameworks, external systems, and operating system capabilities through narrow adapters chosen by constructor injection. Add a boundary only for durable replaceability or locality.
- Use Apple's Human Interface Guidelines for macOS 27 as the UI design baseline.

## Safety

- Errors and Terminal Diagnostics carry typed classifications and bounded metadata only. They never carry terminal or clipboard contents, environment values, paths, credentials, or raw native errors.
- SpaceTerm sends no automatic telemetry or crash reports.
- Local file actions require explicit local authority. Remote values grant none.

## Work

- Run commands with `mise run` and follow the task conventions in [`.mise.toml`](.mise.toml).
