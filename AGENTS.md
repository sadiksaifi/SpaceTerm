# SpaceTerm

A native desktop terminal multiplexer for macOS and Linux, coming soon for Windows.

## Sources of truth

- Use the terms defined in [CONTEXT.md](CONTEXT.md).
- Read the relevant [ADRs](docs/adr/) before changing architecture, authentication, behavior, shortcuts, presentation, or lifecycle.
- Write docs only for what code, configuration, or the UI cannot express. State each fact once and reference its owning file elsewhere.
- Keep task plans and validation reports in issues or PRs.

## Architecture

- Each SpaceTerm Server owns its Workspaces, Tabs, Panes, and Terminal Sessions. Each SpaceTerm Client owns its presentation and client view over account-local IPC or paired QUIC connections. SSH is temporary provisioning authority inside the desktop's Add Server.
- Put behavior, invariants, and cleanup in deep modules with narrow interfaces. Expose intentional operations and typed recoverable failures.
- Keep product policy portable. Reach frameworks, external systems, and operating system capabilities through narrow adapters chosen by constructor injection. Add a boundary only for durable replaceability or locality.
- Use Apple's Human Interface Guidelines for macOS 27 as the UI design baseline.

## Safety

- Errors and Terminal Diagnostics carry typed classifications and bounded metadata only. They never carry terminal or clipboard contents, environment values, paths, credentials, labels, or raw native errors.
- SpaceTerm sends no automatic telemetry or crash reports.
- Local file actions require explicit local authority. Remote values grant none.
- Replace obsolete server-client formats directly, including Settings and serialized Commands. Add no migrations, legacy readers, dual transports or SSH runtime fallbacks.

## Work

- Run commands with `mise run` and follow the task conventions in [`.mise.toml`](.mise.toml).
- Parallelize when savings exceed coordination; agree interfaces and ownership, then integrate early.
- Reuse delegates when role and context fit; brief with goals, agreed constraints and evidence, leaving open decisions to them.
- Coordinate shared caches and native resources per host; separate baseline artifacts and overlap independent checks and reviews.
- Run scoped tests, then required acceptance checks; reuse evidence only while relevant code, artifacts, configuration and environment still match.
- Before any user-requested commit, push, or PR merge into main, disclose any known pre-existing test or check failures unrelated to your changes.
- For local server setup or incompatible development state, follow [the local workflow](docs/local-server.md).
- For Linux work and native verification, use Penguin directly: `ssh penguin` for commands and Remora for its desktop.
- Repositories live at `~/Projects/{SpaceTerm,zed,accesskit}` on both hosts. Clone missing forks from `github.com/sadiksaifi/{zed,accesskit}`.

## GitHub Project Board

The board has four states:

- `Backlog`: Issue created. Managed by GitHub.
- `In Progress`: Implementation started. Managed by the agent (you).
- `In Review`: Work under review by an agent (you), subagent, or external reviewer, or a PR awaiting review. Managed by the agent (you).
- `Done`: PR merged. Managed by GitHub.

When starting an issue, assign yourself using `gh issue edit <number> --add-assignee @me` and move it to `In Progress`. Keep it there while implementing, even with an open PR. Move it to `In Review` when review begins, even without a PR.
