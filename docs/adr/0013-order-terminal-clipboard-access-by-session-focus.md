# Order terminal clipboard access by Session focus

Terminal programs need the system clipboard in Local and Remote Panes, including fullscreen
interfaces and nested multiplexers. SpaceTerm recognizes OSC 52 before Terminal Emulation and
consumes each clipboard sequence once. The worker preserves the request's order relative to
terminal output and replies.

Terminal Clipboard Access requires the originating Pane's Terminal Input Focus. Copying is
allowed by default; reading requires an explicit Privacy Setting. The injected host capability resolves
selection targets after these checks. On Linux, empty and `c` use CLIPBOARD; `p` and `s` use PRIMARY.
On macOS, all four retain their system clipboard alias. Unsupported selectors, including cut buffers
`0` through `7`, are consumed without accessing any selection. An unavailable PRIMARY never falls
back to CLIPBOARD. Read replies preserve the selector and BEL or ST terminator.
Denied, expired, oversized, or unavailable reads return empty text. Plain text is bounded to 1 MiB.
Native text reads negotiate only text representations and preserve valid UTF-8 bytes, including
line endings, without requesting images or files or synthesizing file paths. Remote programs
receive no Local Filesystem Authority from clipboard access.

Wayland writes require a native key or pointer press serial and the requested selection protocol.
Missing prerequisites return a typed, content-free availability failure before retained contents
change. Terminal Input Focus alone does not provide a selection serial.

The Terminal Session retains a focus grant that changes on focus transitions and is revoked on
retirement and shutdown. The UI checks the grant and current policy immediately before consulting
the injected text Clipboard Adapter. Clipboard requests use a separate bounded channel because
Screen publication can replace older events.

One request can be outstanding per Session. The worker defers subsequent output effects and uses
PTY backpressure while continuing to serve input, Selection queries, and shutdown. A one-second
deadline releases the output barrier if the UI cannot respond. Blocking the worker on a UI reply
would deadlock when the UI synchronously requests a Selection.

PTY input uses nonblocking writes and a bounded ordered queue. A program that stops reading
must not prevent the worker from answering synchronous Selection queries. Clipboard replies,
terminal replies, and user input share this queue so backpressure cannot interleave their bytes.

SpaceTerm does not install remote helpers or edit application and multiplexer configurations.
Multiplexers own forwarding from nested programs to the outer terminal.
