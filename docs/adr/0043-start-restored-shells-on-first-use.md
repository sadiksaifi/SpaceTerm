# Start restored shells on first use

After a SpaceTerm Server restarts, it immediately restores its saved [Workspace organization](0031-retain-workspace-organization-across-server-restarts.md). Each retained Pane starts a fresh shell when a client first selects it and holds control under [ADR 0022](0022-give-one-client-control-of-each-pane.md). Opening a Tab or Workspace the person chose applies this rule to that client's Focused Pane; other retained Panes wait until selected, so unused Panes require no shell processes. A Workspace that becomes the Active Workspace only because the person killed or detached the one in use takes the keyboard without selecting a Pane, so its Panes also wait.

The first Controlling Client supplies the initial terminal dimensions. The server coordinates control acquisition and startup so concurrent clients cannot create duplicate Terminal Sessions for the same Pane. The previous running programs and in-memory terminal state ended with the server under [ADR 0030](0030-start-servers-at-boot-independently-of-clients.md).

[ADR 0044](0044-restore-panes-in-their-last-confirmed-directory.md) defines directory selection and recovery for restored shells.

First use does not apply to a Pane whose Terminal Session ended in the running server. That Pane waits for Start Session under [ADR 0053](0053-keep-a-pane-whose-shell-exits-unsuccessfully.md).

Reconnecting to a running server reuses its existing Terminal Sessions. Once a restored shell starts, it has the independent server-owned lifetime in [ADR 0030](0030-start-servers-at-boot-independently-of-clients.md).

After a lost shell-start acknowledgement, a fully reopened client first observes the authenticated server's existing Pane and Terminal Session state. An unresolved original start permits no replacement start, including through Take Over. [ADR 0037](0037-resolve-shared-mutations-at-the-server.md) owns operation uncertainty; reconnecting or replacing a presentation does not settle it.
