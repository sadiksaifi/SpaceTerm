# SpaceTerm

SpaceTerm is a native desktop terminal multiplexer.

## Domain and decisions

- Domain behavior and terminology: read [`CONTEXT.md`](CONTEXT.md) and use its canonical terms in
  code, tests, issues, and documentation.
- Architecture and remote-authentication changes: read the relevant records under
  [`docs/adr/`](docs/adr/).

Code and tests own implementation detail and executable invariants. ADRs own the rationale for
important durable decisions.

## Architecture

- Design deep Modules with narrow Interfaces that concentrate behavior, invariants, and cleanup
  with their owner.
- Keep product policy and lifecycle portable. Connect frameworks, external systems, and
  Operating-System capabilities at narrow Seams.
- Select replaceable capabilities through constructor injection and keep ownership explicit.
- Expose intentional operations and typed recoverable failures.
- Create structural boundaries when they provide durable replaceability, Leverage, or Locality.

## Safety

- Keep errors and Local Diagnostics content-free. Use typed classifications and bounded metadata;
  exclude terminal and clipboard contents, environment values, paths, credentials, and raw native
  errors. SpaceTerm sends no automatic telemetry or crash reports.
- Keep local filesystem authority explicit and retained. Remote values remain remote and provide no
  authority for local file actions.

## Work

- Use the tasks in `.mise.toml` through `mise run` as the command authority.
- Keep generic mise tasks platform-neutral. Give every Operating-System-specific task an explicit
  platform segment and every Operating-System-specific script an explicit platform marker.
- Debug the source build; `/Applications/SpaceTerm.app` may be stale.
