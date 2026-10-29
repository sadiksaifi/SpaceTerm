# Derive agent identities at admission

An Agent Name gives a recurring automation actor its own Pane control and operation history without another enrollment. Names follow `^[a-z0-9][a-z0-9-]{0,31}$` and are intended for reuse. `--as` or `SPACETERM_AS` supplies the name. Admission derives the effective identity as `SHA-256("spaceterm.agent.v1" || base_client_id || agent_name)` on local and paired routes. [ADR 0028](0028-use-spaceterm-protocol-over-every-client-connection.md) owns its authority and pairing boundary; [ADR 0022](0022-give-one-client-control-of-each-pane.md) owns Client Name presentation.

The CLI retains a base identity per server route in its own `clients/cli/` namespace. Each effective identity has separate Workspace Attachments, retained inventory and journals under that route. CLI connection-store updates lock the complete read-modify-write transaction, so concurrent enrollments cannot overwrite each other. [CLI state](../../crates/spaceterm-cli/src/state.rs), [client directory binding](../../crates/spaceterm-server/src/client.rs) and [the connection store](../../crates/spaceterm-connections/src/store.rs) own this layout beneath the account-private server data root:

```text
clients/cli/
  connections.json, connections.lock
  routes/<route>/                 local or the 64-hex paired route ID
    base/
      client.json                 local route's base Client Identity
      operations.json, operations.lock
      attachments.json
      runs.json, runs.lock
      inventory.json              paired route only
      route.json, route.lock      paired route only
      last-connected.json         latest service-lane connection on this route
      service-operations.json, service-operations.lock
    agents/<name>/                same identity files except client.json and last-connected.json
```

Files are created as their operations need them. Every identity's pending runs use `runs.lock`, independently of `operations.lock`. Paired identities also bind their retained pairing through `route.json` under `route.lock`. `base/last-connected.json` records the Unix time when any identity on the route last opened its service lane and completed settlement. [The CLI service adapter](../../crates/spaceterm-cli/src/server.rs) owns that timestamp and pending service-operation IDs. An unreachable status reports that dated observation without inferring shutdown.

A shared mutation holds the existing Original Operation writer lock from sequence allocation through dispatch, reply, journal retirement and acknowledgement. Mutations under one identity run one at a time; distinct Agent Names allow independent writers. Run receipts use a separate lock and journal. [The terminal command entry point](../../crates/spaceterm-cli/src/io/start.rs) also holds the shared writer lock across settlement and execution of `run`, `send` and `release`, through [the dispatch adapter](../../crates/spaceterm-cli/src/work/dispatch.rs). `read` uses passive settlement and skips active writers. Process exit retains Pane control; `pane release` relinquishes that identity's grant.

Sending an acknowledgement does not confirm it. After validating and retiring an Original Operation's outcome, [the CLI journal](../../crates/spaceterm-cli/src/journal.rs) sends its acknowledgement and calls `Resolve` until the server answers `Missing`. Only that later answer permits forgetting the retired outcome. Confirmation waits for at most two seconds within the command deadline. A lost or unconfirmed acknowledgement leaves the outcome for a later command to acknowledge again.

Before its own work, each command admitted to the work lane settles leftover journals for every CLI identity held for that server, skipping locks held by active processes. Recovery resolves original operations and runs without replaying input or destructive dispatch. Structured recovered results carry their operational data; warnings stay bounded. Status and connection inventory expose held counts across agent-name reuse. Service-lane commands also attempt work-journal settlement; a different work major leaves those journals for a compatible client. Pending service operations settle for the current identity and appear as service results in `data.recovered_operations`. [ADR 0037](0037-resolve-shared-mutations-at-the-server.md) owns server outcome retention; [ADR 0057](0057-administer-servers-over-the-service-lane.md) owns service receipts.

Workspace, Tab and Split creation with an Agent Name gives the agent control. Creation from a validated Terminal Session without an Agent Name assigns the origin Pane's current controller; the server sends Reveal only to that controller. The desktop applies Reveal only while the origin is still focused and no newer selection exists. Other CLI creation leaves the new Pane unclaimed. Worktree creation follows [ADR 0055](0055-define-the-cli-output-contract-and-scope-model.md)'s creator-controlled Tab policy. These choices use the control policies in [ADR 0022](0022-give-one-client-control-of-each-pane.md) and preserve each client's view under [ADR 0041](0041-keep-each-clients-view-independent.md).

## Shell context

Server Terminal Sessions receive `SPACETERM_IDENTITY`, `SPACETERM_SERVER_ID`, `SPACETERM_PANE`, `SPACETERM_SESSION` and `SPACETERM_CLI`. [The shell launch planner](../../src/platform/shell_launch.rs) removes inherited values before applying the Session's own context. These values are nonsecret fences, not authority. The CLI accepts current context only after matching the application identity, admitted server and Pane in [the context resolver](../../crates/spaceterm-cli/src/scope/current.rs).

Only enabled shell integration prepends the directory containing `SPACETERM_CLI` to PATH. The [integration scripts](../../assets/shell-integration/) apply it after user startup configuration. Disabled integration leaves PATH alone while retaining the Session context. [The integration planner](../../src/platform/shell_integration.rs) starts Fish with `--features no-mark-prompt`, so Fish's own unowned markers do not replace SpaceTerm's Prompt Owner markers. [Nushell integration](../../assets/shell-integration/nushell/vendor/autoload/spaceterm.nu) emits its own owner-marked `133;D;<LAST_EXIT_CODE>` in `pre_prompt`, only after `pre_execution` marked a command as started. [ADR 0055](0055-define-the-cli-output-contract-and-scope-model.md) owns the run admission rules that consume these markers.
