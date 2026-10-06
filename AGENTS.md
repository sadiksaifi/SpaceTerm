# SpaceTerm

SpaceTerm is a native desktop terminal multiplexer. Prior SpaceTerm versions impose no compatibility requirements.

## Sources of truth

- Terminology: use the definitions in [CONTEXT.md](CONTEXT.md).
- Architecture and authentication changes: read the relevant [ADRs](docs/adr/).
- Code, tests, and configuration own executable behavior. ADRs record durable tradeoffs.
- Write docs only for what code, configuration, or the UI cannot express, with one purpose per file in `docs/`.
- Keep task plans and validation reports in issues or PRs.

## Architecture

- The native application owns Workspace and Terminal Session lifecycles.
- Concentrate behavior, invariants, and cleanup in deep modules with narrow interfaces.
- Keep product policy and lifecycle portable. Connect frameworks, external systems, and operating system capabilities through narrow adapters selected by constructor injection.
- Add boundaries for durable replaceability or locality. Expose intentional operations and typed recoverable failures.
- UI: use macOS 27 and Apple's Human Interface Guidelines as the design baseline through Rust and GPUI. Platform adapters own irreducible operating system capabilities.
- Application directories: consume semantic paths from `platform::app_directories` and create directories only at write boundaries.

## Safety

- Keep errors and Terminal Diagnostics content-free: typed classifications and bounded metadata only. Exclude terminal and clipboard contents, environment values, paths, credentials, and raw native errors.
- SpaceTerm sends no automatic telemetry or crash reports.
- Retain explicit local filesystem authority. Remote values grant no authority for local file actions.

## Work

- Write Conventional Commits; [lefthook.yml](lefthook.yml) runs the repository's validator.
- [`.mise.toml`](.mise.toml) owns tools and commands. Use `mise run`; discover tasks with `mise tasks`.
- Define single-tool tasks in `.mise.toml`. Put task logic in standard-library Python file tasks under `mise-tasks/`, with shared code and `unittest` tests in `mise-tasks/lib`. Prefer maintained external tools to new task code.
- Name tasks `<entry>[:<narrowing>...][:<platform>]`. Use a CONTEXT term or the wrapped tool's name for each segment; create no aliases.
- Give each Development launch mode its own task. Use `usage` arguments or flags for other choices.
- End platform-only tasks with the platform segment. Shared names dispatch to `<task>:{{ os() }}` when implementations differ.
- Set environment variables with `env`, guard destructive tasks with `confirm`, and `hide` only internal tasks.
- Debug the source build; `/Applications/SpaceTerm.app` may be stale.
