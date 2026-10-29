# Keep a Pane whose shell exits unsuccessfully

When a Pane's shell exits with code 0, the SpaceTerm Server closes the Pane for every client. A person who types `exit` or presses Control-D at an idle prompt asks for the Pane to close, as Terminal's default "Close if the shell exited cleanly" and other terminals treat it. Closing the last Pane closes its Tab, and closing the last Tab removes its Workspace, as [ADR 0023](0023-allow-confirmed-tab-closure-across-clients.md) describes for Close Pane.

When the shell exits with any other code or after a signal, the server keeps the Pane, its Tab, its Workspace and its control grant, and discards the Terminal Session's output and Scrollback. A crash, a failed login script or a killed shell would otherwise remove the Pane before anyone could read why. A shell that runs `exit` after a failed command also exits with that command's code, so its Pane stays too.

Every client presents the kept Pane as "Session Ended". Its detail names the cause: "The shell exited with code 1." for an exit code, and "A signal ended the shell." for a signal. The server sends the exit code as bounded typed metadata. It never sends the shell's output or the native signal text.

Every desktop client offers a "Start Session" button in SpaceTerm's blue primary-button style and a "Close Pane" link. Nothing runs in an ended Pane, so a client without its control grant shows the same screen instead of the Takeover Screen. Its Start Session takes over and starts a fresh shell in one request, which the server coordinates under [ADR 0043](0043-start-restored-shells-on-first-use.md) so concurrent starts cannot create two Terminal Sessions. Close Pane follows [ADR 0023](0023-allow-confirmed-tab-closure-across-clients.md). A Pane that End All Sessions ended follows the same presentation and actions, with the detail its own cause gives under [ADR 0048](0048-end-all-sessions-from-the-selected-servers-settings.md).

The CLI's explicit `pane run` also performs Start Session on an ended Pane, using the same coordinated start and directory-repair rules. `pane send` requires an existing Terminal Session; `pane read` uses [Observation](0059-observe-panes-without-taking-control.md). Neither starts a shell.

The screen and its cause stay until Start Session or Close Pane is requested, or the Pane closes. Selecting or focusing the Pane, switching to its Tab or Workspace, restoring window focus, opening a window and reconnecting start no shell. Keys go to the screen, never to a shell, and Return activates Start Session only once it has focus.

Closing the Pane, its Tab or its Workspace removes it like any other Pane. End All Sessions ends no Session in it and leaves its cause unchanged. The cause belongs to the running server. After a server restart the Pane is a retained Pane under ADR 0043, like every other Pane whose shell ended with the server.
