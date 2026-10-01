# Order terminal clipboard access by Session focus

Terminal programs need the system clipboard in Local and Remote Panes, including fullscreen
interfaces and nested multiplexers. SpaceTerm recognizes OSC 52 before Terminal Emulation and
consumes each clipboard sequence once. The worker preserves the request's order relative to
terminal output and replies.

Terminal Clipboard Access requires the originating Pane's Terminal Input Focus. Copying is
allowed by default; reading requires an explicit Privacy Setting. Empty, `c`, `p`, and `s` selectors
use the system text clipboard. Read replies preserve the selector and BEL or ST terminator.
Denied, expired, oversized, or unavailable reads return empty text. Plain text is bounded to 1 MiB.
Remote programs receive no Local Filesystem Authority from clipboard access.

The Terminal Session retains a focus grant that changes on focus transitions and is revoked on
retirement and shutdown. The UI checks the grant and current policy immediately before consulting
the injected text Clipboard Adapter. Clipboard requests use a separate bounded channel because
Screen publication can replace older events.

One request can be outstanding per Session. The worker defers subsequent output effects and uses
PTY backpressure while continuing to serve input, Selection queries, and shutdown. A one-second
deadline releases the output barrier if the UI cannot respond. Blocking the worker on a UI reply
would deadlock when the UI synchronously requests a Selection.

SpaceTerm does not install remote helpers or edit application and multiplexer configurations.
Multiplexers own forwarding from nested programs to the outer terminal.
