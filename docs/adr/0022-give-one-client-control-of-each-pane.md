# Give one client control of each Pane

Each server-owned Pane has at most one Controlling Client so terminal input and dimensions come from one client. The client that creates a Pane is its initial Controlling Client. Different clients can control different Panes concurrently. These rules apply equally to clients on the same machine and on different machines.

Other clients present a Takeover Screen that replaces terminal output entirely and centers a "Take Over" button using SpaceTerm's existing blue primary-button style. Take Over transfers control immediately without approval from the previous Controlling Client, so handoff works when that client is asleep or elsewhere.

Selecting an unclaimed Pane by clicking it or navigating to it with the keyboard requests control automatically. The server grants control only while the Pane remains unclaimed; if another client acquires it first, the selecting client presents the Takeover Screen. Selecting a Pane controlled by another client requires Take Over, including when that client is disconnected.

Opening a Tab also applies this acquisition rule to that client's Focused Pane in the opened Tab. It does not acquire control of other Panes or of Focused Panes remembered in inactive Tabs.

Once a client has focused a Pane and acquired control, switching Tabs preserves that control.

Detach Workspace releases that client's control of every Pane in the Workspace. Reattaching does not reinstate those released control grants.

While the server remains running, a reconnecting client resumes control of its previous Panes only if no other client has taken them over and it has not explicitly released control. This rule also applies when a client intentionally quits its GUI and later relaunches it. Terminal Sessions continue running at their last dimensions while disconnected or unclaimed. Reconnection preserves a deliberate takeover.

Connection-loss presentation and input handling follow [ADR 0045](0045-freeze-terminal-screens-while-reconnecting.md).
