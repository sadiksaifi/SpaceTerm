# Give one client control of each Pane

Each server-owned Pane has at most one Controlling Client so terminal input and dimensions come from one client. The creating client chooses a new Pane's initial control: itself, the Controlling Client of the Pane the request came from, or no client. The server revalidates the origin's control at commit; an origin without a controller leaves the new Pane unclaimed. Different clients can control different Panes concurrently. These rules apply equally to clients on the same machine and on different machines.

Desktop clients without control present a Takeover Screen that replaces terminal output entirely and centers a "Take Over" button using SpaceTerm's existing blue primary-button style. Take Over transfers control immediately without approval from the previous Controlling Client, so handoff works when that client is asleep or elsewhere. [Observation](0059-observe-panes-without-taking-control.md) grants no control and shows an `Observed by <Client Name>` Pane Caption while an observer channel is open.

Whenever the server offers Take Over, it names the Controlling Client by its Client Name or says that no client controls the Pane, and the Takeover Screen shows that. Each client supplies its Client Name at admission: its computer's name from the operating system, followed by a named Development client's name, by "CLI" for the command-line client, and by an Agent Name when one is given. Both peers keep a Client Name printable and at most 48 characters, without control or bidirectional characters or invisible format characters other than the joiners that shape Persian, Indic and emoji text. A CLI Client Name truncates only the computer name, preserving " CLI" and the complete Agent Name suffix. The server keeps the latest name of each Client Identity while it runs, up to 256 identities. A Client Name is presentation metadata. Any client can supply any name, so it identifies nothing and grants no authority.

Selecting an unclaimed Pane by clicking it or navigating to it with the keyboard requests control automatically. The server grants control only while the Pane remains unclaimed; if another client acquires it first, the selecting client presents the Takeover Screen. Selecting a Pane controlled by another client requires Take Over, including when that client is disconnected.

Opening a Tab also applies this acquisition rule to that client's Focused Pane in the opened Tab. It does not acquire control of other Panes or of Focused Panes remembered in inactive Tabs.

Once a client has focused a Pane and acquired control, switching Tabs preserves that control.

Detach Workspace releases that client's control of every Pane in the Workspace. Reattaching does not reinstate those released control grants.

While the server remains running, a reconnecting client resumes control of its previous Panes only if no other client has taken them over and it has not explicitly released control. This rule also applies when a client intentionally quits its GUI and later relaunches it. Terminal Sessions continue running at their last dimensions while disconnected or unclaimed. Reconnection preserves a deliberate takeover.

Connection-loss presentation and input handling follow [ADR 0045](0045-freeze-terminal-screens-while-reconnecting.md).

Take Over can start a retained Pane's shell, so it obeys [ADR 0043](0043-start-restored-shells-on-first-use.md)'s first-use and unresolved-start rules. Transferring control does not authorize a replacement for an unconfirmed original start under [ADR 0037](0037-resolve-shared-mutations-at-the-server.md).

A Pane whose Terminal Session ended shows every client the same Session Ended screen instead of the Takeover Screen, under [ADR 0053](0053-keep-a-pane-whose-shell-exits-unsuccessfully.md).
