# Order terminal clipboard access by Terminal Session focus

Clipboard Authority belongs to the originating Pane's Terminal Input Focus, and reads need explicit permission, because terminal output can come from local or remote programs. Clipboard requests use a bounded asynchronous channel because blocking the worker on a UI reply would deadlock a synchronous Selection query.

A program that copies its own selection writes the clipboard of the computer it runs on, which is not the presenting client's when SpaceTerm Core is on another Machine. [The shell launch policy](../../src/platform/shell_launch.rs) therefore sets `PI_CLIPBOARD_OSC52=1` and Herdr's `HERDR_ENV=1` in every Terminal Session, so pi and Codex also send each copy as OSC 52 under this authority. Codex offers no program-neutral marker, so a program that checks `HERDR_ENV` believes it runs inside Herdr.
