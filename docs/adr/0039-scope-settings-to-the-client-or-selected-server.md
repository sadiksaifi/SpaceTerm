# Scope Settings to the client or selected server

For the planned [server architecture](https://github.com/sadiksaifi/SpaceTerm/issues/520), Client Settings remain specific to the client device, while Server Settings belong to the selected SpaceTerm Server. One Settings Window retains the existing client preference sections and adds a searchable Servers section for connections, paired devices, compatibility, server management, and server-specific settings, so every operation identifies the machine it affects.

End All Sessions follows [ADR 0048](0048-end-all-sessions-from-the-selected-servers-settings.md).

Interface, Font, Themes, Keybindings, clipboard preferences, Git presentation preferences, and client update preferences remain client-owned. Clipboard preferences govern that client's clipboard under [ADR 0013](0013-order-terminal-clipboard-access-by-session-focus.md) and the Pane control rules in [ADR 0022](0022-give-one-client-control-of-each-pane.md). GitHub CLI authentication and Pull Request lookups retain the client-local ownership in [ADR 0018](0018-read-repository-status-by-running-git-and-the-github-cli.md).

Git discovery and execution use the owning server's tools and filesystem. New Worktree Location becomes a server-owned preference shared by its authorized clients, and its template expands and validates in that server's filesystem context. System Permissions remain operating-system capabilities on the machine where the terminal program uses the protected resource, following [ADR 0014](0014-guide-permission-setup-on-system-settings.md); a client's permission setup cannot grant another machine's permissions.

Advanced import, export, and reset operate on Client Settings and installed Terminal Themes. Server identity, device credentials and authorization, pending unpair requests, Workspace Attachments, Client View State, server-owned Workspace organization, and deliberate Stop Server state remain outside the Settings Document, so preference transfer or reset cannot change those lifecycles. Server update restarts retain the confirmation requirement in [ADR 0034](0034-confirm-server-update-restarts-and-explain-incompatibility.md).
