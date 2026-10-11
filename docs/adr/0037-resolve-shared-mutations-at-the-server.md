# Resolve shared mutations at the server

The SpaceTerm Server orders and validates shared mutations against its authoritative state before committing and [publishing them](0036-publish-workspace-changes-to-connected-clients.md). An edit identifies the version of the field it changes: when two clients rename the same Workspace from the same version, the first accepted rename succeeds and the second receives a typed conflict. The client preserves the rejected draft and presents the current name for explicit review before retrying; unrelated changes such as Tab creation do not invalidate the rename.

Independent Tab creation requests create distinct Tabs. A mutation targeting a killed Workspace fails without recreating it, and Worktree removal revalidates Git state and the affected terminal work before acting under [ADR 0019](0019-create-and-remove-worktrees-by-running-git.md). Shared state changes do not depend on which client has the affected Tab focused.

Retries carry the original operation identity so the server recognizes the operation and its outcome rather than creating a duplicate Tab or repeating a destructive action. Reconnecting clients reconcile with authoritative server state before retrying an unresolved edit.
