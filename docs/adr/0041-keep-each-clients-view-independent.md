# Keep each client's view independent

Beyond the independent Active Tab in [ADR 0024](0024-keep-tab-selection-independent-per-client.md), each SpaceTerm Client owns its Active Workspace, Focused Pane and focus history, Zoomed Pane, Selection, Terminal Find state, and scroll position. Operating-System Window geometry and sidebar presentation also remain client-local, so one client inspecting or zooming terminal work does not move another client's view.

[ADR 0021](0021-present-workspaces-from-several-servers-together.md) defines each client's Workspace display order. [ADR 0042](0042-keep-tab-order-and-pane-layouts-specific-to-each-client.md) defines its persistent Tab display order and Pane Layouts.

Desktop presentation retains the single Workspace Window in [ADR 0046](0046-keep-one-workspace-window-per-desktop-client.md).

The server owns terminal content and Scrollback; clients present independent views of that content while respecting [Pane control and the Takeover Screen](0022-give-one-client-control-of-each-pane.md). Shared creation and deletion reconcile each client's view against the remaining targets without copying the initiating client's focus or zoom changes to other clients.

Membership reconciliation is independent of keyboard ownership. A later Workspace or Tab selection, sidebar edit, Terminal Find or chooser focus wins over an older operation's completion. Pane navigation focuses the rendered terminal or replacement screen. Explicit Workspace opening selects its restored Focused Pane; passive sidebar navigation starts no shell. An initiating split validates the restored allocation and reveals the resulting layout before requesting membership. Another client's membership never inherits this client's viewport limits.

Architecture and transport changes preserve released shortcuts, focus, layout and interaction outcomes unless the owner approves a product change. [ADR 0037](0037-resolve-shared-mutations-at-the-server.md) owns optimistic mutation presentation and uncertainty.
